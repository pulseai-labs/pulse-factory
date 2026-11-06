//! The rules: what a parsed command line does, and whether a role may do it.

use std::collections::HashMap;
use std::path::{Component, Path, PathBuf};

use pf_engine::status::redact;
use pf_workflow::rules::{Allow, RoleRules};

use crate::grammar;
use crate::options;
use crate::shell::{Cmd, Parsed, Word, is_keyword, parse};
use crate::{Context, Part, PushIntent};

/// The most levels of `bash -c` inside `bash -c` that are read.
const MAX_DEPTH: usize = 6;

/// A rule that fired, with its reason.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Finding {
    pub rule: String,
    pub reason: String,
}

fn finding(rule: &str, reason: impl Into<String>) -> Finding {
    Finding {
        rule: rule.to_string(),
        reason: reason.into(),
    }
}

/// Everything that one tool call does.
#[derive(Default)]
pub struct Walk {
    pub denies: Vec<Finding>,
    /// Forms outside the grammar (`crate::grammar`). They deny the line after every guard has had
    /// its say: `decide` reads them only when `denies` is empty.
    pub outside: Vec<Finding>,
    pub allows: Vec<Finding>,
    pub pushes: Vec<PushIntent>,
}

/// A command with its wrappers removed.
struct Final {
    words: Vec<Word>,
    /// True when the command takes its operands from standard input (`xargs`).
    stdin_operands: bool,
}

pub fn basename(text: &str) -> &str {
    text.rsplit('/').next().unwrap_or(text)
}

/// The role that may read the forge and never write to it (ADR-0005, `role-rules.md`). Every `gh`
/// command of this role must be a read on the list in `gh_read_only`; the other roles meet only
/// the `gh` denials of every seat.
const READ_ONLY_FORGE_ROLE: &str = "reviewer";

/// Whether `gh <first> <second>` is a forge read. `gh api` is judged by its method elsewhere. A
/// command that is not named here is not a read: `gh pr create`, `gh issue create`,
/// `gh release create`, `gh workflow run`, `gh run download` (it writes files) and the rest.
fn gh_read_only(first: Option<&str>, second: Option<&str>) -> bool {
    matches!(
        (first, second),
        (None, _)
            | (Some("search" | "status" | "version" | "help"), _)
            | (Some("auth"), Some("status"))
            | (
                Some(
                    "pr" | "issue" | "run" | "release" | "repo" | "workflow" | "label" | "ruleset"
                ),
                Some("view" | "list" | "diff" | "checks" | "status"),
            )
    )
}

const SHELLS: [&str; 8] = ["sh", "bash", "zsh", "dash", "ksh", "ash", "csh", "tcsh"];

/// Commands that run another command, with options that this crate does not read. A call that
/// uses one is denied, because the command it runs could be any.
const UNREAD_WRAPPERS: [&str; 33] = [
    "taskset",
    "chrt",
    "setpriv",
    "unshare",
    "nsenter",
    "strace",
    "ltrace",
    "su",
    "runuser",
    "sg",
    "script",
    "busybox",
    "systemd-run",
    "chroot",
    "watch",
    "parallel",
    "pkexec",
    "fakeroot",
    "bwrap",
    "firejail",
    "cgexec",
    "eatmydata",
    "setarch",
    "linux32",
    "linux64",
    "prlimit",
    "choom",
    "numactl",
    "cpulimit",
    "xvfb-run",
    "torsocks",
    "proxychains",
    "proxychains4",
];

// ---------------------------------------------------------------- walking

/// What the call does to the working directory and the branch, in text order.
struct Flow {
    /// Every directory a relative path may mean: the working directory, and the directory of
    /// each literal `cd` so far. A subshell is not told apart, so a path must hold in all of them.
    cwds: Vec<PathBuf>,
    /// The raw target of a `cd` that cannot be read. After it a relative path has no meaning.
    cwd_unknown: Option<String>,
    /// The branch that a literal `git switch` or `git checkout` set, or the raw text of one that
    /// cannot be read.
    switched: Option<Part>,
}

/// The directory of a push, as far as the command lets us read it.
enum PushDir {
    /// Every directory the push may run in lies inside the worktree or the push directory.
    Inside,
    /// A directory lies outside both. The text says which.
    Outside(String),
    /// A variable, or a `cd` to one, names the directory.
    Unread,
}

pub struct Analyzer<'a> {
    pub rules: &'a RoleRules,
    pub ctx: &'a Context,
    pub walk: Walk,
    /// For each variable that the call assigns: true while every assignment is `mktemp` or the
    /// empty string. A first pass over the call fills it, so an assignment after its use counts.
    pub vars: HashMap<String, bool>,
    /// A `declare -n` makes any variable an alias of another. No variable is then known.
    poisoned: bool,
    flow: Flow,
}

impl<'a> Analyzer<'a> {
    pub fn new(rules: &'a RoleRules, ctx: &'a Context) -> Analyzer<'a> {
        Analyzer {
            rules,
            ctx,
            walk: Walk::default(),
            vars: HashMap::new(),
            poisoned: false,
            flow: Flow {
                cwds: vec![ctx.cwd.clone()],
                cwd_unknown: None,
                switched: None,
            },
        }
    }

    /// An analyzer for the second pass, with the variables that the first pass saw.
    pub fn with_vars(
        rules: &'a RoleRules,
        ctx: &'a Context,
        vars: HashMap<String, bool>,
        poisoned: bool,
    ) -> Analyzer<'a> {
        let mut a = Analyzer::new(rules, ctx);
        a.vars = vars;
        a.poisoned = poisoned;
        a
    }

    /// Read the text twice: the first pass learns every assignment of the call, wherever it sits,
    /// and the second judges.
    pub fn judged(rules: &'a RoleRules, ctx: &'a Context, src: &str) -> Analyzer<'a> {
        let mut first = Analyzer::new(rules, ctx);
        first.text(src, 0);
        let poisoned = first.poisoned;
        let mut second = Analyzer::with_vars(rules, ctx, first.vars, poisoned);
        second.text(src, 0);
        second
    }

    fn note_var(&mut self, name: &str, safe: bool) {
        let e = self.vars.entry(name.to_string()).or_insert(true);
        *e = *e && safe;
    }

    /// Record the assignments of a parsed text.
    fn record_assigns(&mut self, parsed: &Parsed) {
        for cmd in &parsed.cmds {
            for a in &cmd.assigns {
                if let Some((name, value, append)) = split_assign(&a.raw) {
                    self.note_var(&name, !append && safe_value(&value));
                }
            }
            if let (true, Some(v)) = (cmd.loop_header, cmd.head.first()) {
                self.note_var(&v.text, false);
            }
            for w in cmd.words.iter().chain(&cmd.assigns).chain(&cmd.head) {
                for name in default_assigned(&w.raw) {
                    self.note_var(&name, false);
                }
            }
            let Some(first) = cmd.words.first() else {
                continue;
            };
            if first.dynamic {
                continue;
            }
            let args = &cmd.words[1..];
            match basename(&first.text) {
                "read" | "mapfile" | "readarray" => {
                    let mut i = 0;
                    while i < args.len() {
                        let t = args[i].text.as_str();
                        if t.starts_with('-') && t.len() > 1 {
                            if t[1..].chars().any(|c| "pdnNutaCcOs".contains(c))
                                && t.len() == 2
                                && !matches!(t, "-r" | "-s" | "-e" | "-a")
                            {
                                i += 1;
                            }
                            i += 1;
                            continue;
                        }
                        self.note_var(&args[i].text, false);
                        i += 1;
                    }
                }
                "getopts" => {
                    if let Some(v) = args.get(1) {
                        self.note_var(&v.text, false);
                    }
                }
                "printf" => {
                    let named = args
                        .iter()
                        .position(|a| a.text == "-v")
                        .and_then(|k| args.get(k + 1));
                    if let Some(v) = named {
                        self.note_var(&v.text, false);
                    }
                }
                "export" | "declare" | "typeset" | "local" | "readonly" | "let" => {
                    for a in args {
                        let t = a.text.as_str();
                        if t.starts_with('-') && t.len() > 1 {
                            if t[1..].contains('n') {
                                self.poisoned = true;
                            }
                            continue;
                        }
                        if let Some((name, value, append)) = split_assign(&a.raw) {
                            self.note_var(&name, !append && safe_value(&value));
                        } else if basename(&first.text) == "let" {
                            self.note_var(t.split('=').next().unwrap_or(t), false);
                        }
                    }
                }
                _ => {}
            }
        }
    }

    fn deny(&mut self, rule: &str, reason: impl Into<String>) {
        self.walk.denies.push(finding(rule, reason));
    }

    fn allow(&mut self, rule: &str, reason: impl Into<String>) {
        self.walk.allows.push(finding(rule, reason));
    }

    /// Record a form outside the grammar, named in the reason.
    fn outside(&mut self, form: &str) {
        self.walk
            .outside
            .push(finding("unparsed-command", grammar::reason(form)));
    }

    fn has(&self, a: Allow) -> bool {
        self.rules.allow.contains(&a)
    }

    /// Parse a command text and judge every command in it.
    pub fn text(&mut self, src: &str, depth: usize) {
        if depth > MAX_DEPTH {
            self.deny("unparsed-command", "shell strings nest too deep to read");
            return;
        }
        match parse(src) {
            Err(why) => self.deny(
                "unparsed-command",
                format!("the command cannot be parsed: {why}"),
            ),
            Ok(parsed) => {
                for form in &parsed.outside {
                    self.outside(form);
                }
                self.record_assigns(&parsed);
                self.parsed(&parsed, depth);
            }
        }
    }

    fn parsed(&mut self, parsed: &Parsed, depth: usize) {
        for (idx, cmd) in parsed.cmds.iter().enumerate() {
            self.git_config_env(cmd);
            if cmd.header || cmd.words.is_empty() {
                continue;
            }
            if cmd.words.iter().any(|w| w.brace_quoted) {
                // `"--no-ver"{ify,}`: the shell makes words that the rules do not read.
                self.deny(
                    "unparsed-command",
                    "a brace expansion that mixes quoted text cannot be read",
                );
                continue;
            }
            for fin in self.unwrap(&cmd.words, depth) {
                self.check(parsed, idx, cmd, &fin, depth);
            }
        }
    }

    /// `GIT_CONFIG_COUNT`, `GIT_CONFIG_KEY_n`, `GIT_CONFIG_VALUE_n` and `GIT_CONFIG_PARAMETERS`
    /// give git a config as `-c` does, so a call that sets one is denied: an alias or a command
    /// key in it cannot be read.
    fn git_config_env(&mut self, cmd: &Cmd) {
        for w in cmd.assigns.iter().chain(&cmd.words) {
            if let Some((name, _, _)) = split_assign(&w.text)
                && is_git_config_env(&name)
            {
                self.deny(
                    "unparsed-command",
                    format!("{name} gives git a config that cannot be read"),
                );
            }
        }
    }

    // ------------------------------------------------------------ wrappers

    /// Remove wrappers such as `timeout`, `nice` and `env`. `find -exec` and `xargs` give the
    /// commands they run.
    fn unwrap(&mut self, words: &[Word], depth: usize) -> Vec<Final> {
        let mut w: Vec<Word> = words.to_vec();
        let mut stdin_operands = false;
        for _ in 0..16 {
            let Some(first) = w.first().cloned() else {
                return Vec::new();
            };
            if !first.any_quote && !first.dynamic && is_keyword(&first.text) {
                // `time { cmd; }`, `time while cmd; do …; done`: the keyword that follows a
                // wrapper starts a group or a compound command. The command is inside it.
                if matches!(first.text.as_str(), "function" | "coproc") {
                    // The parser leaves `function` and `coproc` out of a command when they start
                    // it, so one that reaches here follows a wrapper. Its name word cannot be told
                    // from a command, so the call is denied rather than guessed.
                    self.deny(
                        "unparsed-command",
                        format!("`{}` after a wrapper is not read", first.text),
                    );
                    return Vec::new();
                }
                w.remove(0);
                continue;
            }
            // `[` is the test command. Any other glob in a command name may match any file.
            let glob = first.glob && !matches!(first.text.as_str(), "[" | "[[");
            if first.dynamic || glob {
                self.deny(
                    "unparsed-command",
                    format!("the command name {:?} is not readable", first.raw),
                );
                return Vec::new();
            }
            let name = basename(&first.text).to_string();
            let args = &w[1..];
            let rest: Option<Vec<Word>> = match name.as_str() {
                "timeout" => Some(after_opts(
                    args,
                    &["-s", "-k", "--signal", "--kill-after"],
                    1,
                )),
                "nice" => Some(after_opts(args, &["-n", "--adjustment"], 0)),
                "ionice" => Some(after_opts(
                    args,
                    &["-c", "-n", "-p", "-P", "-u", "--class", "--classdata"],
                    0,
                )),
                "nohup" | "setsid" | "unbuffer" | "builtin" => Some(after_opts(args, &[], 0)),
                "time" => Some(after_opts(args, &["-f", "-o", "--format", "--output"], 0)),
                "exec" => {
                    // The command that `exec` runs is still judged, so a guard's rule comes first.
                    self.outside("`exec` (it replaces the shell with a command)");
                    Some(after_opts(args, &["-a"], 0))
                }
                "stdbuf" => Some(after_opts(args, &["-i", "-o", "-e"], 0)),
                "command" => {
                    if args.iter().any(|a| matches!(a.text.as_str(), "-v" | "-V")) {
                        return Vec::new();
                    }
                    Some(after_opts(args, &[], 0))
                }
                "sudo" => Some(skip_assigns(after_opts(
                    args,
                    &[
                        "-u",
                        "-g",
                        "-h",
                        "-p",
                        "-C",
                        "-D",
                        "-R",
                        "-T",
                        "-U",
                        "-r",
                        "-t",
                        "--user",
                        "--group",
                        "--host",
                        "--prompt",
                        "--chdir",
                        "--role",
                        "--type",
                        "--other-user",
                    ],
                    0,
                ))),
                "doas" => Some(after_opts(args, &["-u", "-C"], 0)),
                "env" => {
                    if let Some(k) = args.iter().position(|a| {
                        a.text == "-S"
                            || a.text == "--split-string"
                            || a.text.starts_with("--split-string=")
                    }) {
                        // The string is a command line. Read it as one.
                        let mut text = String::new();
                        let a = &args[k];
                        if let Some(v) = a.text.strip_prefix("--split-string=") {
                            text.push_str(v);
                        } else if let Some(v) = args.get(k + 1) {
                            if v.dynamic {
                                self.deny(
                                    "unparsed-command",
                                    "env -S with a string that is not readable",
                                );
                                return Vec::new();
                            }
                            text.push_str(&v.text);
                        }
                        for extra in args.iter().skip(k + 2) {
                            text.push(' ');
                            text.push_str(&extra.text);
                        }
                        self.text(&text, depth + 1);
                        return Vec::new();
                    }
                    Some(skip_assigns(after_opts(
                        args,
                        &["-u", "-C", "--unset", "--chdir"],
                        0,
                    )))
                }
                "xargs" => {
                    stdin_operands = true;
                    Some(after_opts(
                        args,
                        &[
                            "-a",
                            "-d",
                            "-E",
                            "-I",
                            "-L",
                            "-n",
                            "-P",
                            "-s",
                            "--arg-file",
                            "--delimiter",
                            "--max-args",
                            "--max-procs",
                            "--replace",
                            "--max-lines",
                            "--max-chars",
                        ],
                        0,
                    ))
                }
                "flock" => {
                    if let Some(k) = args
                        .iter()
                        .position(|a| a.text == "-c" || a.text == "--command")
                    {
                        let text = args.get(k + 1).map(|a| (a.dynamic, a.text.clone()));
                        match text {
                            Some((false, t)) => self.text(&t, depth + 1),
                            _ => self.deny(
                                "unparsed-command",
                                "flock -c with a string that is not readable",
                            ),
                        }
                        return Vec::new();
                    }
                    Some(after_opts(
                        args,
                        &["-w", "-E", "--timeout", "--conflict-exit-code"],
                        1,
                    ))
                }
                "find" => {
                    return self.find_commands(&w, depth);
                }
                n if UNREAD_WRAPPERS.contains(&n) => {
                    self.deny(
                        "unparsed-command",
                        format!("{n} runs a command that the rules cannot read"),
                    );
                    return Vec::new();
                }
                _ => None,
            };
            match rest {
                Some(r) => w = r,
                None => {
                    return vec![Final {
                        words: w,
                        stdin_operands,
                    }];
                }
            }
        }
        self.deny("unparsed-command", "wrappers nest too deep to read");
        Vec::new()
    }

    /// `find`: the command itself, and each command that `-exec` runs.
    fn find_commands(&mut self, w: &[Word], depth: usize) -> Vec<Final> {
        let mut out = vec![Final {
            words: w.to_vec(),
            stdin_operands: false,
        }];
        let starts = find_starts(&w[1..]);
        let mut i = 1;
        while i < w.len() {
            if matches!(w[i].text.as_str(), "-exec" | "-execdir" | "-ok" | "-okdir") {
                let mut j = i + 1;
                let mut cmd: Vec<Word> = Vec::new();
                while j < w.len() && !matches!(w[j].text.as_str(), ";" | "+") {
                    cmd.push(w[j].clone());
                    j += 1;
                }
                if cmd.iter().any(|c| c.text == "{}") && !starts.is_empty() {
                    // The command runs on each path that find was given.
                    for s in &starts {
                        let swapped: Vec<Word> = cmd
                            .iter()
                            .map(|c| if c.text == "{}" { s.clone() } else { c.clone() })
                            .collect();
                        out.extend(self.unwrap(&swapped, depth));
                    }
                } else {
                    out.extend(self.unwrap(&cmd, depth));
                }
                i = j;
            }
            i += 1;
        }
        out
    }

    // ------------------------------------------------------------ checks

    fn check(&mut self, parsed: &Parsed, idx: usize, cmd: &Cmd, fin: &Final, depth: usize) {
        let words = &fin.words;
        let name = basename(&words[0].text).to_string();
        let args = &words[1..];
        if (!cmd.heredocs.is_empty() || cmd.herestring.is_some()) && grammar::is_interpreter(&name)
        {
            self.outside(&format!(
                "a heredoc or here-string that feeds `{name}` (an interpreter whose program the rules cannot read)"
            ));
        }
        match name.as_str() {
            n if SHELLS.contains(&n) => self.shell(parsed, idx, cmd, args, depth),
            "source" | "." => self.outside(&format!(
                "`{}` (it runs a file whose text the rules cannot read: source)",
                words[0].text
            )),
            "select" => self.outside("`select` (a menu loop)"),
            "eval" => {
                let parts: Vec<&Word> = args.iter().collect();
                if parts.iter().any(|a| a.dynamic) {
                    self.deny(
                        "unparsed-command",
                        "eval with an argument that is not readable",
                    );
                } else {
                    let text = parts
                        .iter()
                        .map(|a| a.text.as_str())
                        .collect::<Vec<_>>()
                        .join(" ");
                    self.text(&text, depth + 1);
                }
            }
            "git" => self.git(words, cmd, parsed),
            "gh" => self.gh(args),
            "curl" | "wget" => self.net(&name, args),
            "rm" | "rmdir" | "unlink" => self.rm(args, fin.stdin_operands, true),
            "shred" => self.rm(args, fin.stdin_operands, false),
            "find" => self.find_delete(args),
            "cd" => self.cd(args),
            "pushd" | "popd" => {
                let raw = args.first().map_or(name.clone(), |a| a.raw.clone());
                self.flow.cwd_unknown.get_or_insert(raw);
            }
            "trap" => self.trap(args, depth),
            "cargo" => self.cargo(args),
            _ => {}
        }
    }

    /// `sh -c 'text'`, `sh <<< 'text'`, a heredoc or a pipe that feeds a shell.
    fn shell(&mut self, parsed: &Parsed, idx: usize, cmd: &Cmd, args: &[Word], depth: usize) {
        let mut c_flag = false;
        let mut stdin_flag = false;
        let mut operand: Option<usize> = None;
        let mut i = 0;
        while i < args.len() {
            let a = &args[i];
            let t = a.text.as_str();
            if t == "--" {
                if i + 1 < args.len() {
                    operand = Some(i + 1);
                }
                break;
            }
            if let Some(long) = t.strip_prefix("--") {
                if matches!(long, "rcfile" | "init-file") {
                    i += 1;
                }
                i += 1;
                continue;
            }
            if t.starts_with('-') && t.len() > 1 {
                if t[1..].contains('c') {
                    c_flag = true;
                }
                if t[1..].contains('s') {
                    stdin_flag = true;
                }
                if t.ends_with('o') || t.ends_with('O') {
                    i += 1;
                }
                i += 1;
                continue;
            }
            operand = Some(i);
            break;
        }
        if c_flag {
            match operand.map(|k| &args[k]) {
                None => self.deny("unparsed-command", "a shell -c has no command string"),
                Some(a) if a.dynamic => self.deny(
                    "unparsed-command",
                    "the command string of a shell -c is a variable or a substitution, and cannot be read",
                ),
                Some(a) => {
                    let text = a.text.clone();
                    self.text(&text, depth + 1);
                }
            }
            return;
        }
        if operand.is_some() && !stdin_flag {
            // A script file. Its text is not visible: the known limit of ADR-0005.
            return;
        }
        // A shell that reads standard input.
        let mut fed = false;
        for body in &cmd.heredocs {
            fed = true;
            let body = body.clone();
            self.text(&body, depth + 1);
        }
        if let Some(h) = &cmd.herestring {
            fed = true;
            if h.dynamic {
                self.deny(
                    "unparsed-command",
                    "a here-string that feeds a shell cannot be read",
                );
            } else {
                let t = h.text.clone();
                self.text(&t, depth + 1);
            }
        }
        if let Some(p) = cmd.piped_from {
            fed = true;
            match fed_text(&parsed.cmds[p]) {
                Some(t) => self.text(&t, depth + 1),
                None => self.deny(
                    "unparsed-command",
                    "a pipe feeds a shell, and the text it carries cannot be read",
                ),
            }
        }
        let _ = (fed, idx);
    }

    // ------------------------------------------------------------ cd, trap, cargo

    /// `cd`: a literal target adds a directory that a relative path may mean. Any other `cd`
    /// leaves the directory unknown.
    fn cd(&mut self, args: &[Word]) {
        let mut operands: Vec<&Word> = Vec::new();
        let mut after_dd = false;
        for a in args {
            if !after_dd && a.text == "--" {
                after_dd = true;
            } else if after_dd || !(a.text.starts_with('-') && a.text.len() > 1) || a.dynamic {
                operands.push(a);
            }
        }
        let target = match operands.as_slice() {
            [one] if !one.dynamic && one.text != "-" && !one.text.starts_with('~') => &one.text,
            other => {
                let raw = other.first().map_or("cd".to_string(), |w| w.raw.clone());
                self.flow.cwd_unknown.get_or_insert(raw);
                return;
            }
        };
        let mut next: Vec<PathBuf> = Vec::new();
        for c in &self.flow.cwds {
            let p = normalize(c, target);
            if !next.contains(&p) {
                next.push(p);
            }
        }
        for p in next {
            if !self.flow.cwds.contains(&p) {
                self.flow.cwds.push(p);
            }
        }
        if self.flow.cwds.len() > 16 {
            self.flow.cwd_unknown.get_or_insert(target.clone());
        }
    }

    /// `trap 'body' SIGNAL`: the body is a string the shell will run, so it is read as a command.
    fn trap(&mut self, args: &[Word], depth: usize) {
        let Some(first) = args.first() else { return };
        if !first.dynamic && first.text.starts_with('-') && first.text.len() > 1 {
            // `trap -p` and `trap -l` print. They run nothing.
            if first.text != "--" {
                return;
            }
        }
        let body = if first.text == "--" {
            args.get(1)
        } else {
            Some(first)
        };
        let Some(body) = body else { return };
        if body.dynamic {
            self.deny(
                "unparsed-command",
                "a trap body that is a variable or a substitution cannot be read",
            );
            return;
        }
        if body.text == "-" {
            return;
        }
        let text = body.text.clone();
        self.text(&text, depth + 1);
    }

    /// A bare `cargo clean` wipes the whole target directory, and a shared target is not the
    /// seat's own. `cargo clean -p` names the packages it removes.
    fn cargo(&mut self, args: &[Word]) {
        let mut i = 0;
        while i < args.len() && (args[i].text.starts_with('+') || args[i].text.starts_with('-')) {
            if matches!(args[i].text.as_str(), "--config" | "-C" | "--color" | "-Z") {
                i += 1;
            }
            i += 1;
        }
        if args.get(i).is_none_or(|w| w.text != "clean") {
            return;
        }
        let rest = &args[i + 1..];
        let named = rest.iter().any(|w| {
            let t = w.text.as_str();
            t == "-p"
                || t == "--package"
                || t.starts_with("--package=")
                || (t.starts_with("-p") && !t.starts_with("--") && t.len() > 2)
        });
        if !named {
            self.deny(
                "cargo-clean",
                "a bare cargo clean wipes the whole target directory, which seats may share; name the packages with -p",
            );
        }
    }

    // ------------------------------------------------------------ git

    fn git(&mut self, words: &[Word], cmd: &Cmd, parsed: &Parsed) {
        let mut i = 1;
        let mut dir: Option<Word> = None;
        let mut dirs: Vec<Word> = Vec::new();
        while i < words.len() {
            let t = words[i].text.as_str();
            match t {
                "-C" => {
                    dir = words.get(i + 1).cloned();
                    dirs.extend(dir.clone());
                    i += 2;
                }
                "-c" | "--config-env" => {
                    // A key that redirects git is denied for every value, before the checks of
                    // a value that cannot be read.
                    let redirect = words
                        .get(i + 1)
                        .map(|v| setting_key(&v.text))
                        .filter(|k| config_key_redirects(k));
                    if let Some(key) = redirect {
                        self.deny(CONFIG_TRUST, config_trust_reason(&format!("git {t}"), key));
                    } else {
                        match words.get(i + 1) {
                            Some(v) if v.dynamic => self.deny(
                                "unparsed-command",
                                format!("git {t} sets a config that cannot be read"),
                            ),
                            Some(v) if config_runs_command(&v.text, t == "--config-env") => self
                                .deny(
                                    "unparsed-command",
                                    format!(
                                    "git {t} sets a config that runs a command that cannot be read"
                                ),
                                ),
                            _ => {}
                        }
                    }
                    i += 2;
                }
                "--git-dir" | "--work-tree" | "--namespace" | "--exec-path" | "--super-prefix"
                | "--attr-source" => i += 2,
                _ if t.starts_with("--config-env=") => {
                    let setting = &t["--config-env=".len()..];
                    if config_key_redirects(setting_key(setting)) {
                        self.deny(
                            CONFIG_TRUST,
                            config_trust_reason("git --config-env", setting_key(setting)),
                        );
                    } else if words[i].dynamic || config_runs_command(setting, true) {
                        self.deny(
                            "unparsed-command",
                            "git --config-env sets a config that runs a command that cannot be read",
                        );
                    }
                    i += 1;
                }
                _ if t.starts_with('-') => i += 1,
                _ => break,
            }
        }
        let Some(sub) = words.get(i) else { return };
        if sub.dynamic || sub.glob {
            self.deny("unparsed-command", "the git subcommand is not readable");
            return;
        }
        let sub_name = sub.text.as_str();
        // Git reads a long option by any unique prefix. Write each guarded option in full, so the
        // guards below match by exact name. A prefix that two of them share is denied once the
        // guards have run: git refuses it, so no valid work is lost.
        let (canonical, ambiguous) =
            options::canonical_words(&words[i + 1..], &options::git_options(sub_name));
        let rest = &canonical[..];
        let first_deny = self.walk.denies.len();
        if sub_name == "config"
            && let Some((rule, reason)) = config_trust_deny(rest)
        {
            self.deny(rule, reason);
        }
        if sub_name == "config" && config_writes_command(rest) {
            self.deny(
                "unparsed-command",
                "git config sets a config that runs a command that cannot be read",
            );
        }
        // `--no-verify` skips hooks in any git command.
        let mut options_end = rest.len();
        if let Some(k) = rest.iter().position(|w| w.text == "--") {
            options_end = k;
        }
        if rest[..options_end].iter().any(|w| w.text == "--no-verify") {
            self.deny(
                "no-verify",
                format!("git {sub_name} --no-verify skips the repository's checks"),
            );
        }
        match sub_name {
            "push" => self.git_push(rest, &dirs),
            "switch" | "checkout" => self.git_switch(sub_name, &rest[..options_end]),
            "commit" => {
                if short_cluster_has(
                    &rest[..options_end],
                    'n',
                    &['m', 'F', 'C', 'c', 't', 'u', 'S'],
                ) {
                    self.deny("no-verify", "git commit -n skips the repository's checks");
                }
                self.trailers(sub_name, rest, cmd, parsed);
            }
            "merge" | "tag" | "revert" | "cherry-pick" | "notes" | "commit-tree" | "am" => {
                self.trailers(sub_name, rest, cmd, parsed);
            }
            "branch" => {
                let del = rest[..options_end].iter().any(|w| {
                    w.text == "--delete"
                        || short_cluster_has(std::slice::from_ref(w), 'd', &[])
                        || short_cluster_has(std::slice::from_ref(w), 'D', &[])
                });
                if del {
                    self.deny(
                        "branch-delete",
                        "git branch -d, -D or --delete deletes a branch",
                    );
                }
            }
            "update-ref" => {
                if rest.iter().any(|w| w.text == "-d") {
                    self.deny("branch-delete", "git update-ref -d deletes a ref");
                }
            }
            "clean" => {
                let dry = rest[..options_end].iter().any(|w| {
                    w.text == "--dry-run" || short_cluster_has(std::slice::from_ref(w), 'n', &[])
                });
                if !dry {
                    let target = dir.clone().unwrap_or_else(|| Word::literal("."));
                    let mut targets = vec![target];
                    if options_end < rest.len() {
                        targets.extend(rest[options_end + 1..].iter().cloned());
                    }
                    self.delete_targets("git clean", &targets, false, false);
                }
            }
            "worktree" if rest.first().is_some_and(|w| w.text == "remove") => {
                // After a `--`, a word that starts with `-` is a path, not an option.
                let mut after_dashes = false;
                let targets: Vec<Word> = rest[1..]
                    .iter()
                    .filter(|w| {
                        let is_path = after_dashes || !w.text.starts_with('-');
                        after_dashes |= w.text == "--";
                        is_path
                    })
                    .cloned()
                    .collect();
                self.delete_targets("git worktree remove", &targets, false, false);
            }
            _ => {}
        }
        if let Some(why) = ambiguous {
            // A guard that fired on an option written in full keeps its rule. Otherwise the
            // ambiguous prefix is the first deny of the command.
            let f = finding("unparsed-command", format!("git {sub_name}: {why}"));
            let keep = self.walk.denies[first_deny..].iter().any(|d| {
                matches!(
                    d.rule.as_str(),
                    "force-push" | "no-verify" | "branch-delete" | "push-default-branch"
                )
            });
            if keep {
                self.walk.denies.push(f);
            } else {
                self.walk.denies.insert(first_deny, f);
            }
        }
    }

    /// `git switch` and `git checkout` change the branch that a later bare push names.
    fn git_switch(&mut self, sub: &str, args: &[Word]) {
        // `git checkout -- path` and `git checkout <tree> -- path` restore files. They switch
        // nothing.
        if sub == "checkout" && args.iter().any(|w| w.text == "--") {
            return;
        }
        let mut name: Option<Part> = None;
        let mut i = 0;
        while i < args.len() {
            let w = &args[i];
            let t = w.text.as_str();
            let create = matches!(t, "-c" | "-C" | "-b" | "-B" | "--create" | "--force-create")
                || t.starts_with("--orphan");
            if create && !w.dynamic {
                if let Some(n) = args.get(i + 1) {
                    name = Some(branch_part(n));
                } else {
                    name = Some(Part::Unresolved(t.to_string()));
                }
                break;
            }
            if matches!(t, "-d" | "--detach" | "-p" | "--patch" | "--detach=") {
                // A detached HEAD has no branch to read.
                name = Some(Part::Unresolved(t.to_string()));
                break;
            }
            if t.starts_with('-') && t.len() > 1 && !w.dynamic {
                i += 1;
                continue;
            }
            name = Some(if t == "-" {
                Part::Unresolved("-".to_string())
            } else {
                branch_part(w)
            });
            break;
        }
        if let Some(n) = name {
            self.flow.switched = Some(n);
        }
    }

    /// Where a push runs: the `git -C` words, else the working directory, each read against
    /// every directory a literal `cd` may have reached.
    fn push_dir(&self, dirs: &[Word]) -> PushDir {
        let absolute = dirs
            .first()
            .is_some_and(|d| !d.dynamic && Path::new(&d.text).is_absolute());
        let lost = self.flow.cwd_unknown.is_some() && !absolute;
        if lost || dirs.iter().any(|d| d.dynamic) {
            return PushDir::Unread;
        }
        let mut places: Vec<PathBuf> = if absolute {
            vec![PathBuf::from("/")]
        } else {
            self.flow.cwds.clone()
        };
        for d in dirs {
            places = places.iter().map(|p| normalize(p, &d.text)).collect();
        }
        let wt = &self.ctx.paths.worktree;
        let pd = &self.ctx.push_dir;
        for p in &places {
            if !(under(p, wt) || under(p, pd)) {
                return PushDir::Outside(format!(
                    "the push runs in {}, which lies outside the worktree and the push directory",
                    p.display()
                ));
            }
        }
        PushDir::Inside
    }

    /// `git push`: force, delete, the default branch and the role's own branch or tag.
    fn git_push(&mut self, rest: &[Word], dirs: &[Word]) {
        let p = push_args(rest);
        for w in &p.unreadable {
            self.deny(
                "push-unreadable",
                format!("the push has {:?}, which cannot be read (a variable outside quotes, or one that changes the meaning)", w.raw),
            );
        }
        for d in dirs.iter().filter(|d| d.dynamic && d.quoted_var.is_none()) {
            self.deny(
                "push-unreadable",
                format!(
                    "git -C {:?} names a directory that cannot be read (a variable outside quotes)",
                    d.raw
                ),
            );
        }
        if p.force {
            self.deny(
                "force-push",
                "a force push in any spelling is denied for every seat",
            );
        }
        if p.delete {
            self.deny(
                "branch-delete",
                "a push that deletes a ref is denied for every seat",
            );
        }
        if p.all {
            self.deny(
                "push-default-branch",
                "git push --all or --mirror pushes the default branch too",
            );
        }
        if p.tags {
            self.deny(
                "push-not-allowed",
                "git push --tags pushes every tag, not only the seat's own",
            );
        }
        // The intents for `push_of`. A directory or a branch that no literal gives is marked
        // unresolved, so the adapter measures the push.
        let dir_part = if let Some(d) = dirs.iter().find(|d| d.dynamic) {
            Part::Unresolved(d.raw.clone())
        } else if let Some(d) = dirs.last() {
            Part::Literal(d.text.clone())
        } else if let Some(raw) = &self.flow.cwd_unknown {
            Part::Unresolved(raw.clone())
        } else {
            Part::Absent
        };
        let intents = p.intents(dir_part, self.flow.switched.as_ref());
        self.walk.pushes.extend(intents);
        if !p.unreadable.is_empty() || p.force || p.delete || p.all || p.tags {
            return;
        }
        let pdir = self.push_dir(dirs);
        let ctx = self.ctx;
        let default = ctx.default_branch.as_str();
        // Each target the push names; a bare push names the branch of the last switch, else the
        // current branch.
        let mut targets: Vec<Target> = p.specs.iter().map(|s| s.target.clone()).collect();
        if targets.is_empty() {
            targets.push(Target::Branch("HEAD".to_string()));
        }
        for t in targets {
            let t = match (t, &self.flow.switched) {
                (Target::Branch(b), Some(Part::Literal(n))) if b == "HEAD" => {
                    Target::Branch(n.clone())
                }
                (Target::Branch(b), Some(Part::Unresolved(_))) if b == "HEAD" => Target::QuotedVar,
                (t, _) => t,
            };
            let before = self.walk.allows.len();
            match t {
                Target::QuotedVar => {
                    if self.has(Allow::PushOwnBranch) {
                        self.allow("own-branch-unverified", "the push target is one quoted variable; the role may push its own branch, and the check after the push guards the default branch");
                    } else {
                        self.deny(
                            "push-not-allowed",
                            format!("role {} may not push", self.rules.role),
                        );
                    }
                }
                Target::Tag(name) => {
                    if self.has(Allow::PushOwnTag) && ctx.own_tag.as_deref() == Some(name.as_str())
                    {
                        self.allow("push-own-tag", format!("pushes its own tag {name}"));
                    } else if self.has(Allow::PushOwnTag) {
                        self.deny(
                            "push-other-tag",
                            format!("tag {name} is not this seat's own tag"),
                        );
                    } else {
                        self.deny(
                            "push-not-allowed",
                            format!("role {} may not push a tag", self.rules.role),
                        );
                    }
                }
                Target::Branch(b) => {
                    let name = if b == "HEAD" {
                        ctx.current_branch.clone()
                    } else {
                        b
                    };
                    if !name.is_empty() && name == default {
                        self.deny(
                            "push-default-branch",
                            format!(
                                "pushing the default branch {default} is denied for every seat"
                            ),
                        );
                    } else if !self.has(Allow::PushOwnBranch) {
                        self.deny(
                            "push-not-allowed",
                            format!("role {} may not push", self.rules.role),
                        );
                    } else if name == ctx.own_branch && !name.is_empty() {
                        if matches!(pdir, PushDir::Unread) {
                            self.allow(
                                "own-branch-unverified",
                                format!("the push is to {name}, but its directory is a variable; the check after the push reads what moved"),
                            );
                        } else {
                            self.allow("push-own-branch", format!("pushes its own branch {name}"));
                        }
                    } else {
                        self.deny(
                            "push-other-branch",
                            format!(
                                "{name:?} is not this seat's own branch {:?}",
                                ctx.own_branch
                            ),
                        );
                    }
                }
            }
            if let (PushDir::Outside(why), true) = (&pdir, self.walk.allows.len() > before) {
                self.walk.allows.truncate(before);
                self.deny("push-outside-worktree", why.clone());
            }
        }
    }

    /// An AI trailer in the message of a commit-like command.
    fn trailers(&mut self, sub: &str, rest: &[Word], cmd: &Cmd, parsed: &Parsed) {
        let mut texts: Vec<String> = Vec::new();
        for w in rest {
            texts.push(w.text.clone());
            texts.push(w.raw.clone());
        }
        texts.extend(cmd.heredocs.iter().cloned());
        if let Some(h) = &cmd.herestring {
            texts.push(h.text.clone());
            texts.push(h.raw.clone());
        }
        if let Some(p) = cmd.piped_from {
            for w in &parsed.cmds[p].words {
                texts.push(w.text.clone());
                texts.push(w.raw.clone());
            }
            texts.extend(parsed.cmds[p].heredocs.iter().cloned());
        }
        if texts.iter().any(|t| has_ai_trailer(t)) {
            self.deny(
                "ai-trailer",
                format!("the message of git {sub} carries an AI trailer or credit line"),
            );
        }
    }

    // ------------------------------------------------------------ gh and the network

    fn gh(&mut self, args: &[Word]) {
        // The positional words, without flags and their values.
        let mut pos: Vec<&Word> = Vec::new();
        let mut i = 0;
        while i < args.len() {
            let t = args[i].text.as_str();
            if matches!(t, "-R" | "--repo" | "--hostname") {
                i += 2;
                continue;
            }
            if !t.starts_with('-') {
                pos.push(&args[i]);
            }
            i += 1;
        }
        let first = pos.first().map(|w| w.text.as_str());
        let second = pos.get(1).map(|w| w.text.as_str());
        let unreadable = |k: usize| pos.get(k).is_some_and(|w| w.dynamic || w.glob);
        if unreadable(0) || (first == Some("pr") && unreadable(1)) {
            self.deny("unparsed-command", "the gh subcommand is not readable");
            return;
        }
        if first == Some("alias") && matches!(second, Some("set" | "import")) {
            self.deny(
                "unparsed-command",
                "gh alias defines a command that cannot be read",
            );
            return;
        }
        match (first, second) {
            (Some("pr"), Some("merge")) => {
                self.deny(
                    "gh-pr-merge",
                    "gh pr merge is denied for every seat: only the workflow's merge rule merges",
                );
            }
            (
                Some("pr"),
                Some(
                    "review" | "comment" | "edit" | "close" | "reopen" | "ready" | "lock"
                    | "unlock" | "update-branch",
                ),
            )
            | (
                Some("issue"),
                Some("comment" | "edit" | "close" | "reopen" | "lock" | "unlock" | "delete"),
            ) => {
                self.deny(
                    "gh-pr-write",
                    format!(
                        "gh {} {} writes to the forge: a seat reports through its files, and the operator speaks on the forge",
                        first.unwrap_or_default(),
                        second.unwrap_or_default()
                    ),
                );
            }
            (Some("api"), _) => self.gh_api(args),
            _ if self.rules.role == READ_ONLY_FORGE_ROLE && !gh_read_only(first, second) => {
                self.deny(
                    "gh-forge-write",
                    format!(
                        "role {} reads the forge only: gh {} {} is not on its read list",
                        self.rules.role,
                        first.unwrap_or_default(),
                        second.unwrap_or_default()
                    ),
                );
            }
            _ => {}
        }
    }

    /// `gh api`: any method but GET is a write, and fields without a method count as POST.
    fn gh_api(&mut self, args: &[Word]) {
        let mut methods: Vec<&Word> = Vec::new();
        let mut fields = false;
        let mut i = 0;
        while i < args.len() {
            let t = args[i].text.as_str();
            match t {
                "-X" | "--method" => {
                    if let Some(m) = args.get(i + 1) {
                        methods.push(m);
                    }
                    i += 2;
                    continue;
                }
                "-f" | "-F" | "--field" | "--raw-field" | "--input" => {
                    fields = true;
                    i += 2;
                    continue;
                }
                "-H" | "--header" | "-q" | "--jq" | "-t" | "--template" | "--cache"
                | "--hostname" | "-p" | "--preview" => {
                    i += 2;
                    continue;
                }
                _ => {}
            }
            if let Some(v) = t.strip_prefix("--method=") {
                let _ = v;
                methods.push(&args[i]);
            } else if t.starts_with("-X") && t.len() > 2 {
                methods.push(&args[i]);
            } else if t.starts_with("--field=")
                || t.starts_with("--raw-field=")
                || t.starts_with("--input=")
                || (t.starts_with("-f") && t.len() > 2 && !t.starts_with("--"))
                || (t.starts_with("-F") && t.len() > 2)
            {
                fields = true;
            }
            i += 1;
        }
        for m in &methods {
            if m.dynamic {
                self.deny(
                    "unparsed-command",
                    "gh api with a method that is not readable",
                );
                return;
            }
            let name = m
                .text
                .trim_start_matches("--method=")
                .trim_start_matches("-X")
                .trim_start_matches('=')
                .to_ascii_uppercase();
            if name != "GET" {
                self.deny(
                    "gh-api-write",
                    format!("gh api with method {name} writes to the forge"),
                );
                return;
            }
        }
        if methods.is_empty() && fields {
            self.deny(
                "gh-api-write",
                "gh api with fields and no method counts as POST, as gh does",
            );
        }
    }

    fn net(&mut self, name: &str, args: &[Word]) {
        let hosts: Vec<String> = self
            .ctx
            .api_hosts
            .iter()
            .map(|h| h.to_ascii_lowercase())
            .collect();
        for a in args {
            let text = a.text.to_ascii_lowercase();
            let raw = a.raw.to_ascii_lowercase();
            if hosts
                .iter()
                .any(|h| !h.is_empty() && (text.contains(h) || raw.contains(h)))
            {
                self.deny(
                    "forge-api-curl",
                    format!("{name} to the forge's API host is denied, with any method"),
                );
                return;
            }
        }
    }

    // ------------------------------------------------------------ delete class

    fn rm(&mut self, args: &[Word], stdin_operands: bool, worktree_ok: bool) {
        let mut targets: Vec<Word> = Vec::new();
        let mut done = false;
        for a in args {
            if !done && a.text == "--" {
                done = true;
                continue;
            }
            if !done && a.text.starts_with('-') && a.text.len() > 1 && !a.dynamic {
                continue;
            }
            targets.push(a.clone());
        }
        self.delete_targets("rm", &targets, stdin_operands, worktree_ok);
    }

    fn find_delete(&mut self, args: &[Word]) {
        if args.iter().any(|a| a.text == "-delete") {
            let starts = find_starts(args);
            let targets = if starts.is_empty() {
                vec![Word::literal(".")]
            } else {
                starts
            };
            self.delete_targets("find -delete", &targets, false, false);
        }
    }

    /// The delete class. `rm`, `rmdir` and `unlink` of a path inside the worktree are a seat's
    /// normal work (`worktree_ok`). Any other delete needs the seat's scratch or build target, and
    /// the role must list it.
    fn delete_targets(
        &mut self,
        what: &str,
        targets: &[Word],
        stdin_operands: bool,
        worktree_ok: bool,
    ) {
        if stdin_operands {
            self.deny(
                "delete-unreadable",
                format!("{what} takes its targets from standard input, and they cannot be read"),
            );
            return;
        }
        if targets.is_empty() {
            return;
        }
        let scratch = self.has(Allow::DeleteOwnScratch);
        let build = self.has(Allow::DeleteOwnBuildTarget);
        for t in targets {
            if t.dynamic {
                // The word must be one quoted variable that the whole call assigns only from
                // `mktemp` or the empty string.
                let known = !self.poisoned
                    && t.quoted_var
                        .as_ref()
                        .is_some_and(|n| self.vars.get(n).copied().unwrap_or(false));
                if known && scratch {
                    self.allow(
                        "delete-own-scratch",
                        format!("{what} {:?}: a file that mktemp made", t.raw),
                    );
                    continue;
                }
                self.deny(
                    "delete-unreadable",
                    format!(
                        "{what} {:?}: a path held in a variable cannot be read{}",
                        t.raw,
                        if known {
                            format!(
                                ", and role {} may not delete scratch files",
                                self.rules.role
                            )
                        } else {
                            String::new()
                        }
                    ),
                );
                return;
            }
            if t.text.starts_with('~') {
                self.deny(
                    "delete-unreadable",
                    format!("{what} {:?}: ~ is not expanded here", t.raw),
                );
                return;
            }
            let mut path = t.text.clone();
            let mut rest = String::new();
            if t.glob {
                // Judge the directory in front of the first glob character.
                let cut = path.find(['*', '?', '[']).unwrap_or(path.len());
                let dir = &path[..cut];
                let dir_len = dir.rfind('/').map_or(0, |k| k + 1);
                rest = path[dir_len..].to_string();
                path = if dir_len == 0 {
                    ".".to_string()
                } else {
                    path[..dir_len].to_string()
                };
            }
            let relative = !Path::new(&path).is_absolute();
            if relative && self.flow.cwd_unknown.is_some() {
                self.deny(
                    "delete-unreadable",
                    format!(
                        "{what} {:?}: the working directory changed to a variable, so a relative path has no meaning",
                        t.raw
                    ),
                );
                return;
            }
            let places: Vec<PathBuf> = if relative {
                self.flow.cwds.iter().map(|c| normalize(c, &path)).collect()
            } else {
                vec![normalize(&self.ctx.cwd, &path)]
            };
            for abs in places {
                let hit = self.delete_place(what, &abs, &rest, worktree_ok, scratch, build);
                match hit {
                    Ok(()) => {}
                    Err((rule, why)) => {
                        self.deny(rule, format!("{what} {:?} {why}", t.text));
                        return;
                    }
                }
            }
        }
    }

    /// Judge one resolved delete place and record the allow, or give the deny.
    fn delete_place(
        &mut self,
        what: &str,
        abs: &Path,
        glob_rest: &str,
        worktree_ok: bool,
        scratch: bool,
        build: bool,
    ) -> Result<(), (&'static str, String)> {
        let wt = self.ctx.paths.worktree.clone();
        if worktree_ok && !wt.as_os_str().is_empty() && under(abs, &wt) {
            let git_dir = wt.join(".git");
            let is_wt = abs == wt;
            if under(abs, &git_dir) {
                return Err((
                    "delete-outside-own-paths",
                    "lies in the worktree's .git directory".to_string(),
                ));
            }
            let named = glob_rest
                .chars()
                .any(|c| !matches!(c, '*' | '?' | '[' | ']' | '/'));
            if !is_wt || (named && !glob_rest.starts_with('.')) {
                self.allow(
                    "delete-in-worktree",
                    format!("{what} inside the seat's worktree is its normal work"),
                );
                return Ok(());
            }
            return Err((
                "delete-outside-own-paths",
                "is the worktree itself, or a glob that may take most of it or its .git directory"
                    .to_string(),
            ));
        }
        if !scratch && !build {
            return Err((
                "delete-not-allowed",
                format!(
                    "is not deletable: role {} may not delete there",
                    self.rules.role
                ),
            ));
        }
        if scratch && under(abs, &self.ctx.paths.scratch) {
            self.allow(
                "delete-own-scratch",
                format!("{what} inside the seat's scratch"),
            );
            Ok(())
        } else if build && under(abs, &self.ctx.paths.build_target) {
            self.allow(
                "delete-own-build-target",
                format!("{what} inside the seat's build target"),
            );
            Ok(())
        } else {
            Err((
                "delete-outside-own-paths",
                "lies outside the seat's own paths".to_string(),
            ))
        }
    }
}

// ---------------------------------------------------------------- helpers

/// A branch word of `git switch` or `git checkout`, as a part.
fn branch_part(w: &Word) -> Part {
    if w.dynamic {
        Part::Unresolved(w.raw.clone())
    } else {
        let t = w.text.strip_prefix("refs/heads/").unwrap_or(&w.text);
        Part::Literal(t.to_string())
    }
}

/// Split `NAME=value` or `NAME+=value`. Give the name, the value as written and whether it
/// appends.
fn split_assign(raw: &str) -> Option<(String, String, bool)> {
    let name_len = raw.chars().take_while(|c| is_name(*c)).count();
    if name_len == 0 || raw.chars().next().is_some_and(|c| c.is_ascii_digit()) {
        return None;
    }
    let name: String = raw.chars().take(name_len).collect();
    let rest = &raw[name_len..];
    if let Some(v) = rest.strip_prefix("+=") {
        Some((name, v.to_string(), true))
    } else {
        rest.strip_prefix('=').map(|v| (name, v.to_string(), false))
    }
}

fn is_name(c: char) -> bool {
    c.is_ascii_alphanumeric() || c == '_'
}

/// Is a value as written the empty string or the output of `mktemp`?
fn safe_value(value: &str) -> bool {
    let v = value.trim();
    if v.is_empty() || v == "\"\"" || v == "''" {
        return true;
    }
    let v = v
        .strip_prefix('"')
        .and_then(|x| x.strip_suffix('"'))
        .unwrap_or(v);
    let inner = if let Some(x) = v.strip_prefix("$(").and_then(|x| x.strip_suffix(')')) {
        x
    } else if let Some(x) = v.strip_prefix('`').and_then(|x| x.strip_suffix('`')) {
        x
    } else {
        return false;
    };
    let inner = inner.trim();
    let Some(args) = inner.strip_prefix("mktemp") else {
        return false;
    };
    args.is_empty()
        || (args.starts_with(' ')
            && args
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || " ._/=+-".contains(c)))
}

/// The names that a `${NAME:=…}` or `${NAME=…}` in a word may assign.
fn default_assigned(raw: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut rest = raw;
    while let Some(k) = rest.find("${") {
        rest = &rest[k + 2..];
        let name: String = rest.chars().take_while(|c| is_name(*c)).collect();
        let after = &rest[name.len()..];
        if !name.is_empty() && (after.starts_with(":=") || after.starts_with('=')) {
            out.push(name);
        }
    }
    out
}

/// Skip options. `takes_arg` lists the options with a separate value. Then skip `positional`
/// operands (the duration of `timeout`). Give the words from the command on.
fn after_opts(args: &[Word], takes_arg: &[&str], positional: usize) -> Vec<Word> {
    let mut i = 0;
    while i < args.len() {
        let t = args[i].text.as_str();
        if t == "--" {
            i += 1;
            break;
        }
        if t.starts_with('-') && t.len() > 1 && !args[i].dynamic {
            if takes_arg.contains(&t) {
                i += 1;
            }
            i += 1;
        } else {
            break;
        }
    }
    i = (i + positional).min(args.len());
    args[i..].to_vec()
}

/// How a subsection takes part in a config key's name.
#[derive(Clone, Copy)]
enum Subsection {
    /// `section.variable`.
    Without,
    /// `section.<subsection>.variable`.
    With,
    /// Both forms name the key.
    Either,
}

/// Git config keys that redirect git, as (section, variable, subsection), in lower case. A seat may
/// not set one, whatever the value: a bare path, a bare host and an empty value each redirect.
///
/// Source: git-config(1): `core.hooksPath`, `core.sshCommand`, `core.gitProxy`,
/// `remote.<name>.receivepack|uploadpack|vcs|pushurl|proxy`, `url.<base>.insteadOf|pushInsteadOf`, `http.proxy`,
/// `http.<url>.proxy`, `http.sslVerify`, `http.<url>.sslVerify`, `credential.helper`,
/// `credential.<url>.helper` and, under "Includes", `include.path` and `includeIf.<condition>.path`.
const REDIRECT_KEYS: [(&str, &str, Subsection); 15] = [
    ("core", "hookspath", Subsection::Without),
    ("remote", "receivepack", Subsection::With),
    ("remote", "uploadpack", Subsection::With),
    ("remote", "vcs", Subsection::With),
    ("remote", "pushurl", Subsection::With),
    ("remote", "proxy", Subsection::With),
    ("url", "insteadof", Subsection::With),
    ("url", "pushinsteadof", Subsection::With),
    ("include", "path", Subsection::Without),
    ("includeif", "path", Subsection::With),
    ("core", "sshcommand", Subsection::Without),
    ("core", "gitproxy", Subsection::Without),
    ("http", "proxy", Subsection::Either),
    ("http", "sslverify", Subsection::Either),
    ("credential", "helper", Subsection::Either),
];

/// The rule for a write of a key that redirects git.
const CONFIG_TRUST: &str = "git-config-trust";

/// The reason names the key as written. A key such as `url.<base>.insteadOf` may hold a
/// credential in `<base>`, so the key goes through `redact`.
fn config_trust_reason(form: &str, key: &str) -> String {
    format!(
        "{form} sets {}, which redirects git; a seat may not set it",
        redact(key)
    )
}

/// The key of a `key=value` setting, as `git -c` and `git --config-env` read it: the text before
/// the first `=`, or all of it.
fn setting_key(setting: &str) -> &str {
    setting.split_once('=').map_or(setting, |(k, _)| k).trim()
}

/// Is this a config key that redirects git? Git compares the section and the variable without case
/// and keeps the case of a subsection. The section is the text before the first `.`, the variable
/// the text after the last, and the subsection what lies between, dots included.
fn config_key_redirects(key: &str) -> bool {
    let Some((section, rest)) = key.trim().split_once('.') else {
        return false;
    };
    let (sub, variable) = match rest.rsplit_once('.') {
        Some((sub, variable)) => (Some(sub), variable),
        None => (None, rest),
    };
    let (section, variable) = (section.to_ascii_lowercase(), variable.to_ascii_lowercase());
    REDIRECT_KEYS.iter().any(|(s, v, subsection)| {
        *s == section && *v == variable && subsection_fits(*subsection, sub.is_some())
    })
}

/// Does a section of this name, with or without a subsection, hold a key of `REDIRECT_KEYS`? A
/// rename into such a section gives the renamed keys those names.
fn config_section_redirects(name: &str) -> bool {
    let (section, sub) = match name.trim().split_once('.') {
        Some((section, _)) => (section, true),
        None => (name.trim(), false),
    };
    let section = section.to_ascii_lowercase();
    REDIRECT_KEYS
        .iter()
        .any(|(s, _, subsection)| *s == section && subsection_fits(*subsection, sub))
}

fn subsection_fits(subsection: Subsection, has_subsection: bool) -> bool {
    match subsection {
        Subsection::Without => !has_subsection,
        Subsection::With => has_subsection,
        Subsection::Either => true,
    }
}

/// Git config keys whose value is a command that git runs.
const COMMAND_KEYS: [&str; 14] = [
    "core.fsmonitor",
    "core.sshcommand",
    "core.pager",
    "core.editor",
    "core.askpass",
    "sequence.editor",
    "diff.external",
    "credential.helper",
    "gpg.program",
    "gpg.ssh.program",
    "gpg.x509.program",
    "gpg.openpgp.program",
    "uploadpack.packobjectshook",
    "web.browser",
];

/// Key endings that name a command in a config: `diff.<driver>.textconv`, `filter.<name>.clean`,
/// `difftool.<tool>.cmd` and the like.
const COMMAND_KEY_ENDINGS: [&str; 9] = [
    ".program",
    ".command",
    ".helper",
    ".textconv",
    ".cmd",
    ".clean",
    ".smudge",
    ".process",
    ".driver",
];

/// Does a `NAME=value` config key run a command the rules cannot read? An alias always does. A key
/// that names a command does when its value is more than a bare word. `value_unread` is true when
/// the value comes from the environment, as with `--config-env`.
fn config_runs_command(setting: &str, value_unread: bool) -> bool {
    let (key, value) = setting.split_once('=').unwrap_or((setting, ""));
    let key = key.trim().to_ascii_lowercase();
    if key.starts_with("alias.") {
        return true;
    }
    let names_command = COMMAND_KEYS.contains(&key.as_str())
        || key.starts_with("pager.")
        || COMMAND_KEY_ENDINGS.iter().any(|e| key.ends_with(e));
    if !names_command {
        return false;
    }
    if value_unread {
        return true;
    }
    let bare = !value.is_empty()
        && value
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '.' | '/' | '-' | '+' | ':'));
    // An empty value turns the command off, and a bare word is one program with no arguments.
    !(value.is_empty() || bare)
}

/// The words of a `git config …` call, read once for the guards below.
struct ConfigCall<'a> {
    /// A read or a removal: `--get` to `--remove-section`.
    reads: bool,
    /// `--add` or `--replace-all`: the call writes even with one positional word.
    writes_more: bool,
    /// `--edit` or `-e`.
    edit: bool,
    /// `--rename-section`.
    rename: bool,
    /// The positional words. An option that takes a value hides that value from them.
    positional: Vec<&'a Word>,
}

/// The long options of `git config` that take a value: `--file`, `--blob`, `--default`, `--comment`,
/// `--type`, and `--value` and `--url` of the `get` and `set` verbs. Without an `=`, the next word
/// is their value, whatever it looks like.
const CONFIG_VALUE_OPTIONS: [&str; 7] =
    ["file", "blob", "default", "comment", "type", "value", "url"];

/// Read the words of a `git config …` call. A flag counts only where git reads it as a flag: not
/// as the value of an option that takes one (`--comment --get`), not after a `--`, and not after
/// the first operand (`core.hooksPath --unset` writes the value `--unset`). Git stops reading
/// flags at the first operand. The one word before it that does not end the flags is a verb
/// (`set`, `get`, …) in first place, which keeps reading them up to its own operand. The words
/// after `--` or after the first operand are all operands. A short flag is read inside a cluster
/// (`-ze`).
fn config_call(rest: &[Word]) -> ConfigCall<'_> {
    let mut call = ConfigCall {
        reads: false,
        writes_more: false,
        edit: false,
        rename: false,
        positional: Vec::new(),
    };
    let mut i = 0;
    while i < rest.len() {
        let t = rest[i].text.as_str();
        if t == "--" {
            call.positional.extend(rest[i + 1..].iter());
            break;
        }
        // Past the first operand, every word is an operand, whatever it looks like.
        let verb_first = rest.first().is_some_and(|w| config_verb(&w.text));
        if !call.positional.is_empty() && !(call.positional.len() == 1 && verb_first) {
            call.positional.extend(rest[i..].iter());
            break;
        }
        if let Some(long) = t.strip_prefix("--") {
            let (name, has_value) = long
                .split_once('=')
                .map_or((long, false), |(name, _)| (name, true));
            match name {
                "get" | "get-all" | "get-regexp" | "get-urlmatch" | "list" | "unset"
                | "unset-all" | "remove-section" => call.reads = true,
                "add" | "replace-all" => call.writes_more = true,
                "edit" => call.edit = true,
                "rename-section" => call.rename = true,
                _ => {}
            }
            if !has_value && CONFIG_VALUE_OPTIONS.contains(&name) {
                i += 1;
            }
        } else if t.starts_with('-') && t.len() > 1 {
            for (at, c) in t.char_indices().skip(1) {
                match c {
                    'l' => call.reads = true,
                    'e' => call.edit = true,
                    // `-f` and `-t` end the cluster. The rest of the word is the value, or the
                    // next word is.
                    'f' | 't' => {
                        if at + 1 == t.len() {
                            i += 1;
                        }
                        break;
                    }
                    _ => {}
                }
            }
        } else {
            call.positional.push(&rest[i]);
        }
        i += 1;
    }
    call
}

/// The verbs of `git config` that git reads in first place, before any flag.
fn config_verb(word: &str) -> bool {
    matches!(
        word,
        "get" | "set" | "list" | "unset" | "edit" | "rename-section" | "remove-section"
    )
}

/// Does `git config …` write a config that runs a command the rules cannot read? Reading a key,
/// listing and removing one do not. A key the rules cannot read, in a write, counts as one that
/// does.
fn config_writes_command(rest: &[Word]) -> bool {
    let call = config_call(rest);
    if call.reads {
        return false;
    }
    let writes_more = call.writes_more;
    let mut positional = call.positional;
    // `git config get|list|unset …` read or remove. `git config set` writes.
    match positional.first().map(|w| w.text.as_str()) {
        Some("get" | "list" | "unset" | "edit" | "rename-section" | "remove-section") => {
            return false;
        }
        Some("set") => {
            positional.remove(0);
        }
        _ => {}
    }
    // `git config key` reads the key. A value after it, or `--add`, writes it. A lone positional
    // that is a variable (`"$@"`, `$x`) may expand to a key and a value, so it counts as a write.
    let lone_dynamic = positional.len() == 1 && positional[0].dynamic;
    if !(writes_more || positional.len() > 1 || lone_dynamic) {
        return false;
    }
    let Some(key) = positional.first() else {
        return false;
    };
    if key.dynamic {
        return true;
    }
    let value = positional.get(1);
    let setting = format!("{}={}", key.text, value.map_or("", |v| v.text.as_str()));
    config_runs_command(&setting, value.is_some_and(|v| v.dynamic))
}

/// Does `git config …` set a key that redirects git, rename a section into one, or open the
/// config in an editor? Returns the rule and the reason. A read, a listing and a removal do not.
/// The positional reading is the one of `config_writes_command`, so a read stays a read.
fn config_trust_deny(rest: &[Word]) -> Option<(&'static str, String)> {
    let call = config_call(rest);
    let editor = || {
        (
            CONFIG_TRUST,
            "git config opens the config in an editor, and the editor's writes cannot be read; a seat may not use it".to_string(),
        )
    };
    if call.edit {
        return Some(editor());
    }
    let mut positional = call.positional.clone();
    // The verb forms need no flag. A flag that reads, removes or renames makes a word that looks
    // like a verb a key or a section name.
    let verb = if call.reads || call.rename {
        None
    } else {
        positional.first().map(|w| w.text.as_str())
    };
    match verb {
        Some("edit") => return Some(editor()),
        Some("get" | "list" | "unset" | "remove-section") => return None,
        Some("set") => {
            positional.remove(0);
        }
        Some("rename-section") => {
            positional.remove(0);
            return config_rename_deny(&positional);
        }
        _ => {}
    }
    if call.rename {
        return config_rename_deny(&positional);
    }
    if call.reads {
        return None;
    }
    // `git config key` reads the key. A value after it, or `--add`, writes it. A key that is a
    // variable stays with `config_writes_command`.
    if !(call.writes_more || positional.len() > 1) {
        return None;
    }
    let key = positional.first().filter(|k| !k.dynamic)?;
    config_key_redirects(&key.text).then(|| {
        (
            CONFIG_TRUST,
            config_trust_reason("git config", key.text.trim()),
        )
    })
}

/// `git config --rename-section <old> <new>`: is `<new>` a section that holds a key that redirects
/// git? A section the rules cannot read is refused as `unparsed-command`.
fn config_rename_deny(names: &[&Word]) -> Option<(&'static str, String)> {
    let new = names.get(1)?;
    if new.dynamic {
        return Some((
            "unparsed-command",
            "git config --rename-section renames a section into a name that cannot be read"
                .to_string(),
        ));
    }
    config_section_redirects(&new.text).then(|| {
        (
            CONFIG_TRUST,
            format!(
                "git config --rename-section moves keys into {}, which holds keys that redirect git; a seat may not do it",
                redact(&new.text)
            ),
        )
    })
}

/// Is this env name one that gives git config without a `-c`?
fn is_git_config_env(name: &str) -> bool {
    name == "GIT_CONFIG_COUNT"
        || name == "GIT_CONFIG_PARAMETERS"
        || name.starts_with("GIT_CONFIG_KEY_")
        || name.starts_with("GIT_CONFIG_VALUE_")
}

/// Skip `NAME=value` words.
fn skip_assigns(words: Vec<Word>) -> Vec<Word> {
    let n = words
        .iter()
        .take_while(|w| {
            let t = w.text.as_str();
            t.contains('=')
                && t.split('=').next().is_some_and(|n| {
                    !n.is_empty() && n.chars().all(|c| c.is_ascii_alphanumeric() || c == '_')
                })
        })
        .count();
    words[n..].to_vec()
}

/// The start paths of `find`: the words before the first expression.
fn find_starts(args: &[Word]) -> Vec<Word> {
    args.iter()
        .take_while(|a| {
            !(a.text.starts_with('-') || a.text == "(" || a.text == "!") || a.text == "-"
        })
        .cloned()
        .collect()
}

/// Does a short-option word hold `want`? Letters in `takes_value` end the cluster: what follows
/// them is their value.
fn short_cluster_has(words: &[Word], want: char, takes_value: &[char]) -> bool {
    words.iter().any(|w| {
        let t = w.text.as_str();
        if !t.starts_with('-') || t.starts_with("--") || t.len() < 2 {
            return false;
        }
        for c in t[1..].chars() {
            if c == want {
                return true;
            }
            if takes_value.contains(&c) {
                return false;
            }
        }
        false
    })
}

/// Make a path absolute and remove `.` and `..` by text. No file is read.
pub fn normalize(cwd: &Path, text: &str) -> PathBuf {
    let p = Path::new(text);
    let joined = if p.is_absolute() {
        p.to_path_buf()
    } else {
        cwd.join(p)
    };
    let mut out = PathBuf::new();
    for c in joined.components() {
        match c {
            Component::ParentDir => {
                out.pop();
            }
            Component::CurDir => {}
            other => out.push(other.as_os_str()),
        }
    }
    out
}

/// Is `path` the directory or inside it?
pub fn under(path: &Path, root: &Path) -> bool {
    !root.as_os_str().is_empty() && path.starts_with(root)
}

/// The text a pipe stage carries into a shell, when it is `echo`, `printf` or `cat` with a heredoc.
fn fed_text(cmd: &Cmd) -> Option<String> {
    let first = cmd.words.first()?;
    if first.dynamic {
        return None;
    }
    match basename(&first.text) {
        "echo" => {
            let args: Vec<&Word> = cmd.words[1..]
                .iter()
                .skip_while(|w| matches!(w.text.as_str(), "-n" | "-e" | "-E"))
                .collect();
            if args.iter().any(|w| w.dynamic) {
                return None;
            }
            Some(
                args.iter()
                    .map(|w| w.text.as_str())
                    .collect::<Vec<_>>()
                    .join(" "),
            )
        }
        "printf" => {
            let args = &cmd.words[1..];
            if args.iter().any(|w| w.dynamic) || args.is_empty() {
                return None;
            }
            let format = &args[0].text;
            let mut out = String::new();
            if !format.contains('%') {
                out.push_str(format);
            }
            for a in &args[1..] {
                out.push(' ');
                out.push_str(&a.text);
            }
            Some(out)
        }
        "cat" if cmd.words.len() == 1 && !cmd.heredocs.is_empty() => Some(cmd.heredocs.join("\n")),
        _ => None,
    }
}

// ---------------------------------------------------------------- git push

/// What a push names.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Target {
    Branch(String),
    Tag(String),
    /// One variable in double quotes, as the whole branch.
    QuotedVar,
}

#[derive(Clone, Debug)]
pub struct Spec {
    pub word: Word,
    pub target: Target,
    /// The name as the intent shows it.
    pub name: String,
}

pub struct PushArgs {
    pub force: bool,
    pub delete: bool,
    pub all: bool,
    pub tags: bool,
    pub remote: Option<Word>,
    pub specs: Vec<Spec>,
    pub unreadable: Vec<Word>,
}

impl PushArgs {
    pub fn intents(&self, dir: Part, switched: Option<&Part>) -> Vec<PushIntent> {
        let part = |w: &Word| {
            if w.dynamic {
                Part::Unresolved(w.raw.clone())
            } else {
                Part::Literal(w.text.clone())
            }
        };
        let remote = self.remote.as_ref().map_or(Part::Absent, part);
        if self.specs.is_empty() {
            return vec![PushIntent {
                remote,
                branch: switched.cloned().unwrap_or(Part::Absent),
                dir,
            }];
        }
        self.specs
            .iter()
            .map(|s| PushIntent {
                remote: remote.clone(),
                branch: if s.word.dynamic {
                    Part::Unresolved(s.word.raw.clone())
                } else if s.name == "HEAD" && switched.is_some() {
                    switched.cloned().unwrap_or(Part::Absent)
                } else {
                    Part::Literal(s.name.clone())
                },
                dir: dir.clone(),
            })
            .collect()
    }
}

/// Read the words after `git push`.
pub fn push_args(rest: &[Word]) -> PushArgs {
    let mut a = PushArgs {
        force: false,
        delete: false,
        all: false,
        tags: false,
        remote: None,
        specs: Vec::new(),
        unreadable: Vec::new(),
    };
    let mut positional: Vec<&Word> = Vec::new();
    let mut after_dd = false;
    let mut i = 0;
    while i < rest.len() {
        let w = &rest[i];
        let t = w.text.as_str();
        if after_dd || !t.starts_with('-') || t == "-" || w.dynamic {
            positional.push(w);
            i += 1;
            continue;
        }
        if t == "--" {
            after_dd = true;
            i += 1;
            continue;
        }
        if let Some(long) = t.strip_prefix("--") {
            let name = long.split('=').next().unwrap_or(long);
            match name {
                "force" | "force-with-lease" | "force-if-includes" => a.force = true,
                "delete" => a.delete = true,
                "all" | "branches" | "mirror" => a.all = true,
                "tags" => a.tags = true,
                "push-option" | "repo" | "receive-pack" | "exec" if !long.contains('=') => i += 1,
                _ => {}
            }
        } else {
            // A cluster of short options such as `-fu`.
            for c in t[1..].chars() {
                match c {
                    'f' => a.force = true,
                    'd' => a.delete = true,
                    'o' => {
                        if t.ends_with('o') {
                            i += 1;
                        }
                        break;
                    }
                    _ => {}
                }
            }
        }
        i += 1;
    }
    let mut it = positional.into_iter();
    if let Some(r) = it.next() {
        if r.dynamic && r.quoted_var.is_none() {
            a.unreadable.push(r.clone());
        }
        a.remote = Some(r.clone());
    }
    for w in it {
        if w.dynamic {
            if w.quoted_var.is_some() {
                a.specs.push(Spec {
                    word: w.clone(),
                    target: Target::QuotedVar,
                    name: w.raw.clone(),
                });
            } else {
                a.unreadable.push(w.clone());
            }
            continue;
        }
        let mut spec = w.text.as_str();
        if let Some(s) = spec.strip_prefix('+') {
            a.force = true;
            spec = s;
        }
        let dst = match spec.split_once(':') {
            Some((src, dst)) => {
                if src.starts_with('+') {
                    a.force = true;
                }
                if dst.is_empty() {
                    a.delete = true;
                }
                if src.is_empty() {
                    a.delete = true;
                }
                dst
            }
            None => spec,
        };
        let (target, name) = if let Some(tag) = dst.strip_prefix("refs/tags/") {
            (Target::Tag(tag.to_string()), tag.to_string())
        } else {
            let b = dst.strip_prefix("refs/heads/").unwrap_or(dst);
            (Target::Branch(b.to_string()), b.to_string())
        };
        a.specs.push(Spec {
            word: w.clone(),
            target,
            name,
        });
    }
    a
}

// ---------------------------------------------------------------- AI trailers

// The list names AI tools in general. The name of one agent runtime is not listed: this crate
// stays neutral (`pf-engine/tests/port_is_neutral.rs`).
const AI_NAMES: [&str; 11] = [
    "claude",
    "anthropic",
    "openai",
    "chatgpt",
    "copilot",
    "codex",
    "gemini",
    "cursor",
    "aider",
    "gpt",
    "llm",
];

fn mentions_ai(line: &str) -> bool {
    let lower = line.to_lowercase();
    lower
        .split(|c: char| !(c.is_alphanumeric() || c == '-'))
        .any(|tok| {
            let tok = tok.trim_matches('-');
            AI_NAMES.contains(&tok)
                || [
                    "claude",
                    "anthropic",
                    "openai",
                    "chatgpt",
                    "copilot",
                    "gemini",
                ]
                .iter()
                .any(|n| tok.contains(n))
                || tok.starts_with("gpt-")
                || tok.starts_with("gpt4")
        })
}

/// Does the text carry an AI trailer? `Co-Authored-By:` with an AI name, or a "Generated with"
/// line that names one. Line breaks may be real or written `\n`.
pub fn has_ai_trailer(text: &str) -> bool {
    let normalized = text.replace("\\n", "\n").replace("\\r", "\n");
    normalized.lines().any(|line| {
        let l = line.trim().to_lowercase();
        let l = l.trim_start_matches(['-', '*', '>', ' ']);
        let credit = l.starts_with("co-authored-by:") && mentions_ai(l);
        let generated =
            (l.contains("generated with") || l.contains("generated by") || l.starts_with('🤖'))
                && mentions_ai(l);
        credit || generated
    })
}
