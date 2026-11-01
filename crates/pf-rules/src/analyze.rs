//! The rules: what a parsed command line does, and whether a role may do it.

use std::collections::HashMap;
use std::path::{Component, Path, PathBuf};

use pf_workflow::rules::{Allow, RoleRules};

use crate::shell::{Cmd, Parsed, Word, parse};
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

const SHELLS: [&str; 8] = ["sh", "bash", "zsh", "dash", "ksh", "ash", "csh", "tcsh"];

/// Commands that run another command, with options that this crate does not read. A call that
/// uses one is denied, because the command it runs could be any.
const UNREAD_WRAPPERS: [&str; 22] = [
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
                self.record_assigns(&parsed);
                self.parsed(&parsed, depth);
            }
        }
    }

    fn parsed(&mut self, parsed: &Parsed, depth: usize) {
        for (idx, cmd) in parsed.cmds.iter().enumerate() {
            if cmd.header || cmd.words.is_empty() {
                continue;
            }
            for fin in self.unwrap(&cmd.words, depth) {
                self.check(parsed, idx, cmd, &fin, depth);
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
            if !first.any_quote && !first.dynamic && matches!(first.text.as_str(), "{" | "!") {
                // `time { cmd; }`: the keyword that follows a wrapper starts a group. The group
                // holds the command.
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
                "exec" => Some(after_opts(args, &["-a"], 0)),
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
        match name.as_str() {
            n if SHELLS.contains(&n) => self.shell(parsed, idx, cmd, args, depth),
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
                    if words.get(i + 1).is_some_and(|v| sets_alias(&v.text)) {
                        self.deny(
                            "unparsed-command",
                            "git -c defines an alias, and the command it runs cannot be read",
                        );
                    }
                    i += 2;
                }
                "--git-dir" | "--work-tree" | "--namespace" | "--exec-path" | "--super-prefix"
                | "--attr-source" => i += 2,
                _ if t.starts_with("--config-env=") && sets_alias(&t["--config-env=".len()..]) => {
                    self.deny(
                        "unparsed-command",
                        "git --config-env defines an alias, and the command it runs cannot be read",
                    );
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
        let rest = &words[i + 1..];
        if sub_name == "config" && rest.iter().any(|w| sets_alias(&w.text)) {
            self.deny(
                "unparsed-command",
                "git config names an alias, and the command it runs cannot be read",
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
                let targets: Vec<Word> = rest[1..]
                    .iter()
                    .filter(|w| !w.text.starts_with('-'))
                    .cloned()
                    .collect();
                self.delete_targets("git worktree remove", &targets, false, false);
            }
            _ => {}
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
            (Some("api"), _) => self.gh_api(args),
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

/// True for `alias.name=value` and `alias.name`: a git config key that defines an alias.
fn sets_alias(text: &str) -> bool {
    text.trim_start().to_ascii_lowercase().starts_with("alias.")
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
                "all" | "mirror" => a.all = true,
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
