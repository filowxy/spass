//! Defines parser errors with source spans.

use spass_lexer::Span;

#[derive(Debug, Clone, thiserror::Error, PartialEq)]
pub enum ParseError {
    #[error("E006: parse error at {span}: {message}")]
    Syntax { message: String, span: Span },

    #[error("E001: value-domain declaration invalid at {span}: {message}")]
    InvalidRange { message: String, span: Span },

    #[error("E003: cyclic include detected through `{path}` (at {span})")]
    CyclicInclude { path: String, span: Span },

    #[error("E005: undefined identifier `{name}` at {span}")]
    UndefinedIdent { name: String, span: Span },

    #[error("E007: undefined or duplicate checkpoint `{name}` at {span}")]
    BadCheckpoint { name: String, span: Span },

    #[error("E012: duplicate function name `{name}` at {span}")]
    DuplicateFunction { name: String, span: Span },

    #[error("E015: duplicate @tag key `{key}` at {span}")]
    DuplicateTagKey { key: String, span: Span },

    #[error("internal parser invariant violated: {message}")]
    Internal { message: String },
}

impl ParseError {
    pub fn span(&self) -> Span {
        match self {
            ParseError::Syntax { span, .. }
            | ParseError::InvalidRange { span, .. }
            | ParseError::CyclicInclude { span, .. }
            | ParseError::UndefinedIdent { span, .. }
            | ParseError::BadCheckpoint { span, .. }
            | ParseError::DuplicateFunction { span, .. }
            | ParseError::DuplicateTagKey { span, .. } => span.clone(),
            ParseError::Internal { .. } => Span::new("<internal>", 0, 0, 0, 0),
        }
    }
}
