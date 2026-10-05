//! Splitting a BigQuery script into its statements.
//!
//! Ported from `src/kumosql/scripts.py`.
//!
//! # Why this exists
//!
//! A BigQuery script is not SQL. `DECLARE`, `SET`, `BEGIN ... END`,
//! `IF ... THEN ... END IF`, `WHILE`, `FOR ... IN`, `CREATE PROCEDURE` are all
//! part of the scripting language, and no generic SQL parser reads them. So the
//! script is split *first*, into the statements a SQL parser can read, and each
//! statement is then parsed on its own.
//!
//! Splitting is not flattening. A statement inside `IF ... THEN` only runs on
//! some rows, so [`ScriptPart`] records that it is **conditional**, and a
//! statement inside a procedure records which procedure it belongs to. A caller
//! that ignored that would treat a conditional delete as an unconditional one.
//!
//! Blocks are *opened*, not returned as statements: `split_script` returns the
//! statements in order, and the blocks that contain them only as the flags on
//! each part.

use std::fmt;

/// What a [`Node`] is: a statement, or a control structure around some.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NodeKind {
    /// A plain statement.
    Statement,
    /// `BEGIN ... END`
    Begin,
    /// `IF ... THEN ... END IF`
    If,
    /// `WHILE ... DO ... END WHILE`
    While,
    /// `LOOP ... END LOOP`
    Loop,
    /// `FOR ... IN ... DO ... END FOR`
    For,
    /// `REPEAT ... UNTIL ... END REPEAT`
    Repeat,
    /// `CASE ... WHEN ... END CASE`
    Case,
    /// `CREATE PROCEDURE ... BEGIN ... END`
    Procedure,
}

impl fmt::Display for NodeKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            NodeKind::Statement => "stmt",
            NodeKind::Begin => "begin",
            NodeKind::If => "if",
            NodeKind::While => "while",
            NodeKind::Loop => "loop",
            NodeKind::For => "for",
            NodeKind::Repeat => "repeat",
            NodeKind::Case => "case",
            NodeKind::Procedure => "procedure",
        })
    }
}

/// One statement, or one control structure, of a script.
#[derive(Debug, Clone, PartialEq)]
pub struct Node {
    /// What this node is.
    pub kind: NodeKind,
    /// Byte offset where it starts.
    pub start: usize,
    /// Byte offset just past where it ends.
    pub end: usize,
    /// A statement's text, or a control structure's condition text.
    pub header: String,
    /// The statements or blocks inside it.
    ///
    /// A block has one branch; `IF`, `CASE`, `BEGIN ... EXCEPTION` have one per
    /// branch, in source order.
    pub branches: Vec<Vec<Node>>,
    /// A procedure's name, or a `FOR` loop's variable.
    pub name: String,
    /// A `FOR` loop's query.
    pub query: String,
    /// A procedure's parameter list.
    pub params: String,
    /// Every condition that picks a path: `IF`/`ELSEIF`, `CASE` and `WHEN`,
    /// `WHILE`, `UNTIL`.
    pub conditions: Vec<String>,
}

impl Node {
    /// A statement node with no children.
    fn statement(start: usize, end: usize, header: String) -> Self {
        Node {
            kind: NodeKind::Statement,
            start,
            end,
            header,
            branches: Vec::new(),
            name: String::new(),
            query: String::new(),
            params: String::new(),
            conditions: Vec::new(),
        }
    }

    /// A control-structure node with one branch.
    fn container(kind: NodeKind, start: usize) -> Self {
        Node {
            kind,
            start,
            end: start,
            header: String::new(),
            branches: Vec::new(),
            name: String::new(),
            query: String::new(),
            params: String::new(),
            conditions: Vec::new(),
        }
    }

    /// Whether this node is a statement rather than a block.
    pub fn is_statement(&self) -> bool {
        self.kind == NodeKind::Statement
    }
}

/// One statement of a script, with what is known about its context.
#[derive(Debug, Clone, PartialEq)]
pub struct ScriptPart {
    /// The statement's own text, without its trailing semicolon.
    pub text: String,
    /// 1-based line the statement starts on.
    pub line: usize,
    /// Whether the statement only runs on some paths.
    ///
    /// True inside `IF`, `ELSEIF`, `WHILE`, `FOR`, `REPEAT`, `CASE`, a loop, a
    /// `BEGIN` that is not the first branch, and a procedure.
    pub conditional: bool,
    /// The procedure the statement is inside, if any; empty when it is not.
    ///
    /// Named  to match the Python original's field.
    pub in_procedure: String,
}

/// Splits a script into statements and the blocks around them.
pub struct Splitter<'a> {
    text: &'a str,
    toks: Vec<Token>,
    i: usize,
}

/// One lexical token, with its byte span.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Token {
    /// Byte span in the script.
    span: std::ops::Range<usize>,
    /// The token's text.
    text: String,
    /// The text folded to upper case, for keyword comparison.
    upper: String,
}

impl Token {
    fn is_word(&self, keyword: &str) -> bool {
        self.upper == keyword
    }
}

/// Tokenize a script, **dropping comments**.
///
/// Reuses [`crate::token::tokenize`] so that a `;` or a keyword inside a string
/// or a quoted name is never mistaken for structure. Comments are dropped
/// rather than kept, matching the Python original's lexer: a comment between two
/// statements must not be absorbed into the statement that follows it, or the
/// statement's text and its reported line both shift.
fn lex(text: &str) -> Vec<Token> {
    crate::token::tokenize(text)
        .into_iter()
        .filter(|t| {
            !matches!(
                t.kind,
                crate::token::TokenKind::LineComment | crate::token::TokenKind::BlockComment
            )
        })
        .map(|t| {
            let slice = t.text(text);
            Token {
                span: t.span,
                text: slice.to_string(),
                upper: slice.to_uppercase(),
            }
        })
        .collect()
}

/// A `CASE` expression inside a condition, not a `CASE` statement.
const CASE_DEPTH_WORDS: [&str; 1] = ["CASE"];

impl Splitter<'_> {
    /// Split `text` into its top-level nodes.
    pub fn parse(text: &str) -> Vec<Node> {
        Splitter {
            text,
            toks: lex(text),
            i: 0,
        }
        .block(&[])
    }

    fn len(&self) -> usize {
        self.toks.len()
    }

    /// The upper-cased text of the token `offset` positions ahead, or `""`.
    fn word_at(&self, offset: usize) -> String {
        self.toks
            .get(self.i + offset)
            .map(|t| t.upper.clone())
            .unwrap_or_default()
    }

    fn text_at(&self, offset: usize) -> String {
        self.toks
            .get(self.i + offset)
            .map(|t| t.text.clone())
            .unwrap_or_default()
    }

    /// Consume tokens up to and including the next top-level `;`.
    fn skip_to_semicolon(&mut self) {
        while self.i < self.len() && self.toks[self.i].text != ";" {
            self.i += 1;
        }
        if self.i < self.len() {
            self.i += 1;
        }
    }

    /// Advance to a keyword in `stop` at parenthesis and `CASE` depth zero,
    /// leaving the position on it.
    ///
    /// The `CASE` depth matters because a condition may contain `CASE WHEN ...
    /// END`, and the `END` there does not close the surrounding block.
    fn scan_until(&mut self, stop: &[&str]) -> (usize, usize) {
        let start = self.i;
        let mut depth = 0i32;
        let mut case_depth = 0i32;
        while self.i < self.len() {
            let token = &self.toks[self.i];
            if token.text == "(" {
                depth += 1;
            } else if token.text == ")" {
                depth = (depth - 1).max(0);
            } else if token.text == ";" && depth == 0 {
                break;
            } else if CASE_DEPTH_WORDS.contains(&token.upper.as_str()) && token.text == "CASE" {
                case_depth += 1;
            } else if token.is_word("END") && case_depth > 0 {
                case_depth -= 1;
            } else if depth == 0
                && case_depth == 0
                && (stop.iter().any(|s| token.is_word(s)) || token.text == ";")
            {
                break;
            }
            self.i += 1;
        }
        (start, self.i)
    }

    /// The source text spanned by tokens `a..b`.
    fn text_of(&self, a: usize, b: usize) -> String {
        if a >= b || a >= self.len() {
            return String::new();
        }
        let start = self.toks[a].span.start;
        let end = self.toks[b - 1].span.end;
        self.text[start..end].to_string()
    }

    /// Consume `END`, its optional keyword and label, and the `;`.
    fn end_statement(&mut self) {
        if self.word_at(0) == "END" {
            self.i += 1;
        }
        self.skip_to_semicolon();
    }

    /// The byte offset just past the statement's terminator.
    fn end_offset(&self, fallback: usize) -> usize {
        if self.i == 0 {
            fallback
        } else {
            self.toks[self.i - 1].span.end
        }
    }

    /// Parse statements until a terminator keyword is next.
    fn block(&mut self, terminators: &[&str]) -> Vec<Node> {
        let mut nodes = Vec::new();
        while self.i < self.len() {
            let token = &self.toks[self.i];
            if token.text == ";" {
                self.i += 1;
                continue;
            }
            // Only a word can close a block: a string or a quoted name that
            // happens to spell `END` does not.
            if terminators.iter().any(|t| &token.upper == t) {
                break;
            }
            if let Some(node) = self.statement(!terminators.is_empty()) {
                nodes.push(node);
            }
        }
        nodes
    }

    fn statement(&mut self, inside_block: bool) -> Option<Node> {
        let begin = self.toks[self.i].span.start;

        // An optional `label:` in front of a block: the cursor is on the label
        // and the next token is its colon.
        if self.text_at(1) == ":"
            && ["BEGIN", "LOOP", "WHILE", "FOR", "REPEAT"].contains(&self.word_at(2).as_str())
        {
            self.i += 2;
        }

        let first = self.word_at(0);
        let next_is_paren = self.text_at(1) == "(";

        // `BEGIN` opens a block, but `BEGIN TRANSACTION` is a statement.
        if first == "BEGIN"
            && self.word_at(1) != "TRANSACTION"
            && !self.word_at(1).is_empty()
            && self.text_at(1) != ";"
        {
            return Some(self.begin_block(begin));
        }
        // `IF(` opens a call; an `IF` statement is `IF cond THEN`.
        if first == "IF" && (next_is_paren && !self.control_if_is_statement() || !next_is_paren) {
            return Some(self.if_block(begin));
        }
        match first.as_str() {
            "WHILE" => return Some(self.simple_loop(begin, NodeKind::While, &["DO"])),
            "LOOP" => return Some(self.simple_loop(begin, NodeKind::Loop, &[])),
            "REPEAT" => return Some(self.repeat_block(begin)),
            "FOR" if self.word_at(2) == "IN" => return Some(self.for_block(begin)),
            "CASE" => return Some(self.case_block(begin)),
            "CREATE" => {
                if let Some(node) = self.maybe_procedure(begin) {
                    return Some(node);
                }
            }
            _ => {}
        }
        self.simple(begin, inside_block)
    }

    /// Whether an `IF` starting at the cursor opens a statement rather than a
    /// call: an `IF` statement has a `THEN` at parenthesis depth zero.
    fn control_if_is_statement(&self) -> bool {
        let mut j = self.i;
        let mut depth = 0i32;
        while j < self.len() {
            let token = &self.toks[j];
            if token.text == "(" {
                depth += 1;
            } else if token.text == ")" {
                depth -= 1;
            } else if token.text == ";" {
                return false;
            } else if token.is_word("THEN") && depth == 0 {
                return true;
            }
            j += 1;
        }
        false
    }

    /// One statement, up to its `;`.
    ///
    /// The statement's text stops *before* the semicolon. Inside a block, a
    /// top-level `END` also stops it, which is how a last statement written
    /// without its semicolon is found.
    fn simple(&mut self, _begin: usize, inside_block: bool) -> Option<Node> {
        let start = self.i;
        let mut depth = 0i32;
        let mut case_depth = 0i32;

        while self.i < self.len() {
            let token = &self.toks[self.i];
            if token.text == ";" {
                break;
            }
            if token.text == "(" {
                depth += 1;
            } else if token.text == ")" && depth > 0 {
                depth -= 1;
            } else if token.is_word("CASE") {
                case_depth += 1;
            } else if token.is_word("END") {
                if case_depth > 0 {
                    case_depth -= 1;
                } else if inside_block && depth == 0 {
                    break;
                }
            }
            self.i += 1;
        }

        let stop = self.i;
        if self.i < self.len() && self.toks[self.i].text == ";" {
            self.i += 1;
        }

        if stop == start {
            // Nothing was consumed: a stray `END` or keyword at this level.
            // Step over it so the walk makes progress, and report no statement.
            if self.i == start {
                self.i += 1;
            }
            return None;
        }

        Some(Node::statement(
            self.toks[start].span.start,
            self.toks[stop - 1].span.end,
            self.text_of(start, stop),
        ))
    }

    fn begin_block(&mut self, begin: usize) -> Node {
        self.i += 1;
        let mut node = Node::container(NodeKind::Begin, begin);
        let body = self.block(&["END", "EXCEPTION"]);
        node.branches.push(body);
        if self.word_at(0) == "EXCEPTION" {
            self.i += 1;
            if self.word_at(0) == "WHEN" {
                self.scan_until(&["THEN"]);
                self.i += 1;
            }
            node.branches.push(self.block(&["END"]));
        }
        self.end_statement();
        node.end = self.end_offset(begin);
        node
    }

    fn if_block(&mut self, begin: usize) -> Node {
        self.i += 1;
        let (a, b) = self.scan_until(&["THEN"]);
        let mut node = Node::container(NodeKind::If, begin);
        node.header = self.text_of(a, b);
        node.conditions.push(node.header.clone());
        self.i += 1; // THEN
        node.branches.push(self.block(&["ELSEIF", "ELSE", "END"]));

        while ["ELSEIF", "ELSE"].contains(&self.word_at(0).as_str()) {
            if self.word_at(0) == "ELSEIF" {
                self.i += 1;
                let (a, b) = self.scan_until(&["THEN"]);
                let header = self.text_of(a, b);
                node.conditions.push(header);
            }
            self.i += 1;
            node.branches.push(self.block(&["ELSEIF", "ELSE", "END"]));
        }
        self.end_statement();
        node.end = self.end_offset(begin);
        node
    }

    /// `WHILE cond DO ... END WHILE` or `LOOP ... END LOOP`.
    fn simple_loop(&mut self, begin: usize, kind: NodeKind, header_stop: &[&str]) -> Node {
        self.i += 1;
        let mut node = Node::container(kind, begin);
        if !header_stop.is_empty() {
            let (a, b) = self.scan_until(header_stop);
            node.header = self.text_of(a, b);
            node.conditions.push(node.header.clone());
            self.i += 1;
        }
        node.branches.push(self.block(&["END"]));
        self.end_statement();
        node.end = self.end_offset(begin);
        node
    }

    fn repeat_block(&mut self, begin: usize) -> Node {
        self.i += 1;
        let mut node = Node::container(NodeKind::Repeat, begin);
        node.branches.push(self.block(&["UNTIL", "END"]));
        if self.word_at(0) == "UNTIL" {
            self.i += 1;
            let (a, b) = self.scan_until(&["END"]);
            node.header = self.text_of(a, b);
            node.conditions.push(node.header.clone());
        }
        self.end_statement();
        node.end = self.end_offset(begin);
        node
    }

    fn for_block(&mut self, begin: usize) -> Node {
        let variable = self.text_at(1);
        self.i += 3; // FOR var IN
        let (a, b) = self.scan_until(&["DO"]);
        let mut query = self.text_of(a, b).trim().to_string();
        if query.starts_with('(') && query.ends_with(')') {
            query = query[1..query.len() - 1].to_string();
        }
        let mut node = Node::container(NodeKind::For, begin);
        node.name = variable;
        node.query = query;
        self.i += 1; // DO
        node.branches.push(self.block(&["END"]));
        self.end_statement();
        node.end = self.end_offset(begin);
        node
    }

    fn case_block(&mut self, begin: usize) -> Node {
        self.i += 1;
        let mut node = Node::container(NodeKind::Case, begin);
        let (a, b) = self.scan_until(&["WHEN"]);
        node.header = self.text_of(a, b);
        if !node.header.trim().is_empty() {
            // `CASE expr WHEN ...`: the value every WHEN compares against.
            node.conditions.push(node.header.clone());
        }
        while ["WHEN", "ELSE"].contains(&self.word_at(0).as_str()) {
            if self.word_at(0) == "WHEN" {
                self.i += 1;
                let (a, b) = self.scan_until(&["THEN"]);
                let header = self.text_of(a, b);
                node.conditions.push(header);
            }
            self.i += 1;
            node.branches.push(self.block(&["WHEN", "ELSE", "END"]));
        }
        self.end_statement();
        node.end = self.end_offset(begin);
        node
    }

    /// Recognise `CREATE [OR REPLACE] [TEMP] PROCEDURE name(...) ... BEGIN`.
    ///
    /// Returns `None` when the body is not SQL -- a `LANGUAGE js` function with
    /// no `BEGIN` -- so the caller reads it as one ordinary statement instead.
    fn maybe_procedure(&mut self, begin: usize) -> Option<Node> {
        let mut j = self.i + 1;
        while j < self.len()
            && ["OR", "REPLACE", "TEMP", "TEMPORARY"].contains(&self.toks[j].upper.as_str())
        {
            j += 1;
        }
        if j >= self.len() || self.toks[j].upper != "PROCEDURE" {
            return None;
        }
        j += 1;
        // `IF NOT EXISTS` between `PROCEDURE` and the name.
        if j < self.len() && self.toks[j].upper == "IF" {
            j += 3;
        }

        // The name, which may be dotted and may be backticked.
        let mut name_parts: Vec<String> = Vec::new();
        while j < self.len()
            && (self.toks[j]
                .upper
                .chars()
                .all(|c| c.is_alphanumeric() || c == '_')
                || self.toks[j].text == "."
                || self.toks[j].text.starts_with('`'))
        {
            name_parts.push(self.toks[j].text.clone());
            j += 1;
        }

        // Then the parameter list, `OPTIONS(...)`, `LANGUAGE ...`, and finally
        // the `BEGIN` that opens the body.
        let params_start = j;
        let mut k = j;
        let mut depth = 0i32;
        loop {
            if k >= self.len() {
                // No body here at all.
                return None;
            }
            let token = &self.toks[k];
            if token.text == "(" {
                depth += 1;
            } else if token.text == ")" {
                depth -= 1;
            } else if token.text == ";" && depth == 0 {
                // No body here: a language other than SQL.
                return None;
            } else if token.is_word("BEGIN") && depth == 0 {
                break;
            }
            k += 1;
        }

        let mut node = Node::container(NodeKind::Procedure, begin);
        node.name = name_parts.concat().replace('`', "");
        node.params = self.text_of(params_start, k);
        self.i = k + 1;
        node.branches.push(self.block(&["END", "EXCEPTION"]));
        if self.word_at(0) == "EXCEPTION" {
            self.i += 1;
            if self.word_at(0) == "WHEN" {
                self.scan_until(&["THEN"]);
                self.i += 1;
            }
            node.branches.push(self.block(&["END"]));
        }
        self.end_statement();
        node.end = self.end_offset(begin);
        Some(node)
    }
}

/// Parse a script into its nodes.
pub fn parse_script(text: &str) -> Vec<Node> {
    Splitter::parse(text)
}

/// The statements of `text`, in order.
///
/// Blocks and control flow are *opened*, not returned as statements; what each
/// statement knows about its context is on its [`ScriptPart`].
pub fn split_script(text: &str) -> Vec<ScriptPart> {
    // Byte offset of every line start, so a statement's line can be reported.
    let mut line_starts = vec![0usize];
    for (offset, ch) in text.char_indices() {
        if ch == '\n' {
            line_starts.push(offset + 1);
        }
    }
    let line_of = |offset: usize| -> usize {
        match line_starts.binary_search(&offset) {
            Ok(index) => index + 1,
            Err(index) => index,
        }
    };

    let mut parts = Vec::new();
    walk(&parse_script(text), false, "", &mut parts, &line_of);
    parts
}

fn walk(
    nodes: &[Node],
    conditional: bool,
    procedure: &str,
    parts: &mut Vec<ScriptPart>,
    line_of: &impl Fn(usize) -> usize,
) {
    for node in nodes {
        if node.is_statement() {
            parts.push(ScriptPart {
                text: node.header.clone(),
                line: line_of(node.start),
                conditional,
                in_procedure: procedure.to_string(),
            });
            continue;
        }
        let inner_procedure = if !procedure.is_empty() {
            procedure.to_string()
        } else if node.kind == NodeKind::Procedure {
            node.name.clone()
        } else {
            String::new()
        };
        for (index, branch) in node.branches.iter().enumerate() {
            // Everything inside a conditional construct is conditional, and so
            // is every branch but the first of a `BEGIN` or a procedure: only
            // the first path is the unconditional one.
            let gated = conditional
                || !matches!(node.kind, NodeKind::Procedure)
                || index > 0
                || !matches!(node.kind, NodeKind::Begin | NodeKind::Procedure);
            walk(branch, gated, &inner_procedure, parts, line_of);
        }
    }
}

/// Just the statement texts, in order.
pub fn split_statements(text: &str) -> Vec<String> {
    split_script(text).into_iter().map(|p| p.text).collect()
}

/// The statement nodes of `text`, in order, with their offsets.
///
/// Blocks are opened.
pub fn leaf_statements(text: &str) -> Vec<Node> {
    fn collect(nodes: &[Node], out: &mut Vec<Node>) {
        for node in nodes {
            if node.is_statement() {
                out.push(node.clone());
                continue;
            }
            for branch in &node.branches {
                collect(branch, out);
            }
        }
    }
    let mut out = Vec::new();
    collect(&parse_script(text), &mut out);
    out
}

/// Whether `text` has a `BEGIN ... END`, an `IF`, a loop or a procedure around
/// its statements.
pub fn has_blocks(text: &str) -> bool {
    parse_script(text).iter().any(|node| !node.is_statement())
}
