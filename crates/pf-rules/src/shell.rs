//! A small shell parser. It finds the simple commands in a command line and nothing more.
//!
//! It reads quotes, `$'…'`, heredocs, here-strings, `&&`, `||`, `;`, pipes, `$(…)`, backticks,
//! process substitution, `if … fi`, `for`, `while`, `case`, `{ …; }`, subshells, assignments,
//! environment prefixes and redirections. It never runs anything and expands nothing: a variable
//! stays a variable. A text it cannot read is an `Err`, and the caller denies it.
//!
//! The parser also keeps the grammar (`crate::grammar`). A form that it passes over without
//! modelling it, such as `${x:=y}` or `$'…'`, is not an `Err`: its source text goes in
//! `Parsed::outside`, and the caller denies the line after the guards have had their say. A
//! parameter expansion is read by a closed rule (`param`), so a form that the rule does not list
//! lands in `outside` with no new code.

/// What one part of a word is.
#[derive(Clone, Debug, PartialEq, Eq)]
enum Seg {
    /// Text that the shell reads as it stands.
    Lit(String),
    /// Text that no expansion touches, such as the inside of single quotes.
    Quoted(String),
    /// A variable. `quoted` is true inside double quotes.
    Var { name: String, quoted: bool },
    /// A command substitution, an arithmetic expansion or something else the parser cannot
    /// evaluate.
    Subst,
}

/// One word of a command.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Word {
    /// The word without its quotes. A variable stays as `$name`.
    pub text: String,
    /// The word as written, quotes included. For a `$(…)` inside it, this holds the whole
    /// substitution, heredocs included.
    pub raw: String,
    /// True when an expansion (a variable or a substitution) is part of the word.
    pub dynamic: bool,
    /// The variable name when the word is exactly one variable in double quotes.
    pub quoted_var: Option<String>,
    /// True when an unquoted `*`, `?` or `[` is in the word.
    pub glob: bool,
    /// True when any part of the word was in quotes.
    pub any_quote: bool,
    /// The words that an unquoted brace expansion makes of this word, such as `{merge,x}`.
    /// Empty when the word has none. The parser puts these words in the command in its place.
    pub alts: Vec<String>,
    /// True when a brace expansion mixes quoted text with other parts, such as `"--no-ver"{ify,}`.
    /// The words it makes are not read, so a flag could hide in them.
    pub brace_quoted: bool,
}

impl Word {
    /// A word of plain text, for a caller that builds a command line itself.
    pub fn literal(text: &str) -> Word {
        Word {
            text: text.to_string(),
            raw: text.to_string(),
            dynamic: false,
            quoted_var: None,
            glob: false,
            any_quote: false,
            alts: Vec::new(),
            brace_quoted: false,
        }
    }
}

/// One simple command.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Cmd {
    /// The words, without assignments and redirections.
    pub words: Vec<Word>,
    /// `NAME=value` words before the command.
    pub assigns: Vec<Word>,
    /// The bodies of the heredocs that feed this command.
    pub heredocs: Vec<String>,
    /// The word of `<<<`.
    pub herestring: Option<Word>,
    /// The index of the command whose output pipes into this one.
    pub piped_from: Option<usize>,
    /// True for a `for`, `select` or `case` header: it holds no command.
    pub header: bool,
    /// True when the header is a `for` or a `select`.
    pub loop_header: bool,
    /// The words of a `for` or `select` header after the keyword. The first is the variable.
    pub head: Vec<Word>,
    /// 1 after `function`: the next word names the function. 2 after `coproc`: the next word
    /// names the coprocess when a body follows it. 0 otherwise.
    pub name_next: u8,
}

/// All commands of a command line, nested ones included.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Parsed {
    pub cmds: Vec<Cmd>,
    /// The forms outside the grammar that the parser passed over, each as its source text with a
    /// name. The caller denies the line as `unparsed-command` when no guard denied it.
    pub outside: Vec<String>,
}

const RESERVED: [&str; 13] = [
    "if", "then", "elif", "else", "fi", "while", "until", "do", "done", "!", "{", "function",
    "coproc",
];

/// The words that may follow `coproc`: a group, or a compound command that the parser reads
/// (`(` is not a word: the parser reads it as a subshell). Any other word after `coproc` is
/// denied as unread.
const COPROC_BODY: [&str; 7] = ["{", "while", "until", "if", "for", "select", "case"];

/// Is this word a shell keyword that starts a compound command or a group? After a wrapper such
/// as `time`, the command that the keyword introduces is the one that runs.
pub fn is_keyword(text: &str) -> bool {
    RESERVED.contains(&text) || text == "}"
}

/// Parse a command line.
pub fn parse(src: &str) -> Result<Parsed, String> {
    let chars: Vec<char> = src.chars().collect();
    let mut p = Parser {
        chars: &chars,
        i: 0,
        cmds: Vec::new(),
        outside: Vec::new(),
        closers: Vec::new(),
    };
    p.list(None, 0)?;
    Ok(Parsed {
        cmds: p.cmds,
        outside: p.outside,
    })
}

struct Parser<'a> {
    chars: &'a [char],
    i: usize,
    cmds: Vec<Cmd>,
    /// Forms outside the grammar that were passed over (see `Parsed::outside`).
    outside: Vec<String>,
    /// The closing character of each open `list`, innermost last.
    closers: Vec<Option<char>>,
}

/// What a word at the start of a command does to the parser.
enum Action {
    Nothing,
    /// A `case` header began.
    CaseHeader,
    /// The `in` of a `case` header.
    CaseIn,
    /// An `esac`.
    Esac,
}

struct Heredoc {
    delim: String,
    quoted: bool,
    strip_tabs: bool,
    cmd: usize,
}

fn is_name_start(c: char) -> bool {
    c.is_ascii_alphabetic() || c == '_'
}

fn is_name_char(c: char) -> bool {
    c.is_ascii_alphanumeric() || c == '_'
}

/// Is this `NAME=` (or `NAME+=`) at the start of the text?
fn assignment_prefix(text: &str) -> bool {
    let mut it = text.chars();
    match it.next() {
        Some(c) if is_name_start(c) => {}
        _ => return false,
    }
    let rest: String = text.chars().skip(1).collect();
    let name_len = rest.chars().take_while(|c| is_name_char(*c)).count();
    let after: String = rest.chars().skip(name_len).collect();
    after.starts_with('=') || after.starts_with("+=")
}

impl Parser<'_> {
    fn peek(&self) -> Option<char> {
        self.chars.get(self.i).copied()
    }

    fn peek_at(&self, n: usize) -> Option<char> {
        self.chars.get(self.i + n).copied()
    }

    fn starts_with(&self, s: &str) -> bool {
        s.chars()
            .enumerate()
            .all(|(k, c)| self.peek_at(k) == Some(c))
    }

    /// The source text from `start` up to the current position.
    fn since(&self, start: usize) -> String {
        self.chars[start..self.i.min(self.chars.len())]
            .iter()
            .collect()
    }

    /// Record a form outside the grammar. The line is denied after the guards have run.
    fn note(&mut self, form: String) {
        self.outside.push(form);
    }

    /// Find the commands in text that the parser passed over, such as the inside of `${x:=…}`, so
    /// that a guard still sees them.
    fn scan_text(&mut self, text: &str, depth: usize) -> Result<(), String> {
        let sub: Vec<char> = text.chars().collect();
        let mut inner = Parser {
            chars: &sub,
            i: 0,
            cmds: Vec::new(),
            outside: Vec::new(),
            closers: Vec::new(),
        };
        inner.scan_expansions(depth + 1)?;
        self.cmds.extend(inner.cmds);
        self.outside.extend(inner.outside);
        Ok(())
    }

    /// Read commands until the end, or until the `)` or backtick that closes a substitution.
    fn list(&mut self, close: Option<char>, depth: usize) -> Result<(), String> {
        if depth > 24 {
            return Err("substitutions nest too deep".to_string());
        }
        self.closers.push(close);
        let result = self.list_inner(close, depth);
        self.closers.pop();
        result
    }

    fn list_inner(&mut self, close: Option<char>, depth: usize) -> Result<(), String> {
        let mut cur: Option<usize> = None;
        let mut pipe_from: Option<usize> = None;
        let mut heredocs: Vec<Heredoc> = Vec::new();
        let mut paren_depth = 0usize;
        let mut in_case = 0usize;
        let mut case_headers: Vec<usize> = Vec::new();
        // True while the patterns of a `case` arm are read. They are not commands.
        let mut in_pattern = false;
        // The depth of `@(…)` and `+(…)` groups inside a pattern.
        let mut pat_paren = 0usize;
        loop {
            // Skip blanks and line continuations.
            while let Some(c) = self.peek() {
                if c == ' ' || c == '\t' || c == '\r' {
                    self.i += 1;
                } else if c == '\\' && self.peek_at(1) == Some('\n') {
                    self.i += 2;
                } else {
                    break;
                }
            }
            let Some(c) = self.peek() else {
                if close.is_some() {
                    return Err("a substitution is not closed".to_string());
                }
                self.fill_heredocs(&mut heredocs, depth)?;
                return Ok(());
            };
            match c {
                '#' => {
                    while self.peek().is_some_and(|c| c != '\n') {
                        self.i += 1;
                    }
                }
                '\n' => {
                    self.i += 1;
                    cur = None;
                    pipe_from = None;
                    self.fill_heredocs(&mut heredocs, depth)?;
                }
                ';' => {
                    self.i += 1;
                    let mut arm_end = false;
                    if self.peek() == Some(';') || self.peek() == Some('&') {
                        self.i += 1;
                        arm_end = true;
                    }
                    cur = None;
                    pipe_from = None;
                    if arm_end && in_case > 0 {
                        // `;;`, `;&` and `;;&` end an arm. The next words are patterns.
                        in_pattern = true;
                    }
                }
                '&' => {
                    if self.peek_at(1) == Some('>') {
                        // `&>file` and `&>>file`.
                        self.i += 2;
                        if self.peek() == Some('>') {
                            self.i += 1;
                        }
                        self.skip_target(depth)?;
                        if cur.is_none() {
                            cur = Some(self.new_cmd(pipe_from.take()));
                        }
                        continue;
                    }
                    self.i += 1;
                    if self.peek() == Some('&') {
                        self.i += 1;
                    }
                    cur = None;
                    pipe_from = None;
                }
                '|' => {
                    self.i += 1;
                    if self.peek() == Some('|') {
                        self.i += 1;
                        pipe_from = None;
                    } else {
                        if self.peek() == Some('&') {
                            self.i += 1;
                        }
                        pipe_from = cur;
                    }
                    cur = None;
                    if in_pattern {
                        pipe_from = None;
                    }
                }
                '(' => {
                    if in_pattern {
                        // The `(` that opens a pattern, or an extended glob group inside it.
                        if self.i > 0
                            && matches!(self.chars[self.i - 1], '?' | '*' | '+' | '@' | '!')
                        {
                            pat_paren += 1;
                        }
                        self.i += 1;
                    } else if cur.is_some() && self.i > 0 && self.chars[self.i - 1] == '=' {
                        // `name=( … )`: an array. Its words are values, not commands.
                        self.i += 1;
                        self.array_literal(depth)?;
                        // The variable now holds values the rules do not read. Mark its assign
                        // word so that `name=(` is not the empty string.
                        if let Some(idx) = cur {
                            let c = &mut self.cmds[idx];
                            let last = if c.words.is_empty() {
                                c.assigns.last_mut()
                            } else {
                                c.words.last_mut()
                            };
                            if let Some(w) = last {
                                w.raw.push('(');
                            }
                        }
                    } else if self.peek_at(1) == Some('(') && cur.is_none() {
                        // `(( … ))` arithmetic command: its text is not read, so it is outside the
                        // grammar. A command substitution in it is still found.
                        let start = self.i;
                        self.i += 2;
                        let (inner, _) = self.arith()?;
                        let raw = self.since(start);
                        self.note(format!("`{raw}` (the arithmetic command)"));
                        self.scan_text(&inner, depth)?;
                    } else if cur.is_some_and(|k| {
                        self.cmds[k].loop_header
                            && self.cmds[k].head.is_empty()
                            && self.peek_at(1) == Some('(')
                    }) {
                        // `for ((i=0; i<3; i++))`: the text is not read.
                        let start = self.i;
                        self.i += 2;
                        let (inner, _) = self.arith()?;
                        let raw = self.since(start);
                        self.note(format!("`{raw}` (a C-style `for` loop)"));
                        self.scan_text(&inner, depth)?;
                    } else if let Some(idx) = cur
                        .filter(|k| self.cmds[*k].words.len() == 1 && self.peek_at(1) == Some(')'))
                    {
                        // `name() { … }`: a function definition. Its body is read as commands.
                        self.i += 2;
                        self.cmds[idx].words.clear();
                        cur = None;
                    } else {
                        self.i += 1;
                        paren_depth += 1;
                        cur = None;
                    }
                }
                ')' => {
                    if in_pattern {
                        self.i += 1;
                        if pat_paren > 0 {
                            pat_paren -= 1;
                        } else {
                            in_pattern = false;
                            cur = None;
                            pipe_from = None;
                        }
                    } else if paren_depth > 0 {
                        self.i += 1;
                        paren_depth -= 1;
                        cur = None;
                        pipe_from = None;
                    } else if in_case > 0 {
                        // The end of a case pattern: what came before it is not a command.
                        self.i += 1;
                        if let Some(idx) = cur {
                            self.cmds[idx].words.clear();
                            self.cmds[idx].header = true;
                        }
                        cur = None;
                        pipe_from = None;
                    } else if close == Some(')') {
                        self.i += 1;
                        self.fill_heredocs(&mut heredocs, depth)?;
                        return Ok(());
                    } else {
                        // A stray `)`, such as the end of a case pattern. It ends the command.
                        self.i += 1;
                        cur = None;
                    }
                }
                '`' if close == Some('`') => {
                    self.i += 1;
                    self.fill_heredocs(&mut heredocs, depth)?;
                    return Ok(());
                }
                '<' | '>' => {
                    self.redirect(depth, &mut heredocs, &mut cur, &mut pipe_from)?;
                }
                _ => {
                    // A digit word right before `<` or `>` is a file descriptor.
                    if c.is_ascii_digit() {
                        let mut k = 0;
                        while self.peek_at(k).is_some_and(|d| d.is_ascii_digit()) {
                            k += 1;
                        }
                        if matches!(self.peek_at(k), Some('<' | '>')) {
                            self.i += k;
                            self.redirect(depth, &mut heredocs, &mut cur, &mut pipe_from)?;
                            continue;
                        }
                    }
                    let word = self.word(depth)?;
                    if in_pattern
                        && !word.any_quote
                        && !word.dynamic
                        && word.alts.is_empty()
                        && word.text == "esac"
                    {
                        // An arm ended with `;;` and the next word closes the `case`.
                        in_case = in_case.saturating_sub(1);
                        in_pattern = false;
                        continue;
                    }
                    let pattern_cmd = in_pattern;
                    let idx = *cur.get_or_insert_with(|| {
                        let v = self.cmds.len();
                        self.cmds.push(Cmd {
                            piped_from: pipe_from.take(),
                            header: pattern_cmd,
                            ..Cmd::default()
                        });
                        v
                    });
                    // Brace expansion happens before the command runs: one word may be several.
                    let words = if word.alts.is_empty() {
                        vec![word]
                    } else {
                        // An empty result of an unquoted expansion is dropped, as the shell does.
                        word.alts
                            .iter()
                            .filter(|alt| !alt.is_empty())
                            .map(|alt| Word {
                                text: alt.clone(),
                                raw: alt.clone(),
                                dynamic: false,
                                quoted_var: None,
                                glob: word.glob,
                                any_quote: false,
                                alts: Vec::new(),
                                brace_quoted: false,
                            })
                            .collect()
                    };
                    for word in words {
                        match self.add_word(idx, word, &case_headers)? {
                            Action::Nothing => {}
                            Action::CaseHeader => case_headers.push(idx),
                            Action::CaseIn => {
                                case_headers.retain(|k| *k != idx);
                                in_case += 1;
                                in_pattern = true;
                            }
                            Action::Esac => {
                                in_case = in_case.saturating_sub(1);
                                in_pattern = false;
                            }
                        }
                    }
                }
            }
        }
    }

    /// Read the inside of `name=( … )` after the `(`. The words are values. A substitution in
    /// one is still read as commands.
    fn array_literal(&mut self, depth: usize) -> Result<(), String> {
        loop {
            while self
                .peek()
                .is_some_and(|c| matches!(c, ' ' | '\t' | '\r' | '\n'))
            {
                self.i += 1;
            }
            match self.peek() {
                None => return Err("an array is not closed".to_string()),
                Some(')') => {
                    self.i += 1;
                    return Ok(());
                }
                Some('#') => {
                    while self.peek().is_some_and(|c| c != '\n') {
                        self.i += 1;
                    }
                }
                Some(';' | '&' | '|' | '<' | '>' | '(') => {
                    return Err("an array holds an operator that is not read".to_string());
                }
                Some(_) => {
                    let before = self.i;
                    self.word(depth)?;
                    if self.i == before {
                        return Err("an array holds text that is not read".to_string());
                    }
                }
            }
        }
    }

    /// True when the next thing after blanks is `{` or `(`.
    fn body_follows(&self) -> bool {
        let mut k = 0;
        while matches!(self.peek_at(k), Some(' ' | '\t')) {
            k += 1;
        }
        matches!(self.peek_at(k), Some('{' | '('))
    }

    fn new_cmd(&mut self, piped_from: Option<usize>) -> usize {
        self.cmds.push(Cmd {
            piped_from,
            ..Cmd::default()
        });
        self.cmds.len() - 1
    }

    /// Put a word into a command, treating assignments and reserved words at the start.
    fn add_word(
        &mut self,
        idx: usize,
        word: Word,
        case_headers: &[usize],
    ) -> Result<Action, String> {
        let plain = !word.any_quote && !word.dynamic;
        let body_follows = self.body_follows();
        let cmd = &mut self.cmds[idx];
        if cmd.header {
            // The rest of a `for`, `select` or `case` header holds no command.
            if plain && word.text == "in" && case_headers.contains(&idx) {
                return Ok(Action::CaseIn);
            }
            if cmd.loop_header {
                cmd.head.push(word);
            }
            return Ok(Action::Nothing);
        }
        if cmd.words.is_empty() {
            if cmd.name_next != 0 {
                // The name after `function`, or after `coproc` when a body follows it. The body
                // is read as commands, so the name must not become the command name.
                let mode = std::mem::take(&mut cmd.name_next);
                if mode == 2 && !(plain && COPROC_BODY.contains(&word.text.as_str())) {
                    // A plain word after `coproc` is a name or a simple command, and no lookahead
                    // tells them apart for every spelling. Deny what is not modelled.
                    return Err(format!(
                        "the word {:?} after `coproc` may be a name or a command",
                        word.raw
                    ));
                }
                if !(plain && word.text == "{") && (mode == 1 || body_follows) {
                    return Ok(Action::Nothing);
                }
            }
            if plain && word.text == "function" {
                cmd.name_next = 1;
                return Ok(Action::Nothing);
            }
            if plain && word.text == "coproc" {
                cmd.name_next = 2;
                return Ok(Action::Nothing);
            }
            if assignment_prefix(&word.raw) {
                cmd.assigns.push(word);
                return Ok(Action::Nothing);
            }
            if plain && word.text == "esac" {
                return Ok(Action::Esac);
            }
            if plain && (RESERVED.contains(&word.text.as_str()) || word.text == "}") {
                // Skip the reserved word. The command that follows it uses the same slot.
                return Ok(Action::Nothing);
            }
            if plain && matches!(word.text.as_str(), "for" | "select") {
                cmd.header = true;
                cmd.loop_header = true;
                if word.text == "select" {
                    self.outside.push("`select` (a menu loop)".to_string());
                }
                return Ok(Action::Nothing);
            }
            if plain && word.text == "case" {
                cmd.header = true;
                return Ok(Action::CaseHeader);
            }
        } else if plain
            && word.text == "case"
            && cmd.words[0].text == "time"
            && !cmd.words[0].any_quote
            && !cmd.words[0].dynamic
            && cmd.words[1..].iter().all(|w| {
                w.text.starts_with('-')
                    || (!w.any_quote
                        && !w.dynamic
                        && matches!(
                            w.text.as_str(),
                            "{" | "!" | "while" | "until" | "if" | "time" | "coproc"
                        ))
            })
        {
            // `time case …`, `time -p case …`, and `case` after `time { `, `time ! `,
            // `time while `, `time until `, `time if `, a second `time` and `time coproc `: the
            // `case` header holds no command.
            cmd.header = true;
            return Ok(Action::CaseHeader);
        }
        cmd.words.push(word);
        Ok(Action::Nothing)
    }

    /// Read heredoc bodies that wait for a line break.
    fn fill_heredocs(&mut self, pending: &mut Vec<Heredoc>, depth: usize) -> Result<(), String> {
        for h in std::mem::take(pending) {
            let mut body = String::new();
            let mut found = false;
            while self.i < self.chars.len() {
                let start = self.i;
                while self.peek().is_some_and(|c| c != '\n') {
                    self.i += 1;
                }
                let line: String = self.chars[start..self.i].iter().collect();
                if self.peek() == Some('\n') {
                    self.i += 1;
                }
                let cmp = if h.strip_tabs {
                    line.trim_start_matches('\t')
                } else {
                    line.as_str()
                };
                if cmp == h.delim {
                    found = true;
                    break;
                }
                body.push_str(&line);
                body.push('\n');
            }
            let _ = found; // A body without its delimiter runs to the end of the text.
            if !h.quoted {
                // An unquoted heredoc expands its body: each expansion is read as it is in a
                // double-quoted word.
                self.scan_text(&body, depth)?;
            }
            self.cmds[h.cmd].heredocs.push(body);
        }
        Ok(())
    }

    /// Read the expansions in text that the shell would expand, as a double-quoted word has them:
    /// `$(…)` and backticks are parsed as commands, and `${…}` and `$((…))` are read by the rules
    /// of a word.
    fn scan_expansions(&mut self, depth: usize) -> Result<(), String> {
        while let Some(c) = self.peek() {
            match c {
                '\\' => self.i += 2,
                '$' => {
                    let mut scratch: Vec<Seg> = Vec::new();
                    self.dollar(&mut scratch, true, depth)?;
                }
                '`' => {
                    self.i += 1;
                    self.list(Some('`'), depth)?;
                }
                _ => self.i += 1,
            }
        }
        Ok(())
    }

    /// `<`, `>`, `<<`, `<<<`, `>>`, `<&`, `>&`, `<>`, `>|` and process substitution.
    fn redirect(
        &mut self,
        depth: usize,
        heredocs: &mut Vec<Heredoc>,
        cur: &mut Option<usize>,
        pipe_from: &mut Option<usize>,
    ) -> Result<(), String> {
        let c = self.peek().unwrap_or('<');
        if self.peek_at(1) == Some('(') {
            // `<(cmd)` and `>(cmd)`: a command runs.
            self.i += 2;
            self.list(Some(')'), depth + 1)?;
            return Ok(());
        }
        if self.starts_with("<<<") {
            self.i += 3;
            while self.peek().is_some_and(|c| c == ' ' || c == '\t') {
                self.i += 1;
            }
            let w = self.word(depth)?;
            if w.raw.is_empty() {
                return Err("a here-string has no word".to_string());
            }
            let idx = *cur.get_or_insert_with(|| {
                let v = self.cmds.len();
                self.cmds.push(Cmd {
                    piped_from: pipe_from.take(),
                    ..Cmd::default()
                });
                v
            });
            self.cmds[idx].herestring = Some(w);
            return Ok(());
        }
        if self.starts_with("<<") && !self.starts_with("<<<") {
            self.i += 2;
            let strip_tabs = self.peek() == Some('-');
            if strip_tabs {
                self.i += 1;
            }
            while self.peek().is_some_and(|c| c == ' ' || c == '\t') {
                self.i += 1;
            }
            let w = self.word(depth)?;
            if w.raw.is_empty() {
                return Err("a heredoc has no delimiter word".to_string());
            }
            let idx = *cur.get_or_insert_with(|| {
                let v = self.cmds.len();
                self.cmds.push(Cmd {
                    piped_from: pipe_from.take(),
                    ..Cmd::default()
                });
                v
            });
            heredocs.push(Heredoc {
                delim: w.text.clone(),
                quoted: w.any_quote,
                strip_tabs,
                cmd: idx,
            });
            return Ok(());
        }
        self.i += 1;
        let next = self.peek();
        let two_chars = match c {
            '>' => matches!(next, Some('>' | '|' | '&')),
            _ => matches!(next, Some('&' | '>')),
        };
        if two_chars {
            self.i += 1;
        }
        self.skip_target(depth)?;
        if cur.is_none() {
            *cur = Some(self.new_cmd(pipe_from.take()));
        }
        Ok(())
    }

    /// Read the word after a redirection operator. Its text is not a command.
    fn skip_target(&mut self, depth: usize) -> Result<(), String> {
        while self.peek().is_some_and(|c| c == ' ' || c == '\t') {
            self.i += 1;
        }
        if matches!(self.peek(), Some('<' | '>')) && self.peek_at(1) == Some('(') {
            // `cmd > >(tee x)`: the target is a process substitution, which the caller reads.
            return Ok(());
        }
        let w = self.word(depth)?;
        if w.raw.is_empty() {
            return Err("a redirection has no target word".to_string());
        }
        Ok(())
    }

    /// Read `… ))` of an arithmetic expression, after its two opening parentheses. The text comes
    /// back with a flag: false when a single `)` closed it, which a shell may read as a command
    /// substitution that holds a subshell.
    fn arith(&mut self) -> Result<(String, bool), String> {
        let mut depth = 0usize;
        let mut text = String::new();
        while let Some(c) = self.peek() {
            match c {
                '(' => depth += 1,
                ')' => {
                    if depth == 0 {
                        if self.peek_at(1) == Some(')') {
                            self.i += 2;
                            return Ok((text, true));
                        }
                        self.i += 1;
                        return Ok((text, false));
                    }
                    depth -= 1;
                }
                _ => {}
            }
            text.push(c);
            self.i += 1;
        }
        Err("an arithmetic expression is not closed".to_string())
    }

    /// Read one word.
    fn word(&mut self, depth: usize) -> Result<Word, String> {
        let start = self.i;
        let mut segs: Vec<Seg> = Vec::new();
        let mut any_quote = false;
        let mut glob = false;
        let mut lit = String::new();
        macro_rules! flush {
            () => {
                if !lit.is_empty() {
                    segs.push(Seg::Lit(std::mem::take(&mut lit)));
                }
            };
        }
        while let Some(c) = self.peek() {
            match c {
                ' ' | '\t' | '\n' | '\r' | ';' | '&' | '|' | '(' | ')' | '<' | '>' => {
                    // `<(`, `>(` after text are part of the line only at word start; here they end it.
                    break;
                }
                '`' if self.closers.last() == Some(&Some('`')) => break,
                '`' => {
                    self.i += 1;
                    flush!();
                    self.list(Some('`'), depth + 1)?;
                    segs.push(Seg::Subst);
                }
                '\\' => {
                    if self.peek_at(1) == Some('\n') {
                        self.i += 2;
                    } else if let Some(n) = self.peek_at(1) {
                        lit.push(n);
                        any_quote = true;
                        self.i += 2;
                    } else {
                        self.i += 1;
                    }
                }
                '\'' => {
                    self.i += 1;
                    any_quote = true;
                    flush!();
                    let mut s = String::new();
                    loop {
                        match self.peek() {
                            None => return Err("a single quote is not closed".to_string()),
                            Some('\'') => {
                                self.i += 1;
                                break;
                            }
                            Some(ch) => {
                                s.push(ch);
                                self.i += 1;
                            }
                        }
                    }
                    segs.push(Seg::Quoted(s));
                }
                '"' => {
                    self.i += 1;
                    any_quote = true;
                    flush!();
                    self.double_quoted(&mut segs, depth)?;
                }
                '$' => {
                    flush!();
                    if self.dollar_word(&mut segs, depth)? {
                        any_quote = true;
                    }
                }
                '*' | '?' | '[' => {
                    glob = true;
                    lit.push(c);
                    self.i += 1;
                }
                _ => {
                    lit.push(c);
                    self.i += 1;
                }
            }
        }
        flush!();
        let raw: String = self.chars[start..self.i].iter().collect();
        let mut text = String::new();
        let mut dynamic = false;
        for s in &segs {
            match s {
                Seg::Lit(t) | Seg::Quoted(t) => text.push_str(t),
                Seg::Var { name, .. } => {
                    dynamic = true;
                    text.push('$');
                    text.push_str(name);
                }
                Seg::Subst => {
                    dynamic = true;
                    text.push_str("$(…)");
                }
            }
        }
        let quoted_var = match segs.as_slice() {
            [Seg::Var { name, quoted: true }] => Some(name.clone()),
            _ => None,
        };
        // Brace expansion. Quoted and expanded parts stand as a placeholder, so their braces and
        // commas do nothing. A word that is only plain text is expanded here. Any other word that
        // would expand cannot be read, so it counts as dynamic.
        let mut alts = Vec::new();
        let mut brace_quoted = false;
        let pattern: String = segs
            .iter()
            .map(|s| match s {
                Seg::Lit(t) => t.clone(),
                _ => "\u{1}".to_string(),
            })
            .collect();
        if !assignment_prefix(&raw) {
            match expand_braces(&pattern) {
                Expansion::None => {}
                Expansion::Words(v) if !pattern.contains('\u{1}') => alts = v,
                _ => {
                    dynamic = true;
                    brace_quoted = segs.iter().any(|s| matches!(s, Seg::Quoted(_)));
                }
            }
        }
        Ok(Word {
            text,
            raw,
            dynamic,
            quoted_var,
            glob,
            any_quote,
            alts,
            brace_quoted,
        })
    }

    /// The inside of double quotes, after the opening quote.
    fn double_quoted(&mut self, segs: &mut Vec<Seg>, depth: usize) -> Result<(), String> {
        let mut lit = String::new();
        loop {
            match self.peek() {
                None => return Err("a double quote is not closed".to_string()),
                Some('"') => {
                    self.i += 1;
                    break;
                }
                Some('\\') => match self.peek_at(1) {
                    Some(n) if matches!(n, '$' | '`' | '"' | '\\') => {
                        lit.push(n);
                        self.i += 2;
                    }
                    Some('\n') => self.i += 2,
                    Some(_) => {
                        lit.push('\\');
                        self.i += 1;
                    }
                    None => self.i += 1,
                },
                Some('$') => {
                    if !lit.is_empty() {
                        segs.push(Seg::Quoted(std::mem::take(&mut lit)));
                    }
                    self.dollar(segs, true, depth)?;
                }
                Some('`') => {
                    if !lit.is_empty() {
                        segs.push(Seg::Quoted(std::mem::take(&mut lit)));
                    }
                    self.i += 1;
                    self.list(Some('`'), depth + 1)?;
                    segs.push(Seg::Subst);
                }
                Some(ch) => {
                    lit.push(ch);
                    self.i += 1;
                }
            }
        }
        if !lit.is_empty() {
            segs.push(Seg::Quoted(lit));
        }
        Ok(())
    }

    /// An expansion that starts with `$`. `self.i` is on the `$`.
    fn dollar(&mut self, segs: &mut Vec<Seg>, quoted: bool, depth: usize) -> Result<(), String> {
        match self.peek_at(1) {
            Some('(') if self.peek_at(2) == Some('(') => {
                // `$(( … ))`. Only names, numbers, spaces and the listed operators are read. Any
                // other text could hold an array index with a substitution in it.
                let start = self.i;
                self.i += 3;
                let (inner, closed) = self.arith()?;
                if !closed || !crate::grammar::arithmetic_ok(&inner) {
                    let raw = self.since(start);
                    self.note(format!(
                        "`{raw}` (an arithmetic expansion that holds more than names, numbers and operators)"
                    ));
                    self.scan_text(&inner, depth)?;
                }
                segs.push(Seg::Subst);
            }
            Some('(') => {
                self.i += 2;
                self.list(Some(')'), depth + 1)?;
                segs.push(Seg::Subst);
            }
            Some('{') => {
                self.i += 2;
                self.param(segs, quoted, depth)?;
            }
            Some('[') => {
                // `$[ … ]`, the old arithmetic expansion.
                let start = self.i;
                self.i += 2;
                let mut inner = String::new();
                while let Some(c) = self.peek() {
                    self.i += 1;
                    if c == ']' {
                        break;
                    }
                    inner.push(c);
                }
                let raw = self.since(start);
                self.note(format!("`{raw}` (the old arithmetic expansion)"));
                self.scan_text(&inner, depth)?;
                segs.push(Seg::Subst);
            }
            Some(c) if is_name_start(c) => {
                self.i += 1;
                let mut name = String::new();
                while self.peek().is_some_and(is_name_char) {
                    name.push(self.peek().unwrap_or('_'));
                    self.i += 1;
                }
                segs.push(Seg::Var { name, quoted });
            }
            Some(c)
                if c.is_ascii_digit() || matches!(c, '?' | '$' | '!' | '#' | '@' | '*' | '-') =>
            {
                self.i += 2;
                segs.push(Seg::Var {
                    name: c.to_string(),
                    quoted,
                });
            }
            _ => {
                // A `$` that expands nothing is text.
                self.i += 1;
                segs.push(if quoted {
                    Seg::Quoted("$".to_string())
                } else {
                    Seg::Lit("$".to_string())
                });
            }
        }
        Ok(())
    }

    /// A `$` in an unquoted word. `$'…'` and `$"…"` are outside the grammar and are noted; any
    /// other `$` is `dollar`. Gives true when the form was a quoted string.
    fn dollar_word(&mut self, segs: &mut Vec<Seg>, depth: usize) -> Result<bool, String> {
        let start = self.i;
        match self.peek_at(1) {
            Some('\'') => {
                self.i += 2;
                let s = self.ansi_c()?;
                let raw = self.since(start);
                self.note(format!("`{raw}` (ANSI-C quoting)"));
                segs.push(Seg::Quoted(s));
                Ok(true)
            }
            Some('"') => {
                self.i += 2;
                self.double_quoted(segs, depth)?;
                let raw = self.since(start);
                self.note(format!("`{raw}` (locale quoting)"));
                Ok(true)
            }
            _ => {
                self.dollar(segs, false, depth)?;
                Ok(false)
            }
        }
    }

    /// A `${` after its two characters. The grammar lists `${NAME}`, `${#NAME}` and the four
    /// default forms `${NAME:-word}`, `${NAME-word}`, `${NAME:+word}` and `${NAME+word}`, where
    /// `NAME` is a name, a number or one of `? $ ! # @ * -`. Any other text is outside the
    /// grammar: it is noted with its source, and its inside is still searched for commands.
    fn param(&mut self, segs: &mut Vec<Seg>, quoted: bool, depth: usize) -> Result<(), String> {
        if depth > 24 {
            return Err("substitutions nest too deep".to_string());
        }
        let start = self.i - 2;
        let body = self.i;
        let length = self.peek() == Some('#') && self.peek_at(1).is_some_and(|c| c != '}');
        if length {
            self.i += 1;
        }
        let mut name = String::new();
        match self.peek() {
            Some(c) if is_name_start(c) => {
                while let Some(ch) = self.peek().filter(|ch| is_name_char(*ch)) {
                    name.push(ch);
                    self.i += 1;
                }
            }
            Some(c) if c.is_ascii_digit() => {
                while let Some(ch) = self.peek().filter(char::is_ascii_digit) {
                    name.push(ch);
                    self.i += 1;
                }
            }
            Some(c) if matches!(c, '?' | '$' | '!' | '#' | '@' | '*' | '-') => {
                name.push(c);
                self.i += 1;
            }
            _ => {}
        }
        if !name.is_empty() {
            match (self.peek(), self.peek_at(1)) {
                (Some('}'), _) => {
                    self.i += 1;
                    segs.push(if length {
                        Seg::Subst
                    } else {
                        Seg::Var { name, quoted }
                    });
                    return Ok(());
                }
                (Some(':'), Some('-' | '+')) if !length => {
                    self.i += 2;
                    self.param_word(quoted, depth + 1)?;
                    segs.push(Seg::Subst);
                    return Ok(());
                }
                (Some('-' | '+'), _) if !length => {
                    self.i += 1;
                    self.param_word(quoted, depth + 1)?;
                    segs.push(Seg::Subst);
                    return Ok(());
                }
                _ => {}
            }
        }
        // Not a listed form. Read to the closing brace, and find the commands inside.
        self.i = body;
        let mut nest = 1usize;
        let mut inner = String::new();
        loop {
            match self.peek() {
                None => return Err("a ${…} is not closed".to_string()),
                Some('}') => {
                    self.i += 1;
                    nest -= 1;
                    if nest == 0 {
                        break;
                    }
                    inner.push('}');
                }
                Some(ch) => {
                    if ch == '{' {
                        nest += 1;
                    }
                    inner.push(ch);
                    self.i += 1;
                }
            }
        }
        let raw = self.since(start);
        self.note(format!(
            "`{raw}` (a parameter expansion that the grammar does not list)"
        ));
        self.scan_text(&inner, depth)?;
        segs.push(Seg::Subst);
        Ok(())
    }

    /// The word of `${NAME:-word}`, up to the brace that closes it. It is read as a word is: a
    /// quote, a variable or a substitution in it is found, and a `}` inside one does not close it.
    /// `quoted` is true when the expansion stands in double quotes, where a single quote is text.
    fn param_word(&mut self, quoted: bool, depth: usize) -> Result<(), String> {
        let mut scratch: Vec<Seg> = Vec::new();
        loop {
            match self.peek() {
                None => return Err("a ${…} is not closed".to_string()),
                Some('}') => {
                    self.i += 1;
                    return Ok(());
                }
                Some('\\') => self.i += 2,
                Some('\'') if !quoted => {
                    self.i += 1;
                    loop {
                        match self.peek() {
                            None => return Err("a single quote is not closed".to_string()),
                            Some('\'') => {
                                self.i += 1;
                                break;
                            }
                            Some(_) => self.i += 1,
                        }
                    }
                }
                Some('"') => {
                    self.i += 1;
                    self.double_quoted(&mut scratch, depth)?;
                }
                Some('`') if self.closers.last() == Some(&Some('`')) => {
                    return Err("a ${…} is not closed".to_string());
                }
                Some('`') => {
                    self.i += 1;
                    self.list(Some('`'), depth + 1)?;
                }
                Some('$') if quoted => self.dollar(&mut scratch, true, depth)?,
                Some('$') => {
                    self.dollar_word(&mut scratch, depth)?;
                }
                Some(_) => self.i += 1,
            }
        }
    }

    /// The inside of `$'…'`, after the opening quote.
    fn ansi_c(&mut self) -> Result<String, String> {
        let mut s = String::new();
        loop {
            match self.peek() {
                None => return Err("a $'…' string is not closed".to_string()),
                Some('\'') => {
                    self.i += 1;
                    return Ok(s);
                }
                Some('\\') => {
                    let n = self.peek_at(1);
                    self.i += 2;
                    match n {
                        Some('n') => s.push('\n'),
                        Some('t') => s.push('\t'),
                        Some('r') => s.push('\r'),
                        Some('a') => s.push('\u{7}'),
                        Some('e' | 'E') => s.push('\u{1b}'),
                        Some('0') => s.push('\0'),
                        Some('x') => {
                            let mut v = 0u32;
                            let mut k = 0;
                            while k < 2 && self.peek().is_some_and(|c| c.is_ascii_hexdigit()) {
                                v = v * 16 + self.peek().and_then(|c| c.to_digit(16)).unwrap_or(0);
                                self.i += 1;
                                k += 1;
                            }
                            s.push(char::from_u32(v).unwrap_or('?'));
                        }
                        Some('u' | 'U') => {
                            let mut v = 0u32;
                            let mut k = 0;
                            while k < 8 && self.peek().is_some_and(|c| c.is_ascii_hexdigit()) {
                                v = v * 16 + self.peek().and_then(|c| c.to_digit(16)).unwrap_or(0);
                                self.i += 1;
                                k += 1;
                            }
                            s.push(char::from_u32(v).unwrap_or('?'));
                        }
                        Some(c) => s.push(c),
                        None => return Err("a $'…' string is not closed".to_string()),
                    }
                }
                Some(ch) => {
                    s.push(ch);
                    self.i += 1;
                }
            }
        }
    }
}

/// What brace expansion makes of a word.
#[derive(Debug, PartialEq, Eq)]
enum Expansion {
    /// The word has nothing to expand.
    None,
    /// The words it expands to.
    Words(Vec<String>),
    /// It expands to more words than are read.
    TooMany,
}

/// The most words one brace expansion is read as.
const MAX_BRACE_WORDS: usize = 256;

/// Expand `{a,b}` and `{1..3}` as the shell does, before anything else. The text holds no
/// quotes: a quoted part is a placeholder character.
fn expand_braces(src: &str) -> Expansion {
    let chars: Vec<char> = src.chars().collect();
    let mut from = 0;
    while let Some(open) = chars[from..]
        .iter()
        .position(|c| *c == '{')
        .map(|k| k + from)
    {
        from = open + 1;
        // The matching close brace, and the commas at the top level between them.
        let mut depth = 0usize;
        let mut commas: Vec<usize> = Vec::new();
        let mut close = None;
        for (k, c) in chars.iter().enumerate().skip(open) {
            match c {
                '{' => depth += 1,
                '}' => {
                    depth -= 1;
                    if depth == 0 {
                        close = Some(k);
                        break;
                    }
                }
                ',' if depth == 1 => commas.push(k),
                _ => {}
            }
        }
        let Some(close) = close else { continue };
        let inside: String = chars[open + 1..close].iter().collect();
        let alts: Vec<String> = if commas.is_empty() {
            match sequence(&inside) {
                Some(Some(v)) => v,
                Some(None) => return Expansion::TooMany,
                None => continue,
            }
        } else {
            let mut parts = Vec::new();
            let mut start = open + 1;
            for k in commas.iter().copied().chain(std::iter::once(close)) {
                parts.push(chars[start..k].iter().collect::<String>());
                start = k + 1;
            }
            parts
        };
        let prefix: String = chars[..open].iter().collect();
        let suffix: String = chars[close + 1..].iter().collect();
        let mut out: Vec<String> = Vec::new();
        for alt in alts {
            let joined = format!("{alt}{suffix}");
            match expand_braces(&joined) {
                Expansion::None => out.push(format!("{prefix}{joined}")),
                Expansion::Words(v) => out.extend(v.into_iter().map(|w| format!("{prefix}{w}"))),
                Expansion::TooMany => return Expansion::TooMany,
            }
            if out.len() > MAX_BRACE_WORDS {
                return Expansion::TooMany;
            }
        }
        return Expansion::Words(out);
    }
    Expansion::None
}

/// Is this text a whole number: an optional sign and digits?
fn looks_whole(t: &str) -> bool {
    let digits = t.strip_prefix(['-', '+']).unwrap_or(t);
    !digits.is_empty() && digits.chars().all(|c| c.is_ascii_digit())
}

/// The words of `a..b` or `a..b..step`: whole numbers or single letters. `None` when the text is
/// not a sequence, `Some(None)` when it is one that is too long or too large to read. The
/// arithmetic is in `i128`, so no `i64` input can overflow it, and the word count is checked
/// before any word is made.
fn sequence(inside: &str) -> Option<Option<Vec<String>>> {
    let parts: Vec<&str> = inside.split("..").collect();
    if !(2..=3).contains(&parts.len()) {
        return None;
    }
    let whole = |t: &str| t.parse::<i64>().ok().map(i128::from);
    let step: i128 = match parts.get(2) {
        None => 1,
        Some(t) if looks_whole(t) => match whole(t) {
            // The shell takes a step of 0 as 1.
            Some(0) => 1,
            Some(n) => n.abs(),
            None => return Some(None),
        },
        Some(_) => return None,
    };
    let width = |t: &str| {
        let digits = t.trim_start_matches('-');
        if digits.len() > 1 && digits.starts_with('0') {
            digits.len()
        } else {
            0
        }
    };
    let (a, b) = (parts[0], parts[1]);
    let render = |n: i128, w: usize| {
        if w > 0 {
            format!("{n:0w$}", w = w + usize::from(n < 0))
        } else {
            n.to_string()
        }
    };
    // The words from `x` to `y` in steps of `step`: the count first, then the words.
    let walk = |x: i128, y: i128, render: &dyn Fn(i128) -> String| {
        let count = (x - y).abs() / step + 1;
        if count > MAX_BRACE_WORDS as i128 {
            return None;
        }
        let sign = if x <= y { 1 } else { -1 };
        Some(
            (0..count)
                .map(|k| render(x + sign * k * step))
                .collect::<Vec<String>>(),
        )
    };
    if looks_whole(a) && looks_whole(b) {
        let (Some(x), Some(y)) = (whole(a), whole(b)) else {
            return Some(None);
        };
        let w = width(a).max(width(b));
        return Some(walk(x, y, &|n| render(n, w)));
    }
    let single = |t: &str| {
        let mut it = t.chars();
        match (it.next(), it.next()) {
            (Some(c), None) if c.is_ascii_alphabetic() => Some(i128::from(c as u32)),
            _ => None,
        }
    };
    let (x, y) = (single(a)?, single(b)?);
    Some(walk(x, y, &|n| {
        u32::try_from(n)
            .ok()
            .and_then(char::from_u32)
            .map_or(String::new(), |c| c.to_string())
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn names(src: &str) -> Vec<String> {
        parse(src)
            .unwrap()
            .cmds
            .iter()
            .filter(|c| !c.words.is_empty())
            .map(|c| {
                c.words
                    .iter()
                    .map(|w| w.text.clone())
                    .collect::<Vec<_>>()
                    .join(" ")
            })
            .collect()
    }

    #[test]
    fn splits_on_operators_and_reads_quotes() {
        assert_eq!(
            names("a x && b 'y z' ; c \"p q\" | d || e"),
            vec!["a x", "b y z", "c p q", "d", "e"]
        );
    }

    #[test]
    fn nested_commands_are_found() {
        let n = names("echo \"$(gh pr merge 1)\" `date` <(ls)");
        assert!(n.contains(&"gh pr merge 1".to_string()), "{n:?}");
        assert!(
            n.contains(&"date".to_string()) && n.contains(&"ls".to_string()),
            "{n:?}"
        );
    }

    #[test]
    fn reserved_words_and_braces_are_skipped() {
        assert_eq!(names("if gh x; then y; fi"), vec!["gh x", "y"]);
        assert_eq!(names("{ a; b; }"), vec!["a", "b"]);
        assert_eq!(names("for f in a b; do rm $f; done"), vec!["rm $f"]);
    }

    #[test]
    fn assignments_and_env_prefixes_are_not_words() {
        let p = parse("X=1 Y=\"$(date)\" cmd arg").unwrap();
        let c = p.cmds.iter().find(|c| !c.words.is_empty()).unwrap();
        assert_eq!(c.assigns.len(), 2);
        assert_eq!(c.words[0].text, "cmd");
    }

    #[test]
    fn heredoc_bodies_attach_and_expand_only_when_unquoted() {
        let p = parse("cat <<'EOF'\ngh pr merge 1\nEOF\necho done").unwrap();
        assert_eq!(p.cmds[0].heredocs, vec!["gh pr merge 1\n".to_string()]);
        assert!(
            !names("cat <<'EOF'\n$(gh pr merge 1)\nEOF")
                .iter()
                .any(|n| n.starts_with("gh"))
        );
        assert!(
            names("cat <<EOF\n$(gh pr merge 1)\nEOF")
                .iter()
                .any(|n| n.starts_with("gh"))
        );
    }

    #[test]
    fn unclosed_quotes_are_errors() {
        assert!(parse("echo 'x").is_err());
        assert!(parse("echo \"x").is_err());
        assert!(parse("echo $(x").is_err());
    }

    #[test]
    fn a_quoted_variable_is_marked() {
        let p = parse("git push origin \"$b\" $c \"+$b\"").unwrap();
        let w = &p.cmds[0].words;
        assert_eq!(w[3].quoted_var.as_deref(), Some("b"));
        assert!(w[4].dynamic && w[4].quoted_var.is_none());
        assert!(w[5].dynamic && w[5].quoted_var.is_none());
    }

    #[test]
    fn ansi_c_quotes_make_real_line_breaks() {
        let p = parse("git commit -m $'a\\n\\nCo-Authored-By: x'").unwrap();
        assert!(p.cmds[0].words[3].text.contains("\n\nCo-Authored-By"));
    }

    #[test]
    fn case_patterns_are_not_commands() {
        let n = names("case \"$x\" in a|b) rm -rf y ;; *) z ;; esac");
        assert!(
            n.contains(&"rm -rf y".to_string()) && n.contains(&"z".to_string()),
            "{n:?}"
        );
    }
}

#[cfg(test)]
mod brace_tests {
    use super::*;

    fn expand(src: &str) -> Vec<String> {
        match expand_braces(src) {
            Expansion::Words(v) => v,
            other => panic!("{src}: {other:?}"),
        }
    }

    #[test]
    fn lists_sequences_and_nests() {
        assert_eq!(expand("a{b,c}d"), ["abd", "acd"]);
        assert_eq!(expand("f{,.bak}"), ["f", "f.bak"]);
        assert_eq!(expand("{1..3}"), ["1", "2", "3"]);
        assert_eq!(expand("{3..1}"), ["3", "2", "1"]);
        assert_eq!(expand("{a..c}"), ["a", "b", "c"]);
        assert_eq!(expand("{01..03}"), ["01", "02", "03"]);
        assert_eq!(expand("{1..7..3}"), ["1", "4", "7"]);
        assert_eq!(expand("{a,{b,c}}"), ["a", "b", "c"]);
        assert_eq!(expand("{a,b}{c,d}"), ["ac", "ad", "bc", "bd"]);
        assert_eq!(expand("x{a}{b,c}"), ["x{a}b", "x{a}c"]);
    }

    #[test]
    fn leaves_what_the_shell_leaves() {
        for src in [
            "{}", "{a}", "{", "}", "a{b", "{a..}", "{..a}", "{1..a}", "plain",
        ] {
            assert_eq!(expand_braces(src), Expansion::None, "{src}");
        }
        assert_eq!(expand_braces("{1..100000}"), Expansion::TooMany);
    }

    #[test]
    fn a_sequence_never_overflows_and_never_runs_long() {
        // Each of these made the old code loop for ever or panic on an `i64` overflow.
        let max = i64::MAX;
        assert_eq!(
            expand(&format!("{{0..{max}..{max}}}")),
            ["0", &max.to_string()]
        );
        assert_eq!(expand(&format!("{{1..3..{max}}}")), ["1"]);
        assert_eq!(expand(&format!("{{a..z..{max}}}")), ["a"]);
        assert_eq!(expand(&format!("{{3..1..{}}}", i64::MIN)), ["3"]);
        for src in [
            format!("{{{}..{max}}}", i64::MIN),
            format!("{{{max}..{}}}", i64::MIN),
            "{1..3..99999999999999999999}".to_string(),
            "{99999999999999999999..1}".to_string(),
        ] {
            assert_eq!(expand_braces(&src), Expansion::TooMany, "{src}");
        }
        // A step that is not a whole number, and a step of nothing, are text.
        for src in ["{1..3..x}", "{a..z..}"] {
            assert_eq!(expand_braces(src), Expansion::None, "{src}");
        }
        // A step of 0 acts as 1, as it does in the shell.
        assert_eq!(expand("{1..3..0}"), ["1", "2", "3"]);
        assert_eq!(expand("{a..c..0}"), ["a", "b", "c"]);
        assert_eq!(expand("{a..e..2}"), ["a", "c", "e"]);
        assert_eq!(expand("{5..1..-2}"), ["5", "3", "1"]);
    }

    #[test]
    fn a_word_becomes_the_commands_words() {
        let p = parse("gh pr {merge,x} 1").unwrap();
        let words: Vec<&str> = p.cmds[0].words.iter().map(|w| w.text.as_str()).collect();
        assert_eq!(words, ["gh", "pr", "merge", "x", "1"]);
    }

    #[test]
    fn function_and_coproc_names_are_not_commands() {
        for (src, want) in [
            ("function f { gh pr merge 1; }", "gh pr merge 1"),
            ("function f() { gh pr merge 1; }", "gh pr merge 1"),
            ("coproc { gh pr merge 1; }", "gh pr merge 1"),
        ] {
            let p = parse(src).unwrap();
            let all: Vec<String> = p
                .cmds
                .iter()
                .filter(|c| !c.words.is_empty())
                .map(|c| {
                    c.words
                        .iter()
                        .map(|w| w.text.as_str())
                        .collect::<Vec<_>>()
                        .join(" ")
                })
                .collect();
            assert_eq!(all, [want], "{src}");
        }
    }

    #[test]
    fn a_word_after_coproc_that_is_not_a_body_start_is_not_parsed() {
        for src in [
            "coproc n { gh pr merge 1; }",
            "coproc gh pr merge 1",
            "coproc rm -rf x",
            "coproc N while read l; do :; done",
            "coproc \"n\" { :; }",
            "coproc $n { :; }",
        ] {
            assert!(parse(src).is_err(), "{src}");
        }
        for src in [
            "coproc { :; }",
            "coproc ( : )",
            "coproc while :; do :; done",
        ] {
            assert!(parse(src).is_ok(), "{src}");
        }
    }
}

#[cfg(test)]
mod arm_and_array_tests {
    use super::*;

    fn commands(src: &str) -> Vec<String> {
        parse(src)
            .unwrap()
            .cmds
            .iter()
            .filter(|c| !c.header && !c.words.is_empty())
            .map(|c| {
                c.words
                    .iter()
                    .map(|w| w.text.as_str())
                    .collect::<Vec<_>>()
                    .join(" ")
            })
            .collect()
    }

    #[test]
    fn no_pattern_of_a_case_arm_is_a_command() {
        for src in [
            "case \"$f\" in\n *.c|*.h) echo src ;;\nesac",
            "case $f in a*|b*|c*) echo x ;; esac",
            "case $f in (a*|b*) echo x ;; esac",
            "case $f in\n a*)\n echo x\n ;;\n *.c | *.h | *.o) echo x ;;\n *) echo y ;;\nesac",
            "case $f in @(a|b)*) echo x ;; esac",
            "case $f in a) echo x ;; b) echo x ;;& *) echo y ;& esac",
        ] {
            let c = commands(src);
            assert!(
                c.iter().all(|t| t.starts_with("echo ")),
                "{src:?} gave {c:?}"
            );
            assert!(!c.is_empty(), "{src:?}");
        }
    }

    #[test]
    fn a_case_arm_body_is_still_read() {
        let c = commands("case $f in\n *.c|*.h) gh pr merge 1 | cat ;;\n x) ls ;;\nesac");
        assert_eq!(c, ["gh pr merge 1", "cat", "ls"]);
        // After the `esac`, words are commands again.
        let c = commands("case $f in a|b) echo x ;; esac; *.sh");
        assert_eq!(c, ["echo x", "*.sh"]);
        // A nested `case` returns to the arm body of the outer one.
        let c = commands("case $a in x) case $b in y|z) echo n ;; esac; ls ;; *) echo o ;; esac");
        assert_eq!(c, ["echo n", "ls", "echo o"]);
    }

    #[test]
    fn an_array_literal_holds_values_and_its_substitutions_are_commands() {
        assert_eq!(commands("files=(*.rs *.toml); echo hi"), ["echo hi"]);
        assert_eq!(
            commands("declare -a f=(a\n b # c\n *.d)"),
            ["declare -a f="]
        );
        assert_eq!(commands("x+=(a b) ls"), ["ls"]);
        assert!(commands("f=($(gh pr merge 1) *.c)").contains(&"gh pr merge 1".to_string()));
        assert!(parse("f=(a b").is_err());
        assert!(parse("f=(a; b)").is_err());
    }

    #[test]
    fn a_quoted_brace_word_is_marked() {
        let p = parse("git commit \"--no-ver\"{ify,} -m x").unwrap();
        assert!(p.cmds[0].words.iter().any(|w| w.brace_quoted));
        // A variable with a brace is dynamic, and is not marked.
        let p = parse("mkdir \"$d\"/{a,b}").unwrap();
        assert!(p.cmds[0].words.iter().all(|w| !w.brace_quoted));
        assert!(p.cmds[0].words.iter().any(|w| w.dynamic));
        let p = parse("echo '{a,b}' {a,b}").unwrap();
        assert!(p.cmds[0].words.iter().all(|w| !w.brace_quoted));
    }
}

#[cfg(test)]
mod grammar_tests {
    use super::*;

    fn outside(src: &str) -> Vec<String> {
        parse(src).unwrap().outside
    }

    fn commands(src: &str) -> Vec<String> {
        parse(src)
            .unwrap()
            .cmds
            .iter()
            .filter(|c| !c.header && !c.words.is_empty())
            .map(|c| {
                c.words
                    .iter()
                    .map(|w| w.text.as_str())
                    .collect::<Vec<_>>()
                    .join(" ")
            })
            .collect()
    }

    #[test]
    fn the_listed_parameter_forms_are_inside_the_grammar() {
        for src in [
            "echo $x ${x} ${#x} ${x:-a} ${x-a} ${x:+a} ${x+a} $? ${?} ${10} ${#}",
            "echo \"${x:-a b}\" ${x:-'a b'} \"${x:-\"a b\"}\" ${x:-${y:-z}} ${x:-}",
            "echo $((n+1)) $(( (1 + 2) * 3 )) $((a<b&&c!=d))",
        ] {
            assert_eq!(outside(src), Vec::<String>::new(), "{src}");
        }
    }

    #[test]
    fn every_other_parameter_form_is_outside_with_its_source_text() {
        for (src, form) in [
            ("echo ${x:=y}", "${x:=y}"),
            ("echo ${x=y}", "${x=y}"),
            ("echo ${x:?m}", "${x:?m}"),
            ("echo ${x/a/b}", "${x/a/b}"),
            ("echo ${!x}", "${!x}"),
            ("echo ${x%y} ${x#y} ${x^^} ${x,,}", "${x%y}"),
            ("echo ${x:1:2}", "${x:1:2}"),
            ("echo ${x[1]}", "${x[1]}"),
            ("echo ${x[@]}", "${x[@]}"),
            ("echo ${x@P}", "${x@P}"),
            ("echo ${#x:-a}", "${#x:-a}"),
            ("echo ${@:2}", "${@:2}"),
            ("echo \"${x:=y}\"", "${x:=y}"),
            ("echo $'a'", "$'a'"),
            ("echo $\"a\"", "$\"a\""),
            ("echo ${x:-$'a'}", "$'a'"),
            ("echo $[1+2]", "$[1+2]"),
            ("echo $((a[1]))", "$((a[1]))"),
            ("echo $(( $x + 1 ))", "$(( $x + 1 ))"),
            ("echo $((a,b))", "$((a,b))"),
            ("((x++))", "((x++))"),
            ("for ((i=0; i<3; i++)); do :; done", "for"),
            ("select x in a; do :; done", "select"),
        ] {
            let o = outside(src);
            assert!(
                o.iter().any(|f| f.contains(form)),
                "{src:?} gave {o:?}, want {form:?}"
            );
        }
    }

    #[test]
    fn a_form_outside_the_grammar_still_shows_its_commands() {
        let c = commands("echo ${x:=$(gh pr merge 1)}");
        assert!(c.contains(&"gh pr merge 1".to_string()), "{c:?}");
        let c = commands("echo $(( $(gh pr merge 2) + 1 ))");
        assert!(c.contains(&"gh pr merge 2".to_string()), "{c:?}");
        let c = commands("((a[$(gh pr merge 3)]++))");
        assert!(c.contains(&"gh pr merge 3".to_string()), "{c:?}");
        let c = commands("echo $[$(gh pr merge 4)]");
        assert!(c.contains(&"gh pr merge 4".to_string()), "{c:?}");
    }

    #[test]
    fn the_word_of_a_default_form_is_read_as_a_word() {
        let c = commands("echo ${x:-$(gh pr merge 1)}");
        assert!(c.contains(&"gh pr merge 1".to_string()), "{c:?}");
        // A brace inside a substitution, a quote or a nested default does not close the word.
        let c = commands("echo \"${x:-$(echo }; gh pr merge 2)}\"");
        assert!(c.contains(&"gh pr merge 2".to_string()), "{c:?}");
        let c = commands("echo ${x:-\"}\"}; gh pr merge 3");
        assert!(c.contains(&"gh pr merge 3".to_string()), "{c:?}");
        let c = commands("echo ${x:-${y:-}}; gh pr merge 4");
        assert!(c.contains(&"gh pr merge 4".to_string()), "{c:?}");
        assert!(parse("echo ${x:-a").is_err());
        assert!(parse("echo ${x:-'a}").is_err());
    }

    #[test]
    fn a_default_word_that_nests_without_end_is_an_error_not_a_stack_overflow() {
        let deep = format!("echo {}x{}", "${a:-".repeat(500), "}".repeat(500));
        assert!(parse(&deep).is_err());
    }

    #[test]
    fn an_unquoted_heredoc_body_is_read_for_its_expansions() {
        assert!(!outside("cat <<EOF\n${x@P}\nEOF").is_empty());
        assert!(outside("cat <<'EOF'\n${x@P}\nEOF").is_empty());
        assert!(outside("cat <<EOF\n${x:-a} $((1+2)) \\${x@P}\nEOF").is_empty());
        let c = commands("cat <<EOF\n${x:-$(gh pr merge 1)}\nEOF");
        assert!(c.contains(&"gh pr merge 1".to_string()), "{c:?}");
    }

    #[test]
    fn a_redirection_or_a_heredoc_without_a_word_is_an_error() {
        for src in ["echo >", "echo > ;", "cat <<(ls)", "cat <<<", "echo 2>&"] {
            assert!(parse(src).is_err(), "{src}");
        }
        for src in [
            "cat < <(echo x)",
            "echo a > >(cat)",
            "echo a 2>&1",
            "echo >&2",
        ] {
            assert!(parse(src).is_ok(), "{src}");
        }
    }
}
