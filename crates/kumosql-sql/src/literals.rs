//! One spelling per BigQuery string value, so the provers and the execution
//! checks agree with BigQuery.
//!
//! Ported from `src/kumosql/string_literals.py` in the Python original.
//!
//! Parsing SQL with a generic parser leaves BigQuery string literals spelled
//! several different ways, and each spelling is a *different string value* to
//! a naive reader even where BigQuery gives both the same string. Two concrete
//! cases, both from the Python original's docstring:
//!
//! * `\'a\\"b\'` reads as the text `a\"b` under a reader that leaves some
//!   backslash escapes undecoded, which differs from `\'a"b\'` even though
//!   BigQuery gives both the same string.
//! * ``col``col`` reads as one name containing a backtick, when it is in fact
//!   two adjacent quoted names.
//!
//! [`canonical_literals`] rewrites the source text *before* it is parsed:
//!
//! * every plain string whose escapes are all among `\\ \' \" \` \? \n \r \t`
//!   becomes a single-quoted literal using only `\\`, `\'`, `\n`, `\r` and `\t`;
//! * a space goes between adjacent backtick names.
//!
//! Raw strings (`r'..'`), strings with other escapes (`\x41`, `\u0041`,
//! octal) and everything else are **left as written**. That is deliberate: a
//! value written with an escape this module does not decode must stay
//! unproven rather than be called different by a wrong reading.
//!
//! Bytes literals (`b'..'`, and raw `rb'..'`) are decoded and written back with
//! printable ASCII as is and every other byte as `\xHH`. With no `\\` left,
//! each byte string has exactly one reading.
//!
//! A single-quoted literal holding a line break is not valid GoogleSQL and is
//! left as written -- rewriting `b'a<newline>b'` as `b'a\x0Ab'` would turn a
//! query BigQuery rejects into one it runs.
//!
//! # Character indexing
//!
//! The Python original indexes `str` by character, and its scanning logic
//! depends on that (a `\\` advances two characters). This module therefore
//! scans a `Vec<char>` rather than a byte slice, so that the port has exactly
//! the original's semantics on non-ASCII input.

use std::fmt::Write as _;

/// The escape sequences this module decodes in a plain string, to their values.
fn simple_escape(c: char) -> Option<char> {
    Some(match c {
        '\\' => '\\',
        '\'' => '\'',
        '"' => '"',
        '`' => '`',
        '?' => '?',
        'n' => '\n',
        'r' => '\r',
        't' => '\t',
        _ => return None,
    })
}

/// The escapes written out by [`canonical_literals`].
///
/// Deliberately narrower than [`simple_escape`]: `"`, `` ` `` and `?` are
/// decoded but re-emitted as themselves, since only `\\`, `\'`, `\n`, `\r` and
/// `\t` need escaping inside a single-quoted literal.
fn canonical_escape(c: char) -> Option<&'static str> {
    Some(match c {
        '\\' => "\\\\",
        '\'' => "\\'",
        '\n' => "\\n",
        '\r' => "\\r",
        '\t' => "\\t",
        _ => return None,
    })
}

/// The escapes BigQuery defines inside a bytes literal, to their byte values.
fn byte_escape(c: char) -> Option<u8> {
    Some(match c {
        'a' => 7,
        'b' => 8,
        'f' => 12,
        'n' => 10,
        'r' => 13,
        't' => 9,
        'v' => 11,
        '\\' => 92,
        '?' => 63,
        '"' => 34,
        '\'' => 39,
        '`' => 96,
        _ => return None,
    })
}

fn hex_value(c: char) -> Option<u8> {
    c.to_digit(16).map(|d| d as u8)
}

fn octal_value(c: char) -> Option<u8> {
    c.to_digit(8).map(|d| d as u8)
}

/// The string value of an escaped literal body.
///
/// `None` if it uses an escape this module does not decode, in which case the
/// caller must leave the literal as written.
fn decode(body: &[char]) -> Option<String> {
    let mut out = String::with_capacity(body.len());
    let mut i = 0;
    while i < body.len() {
        let ch = body[i];
        if ch != '\\' {
            out.push(ch);
            i += 1;
            continue;
        }
        // A trailing lone backslash has nothing to escape.
        let escaped = *body.get(i + 1)?;
        out.push(simple_escape(escaped)?);
        i += 2;
    }
    Some(out)
}

/// The value of a bytes literal body: UTF-8 text plus BigQuery's escapes.
///
/// `None` for an escape it does not have. A `raw` body is taken as UTF-8 text
/// verbatim, since a raw literal decodes nothing.
fn decode_bytes(body: &[char], raw: bool) -> Option<Vec<u8>> {
    if raw {
        let mut out = Vec::with_capacity(body.len());
        for ch in body {
            let mut buf = [0u8; 4];
            out.extend_from_slice(ch.encode_utf8(&mut buf).as_bytes());
        }
        return Some(out);
    }
    let mut out: Vec<u8> = Vec::with_capacity(body.len());
    let mut i = 0;
    while i < body.len() {
        let ch = body[i];
        if ch != '\\' {
            let mut buf = [0u8; 4];
            out.extend_from_slice(ch.encode_utf8(&mut buf).as_bytes());
            i += 1;
            continue;
        }
        // `body.get(i + 1)` returning None means a trailing lone backslash,
        // which is an escape this module does not have.
        let code = *body.get(i + 1)?;

        if let Some(value) = byte_escape(code) {
            out.push(value);
            i += 2;
            continue;
        }

        // `\xHH`, exactly two hex digits.
        if (code == 'x' || code == 'X') && body.len() >= i + 4 {
            let (Some(hi), Some(lo)) = (hex_value(body[i + 2]), hex_value(body[i + 3])) else {
                return None;
            };
            out.push(hi * 16 + lo);
            i += 4;
            continue;
        }

        // Octal `\OOO`, exactly three octal digits, value below 256.
        if body.len() >= i + 4 {
            let digits = &body[i + 1..i + 4];
            if let Some(value) = digits
                .iter()
                .try_fold(0u16, |acc, d| octal_value(*d).map(|d| acc * 8 + d as u16))
                .filter(|v| *v < 256)
            {
                out.push(value as u8);
                i += 4;
                continue;
            }
        }

        return None;
    }
    Some(out)
}

/// `value` as `b'..'`: printable ASCII kept, and every other byte -- including
/// the quote and the backslash -- written `\xHH`.
///
/// With no `\\` left in the output, each byte string has exactly one reading.
fn bytes_literal(value: &[u8]) -> String {
    let mut out = String::from("b'");
    for &b in value {
        if (32..127).contains(&b) && b != 39 && b != 92 {
            out.push(b as char);
        } else {
            let _ = write!(out, "\\x{b:02X}");
        }
    }
    out.push('\'');
    out
}

/// The end of the string literal whose opening quote is at `start`.
///
/// Returns `(end, quote, body)`, where `body` is `None` if the literal is
/// unterminated. A quote that repeats three times is a triple-quoted literal,
/// which is the only kind that may span lines.
fn string_end(sql: &[char], start: usize) -> (usize, String, Option<Vec<char>>) {
    let first = sql[start];
    let needle = [first; 3].to_vec();
    let is_triple = sql.len() >= start + 3 && sql[start..start + 3] == needle[..];
    let quote: String = if is_triple {
        needle.iter().collect()
    } else {
        first.to_string()
    };
    let qlen = quote.chars().count();
    let needle: Vec<char> = quote.chars().collect();

    let mut j = start + qlen;
    while j < sql.len() && sql[j..].len() >= qlen && sql[j..j + qlen] != needle[..] {
        j += if sql[j] == '\\' { 2 } else { 1 };
    }
    if j + qlen > sql.len() || sql[j..j + qlen] != needle[..] {
        return (sql.len(), quote, None);
    }
    (j + qlen, quote, Some(sql[start + qlen..j].to_vec()))
}

/// Whether the literal is unterminated, or holds a line break that GoogleSQL
/// only allows inside triple quotes.
///
/// Such a literal is copied as written.
fn invalid(quote: &str, body: Option<&[char]>) -> bool {
    let Some(body) = body else { return true };
    if quote.chars().count() != 1 {
        return false;
    }
    body.contains(&'\n') || body.contains(&'\r')
}

/// Whether `sql` has a single-quoted string or bytes literal holding a line
/// break.
///
/// GoogleSQL rejects such a query, so the provers must decline it rather than
/// prove it equal to a valid query written with the escaped spelling.
pub fn invalid_literal(sql: &str) -> bool {
    let sql: Vec<char> = sql.chars().collect();
    if !sql.contains(&'\n') && !sql.contains(&'\r') {
        return false;
    }

    let (mut i, size) = (0usize, sql.len());
    while i < size {
        let ch = sql[i];
        if sql[i..].starts_with(&['-', '-']) || ch == '#' {
            // A line comment: skip to the newline (but not past it).
            i = sql[i..]
                .iter()
                .position(|c| *c == '\n')
                .map(|off| i + off)
                .unwrap_or(size);
        } else if sql[i..].starts_with(&['/', '*']) {
            i = sql[i + 2..]
                .windows(2)
                .position(|w| w == ['*', '/'])
                .map(|off| i + 2 + off + 2)
                .unwrap_or(size);
        } else if ch == '`' {
            // A backtick name: skip to its close, honouring `\` escapes.
            let mut j = i + 1;
            while j < size && sql[j] != '`' {
                j += if sql[j] == '\\' { 2 } else { 1 };
            }
            i = (j + 1).min(size);
        } else if ch == '\'' || ch == '"' {
            let (end, quote, body) = string_end(&sql, i);
            if invalid(&quote, body.as_deref()) {
                return true;
            }
            i = end;
        } else {
            i += 1;
        }
    }
    false
}

/// `sql` with BigQuery string literals and adjacent quoted names spelled one way.
///
/// Idempotent: applying it to its own output changes nothing.
pub fn canonical_literals(sql: &str) -> String {
    let sql_chars: Vec<char> = sql.chars().collect();

    // Fast path, matching the original: nothing here can change without a
    // backslash or two adjacent backticks.
    if !sql.contains('\\') && !sql.contains("``") {
        return sql.to_string();
    }

    let mut out = String::with_capacity(sql_chars.len());
    let (mut i, size) = (0usize, sql_chars.len());

    while i < size {
        let ch = sql_chars[i];

        if sql_chars[i..].starts_with(&['-', '-']) || ch == '#' {
            let end = sql_chars[i..]
                .iter()
                .position(|c| *c == '\n')
                .map(|off| i + off)
                .unwrap_or(size);
            out.extend(&sql_chars[i..end]);
            i = end;
        } else if sql_chars[i..].starts_with(&['/', '*']) {
            let end = sql_chars[i + 2..]
                .windows(2)
                .position(|w| w == ['*', '/'])
                .map(|off| i + 2 + off + 2)
                .unwrap_or(size);
            out.extend(&sql_chars[i..end]);
            i = end;
        } else if ch == '`' {
            let mut j = i + 1;
            while j < size && sql_chars[j] != '`' {
                j += if sql_chars[j] == '\\' { 2 } else { 1 };
            }
            let j = (j + 1).min(size);
            out.extend(&sql_chars[i..j]);
            if sql_chars.get(j) == Some(&'`') {
                // `a``b` is two names, not one name containing a backtick.
                out.push(' ');
            }
            i = j;
        } else if ch.is_alphanumeric() || ch == '_' {
            let mut j = i;
            while j < size && (sql_chars[j].is_alphanumeric() || sql_chars[j] == '_') {
                j += 1;
            }
            let word: String = sql_chars[i..j].iter().collect();
            let lowered = word.to_lowercase();
            let is_bytes_prefix = matches!(lowered.as_str(), "r" | "b" | "rb" | "br");

            if is_bytes_prefix && j < size && (sql_chars[j] == '\'' || sql_chars[j] == '"') {
                let (end, quote, body) = string_end(&sql_chars, j);
                let prefix_len = word.chars().count();
                // A raw string is copied as written; a bytes literal is decoded.
                let value = if invalid(&quote, body.as_deref()) || lowered == "r" {
                    None
                } else {
                    body.as_deref()
                        .and_then(|b| decode_bytes(b, prefix_len == 2))
                };
                match value {
                    Some(bytes) => out.push_str(&bytes_literal(&bytes)),
                    None => out.extend(&sql_chars[i..end]),
                }
                i = end;
            } else {
                out.push_str(&word);
                i = j;
            }
        } else if ch == '\'' || ch == '"' {
            let (end, quote, body) = string_end(&sql_chars, i);
            let value = if invalid(&quote, body.as_deref()) {
                None
            } else {
                body.as_deref().and_then(decode)
            };
            match value {
                Some(value) => {
                    out.push('\'');
                    for c in value.chars() {
                        match canonical_escape(c) {
                            Some(escape) => out.push_str(escape),
                            None => out.push(c),
                        }
                    }
                    out.push('\'');
                }
                None => out.extend(&sql_chars[i..end]),
            }
            i = end;
        } else {
            out.push(ch);
            i += 1;
        }
    }

    out
}
