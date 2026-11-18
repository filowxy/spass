//! Defines token kinds and the stream produced by the lexer.

use super::span::Span;

#[derive(Debug, Clone, PartialEq)]
pub struct Token {
    pub kind: TokenKind,
    pub span: Span,
}

#[derive(Debug, Clone, PartialEq)]
pub enum TokenKind {
    // ---------- literals ----------
    IntLit(i64),
    FloatLit(String), // canonical lexeme; parser converts via Rational64
    StringLit(String),
    Ident(String),

    // ---------- keywords ----------
    KwIf, KwElse, KwFor, KwIn, KwWhile, KwBreak, KwContinue, KwStep,
    KwFunc, KwReturn, KwLet, KwStruct, KwOut, KwTrue, KwFalse,
    KwFloat, KwInt, KwUint, KwBool, KwString, KwList,
    KwVec2, KwVec3, KwVec4, KwMat3, KwMat4,
    KwTest, KwWith, KwAssert, KwApprox,

    // ---------- punctuation ----------
    LParen, RParen, LBrace, RBrace, LBracket, RBracket,
    Comma, Semi, Colon, Dot, DotDot, Arrow,
    Eq, // plain `=` (assignment, NOT `==`)
    At, // `@` for annotations (gpu, tag, checkpoint, etc.)

    // ---------- operators ----------
    Plus, Minus, Star, Slash, StarStar,
    EqEq, NotEq, Lt, Le, Gt, Ge,
    AmpAmp, PipePipe, Bang,

    // ---------- end of file ----------
    Eof,
}

impl TokenKind {
    /// Returns the keyword token for a given identifier, or `None` if the
    /// identifier is not a reserved word.
    ///
    /// Per, the reserved keyword set is large. For the MVP we
    /// only need the subset that participates in parsing of the implemented
    /// feature set. Unrecognised identifiers (including unrecognised
    /// reserved keywords) remain as `Ident`, so source code referencing
    /// not-yet-wired features still parses.
    pub fn keyword_from(ident: &str) -> Option<TokenKind> {
        Some(match ident {
            "if" => TokenKind::KwIf,
            "else" => TokenKind::KwElse,
            "for" => TokenKind::KwFor,
            "in" => TokenKind::KwIn,
            "while" => TokenKind::KwWhile,
            "break" => TokenKind::KwBreak,
            "continue" => TokenKind::KwContinue,
            "step" => TokenKind::KwStep,
            "func" => TokenKind::KwFunc,
            "return" => TokenKind::KwReturn,
            "let" => TokenKind::KwLet,
            "struct" => TokenKind::KwStruct,
            "out" => TokenKind::KwOut,
            "true" => TokenKind::KwTrue,
            "false" => TokenKind::KwFalse,
            "float" => TokenKind::KwFloat,
            "int" => TokenKind::KwInt,
            "uint" => TokenKind::KwUint,
            "bool" => TokenKind::KwBool,
            "string" => TokenKind::KwString,
            "list" => TokenKind::KwList,
            "vec2" => TokenKind::KwVec2,
            "vec3" => TokenKind::KwVec3,
            "vec4" => TokenKind::KwVec4,
            "mat3" => TokenKind::KwMat3,
            "mat4" => TokenKind::KwMat4,
            "test" => TokenKind::KwTest,
            "with" => TokenKind::KwWith,
            "assert" => TokenKind::KwAssert,
            "approx" => TokenKind::KwApprox,
            _ => return None,
        })
    }
}
