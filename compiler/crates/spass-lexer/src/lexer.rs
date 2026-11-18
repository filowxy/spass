//! Scans Spass source into tokens.

use super::span::Span;
use super::token::{Token, TokenKind};

#[derive(Debug, Clone, thiserror::Error, PartialEq)]
pub enum LexError {
    #[error("lex error at {span}: {message}")]
    Error { message: String, span: Span },
}

impl LexError {
    pub fn span(&self) -> Span {
        match self {
            LexError::Error { span, .. } => span.clone(),
        }
    }
}

/// Scan a Spass source string into a token stream. The final token is
/// always `Eof` with the source-file span.
///
/// Comments start with `#` and run to end-of-line (per the language, which does
/// not document `//`). Whitespace and comments are silently consumed.
pub fn lex(source: &str, file: &str) -> Result<Vec<Token>, LexError> {
    let mut l = Lexer::new(source, file);
    let mut out = Vec::new();
    loop {
        l.skip_trivia()?;
        let start = l.pos();
        let kind = l.next_token()?;
        let end = l.pos();
        let span = Span::new(file, start, end, l.line, l.col);
        let is_eof = matches!(kind, TokenKind::Eof);
        out.push(Token { kind, span });
        if is_eof {
            return Ok(out);
        }
    }
}

struct Lexer<'a> {
    src: &'a [u8],
    pos: usize,
    line: u32,
    col: u32,
    file: String,
}

impl<'a> Lexer<'a> {
    fn new(src: &'a str, file: &str) -> Self {
        // Windows editors write a UTF-8 byte order mark by default, and a
        // source file that starts with one is still a source file. The
        // mark is not part of the program, so it is skipped before the
        // first token; the column stays at 1 because the mark is not a
        // visible character.
        let pos = if src.as_bytes().starts_with(&[0xEF, 0xBB, 0xBF]) {
            3
        } else {
            0
        };
        Self {
            src: src.as_bytes(),
            pos,
            line: 1,
            col: 1,
            file: file.to_string(),
        }
    }

    fn pos(&self) -> usize {
        self.pos
    }

    fn peek_byte(&self) -> Option<u8> {
        self.src.get(self.pos).copied()
    }

    fn peek2(&self) -> Option<u8> {
        self.src.get(self.pos + 1).copied()
    }

    fn bump(&mut self) -> Option<u8> {
        let b = self.peek_byte()?;
        self.pos += 1;
        if b == b'\n' {
            self.line += 1;
            self.col = 1;
        } else {
            self.col += 1;
        }
        Some(b)
    }

    fn err(&self, message: &str) -> LexError {
        let span = Span::new(&self.file, self.pos, self.pos, self.line, self.col);
        LexError::Error { message: message.to_string(), span }
    }

    fn skip_trivia(&mut self) -> Result<(), LexError> {
        loop {
            match self.peek_byte() {
                Some(b) if b.is_ascii_whitespace() => {
                    self.bump();
                }
                Some(b'#') => {
                    while let Some(c) = self.peek_byte() {
                        if c == b'\n' {
                            break;
                        }
                        self.bump();
                    }
                }
                _ => return Ok(()),
            }
        }
    }

    fn next_token(&mut self) -> Result<TokenKind, LexError> {
        let Some(b) = self.peek_byte() else {
            return Ok(TokenKind::Eof);
        };
        match b {
            b'(' => { self.bump(); Ok(TokenKind::LParen) }
            b')' => { self.bump(); Ok(TokenKind::RParen) }
            b'{' => { self.bump(); Ok(TokenKind::LBrace) }
            b'}' => { self.bump(); Ok(TokenKind::RBrace) }
            b'[' => { self.bump(); Ok(TokenKind::LBracket) }
            b']' => { self.bump(); Ok(TokenKind::RBracket) }
            b',' => { self.bump(); Ok(TokenKind::Comma) }
            b';' => { self.bump(); Ok(TokenKind::Semi) }
            b':' => { self.bump(); Ok(TokenKind::Colon) }
            b'@' => { self.bump(); Ok(TokenKind::At) }
            b'+' => { self.bump(); Ok(TokenKind::Plus) }
            b'*' => {
                self.bump();
                if self.peek_byte() == Some(b'*') {
                    self.bump();
                    Ok(TokenKind::StarStar)
                } else {
                    Ok(TokenKind::Star)
                }
            }
            b'/' => { self.bump(); Ok(TokenKind::Slash) }
            b'.' => {
                self.bump();
                if self.peek_byte() == Some(b'.') {
                    self.bump();
                    Ok(TokenKind::DotDot)
                } else {
                    Ok(TokenKind::Dot)
                }
            }
            b'=' => {
                self.bump();
                if self.peek_byte() == Some(b'=') {
                    self.bump();
                    Ok(TokenKind::EqEq)
                } else {
                    Ok(TokenKind::Eq)
                }
            }
            b'!' => {
                self.bump();
                if self.peek_byte() == Some(b'=') {
                    self.bump();
                    Ok(TokenKind::NotEq)
                } else {
                    Ok(TokenKind::Bang)
                }
            }
            b'<' => {
                self.bump();
                if self.peek_byte() == Some(b'=') {
                    self.bump();
                    Ok(TokenKind::Le)
                } else {
                    Ok(TokenKind::Lt)
                }
            }
            b'>' => {
                self.bump();
                if self.peek_byte() == Some(b'=') {
                    self.bump();
                    Ok(TokenKind::Ge)
                } else {
                    Ok(TokenKind::Gt)
                }
            }
            b'&' => {
                self.bump();
                if self.peek_byte() == Some(b'&') {
                    self.bump();
                    Ok(TokenKind::AmpAmp)
                } else {
                    Err(self.err("expected `&&`, found single `&`"))
                }
            }
            b'|' => {
                self.bump();
                if self.peek_byte() == Some(b'|') {
                    self.bump();
                    Ok(TokenKind::PipePipe)
                } else {
                    Err(self.err("expected `||`, found single `|`"))
                }
            }
            b'-' => {
                self.bump();
                if self.peek_byte() == Some(b'>') {
                    self.bump();
                    Ok(TokenKind::Arrow)
                } else {
                    Ok(TokenKind::Minus)
                }
            }
            b'"' => self.lex_string(),
            c if c.is_ascii_digit() => self.lex_number(),
            c if is_ident_start(c) => self.lex_ident(),
            other => Err(self.err(&format!("unexpected character `{}`", other as char))),
        }
    }

    fn lex_string(&mut self) -> Result<TokenKind, LexError> {
        self.bump(); // opening quote
        let mut s = String::new();
        loop {
            match self.peek_byte() {
                Some(b'"') => {
                    self.bump();
                    return Ok(TokenKind::StringLit(s));
                }
                Some(b'\\') => {
                    self.bump();
                    match self.peek_byte() {
                        Some(b'n') => { self.bump(); s.push('\n'); }
                        Some(b't') => { self.bump(); s.push('\t'); }
                        Some(b'\\') => { self.bump(); s.push('\\'); }
                        Some(b'"') => { self.bump(); s.push('"'); }
                        Some(other) => return Err(self.err(&format!("unknown escape `\\{}`", other as char))),
                        None => return Err(self.err("unterminated string literal")),
                    }
                }
                Some(c) => {
                    self.bump();
                    s.push(c as char);
                }
                None => return Err(self.err("unterminated string literal")),
            }
        }
    }

    fn lex_number(&mut self) -> Result<TokenKind, LexError> {
        let start = self.pos;
        while let Some(c) = self.peek_byte() {
            if c.is_ascii_digit() {
                self.bump();
            } else {
                break;
            }
        }
        let mut is_float = false;
        if self.peek_byte() == Some(b'.') && self.peek2() != Some(b'.') {
            is_float = true;
            self.bump(); // consume '.'
            while let Some(c) = self.peek_byte() {
                if c.is_ascii_digit() {
                    self.bump();
                } else {
                    break;
                }
            }
        }
        if matches!(self.peek_byte(), Some(b'e') | Some(b'E')) {
            is_float = true;
            self.bump();
            if matches!(self.peek_byte(), Some(b'+') | Some(b'-')) {
                self.bump();
            }
            while let Some(c) = self.peek_byte() {
                if c.is_ascii_digit() {
                    self.bump();
                } else {
                    break;
                }
            }
        }
        let lexeme = std::str::from_utf8(&self.src[start..self.pos])
            .map_err(|_| self.err("invalid UTF-8 in numeric literal"))?
            .to_string();
        if is_float {
            Ok(TokenKind::FloatLit(lexeme))
        } else {
            let n: i64 = lexeme.parse().map_err(|_| self.err("integer overflow"))?;
            Ok(TokenKind::IntLit(n))
        }
    }

    fn lex_ident(&mut self) -> Result<TokenKind, LexError> {
        let start = self.pos;
        while let Some(c) = self.peek_byte() {
            if is_ident_continue(c) {
                self.bump();
            } else {
                break;
            }
        }
        let name = std::str::from_utf8(&self.src[start..self.pos])
            .map_err(|_| self.err("invalid UTF-8 in identifier"))?
            .to_string();
        Ok(TokenKind::keyword_from(&name).unwrap_or(TokenKind::Ident(name)))
    }
}

fn is_ident_start(c: u8) -> bool {
    c == b'_' || c.is_ascii_alphabetic()
}

fn is_ident_continue(c: u8) -> bool {
    c == b'_' || c.is_ascii_alphanumeric()
}
