//! The grammar of the shell that `pf-rules` reads (ADR-0005, SPINE.md grill lock G11).
//!
//! The parser reads a closed set of forms. A command line may reach the guards only when each part
//! of it is one of the forms in [`ALLOWED`]. A form that is not in the list is denied as
//! `unparsed-command`, with the form named in the reason, so a shell form that nobody has seen yet
//! fails closed with no new code. The guards run first: a line that a guard denies keeps the rule of
//! that guard, and the grammar deny applies only when no guard denied it.
//!
//! `docs/contracts/role-rules.md` repeats the list, and a test keeps the two in step.

/// One form of the grammar: a name for the reason of a deny, and the shape that it allows.
#[derive(Clone, Copy, Debug)]
pub struct Form {
    pub name: &'static str,
    pub shape: &'static str,
}

const fn form(name: &'static str, shape: &'static str) -> Form {
    Form { name, shape }
}

/// The forms that may reach the guards. The inner text of a substitution, a `trap` body, an `eval`
/// string and a shell string is a command line under the same list, and passes the same guards.
pub const ALLOWED: &[Form] = &[
    // Words, assignments before the command, and redirections.
    form(
        "simple command",
        "words, NAME=value and NAME+=value before it, and redirections",
    ),
    // A pipeline joins stages with a pipe.
    form(
        "pipeline",
        "simple or compound commands joined by | or |&, and ! before a pipeline",
    ),
    // A list joins pipelines.
    form(
        "list",
        "pipelines joined by &&, ||, ;, a newline or a trailing &",
    ),
    // The conditional compound command.
    form("if", "if, then, elif, else, fi"),
    // The counting loop over words.
    form("for", "for NAME in WORDS; do …; done"),
    // The loops that test a command.
    form(
        "while and until",
        "while …; do …; done and until …; do …; done",
    ),
    // The branching compound command.
    form("case", "case WORD in PATTERN) …;; esac, with ;& and ;;&"),
    // A group runs in the current shell.
    form("group", "{ …; }"),
    // A subshell runs a body that is itself in the grammar (ruling of 2026-11-03).
    form("subshell", "( … ), with a body in the grammar"),
    // A function definition and its calls (ruling of 2026-11-03).
    form(
        "function",
        "function NAME { … } and NAME() { … }, with a body in the grammar",
    ),
    // A coprocess with a body that the parser reads (ruling of 2026-11-03).
    form(
        "coproc",
        "coproc followed by { … }, ( … ), while, until, if, for or case",
    ),
    // An array literal holds values, and its substitutions are read as commands.
    form(
        "array literal",
        "NAME=(…), NAME+=(…), declare -a NAME=(…) and local -a NAME=(…)",
    ),
    // The conditional command, kept because the real blocks and the landed tests use it.
    form("conditional", "[[ … ]]"),
    // The test command and the other builtins are simple commands.
    form(
        "test",
        "[ … ], test … and every other builtin as a simple command",
    ),
    // A time wrapper may stand before any command.
    form(
        "time",
        "time and time -p before any command, simple or compound",
    ),
    // Wrappers whose next word is the real command.
    form(
        "wrapper",
        "timeout, nice, ionice, nohup, setsid, stdbuf, command, builtin, sudo, doas, env, xargs, flock and find -exec",
    ),
    // Text that no expansion touches, and text the shell reads as it stands.
    form(
        "literal word",
        "literal text, single quotes, double quotes, brace expansion and globs",
    ),
    // A variable.
    form(
        "variable",
        "$NAME, ${NAME}, ${#NAME} and the parameters $? $$ $! $# $@ $* $- $0 to $9",
    ),
    // A default word or a trim word, whose text is itself a word under this list.
    form(
        "default word",
        "${NAME:-word}, ${NAME-word}, ${NAME:+word}, ${NAME+word}, ${NAME#word}, ${NAME##word}, ${NAME%word} and ${NAME%%word}",
    ),
    // Arithmetic that holds no command and no array index.
    form(
        "arithmetic expansion",
        "$((…)) holding only names, numbers, spaces and + - * / % < > = ! & | ( )",
    ),
    // A command substitution is read as a command line.
    form(
        "command substitution",
        "$(…) and backticks, with an inner text in the grammar",
    ),
    // A process substitution is read as a command line.
    form(
        "process substitution",
        "<(…) and >(…), with an inner text in the grammar",
    ),
    // A comment runs to the end of the line.
    form("comment", "# to the end of the line"),
    // The redirection operators, with a target that is a word.
    form(
        "redirection",
        "> >> >| < <> <& >& &> &>> with an optional file descriptor, and a word as the target",
    ),
    // A heredoc or a herestring feeds a command that is not an interpreter.
    form(
        "heredoc",
        "<<WORD, <<-WORD and <<<WORD fed to a command that is not an interpreter",
    ),
    // A trap body is a command line.
    form("trap", "trap 'body' SIGNAL, with a body in the grammar"),
    // An eval string is a command line when every argument is literal.
    form(
        "eval",
        "eval with literal arguments, read as a command line",
    ),
];

/// The forms that are denied as `unparsed-command`, with the form named in the reason. A form that
/// is neither in [`ALLOWED`] nor here is denied the same way.
pub const DENIED: &[Form] = &[
    // The old arithmetic expansion.
    form("$[…]", "the old arithmetic expansion"),
    // The arithmetic command evaluates text that the rules do not read.
    form(
        "((…))",
        "the arithmetic command, and the C-style for ((…;…;…))",
    ),
    // Arithmetic that holds anything outside the list.
    form(
        "$((…)) outside its list",
        "$((…)) holding $, [ ], a command substitution or another character",
    ),
    // Every other parameter expansion.
    form(
        "other ${…}",
        "${x:=y}, ${x/a/b}, ${!x}, ${x:1:2}, ${x[1]}, ${x@P} and every other form",
    ),
    // ANSI-C quoting hides bytes from the reader.
    form("$'…'", "ANSI-C quoting"),
    // Locale quoting.
    form("$\"…\"", "locale quoting"),
    // The loop that reads a menu.
    form("select", "select NAME in WORDS; do …; done"),
    // `exec` replaces the shell with a command.
    form("exec", "exec, with or without a command"),
    // `source` and `.` run a file whose text the rules cannot read.
    form("source", "source FILE and . FILE"),
    // A heredoc that feeds an interpreter holds a program the rules cannot read.
    form(
        "heredoc to an interpreter",
        "<<WORD or <<<WORD fed to python, perl, ruby, node and the like",
    ),
    // A wrapper whose options the rules do not read.
    form(
        "unread wrapper",
        "taskset, chrt, strace, su, watch, parallel and the like",
    ),
    // Everything that the parser cannot read.
    form(
        "parse error",
        "an unclosed quote or substitution, coproc without a body, a redirection without a target, and any text the parser cannot read",
    ),
];

/// Commands that read a program from standard input. A heredoc or a herestring that feeds one is
/// denied: the program in it is not shell, and the rules cannot read it.
///
/// Source: python(1), perl(1), ruby(1), node(1), deno(1), bun(1), php(1), lua(1), tclsh(1),
/// Rscript(1), julia(1), pwsh(1), osascript(1), expect(1). Each reads a program from standard
/// input when it has no script operand or when the operand is `-`.
pub const INTERPRETERS: &[&str] = &[
    "python",
    "perl",
    "ruby",
    "node",
    "nodejs",
    "deno",
    "bun",
    "php",
    "lua",
    "luajit",
    "tclsh",
    "wish",
    "Rscript",
    "julia",
    "pwsh",
    "osascript",
    "expect",
];

/// Is this command name an interpreter? A version suffix such as `python3.12` counts.
pub fn is_interpreter(name: &str) -> bool {
    INTERPRETERS.iter().any(|i| {
        name.strip_prefix(i)
            .is_some_and(|rest| rest.chars().all(|c| c.is_ascii_digit() || c == '.'))
    })
}

/// Does the text of an arithmetic expansion hold only names, numbers, spaces and the operators
/// `+ - * / % < > = ! & | ( )`?
pub fn arithmetic_ok(text: &str) -> bool {
    text.chars().all(|c| {
        c.is_ascii_alphanumeric()
            || matches!(
                c,
                '_' | ' '
                    | '\t'
                    | '+'
                    | '-'
                    | '*'
                    | '/'
                    | '%'
                    | '<'
                    | '>'
                    | '='
                    | '!'
                    | '&'
                    | '|'
                    | '('
                    | ')'
            )
    })
}

/// The reason of a deny for a form: the form is named, and the grammar is the cause.
pub fn reason(form: &str) -> String {
    format!(
        "{form} is outside the grammar of readable shell (docs/contracts/role-rules.md), so it is denied"
    )
}
