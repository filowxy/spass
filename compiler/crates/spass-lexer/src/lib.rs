//! Defines the Spass lexer's tokens and source spans.

pub mod lexer;
pub mod span;
pub mod token;

pub use lexer::{lex, LexError};
pub use span::Span;
pub use token::{Token, TokenKind};
