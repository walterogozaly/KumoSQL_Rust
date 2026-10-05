//! A BigQuery-aware tokenizer, used by the text rewrites in
//! [`crate::rewrite`].
//!
//! Ported from the tokenizer the Python original drives its BigQuery syntax
//! rewrites with, in `bigquery_syntax.py`.
//!
//! # Why a tokenizer at all
//!
//! The Python original works around `sqlglot`'s BigQuery gaps by rewriting the
//! *source text* before parsing, into calls that every build of `sqlglot` reads
//! as ordinary function calls and then resolves back afterwards. The same trick
//! is what the Rust port needs, because `sqlparser-rs` has the same class of
//! gap: `COUNT(x WHERE c)` is BigQuery's aggregate filter, `STRUCT<>()` reads as
//! a comparison, and script statements are not standard SQL at all.
//!
//! The tokenizer therefore only has to be good enough to find the shapes those
//! rewrites look for, and to never mistake text inside a string, a quoted name
//! or a comment for SQL. It is not a validating lexer and does not build a
//! parse tree; [`crate::parse`] does that with `sqlparser-rs`.

use std::fmt;

/// The kind of a token.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum TokenKind {
    /// A bare or backticked name, or a keyword.
    Word,
    /// A single-quoted or double-quoted string, quotes included.
    String,
    /// A `b'..'` or `rb'..'` bytes literal, prefix included.
    Bytes,
    /// A number.
    Number,
    /// `(`
    LParen,
    /// `)`
    RParen,
    /// `,`
    Comma,
    /// `.`
    Dot,
    /// An operator or any other run of symbol characters: `=`, `<=`, `||`, ...
    Punctuation,
    /// A `--` line comment, newline included.
    LineComment,
    /// A `/* */` block comment.
    BlockComment,
}

impl fmt::Display for TokenKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            TokenKind::Word => "word",
            TokenKind::String => "string",
            TokenKind::Bytes => "bytes",
            TokenKind::Number => "number",
            TokenKind::LParen => "(",
            TokenKind::RParen => ")",
            TokenKind::Comma => ",",
            TokenKind::Dot => ".",
            TokenKind::Punctuation => "punctuation",
            TokenKind::LineComment => "line comment",
            TokenKind::BlockComment => "block comment",
        })
    }
}

/// One token, with the byte range it covers in the source.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Token {
    /// What kind of token this is.
    pub kind: TokenKind,
    /// Byte range in the source, `start..end`.
    pub span: std::ops::Range<usize>,
}

impl Token {
    /// The token's text.
    pub fn text<'a>(&self, source: &'a str) -> &'a str {
        &source[self.span.clone()]
    }

    /// The token's text folded to upper case, for keyword comparison.
    ///
    /// Only meaningful for [`TokenKind::Word`]; a backticked name is not a
    /// keyword, so callers must check `kind` before comparing.
    pub fn upper(&self, source: &str) -> String {
        self.text(source).to_uppercase()
    }

    /// Whether this word is the given keyword, ignoring case.
    pub fn is_keyword(&self, source: &str, keyword: &str) -> bool {
        self.kind == TokenKind::Word && self.text(source).eq_ignore_ascii_case(keyword)
    }
}

fn is_word_start(c: char) -> bool {
    c.is_alphabetic() || c == '_'
}

fn is_word_continue(c: char) -> bool {
    c.is_alphanumeric() || c == '_'
}

fn is_digit(c: char) -> bool {
    c.is_ascii_digit()
}

/// Split `source` into tokens.
///
/// Comments are returned as tokens rather than skipped, so a rewrite that
/// rebuilds the text can put them back unchanged. Everything the tokenizer
/// cannot classify becomes [`TokenKind::Punctuation`], so it never loses text.
pub fn tokenize(source: &str) -> Vec<Token> {
    let bytes: Vec<char> = source.chars().collect();
    let mut tokens = Vec::new();
    let mut i = 0usize;

    // Byte offset of the character at index `i`.
    let offset_of = |i: usize| -> usize { bytes[..i].iter().map(|c| c.len_utf8()).sum() };

    while i < bytes.len() {
        let start = i;
        let ch = bytes[i];

        // Whitespace is not a token.
        if ch.is_whitespace() {
            i += 1;
            continue;
        }

        // Comments.
        if ch == '-' && bytes.get(i + 1) == Some(&'-') {
            i += 2;
            while i < bytes.len() && bytes[i] != '\n' {
                i += 1;
            }
            // The newline belongs to the comment, so that rebuilding the text
            // keeps a line comment from swallowing the next line.
            if i < bytes.len() {
                i += 1;
            }
            tokens.push(Token {
                kind: TokenKind::LineComment,
                span: offset_of(start)..offset_of(i),
            });
            continue;
        }
        if ch == '#' {
            while i < bytes.len() && bytes[i] != '\n' {
                i += 1;
            }
            if i < bytes.len() {
                i += 1;
            }
            tokens.push(Token {
                kind: TokenKind::LineComment,
                span: offset_of(start)..offset_of(i),
            });
            continue;
        }
        if ch == '/' && bytes.get(i + 1) == Some(&'*') {
            i += 2;
            while i < bytes.len() && !(bytes[i] == '*' && bytes.get(i + 1) == Some(&'/')) {
                i += 1;
            }
            i = (i + 2).min(bytes.len());
            tokens.push(Token {
                kind: TokenKind::BlockComment,
                span: offset_of(start)..offset_of(i),
            });
            continue;
        }

        // A bytes literal, with an optional `r`/`rb` prefix.
        if is_word_start(ch) {
            let mut j = i;
            while j < bytes.len() && is_word_continue(bytes[j]) {
                j += 1;
            }
            let word: String = bytes[i..j].iter().collect();
            let lowered = word.to_lowercase();
            if matches!(lowered.as_str(), "b" | "r" | "rb" | "br")
                && matches!(bytes.get(j), Some('"') | Some('\''))
            {
                let (end, _) = scan_quoted(&bytes, j);
                i = end;
                tokens.push(Token {
                    kind: TokenKind::Bytes,
                    span: offset_of(start)..offset_of(i),
                });
                continue;
            }
            i = j;
            tokens.push(Token {
                kind: TokenKind::Word,
                span: offset_of(start)..offset_of(i),
            });
            continue;
        }

        // A quoted string.
        if ch == '\'' || ch == '"' {
            let (end, _) = scan_quoted(&bytes, i);
            i = end;
            tokens.push(Token {
                kind: TokenKind::String,
                span: offset_of(start)..offset_of(i),
            });
            continue;
        }

        // A backticked name. BigQuery escapes a backtick inside a quoted name by
        // doubling it, as in `a``b`, so a doubled backtick does not close the
        // name.
        if ch == '`' {
            let mut j = i + 1;
            while j < bytes.len() {
                if bytes[j] == '`' {
                    if bytes.get(j + 1) == Some(&'`') {
                        j += 2;
                        continue;
                    }
                    break;
                }
                j += 1;
            }
            i = (j + 1).min(bytes.len());
            tokens.push(Token {
                kind: TokenKind::Word,
                span: offset_of(start)..offset_of(i),
            });
            continue;
        }

        // A number.
        if is_digit(ch) || (ch == '.' && bytes.get(i + 1).is_some_and(|c| is_digit(*c))) {
            let mut j = i;
            let hex = ch == '0' && matches!(bytes.get(j + 1), Some('x') | Some('X'));
            if hex {
                j += 2;
                while j < bytes.len() && (bytes[j].is_ascii_hexdigit() || bytes[j] == '_') {
                    j += 1;
                }
            } else {
                while j < bytes.len() && (is_digit(bytes[j]) || bytes[j] == '.' || bytes[j] == '_')
                {
                    j += 1;
                }
                if j < bytes.len() && (bytes[j] == 'e' || bytes[j] == 'E') {
                    j += 1;
                    if j < bytes.len() && (bytes[j] == '+' || bytes[j] == '-') {
                        j += 1;
                    }
                    while j < bytes.len() && is_digit(bytes[j]) {
                        j += 1;
                    }
                }
            }
            i = j;
            tokens.push(Token {
                kind: TokenKind::Number,
                span: offset_of(start)..offset_of(i),
            });
            continue;
        }

        // Punctuation, one character at a time so parens and commas are their
        // own kinds.
        let kind = match ch {
            '(' => TokenKind::LParen,
            ')' => TokenKind::RParen,
            ',' => TokenKind::Comma,
            '.' => TokenKind::Dot,
            _ => TokenKind::Punctuation,
        };
        i += 1;
        tokens.push(Token {
            kind,
            span: offset_of(start)..offset_of(i),
        });
    }

    tokens
}

/// The index just past the quoted run opening at `start`.
///
/// `start` must hold a quote character. A `\` escapes the next character, and a
/// repeated quote is a triple-quoted literal.
fn scan_quoted(bytes: &[char], start: usize) -> (usize, char) {
    let quote = bytes[start];
    let triple = bytes.len() >= start + 3 && bytes[start + 1] == quote && bytes[start + 2] == quote;
    let qlen = if triple { 3 } else { 1 };

    let mut j = start + qlen;
    while j < bytes.len() {
        if bytes[j] == '\\' {
            j += 2;
            continue;
        }
        if bytes[j] == quote {
            if triple {
                if bytes.get(j + 1) == Some(&quote) && bytes.get(j + 2) == Some(&quote) {
                    return (j + 3, quote);
                }
            } else {
                return (j + 1, quote);
            }
        }
        j += 1;
    }
    (bytes.len(), quote)
}

/// Rebuild SQL text from tokens, preserving everything between them.
///
/// Used by the rewrites to reassemble text they only partially changed.
pub fn render(source: &str, tokens: &[Token]) -> String {
    let mut out = String::with_capacity(source.len());
    let mut cursor = 0usize;
    for token in tokens {
        if token.span.start > cursor {
            out.push_str(&source[cursor..token.span.start]);
        }
        out.push_str(&source[token.span.clone()]);
        cursor = token.span.end;
    }
    if cursor < source.len() {
        out.push_str(&source[cursor..]);
    }
    out
}
