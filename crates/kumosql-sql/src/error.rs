//! Typed parse and modelling errors.
//!
//! Ported from `UnmodeledConstruct` and `LossySql` in `ast_utils.py`, plus the
//! `ParseError` handling in `parse_check.py`.
//!
//! The distinction between these errors matters more than it might look. The
//! project's rule is that a rewrite is either proven equivalent or reported as
//! unproven, and a query the tool cannot model must be *reported*, never
//! approximated. So every one of these carries enough information for a caller
//! to say precisely why a query was declined:
//!
//! * [`Error::Unmodeled`] -- the parse succeeded, but a construct KumoSQL does
//!   not model is present. The query is well-formed; it is just outside the
//!   proven subset.
//! * [`Error::Lossy`] -- rendering the tree back to SQL would lose or change
//!   something, so the original text must be kept instead.
//! * [`Error::UnsupportedSyntax`] -- the text is not SQL KumoSQL can read at
//!   all.
//! * [`Error::Rejected`] -- the text is deliberately refused: it is valid-ish
//!   but KumoSQL declines it, and saying so is better than guessing.
//!
//! Every variant records the byte offset where the problem was found, so a
//! caller can point at it. `sqlglot` records the same information in its
//! `meta` positions; the Rust port keeps it in the error instead of threaded
//! through every node.

use std::fmt;

/// Why a query could not be read, or could not be modelled.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ErrorKind {
    /// A construct KumoSQL does not model, in an otherwise valid parse.
    ///
    /// The query is fine; KumoSQL declines to reason about it. Corresponds to
    /// `UnmodeledConstruct`.
    Unmodeled,
    /// The tree does not render back to the same SQL.
    ///
    /// Rendering would change the meaning, so the original text must be kept
    /// and the tree must not be trusted for a rewrite. Corresponds to
    /// `LossySql`.
    Lossy,
    /// The text is not SQL KumoSQL can parse.
    UnsupportedSyntax,
    /// The text is deliberately refused.
    ///
    /// Used where guessing would be wrong: `STRUCT<>()`, which a generic
    /// reader takes for the comparison `STRUCT <> ()`, for example.
    Rejected,
}

impl fmt::Display for ErrorKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let text = match self {
            ErrorKind::Unmodeled => "unmodeled construct",
            ErrorKind::Lossy => "lossy rendering",
            ErrorKind::UnsupportedSyntax => "unsupported syntax",
            ErrorKind::Rejected => "rejected",
        };
        f.write_str(text)
    }
}

/// A parse or modelling failure, with the reason and where it happened.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Error {
    /// Why the query could not be read.
    pub kind: ErrorKind,
    /// Human-readable detail. Never contains SQL from the reference
    /// repository, only from the query being parsed.
    pub detail: String,
    /// Byte offset into the source where the problem was found, when known.
    pub offset: Option<usize>,
}

impl Error {
    /// A failure at a known offset.
    pub fn at(kind: ErrorKind, offset: usize, detail: impl Into<String>) -> Self {
        Error {
            kind,
            detail: detail.into(),
            offset: Some(offset),
        }
    }

    /// A failure whose position is unknown.
    pub fn new(kind: ErrorKind, detail: impl Into<String>) -> Self {
        Error {
            kind,
            detail: detail.into(),
            offset: None,
        }
    }

    /// An unmodelled construct at a known offset.
    pub fn unmodeled(offset: usize, detail: impl Into<String>) -> Self {
        Error::at(ErrorKind::Unmodeled, offset, detail)
    }

    /// A lossy rendering at a known offset.
    pub fn lossy(offset: usize, detail: impl Into<String>) -> Self {
        Error::at(ErrorKind::Lossy, offset, detail)
    }

    /// Text that is not SQL KumoSQL can read.
    pub fn unsupported(detail: impl Into<String>) -> Self {
        Error::new(ErrorKind::UnsupportedSyntax, detail)
    }

    /// Text KumoSQL deliberately refuses.
    pub fn rejected(offset: usize, detail: impl Into<String>) -> Self {
        Error::at(ErrorKind::Rejected, offset, detail)
    }

    /// Whether this query was refused rather than merely unmodelled.
    ///
    /// Callers use this to separate "we cannot reason about this" from "this
    /// text is not a query", which are different claims.
    pub fn is_refusal(&self) -> bool {
        matches!(
            self.kind,
            ErrorKind::Rejected | ErrorKind::UnsupportedSyntax
        )
    }
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}: {}", self.kind, self.detail)?;
        if let Some(offset) = self.offset {
            write!(f, " (at byte {offset})")?;
        }
        Ok(())
    }
}

impl std::error::Error for Error {}

/// A parse result: either statements, or the reason there are none.
pub type Result<T> = std::result::Result<T, Error>;
