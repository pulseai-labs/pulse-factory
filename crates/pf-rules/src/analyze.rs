//! The rules: what a parsed command line does, and whether a role may do it.

use std::path::{Component, Path, PathBuf};

use pf_workflow::rules::{Allow, RoleRules};

use crate::shell::{Cmd, Parsed, Word, parse};
use crate::{Context, PushIntent};

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

// ---------------------------------------------------------------- walking

pub struct Analyzer<'a> {
    pub rules: &'a RoleRules,
    pub ctx: &'a Context,
    pub walk: Walk,
}

impl<'a> Analyzer<'a> {
    pub fn new(rules: &'a RoleRules, ctx: &'a Context) -> Analyzer<'a> {
        Analyzer {
            rules,
            ctx,
            walk: Walk::default(),
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
            Ok(parsed) => self.parsed(&parsed, depth),
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
            if first.dynamic {
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
            "rm" | "rmdir" | "unlink" | "shred" => self.rm(args, fin.stdin_operands),
            "find" => self.find_delete(args),
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

    // ------------------------------------------------------------ git

    fn git(&mut self, words: &[Word], cmd: &Cmd, parsed: &Parsed) {
        let mut i = 1;
        let mut dir: Option<Word> = None;
        while i < words.len() {
            let t = words[i].text.as_str();
            match t {
                "-C" => {
                    dir = words.get(i + 1).cloned();
                    i += 2;
                }
                "-c" | "--git-dir" | "--work-tree" | "--namespace" | "--exec-path"
                | "--super-prefix" | "--config-env" | "--attr-source" => i += 2,
                _ if t.starts_with('-') => i += 1,
                _ => break,
            }
        }
        let Some(sub) = words.get(i) else { return };
        if sub.dynamic {
            self.deny("unparsed-command", "the git subcommand is not readable");
            return;
        }
        let sub_name = sub.text.as_str();
        let rest = &words[i + 1..];
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
            "push" => self.git_push(rest, dir.as_ref()),
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
                    self.delete_targets("git clean", &targets, false);
                }
            }
            "worktree" if rest.first().is_some_and(|w| w.text == "remove") => {
                let targets: Vec<Word> = rest[1..]
                    .iter()
                    .filter(|w| !w.text.starts_with('-'))
                    .cloned()
                    .collect();
                self.delete_targets("git worktree remove", &targets, false);
            }
            _ => {}
        }
    }

    /// `git push`: force, delete, the default branch and the role's own branch or tag.
    fn git_push(&mut self, rest: &[Word], dir: Option<&Word>) {
        let p = push_args(rest);
        for w in &p.unreadable {
            self.deny(
                "push-unreadable",
                format!("the push has {:?}, which cannot be read (a variable outside quotes, or one that changes the meaning)", w.raw),
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
        // The intents for `push_of`.
        let intents = p.intents(dir);
        self.walk.pushes.extend(intents);
        if !p.unreadable.is_empty() || p.force || p.delete || p.all || p.tags {
            return;
        }
        let ctx = self.ctx;
        let default = ctx.default_branch.as_str();
        // Each target the push names; a bare push names the current branch.
        let mut targets: Vec<Target> = p.specs.iter().map(|s| s.target.clone()).collect();
        if targets.is_empty() {
            targets.push(Target::Branch("HEAD".to_string()));
        }
        for t in targets {
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
                        self.allow("push-own-branch", format!("pushes its own branch {name}"));
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

    fn rm(&mut self, args: &[Word], stdin_operands: bool) {
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
        self.delete_targets("rm", &targets, stdin_operands);
    }

    fn find_delete(&mut self, args: &[Word]) {
        if args.iter().any(|a| a.text == "-delete") {
            let starts = find_starts(args);
            let targets = if starts.is_empty() {
                vec![Word::literal(".")]
            } else {
                starts
            };
            self.delete_targets("find -delete", &targets, false);
        }
    }

    /// The delete class: only inside the seat's own paths, and only when the role lists it.
    fn delete_targets(&mut self, what: &str, targets: &[Word], stdin_operands: bool) {
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
                self.deny(
                    "delete-unreadable",
                    format!(
                        "{what} {:?}: a path held in a variable cannot be read",
                        t.raw
                    ),
                );
                return;
            }
            if !scratch && !build {
                self.deny(
                    "delete-not-allowed",
                    format!("role {} may not delete", self.rules.role),
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
            if t.glob {
                // Judge the directory in front of the first glob character.
                let cut = path.find(['*', '?', '[']).unwrap_or(path.len());
                let dir = &path[..cut];
                path = match dir.rfind('/') {
                    Some(k) => dir[..=k].to_string(),
                    None => ".".to_string(),
                };
            }
            let abs = normalize(&self.ctx.cwd, &path);
            let in_scratch = scratch && under(&abs, &self.ctx.paths.scratch);
            let in_build = build && under(&abs, &self.ctx.paths.build_target);
            if in_scratch {
                self.allow(
                    "delete-own-scratch",
                    format!("{what} inside the seat's scratch"),
                );
            } else if in_build {
                self.allow(
                    "delete-own-build-target",
                    format!("{what} inside the seat's build target"),
                );
            } else {
                self.deny(
                    "delete-outside-own-paths",
                    format!("{what} {:?} lies outside the seat's own paths", t.text),
                );
                return;
            }
        }
    }
}

// ---------------------------------------------------------------- helpers

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
    pub fn intents(&self, dir: Option<&Word>) -> Vec<PushIntent> {
        use crate::Part;
        let part = |w: &Word| {
            if w.dynamic {
                Part::Unresolved(w.raw.clone())
            } else {
                Part::Literal(w.text.clone())
            }
        };
        let remote = self.remote.as_ref().map_or(Part::Absent, part);
        let dir = dir.map_or(Part::Absent, part);
        if self.specs.is_empty() {
            return vec![PushIntent {
                remote,
                branch: Part::Absent,
                dir,
            }];
        }
        self.specs
            .iter()
            .map(|s| PushIntent {
                remote: remote.clone(),
                branch: if s.word.dynamic {
                    Part::Unresolved(s.word.raw.clone())
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
