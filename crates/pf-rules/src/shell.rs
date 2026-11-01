//! A small shell parser. It finds the simple commands in a command line and nothing more.
//!
//! It reads quotes, `$'…'`, heredocs, here-strings, `&&`, `||`, `;`, pipes, `$(…)`, backticks,
//! process substitution, `if … fi`, `for`, `while`, `case`, `{ …; }`, subshells, assignments,
//! environment prefixes and redirections. It never runs anything and expands nothing: a variable
//! stays a variable. A text it cannot read is an `Err`, and the caller denies it.

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
}

/// All commands of a command line, nested ones included.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Parsed {
    pub cmds: Vec<Cmd>,
}

const RESERVED: [&str; 13] = [
    "if", "then", "elif", "else", "fi", "while", "until", "do", "done", "!", "{", "function",
    "coproc",
];

/// Parse a command line.
pub fn parse(src: &str) -> Result<Parsed, String> {
    let chars: Vec<char> = src.chars().collect();
    let mut p = Parser {
        chars: &chars,
        i: 0,
        cmds: Vec::new(),
        closers: Vec::new(),
    };
    p.list(None, 0)?;
    Ok(Parsed { cmds: p.cmds })
}

struct Parser<'a> {
    chars: &'a [char],
    i: usize,
    cmds: Vec<Cmd>,
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
                    if self.peek() == Some(';') || self.peek() == Some('&') {
                        self.i += 1;
                    }
                    cur = None;
                    pipe_from = None;
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
                }
                '(' => {
                    if self.peek_at(1) == Some('(') && cur.is_none() {
                        // `(( … ))` arithmetic command.
                        self.i += 2;
                        self.skip_arith()?;
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
                    if paren_depth > 0 {
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
                    let idx = *cur.get_or_insert_with(|| {
                        let v = self.cmds.len();
                        self.cmds.push(Cmd {
                            piped_from: pipe_from.take(),
                            ..Cmd::default()
                        });
                        v
                    });
                    match self.add_word(idx, word, &case_headers) {
                        Action::Nothing => {}
                        Action::CaseHeader => case_headers.push(idx),
                        Action::CaseIn => {
                            case_headers.retain(|k| *k != idx);
                            in_case += 1;
                        }
                        Action::Esac => in_case = in_case.saturating_sub(1),
                    }
                }
            }
        }
    }

    fn new_cmd(&mut self, piped_from: Option<usize>) -> usize {
        self.cmds.push(Cmd {
            piped_from,
            ..Cmd::default()
        });
        self.cmds.len() - 1
    }

    /// Put a word into a command, treating assignments and reserved words at the start.
    fn add_word(&mut self, idx: usize, word: Word, case_headers: &[usize]) -> Action {
        let plain = !word.any_quote && !word.dynamic;
        let cmd = &mut self.cmds[idx];
        if cmd.header {
            // The rest of a `for`, `select` or `case` header holds no command.
            if plain && word.text == "in" && case_headers.contains(&idx) {
                return Action::CaseIn;
            }
            if cmd.loop_header {
                cmd.head.push(word);
            }
            return Action::Nothing;
        }
        if cmd.words.is_empty() {
            if assignment_prefix(&word.raw) {
                cmd.assigns.push(word);
                return Action::Nothing;
            }
            if plain && word.text == "esac" {
                return Action::Esac;
            }
            if plain && (RESERVED.contains(&word.text.as_str()) || word.text == "}") {
                // Skip the reserved word. The command that follows it uses the same slot.
                return Action::Nothing;
            }
            if plain && matches!(word.text.as_str(), "for" | "select") {
                cmd.header = true;
                cmd.loop_header = true;
                return Action::Nothing;
            }
            if plain && word.text == "case" {
                cmd.header = true;
                return Action::CaseHeader;
            }
        }
        cmd.words.push(word);
        Action::Nothing
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
                // An unquoted heredoc expands `$(…)` and backticks in its body.
                let sub: Vec<char> = body.chars().collect();
                let mut inner = Parser {
                    chars: &sub,
                    i: 0,
                    cmds: Vec::new(),
                    closers: Vec::new(),
                };
                inner.scan_expansions(depth + 1)?;
                self.cmds.extend(inner.cmds);
            }
            self.cmds[h.cmd].heredocs.push(body);
        }
        Ok(())
    }

    /// Find `$(…)` and backticks in text that the shell would expand, and parse them.
    fn scan_expansions(&mut self, depth: usize) -> Result<(), String> {
        while let Some(c) = self.peek() {
            match c {
                '\\' => self.i += 2,
                '$' if self.peek_at(1) == Some('(') && self.peek_at(2) != Some('(') => {
                    self.i += 2;
                    self.list(Some(')'), depth)?;
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
        if matches!(self.peek(), None | Some('\n' | ';' | '&' | '|' | ')')) {
            return Ok(());
        }
        self.word(depth).map(|_| ())
    }

    /// Skip `… ))` of an arithmetic expression.
    fn skip_arith(&mut self) -> Result<(), String> {
        let mut depth = 0usize;
        while let Some(c) = self.peek() {
            match c {
                '(' => depth += 1,
                ')' => {
                    if depth == 0 {
                        if self.peek_at(1) == Some(')') {
                            self.i += 2;
                            return Ok(());
                        }
                        self.i += 1;
                        return Ok(());
                    }
                    depth -= 1;
                }
                _ => {}
            }
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
                    match self.peek_at(1) {
                        Some('\'') => {
                            self.i += 2;
                            any_quote = true;
                            let s = self.ansi_c()?;
                            segs.push(Seg::Quoted(s));
                        }
                        Some('"') => {
                            self.i += 2;
                            any_quote = true;
                            self.double_quoted(&mut segs, depth)?;
                        }
                        _ => self.dollar(&mut segs, false, depth)?,
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
        Ok(Word {
            text,
            raw,
            dynamic,
            quoted_var,
            glob,
            any_quote,
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
                self.i += 3;
                self.skip_arith()?;
                segs.push(Seg::Subst);
            }
            Some('(') => {
                self.i += 2;
                self.list(Some(')'), depth + 1)?;
                segs.push(Seg::Subst);
            }
            Some('{') => {
                self.i += 2;
                let mut name = String::new();
                let mut nest = 1usize;
                let mut simple = true;
                loop {
                    match self.peek() {
                        None => return Err("a ${…} is not closed".to_string()),
                        Some('}') => {
                            self.i += 1;
                            nest -= 1;
                            if nest == 0 {
                                break;
                            }
                        }
                        Some('{') => {
                            nest += 1;
                            simple = false;
                            self.i += 1;
                        }
                        Some(ch) => {
                            if !is_name_char(ch) {
                                simple = false;
                            }
                            if simple {
                                name.push(ch);
                            }
                            self.i += 1;
                        }
                    }
                }
                if simple && !name.is_empty() {
                    segs.push(Seg::Var { name, quoted });
                } else {
                    segs.push(Seg::Subst);
                }
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
