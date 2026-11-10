//! The matching rule of case 20 and of `decode_fixtures` (`r1.s4.w2`, Approach 9, A9): what makes a
//! call a *proposal of a spine* and what makes it an *arm*. One definition serves both, so the
//! recorded `Bash` call and the stub's argv records are judged the same way.
//!
//! - A call is a **proposal of `s`** when its program's basename is `pulse-factory`, its first
//!   argument is `propose`, and `s` is a positional argument. A positional is an argument that is
//!   neither a flag (`-x`, `--x` or `--x=v`) nor the value that follows a flag of the CLI's
//!   `propose` that takes a value (`--workflow`, `--ledger`, `--param`, `--plugin-root`, `--paper`,
//!   `--command-id`), in any order and in either spelling (`--flag v` or `--flag=v`).
//! - A call is an **arm** when its program's basename is `pulse-factory` and its first argument is
//!   `arm`.
//!
//! A recorded `Bash` command is split into simple commands on `;`, `&&`, `||`, `|`, `&` and
//! newlines, then into words by POSIX shell quoting. `env` and `VAR=value` prefixes are skipped.
#![allow(dead_code)]

/// The flags of `pulse-factory propose` that take a value.
pub const VALUE_FLAGS: [&str; 6] = [
    "--workflow",
    "--ledger",
    "--param",
    "--plugin-root",
    "--paper",
    "--command-id",
];

/// The words of every simple command of `command`, in order. Quotes are removed; an operator inside
/// quotes is text. Nothing is expanded (`$x` stays as written).
pub fn simple_commands(command: &str) -> Vec<Vec<String>> {
    let chars: Vec<char> = command.chars().collect();
    let mut out: Vec<Vec<String>> = Vec::new();
    let mut words: Vec<String> = Vec::new();
    let mut word = String::new();
    let mut in_word = false;
    let mut i = 0;
    let end_word = |words: &mut Vec<String>, word: &mut String, in_word: &mut bool| {
        if *in_word {
            words.push(std::mem::take(word));
            *in_word = false;
        }
    };
    let end_command = |out: &mut Vec<Vec<String>>, words: &mut Vec<String>| {
        if !words.is_empty() {
            out.push(std::mem::take(words));
        }
    };
    while i < chars.len() {
        let c = chars[i];
        match c {
            '\'' => {
                in_word = true;
                i += 1;
                while i < chars.len() && chars[i] != '\'' {
                    word.push(chars[i]);
                    i += 1;
                }
            }
            '"' => {
                in_word = true;
                i += 1;
                while i < chars.len() && chars[i] != '"' {
                    if chars[i] == '\\'
                        && i + 1 < chars.len()
                        && matches!(chars[i + 1], '"' | '\\' | '$' | '`')
                    {
                        i += 1;
                    }
                    word.push(chars[i]);
                    i += 1;
                }
            }
            '\\' => {
                in_word = true;
                if i + 1 < chars.len() {
                    i += 1;
                    // A backslash before a newline joins the lines.
                    if chars[i] == '\n' {
                        in_word = !word.is_empty();
                    } else {
                        word.push(chars[i]);
                    }
                }
            }
            ' ' | '\t' => end_word(&mut words, &mut word, &mut in_word),
            '\n' | ';' | '|' | '&' => {
                end_word(&mut words, &mut word, &mut in_word);
                end_command(&mut out, &mut words);
                // `&&` and `||` are one operator.
                if matches!(c, '|' | '&') && chars.get(i + 1) == Some(&c) {
                    i += 1;
                }
            }
            _ => {
                in_word = true;
                word.push(c);
            }
        }
        i += 1;
    }
    end_word(&mut words, &mut word, &mut in_word);
    end_command(&mut out, &mut words);
    out
}

fn is_assignment(word: &str) -> bool {
    match word.split_once('=') {
        Some((name, _)) => {
            !name.is_empty()
                && !name.starts_with(|c: char| c.is_ascii_digit())
                && name.chars().all(|c| c.is_ascii_alphanumeric() || c == '_')
        }
        None => false,
    }
}

/// The program and its arguments, with the `env` and `VAR=value` prefixes skipped.
pub fn program_and_args(words: &[String]) -> Option<(&str, &[String])> {
    let mut i = 0;
    while i < words.len() {
        let w = words[i].as_str();
        if is_assignment(w) || w == "env" {
            i += 1;
            continue;
        }
        return Some((w, &words[i + 1..]));
    }
    None
}

fn basename(program: &str) -> &str {
    program.rsplit('/').next().unwrap_or(program)
}

/// The positional arguments of a `propose` call: what is left of `args` (after `propose`) once the
/// flags and their values are removed.
pub fn positionals(args: &[String]) -> Vec<&str> {
    let mut out = Vec::new();
    let mut i = 0;
    while i < args.len() {
        let a = args[i].as_str();
        if a.starts_with('-') && a.len() > 1 {
            if !a.contains('=') && VALUE_FLAGS.contains(&a) {
                i += 1; // the value
            }
        } else {
            out.push(a);
        }
        i += 1;
    }
    out
}

/// Is `words` a proposal of `spine`?
pub fn is_proposal_of(words: &[String], spine: &str) -> bool {
    let Some((program, args)) = program_and_args(words) else {
        return false;
    };
    basename(program) == "pulse-factory"
        && args.first().map(String::as_str) == Some("propose")
        && positionals(&args[1..]).contains(&spine)
}

/// Is `words` an `arm` call?
pub fn is_arm(words: &[String]) -> bool {
    let Some((program, args)) = program_and_args(words) else {
        return false;
    };
    basename(program) == "pulse-factory" && args.first().map(String::as_str) == Some("arm")
}

/// The proposals of `spine` and the arms among the simple commands of one `Bash` command.
pub fn judge_bash(command: &str, spine: &str) -> (usize, usize) {
    let mut proposals = 0;
    let mut arms = 0;
    for words in simple_commands(command) {
        if is_proposal_of(&words, spine) {
            proposals += 1;
        }
        if is_arm(&words) {
            arms += 1;
        }
    }
    (proposals, arms)
}

/// The records of a stub's `calls` file: one call per line, its words NUL-terminated (`$0` first).
pub fn stub_calls(text: &str) -> Vec<Vec<String>> {
    text.split('\n')
        .filter(|r| !r.is_empty())
        .map(|r| {
            r.split('\0')
                .filter(|w| !w.is_empty())
                .map(str::to_string)
                .collect()
        })
        .collect()
}
