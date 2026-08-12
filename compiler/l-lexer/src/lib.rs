//! The L lexer.
//!
//! Turns UTF-8 source text (SPEC §5) into a token stream. The lexer never
//! aborts: on malformed input it emits a diagnostic, skips the offending text,
//! and continues, so that a single typo does not hide the rest of the file.

pub mod token;

pub use token::{is_reserved, Keyword, NumBase, StrPart, Token, TokenKind, RESERVED};

use l_span::{BytePos, DiagCode, Diagnostic, Diagnostics, FileId, Span};

// Lexer diagnostic codes. E0xxx is reserved for lexical errors.
const E_UNKNOWN_CHAR: DiagCode = DiagCode("E0001");
const E_UNTERMINATED_STR: DiagCode = DiagCode("E0002");
const E_UNTERMINATED_CHAR: DiagCode = DiagCode("E0003");
const E_UNTERMINATED_COMMENT: DiagCode = DiagCode("E0004");
const E_BAD_ESCAPE: DiagCode = DiagCode("E0005");
const E_BAD_NUMBER: DiagCode = DiagCode("E0006");
const E_INT_OVERFLOW: DiagCode = DiagCode("E0007");
const E_EMPTY_CHAR: DiagCode = DiagCode("E0008");
const E_MULTI_CHAR: DiagCode = DiagCode("E0009");
const E_RESERVED_WORD: DiagCode = DiagCode("E0010");
const E_BAD_INTERPOLATION: DiagCode = DiagCode("E0011");
const E_USE_ASSIGN: DiagCode = DiagCode("E0012");

/// The result of lexing one file.
pub struct Lexed {
    pub tokens: Vec<Token>,
    pub diagnostics: Diagnostics,
}

/// Lex a source file into tokens.
pub fn lex(file: FileId, src: &str) -> Lexed {
    let mut lexer = Lexer::new(file, src);
    lexer.run();
    Lexed { tokens: lexer.tokens, diagnostics: lexer.diagnostics }
}

struct Lexer<'a> {
    file: FileId,
    src: &'a str,
    /// Byte offset of the next character to read.
    pos: usize,
    tokens: Vec<Token>,
    diagnostics: Diagnostics,
}

impl<'a> Lexer<'a> {
    fn new(file: FileId, src: &'a str) -> Self {
        Lexer { file, src, pos: 0, tokens: Vec::new(), diagnostics: Diagnostics::new() }
    }

    // ---- character access ----

    fn peek(&self) -> Option<char> {
        self.src[self.pos..].chars().next()
    }

    fn peek_at(&self, n: usize) -> Option<char> {
        self.src[self.pos..].chars().nth(n)
    }

    fn bump(&mut self) -> Option<char> {
        let c = self.peek()?;
        self.pos += c.len_utf8();
        Some(c)
    }

    /// Consume `c` if it is next.
    fn eat(&mut self, c: char) -> bool {
        if self.peek() == Some(c) {
            self.pos += c.len_utf8();
            true
        } else {
            false
        }
    }

    fn at_end(&self) -> bool {
        self.pos >= self.src.len()
    }

    fn span(&self, lo: usize) -> Span {
        Span::new(self.file, lo as BytePos, self.pos as BytePos)
    }

    fn span_at(&self, lo: usize, hi: usize) -> Span {
        Span::new(self.file, lo as BytePos, hi as BytePos)
    }

    fn push(&mut self, kind: TokenKind, lo: usize) {
        let span = self.span(lo);
        self.tokens.push(Token::new(kind, span));
    }

    fn error(&mut self, diag: Diagnostic) {
        self.diagnostics.push(diag);
    }

    // ---- main loop ----

    fn run(&mut self) {
        loop {
            self.skip_trivia();
            if self.at_end() {
                break;
            }
            self.lex_token();
        }
        let end = self.src.len();
        self.tokens.push(Token::new(TokenKind::Eof, self.span_at(end, end)));
    }

    /// Skip whitespace and ordinary comments (SPEC §6).
    ///
    /// Doc comments are tokens, not trivia, so this stops in front of one.
    fn skip_trivia(&mut self) {
        loop {
            match self.peek() {
                Some(c) if c.is_whitespace() => {
                    self.bump();
                }
                Some('/') if self.peek_at(1) == Some('/') => {
                    // `///` is a doc comment, which is a real token.
                    if self.peek_at(2) == Some('/') && self.peek_at(3) != Some('/') {
                        return;
                    }
                    while let Some(c) = self.peek() {
                        if c == '\n' {
                            break;
                        }
                        self.bump();
                    }
                }
                Some('/') if self.peek_at(1) == Some('*') => {
                    self.skip_block_comment();
                }
                _ => return,
            }
        }
    }

    /// Block comments nest, so that commenting out a region containing a
    /// comment behaves as expected.
    fn skip_block_comment(&mut self) {
        let lo = self.pos;
        self.bump(); // '/'
        self.bump(); // '*'
        let mut depth = 1usize;
        while depth > 0 {
            match self.bump() {
                None => {
                    self.error(
                        Diagnostic::error(E_UNTERMINATED_COMMENT, "unterminated block comment")
                            .with_primary(self.span_at(lo, lo + 2), "this comment is never closed")
                            .with_note("block comments in L nest and must be closed with `*/`"),
                    );
                    return;
                }
                Some('*') if self.peek() == Some('/') => {
                    self.bump();
                    depth -= 1;
                }
                Some('/') if self.peek() == Some('*') => {
                    self.bump();
                    depth += 1;
                }
                _ => {}
            }
        }
    }

    fn lex_token(&mut self) {
        let lo = self.pos;
        let c = match self.peek() {
            Some(c) => c,
            None => return,
        };

        // Doc comment (§6).
        if c == '/' && self.peek_at(1) == Some('/') && self.peek_at(2) == Some('/') {
            self.pos += 3;
            let start = self.pos;
            while let Some(c) = self.peek() {
                if c == '\n' {
                    break;
                }
                self.bump();
            }
            let text = self.src[start..self.pos].trim().to_string();
            self.push(TokenKind::DocComment(text), lo);
            return;
        }

        if is_ident_start(c) {
            return self.lex_ident();
        }
        if c.is_ascii_digit() {
            return self.lex_number();
        }
        if c == '"' {
            return self.lex_string();
        }
        if c == '\'' {
            return self.lex_char();
        }

        self.lex_punct()
    }

    // ---- identifiers and keywords ----

    fn lex_ident(&mut self) {
        let lo = self.pos;
        while let Some(c) = self.peek() {
            if is_ident_continue(c) {
                self.bump();
            } else {
                break;
            }
        }
        let text = &self.src[lo..self.pos];

        if text == "_" {
            return self.push(TokenKind::Underscore, lo);
        }

        if let Some(kw) = Keyword::from_str(text) {
            return self.push(TokenKind::Keyword(kw), lo);
        }

        if is_reserved(text) {
            let text = text.to_string();
            let span = self.span(lo);
            self.error(
                Diagnostic::error(E_RESERVED_WORD, format!("`{text}` is a reserved word"))
                    .with_primary(span, "reserved for a future version of L")
                    .with_note("reserved words may not be used as identifiers")
                    .with_suggestion("rename the identifier", span, format!("{text}_")),
            );
            // Recover by treating it as an ordinary identifier.
            return self.push(TokenKind::Ident(text), lo);
        }

        let text = text.to_string();
        self.push(TokenKind::Ident(text), lo);
    }

    // ---- numbers ----

    fn lex_number(&mut self) {
        let lo = self.pos;

        // Base prefix.
        let base = if self.peek() == Some('0') {
            match self.peek_at(1) {
                Some('x') | Some('X') => {
                    self.pos += 2;
                    NumBase::Hex
                }
                Some('o') | Some('O') => {
                    self.pos += 2;
                    NumBase::Octal
                }
                Some('b') | Some('B') => {
                    self.pos += 2;
                    NumBase::Binary
                }
                _ => NumBase::Decimal,
            }
        } else {
            NumBase::Decimal
        };

        let digits_start = self.pos;
        let mut digits = String::new();
        while let Some(c) = self.peek() {
            if c == '_' {
                self.bump();
            } else if c.is_digit(base.radix()) {
                digits.push(c);
                self.bump();
            } else if c.is_ascii_alphanumeric() && base != NumBase::Decimal {
                // A digit invalid for this base, e.g. `0b102`.
                break;
            } else {
                break;
            }
        }

        // Float: only in base 10, and only when the dot is followed by a digit
        // so that `x.0` tuple access and `0..10` ranges still lex correctly.
        let mut is_float = false;
        if base == NumBase::Decimal
            && self.peek() == Some('.')
            && self.peek_at(1).is_some_and(|c| c.is_ascii_digit())
        {
            is_float = true;
            digits.push('.');
            self.bump();
            while let Some(c) = self.peek() {
                if c == '_' {
                    self.bump();
                } else if c.is_ascii_digit() {
                    digits.push(c);
                    self.bump();
                } else {
                    break;
                }
            }
        }

        // Exponent.
        if base == NumBase::Decimal && matches!(self.peek(), Some('e') | Some('E')) {
            let after = self.peek_at(1);
            let exp_digit = match after {
                Some('+') | Some('-') => self.peek_at(2).is_some_and(|c| c.is_ascii_digit()),
                Some(c) => c.is_ascii_digit(),
                None => false,
            };
            if exp_digit {
                is_float = true;
                digits.push('e');
                self.bump();
                if let Some(sign @ ('+' | '-')) = self.peek() {
                    digits.push(sign);
                    self.bump();
                }
                while let Some(c) = self.peek() {
                    if c == '_' {
                        self.bump();
                    } else if c.is_ascii_digit() {
                        digits.push(c);
                        self.bump();
                    } else {
                        break;
                    }
                }
            }
        }

        // Type suffix, e.g. `10i64`, `2.5f32`, `7u8`.
        let suffix_start = self.pos;
        while let Some(c) = self.peek() {
            if is_ident_continue(c) {
                self.bump();
            } else {
                break;
            }
        }
        let suffix = if self.pos > suffix_start {
            Some(self.src[suffix_start..self.pos].to_string())
        } else {
            None
        };

        if digits.is_empty() || digits == "." {
            let span = self.span(lo);
            let text = self.src[lo..self.pos].to_string();
            self.error(
                Diagnostic::error(E_BAD_NUMBER, format!("invalid numeric literal `{text}`"))
                    .with_primary(span, "no digits after the base prefix"),
            );
            return self.push(TokenKind::Int { value: 0, base, suffix }, lo);
        }

        if let Some(sfx) = &suffix {
            if !is_valid_num_suffix(sfx) {
                let span = self.span_at(suffix_start, self.pos);
                let sfx = sfx.clone();
                self.error(
                    Diagnostic::error(E_BAD_NUMBER, format!("invalid numeric suffix `{sfx}`"))
                        .with_primary(span, "not a numeric type")
                        .with_note(
                            "valid suffixes are the integer and float types of SPEC §9, \
                             e.g. `i32`, `u64`, `f32`",
                        ),
                );
            }
        }

        if is_float {
            let value: f64 = digits.parse().unwrap_or(0.0);
            self.push(TokenKind::Float { value, suffix }, lo);
        } else {
            match u128::from_str_radix(&digits, base.radix()) {
                Ok(value) => self.push(TokenKind::Int { value, base, suffix }, lo),
                Err(_) => {
                    let span = self.span(lo);
                    let text = self.src[digits_start..self.pos].to_string();
                    self.error(
                        Diagnostic::error(
                            E_INT_OVERFLOW,
                            format!("integer literal `{text}` is too large"),
                        )
                        .with_primary(span, "does not fit in any L integer type")
                        .with_note("the widest integer types are `int128` and `uint128`"),
                    );
                    self.push(TokenKind::Int { value: 0, base, suffix }, lo);
                }
            }
        }
    }

    // ---- strings (§11) ----

    fn lex_string(&mut self) {
        let lo = self.pos;
        self.bump(); // opening quote

        let mut parts: Vec<StrPart> = Vec::new();
        let mut current = String::new();

        loop {
            let c = match self.peek() {
                None => {
                    self.error(
                        Diagnostic::error(E_UNTERMINATED_STR, "unterminated string literal")
                            .with_primary(self.span_at(lo, lo + 1), "this string is never closed")
                            .with_note("string literals cannot span multiple lines"),
                    );
                    break;
                }
                Some('\n') => {
                    self.error(
                        Diagnostic::error(E_UNTERMINATED_STR, "unterminated string literal")
                            .with_primary(self.span_at(lo, self.pos), "newline inside string")
                            .with_note("use `\\n` to write a newline in a string literal"),
                    );
                    break;
                }
                Some(c) => c,
            };

            if c == '"' {
                self.bump();
                break;
            }

            if c == '\\' {
                self.bump();
                match self.lex_escape() {
                    Some(ch) => current.push(ch),
                    None => continue,
                }
                continue;
            }

            if c == '$' {
                let dollar = self.pos;
                self.bump();
                match self.peek() {
                    // `${expr}`
                    Some('{') => {
                        if !current.is_empty() {
                            parts.push(StrPart::Literal(std::mem::take(&mut current)));
                        }
                        self.bump(); // '{'
                        let inner_start = self.pos;
                        let mut depth = 1usize;
                        while depth > 0 {
                            match self.peek() {
                                None | Some('\n') => {
                                    self.error(
                                        Diagnostic::error(
                                            E_BAD_INTERPOLATION,
                                            "unterminated string interpolation",
                                        )
                                        .with_primary(
                                            self.span_at(dollar, self.pos),
                                            "expected `}` to close `${`",
                                        ),
                                    );
                                    break;
                                }
                                Some('{') => {
                                    depth += 1;
                                    self.bump();
                                }
                                Some('}') => {
                                    depth -= 1;
                                    if depth == 0 {
                                        break;
                                    }
                                    self.bump();
                                }
                                _ => {
                                    self.bump();
                                }
                            }
                        }
                        let inner_end = self.pos;
                        self.eat('}');
                        let inner = &self.src[inner_start..inner_end];
                        // Interpolated expressions are lexed recursively; their
                        // spans stay correct because the sub-lexer is offset.
                        let sub = lex_offset(self.file, inner, inner_start);
                        self.diagnostics.extend(sub.diagnostics);
                        parts.push(StrPart::Expr {
                            tokens: sub.tokens,
                            span: self.span_at(dollar, self.pos),
                        });
                    }
                    // `$name`
                    Some(c) if is_ident_start(c) => {
                        if !current.is_empty() {
                            parts.push(StrPart::Literal(std::mem::take(&mut current)));
                        }
                        let name_start = self.pos;
                        while let Some(c) = self.peek() {
                            if is_ident_continue(c) {
                                self.bump();
                            } else {
                                break;
                            }
                        }
                        // `$self.name` and `$user.name` read as field access in
                        // the spec's examples (§27, §85), so trailing `.field`
                        // segments belong to the interpolation.
                        while self.peek() == Some('.')
                            && self.peek_at(1).is_some_and(is_ident_start)
                        {
                            self.bump();
                            while let Some(c) = self.peek() {
                                if is_ident_continue(c) {
                                    self.bump();
                                } else {
                                    break;
                                }
                            }
                        }
                        let name = self.src[name_start..self.pos].to_string();
                        parts.push(StrPart::Ident {
                            name,
                            span: self.span_at(name_start, self.pos),
                        });
                    }
                    // A lone `$` is literal text.
                    _ => current.push('$'),
                }
                continue;
            }

            current.push(c);
            self.bump();
        }

        if !current.is_empty() || parts.is_empty() {
            parts.push(StrPart::Literal(current));
        }

        self.push(TokenKind::Str(parts), lo);
    }

    /// Lex the body of an escape sequence, the backslash already consumed.
    fn lex_escape(&mut self) -> Option<char> {
        let esc_lo = self.pos - 1;
        let c = self.bump()?;
        Some(match c {
            'n' => '\n',
            'r' => '\r',
            't' => '\t',
            '0' => '\0',
            '\\' => '\\',
            '"' => '"',
            '\'' => '\'',
            // Extensions beyond the spec's minimum list, kept conservative.
            'u' => return self.lex_unicode_escape(esc_lo),
            other => {
                let span = self.span_at(esc_lo, self.pos);
                self.error(
                    Diagnostic::error(E_BAD_ESCAPE, format!("unknown escape sequence `\\{other}`"))
                        .with_primary(span, "not a valid escape")
                        .with_note("valid escapes are `\\n` `\\r` `\\t` `\\\\` `\\\"` `\\'` `\\0` `\\u{...}`")
                        .with_suggestion(
                            "to write a literal backslash",
                            span,
                            format!("\\\\{other}"),
                        ),
                );
                return None;
            }
        })
    }

    /// `\u{1F600}`
    fn lex_unicode_escape(&mut self, esc_lo: usize) -> Option<char> {
        if !self.eat('{') {
            let span = self.span_at(esc_lo, self.pos);
            self.error(
                Diagnostic::error(E_BAD_ESCAPE, "malformed unicode escape")
                    .with_primary(span, "expected `{` after `\\u`")
                    .with_note("unicode escapes are written `\\u{1F600}`"),
            );
            return None;
        }
        let start = self.pos;
        while let Some(c) = self.peek() {
            if c == '}' {
                break;
            }
            if !c.is_ascii_hexdigit() {
                break;
            }
            self.bump();
        }
        let hex = self.src[start..self.pos].to_string();
        if !self.eat('}') {
            let span = self.span_at(esc_lo, self.pos);
            self.error(
                Diagnostic::error(E_BAD_ESCAPE, "malformed unicode escape")
                    .with_primary(span, "expected `}`"),
            );
            return None;
        }
        let code = u32::from_str_radix(&hex, 16).ok();
        match code.and_then(char::from_u32) {
            Some(c) => Some(c),
            None => {
                let span = self.span_at(esc_lo, self.pos);
                self.error(
                    Diagnostic::error(
                        E_BAD_ESCAPE,
                        format!("`{hex}` is not a valid unicode scalar value"),
                    )
                    .with_primary(span, "invalid code point"),
                );
                None
            }
        }
    }

    // ---- characters (§11) ----

    fn lex_char(&mut self) {
        let lo = self.pos;
        self.bump(); // opening quote

        let mut value: Option<char> = None;
        let mut count = 0usize;

        loop {
            match self.peek() {
                None | Some('\n') => {
                    self.error(
                        Diagnostic::error(E_UNTERMINATED_CHAR, "unterminated character literal")
                            .with_primary(self.span_at(lo, self.pos), "expected a closing `'`"),
                    );
                    self.push(TokenKind::Char(value.unwrap_or('\0')), lo);
                    return;
                }
                Some('\'') => {
                    self.bump();
                    break;
                }
                Some('\\') => {
                    self.bump();
                    if let Some(c) = self.lex_escape() {
                        if count == 0 {
                            value = Some(c);
                        }
                        count += 1;
                    }
                }
                Some(c) => {
                    self.bump();
                    if count == 0 {
                        value = Some(c);
                    }
                    count += 1;
                }
            }
        }

        let span = self.span(lo);
        match count {
            0 => {
                self.error(
                    Diagnostic::error(E_EMPTY_CHAR, "empty character literal")
                        .with_primary(span, "a `char` must contain exactly one character")
                        .with_suggestion("for an empty string, write", span, "\"\""),
                );
            }
            1 => {}
            _ => {
                let text = self.src[lo + 1..self.pos - 1].to_string();
                self.error(
                    Diagnostic::error(E_MULTI_CHAR, "character literal contains more than one character")
                        .with_primary(span, "expected exactly one character")
                        .with_suggestion("use a string literal instead", span, format!("\"{text}\"")),
                );
            }
        }

        self.push(TokenKind::Char(value.unwrap_or('\0')), lo);
    }

    // ---- punctuation ----

    fn lex_punct(&mut self) {
        use TokenKind::*;
        let lo = self.pos;
        let c = self.bump().expect("lex_punct called at end of input");

        let kind = match c {
            '(' => LParen,
            ')' => RParen,
            '{' => LBrace,
            '}' => RBrace,
            '[' => LBracket,
            ']' => RBracket,
            ',' => Comma,
            ';' => Semi,
            '@' => At,
            '~' => Tilde,
            '$' => Dollar,

            ':' => {
                if self.eat('=') {
                    Assign
                } else if self.eat(':') {
                    ColonColon
                } else {
                    Colon
                }
            }
            '.' => {
                if self.eat('.') {
                    if self.eat('.') {
                        Ellipsis
                    } else if self.eat('=') {
                        DotDotEq
                    } else {
                        DotDot
                    }
                } else {
                    Dot
                }
            }
            '-' => {
                if self.eat('>') {
                    Arrow
                } else if self.eat('=') {
                    MinusEq
                } else {
                    Minus
                }
            }
            '+' => {
                if self.eat('=') {
                    PlusEq
                } else {
                    Plus
                }
            }
            '*' => {
                if self.eat('=') {
                    StarEq
                } else {
                    Star
                }
            }
            '/' => {
                if self.eat('=') {
                    SlashEq
                } else {
                    Slash
                }
            }
            '%' => {
                if self.eat('=') {
                    PercentEq
                } else {
                    Percent
                }
            }
            '=' => {
                if self.eat('=') {
                    EqEq
                } else if self.eat('>') {
                    FatArrow
                } else {
                    // `=` is never valid in L; point at `:=` (§7).
                    let span = self.span(lo);
                    self.error(
                        Diagnostic::error(E_USE_ASSIGN, "`=` is not an operator in L")
                            .with_primary(span, "expected `:=`")
                            .with_note("L uses `:=` for binding and assignment, and `==` for equality")
                            .with_suggestion("use", span, ":="),
                    );
                    Eq
                }
            }
            '!' => {
                if self.eat('=') {
                    NotEq
                } else {
                    Bang
                }
            }
            '<' => {
                if self.eat('=') {
                    Le
                } else if self.eat('<') {
                    Shl
                } else {
                    Lt
                }
            }
            '>' => {
                if self.eat('=') {
                    Ge
                } else if self.eat('>') {
                    Shr
                } else {
                    Gt
                }
            }
            '&' => {
                if self.eat('&') {
                    AndAnd
                } else {
                    Amp
                }
            }
            '|' => {
                if self.eat('|') {
                    OrOr
                } else {
                    Pipe
                }
            }
            '^' => Caret,
            '?' => {
                if self.eat('.') {
                    QuestionDot
                } else if self.eat('?') {
                    QuestionQuestion
                } else {
                    Question
                }
            }

            other => {
                let span = self.span(lo);
                self.error(
                    Diagnostic::error(
                        E_UNKNOWN_CHAR,
                        format!("unexpected character `{}`", other.escape_debug()),
                    )
                    .with_primary(span, "not valid in L source"),
                );
                return;
            }
        };

        self.push(kind, lo);
    }
}

/// Lex a fragment whose text begins at `offset` within its file, so that spans
/// point at the original source. Used for `${...}` interpolations.
fn lex_offset(file: FileId, src: &str, offset: usize) -> Lexed {
    let mut lexer = Lexer::new(file, src);
    lexer.run();
    for token in &mut lexer.tokens {
        token.span = Span::new(
            file,
            token.span.lo + offset as BytePos,
            token.span.hi + offset as BytePos,
        );
    }
    let diagnostics = lexer.diagnostics;
    Lexed { tokens: lexer.tokens, diagnostics }
}

fn is_ident_start(c: char) -> bool {
    c == '_' || c.is_alphabetic()
}

fn is_ident_continue(c: char) -> bool {
    c == '_' || c.is_alphanumeric()
}

/// The numeric type suffixes of SPEC §9.
fn is_valid_num_suffix(s: &str) -> bool {
    matches!(
        s,
        "i8" | "i16"
            | "i32"
            | "i64"
            | "i128"
            | "u8"
            | "u16"
            | "u32"
            | "u64"
            | "u128"
            | "f32"
            | "f64"
            | "int"
            | "int8"
            | "int16"
            | "int32"
            | "int64"
            | "int128"
            | "uint"
            | "uint8"
            | "uint16"
            | "uint32"
            | "uint64"
            | "uint128"
            | "float"
            | "float32"
            | "float64"
            | "byte"
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn kinds(src: &str) -> Vec<TokenKind> {
        let out = lex(FileId(0), src);
        assert!(
            !out.diagnostics.has_errors(),
            "unexpected errors: {:?}",
            out.diagnostics.iter().map(|d| &d.message).collect::<Vec<_>>()
        );
        out.tokens.into_iter().map(|t| t.kind).collect()
    }

    fn errors(src: &str) -> Vec<String> {
        lex(FileId(0), src).diagnostics.iter().map(|d| d.message.clone()).collect()
    }

    #[test]
    fn lexes_a_variable_declaration() {
        use TokenKind::*;
        assert_eq!(
            kinds("let int age := 20"),
            vec![
                Keyword(super::Keyword::Let),
                Ident("int".into()),
                Ident("age".into()),
                Assign,
                Int { value: 20, base: NumBase::Decimal, suffix: None },
                Eof,
            ]
        );
    }

    #[test]
    fn distinguishes_assign_from_equality() {
        use TokenKind::*;
        assert_eq!(kinds("a := b == c")[1], Assign);
        assert_eq!(kinds("a := b == c")[3], EqEq);
    }

    #[test]
    fn rejects_single_equals_with_a_suggestion() {
        let out = lex(FileId(0), "age = 20");
        assert_eq!(out.diagnostics.len(), 1);
        let diag = out.diagnostics.iter().next().unwrap();
        assert!(diag.message.contains("`=` is not an operator"));
        assert_eq!(diag.suggestions[0].replacement, ":=");
    }

    #[test]
    fn lexes_all_compound_assignments() {
        use TokenKind::*;
        assert_eq!(
            kinds("+= -= *= /= %=")[..5],
            [PlusEq, MinusEq, StarEq, SlashEq, PercentEq]
        );
    }

    #[test]
    fn lexes_ranges_without_swallowing_the_dot() {
        use TokenKind::*;
        // §19: `0..10` and `0..=10`
        assert_eq!(
            kinds("0..10"),
            vec![
                Int { value: 0, base: NumBase::Decimal, suffix: None },
                DotDot,
                Int { value: 10, base: NumBase::Decimal, suffix: None },
                Eof
            ]
        );
        assert_eq!(kinds("0..=10")[1], DotDotEq);
    }

    #[test]
    fn tuple_access_is_not_a_float() {
        use TokenKind::*;
        // §15: `point.0`
        assert_eq!(
            kinds("point.0"),
            vec![
                Ident("point".into()),
                Dot,
                Int { value: 0, base: NumBase::Decimal, suffix: None },
                Eof
            ]
        );
    }

    #[test]
    fn lexes_floats_and_exponents() {
        match &kinds("2.5")[0] {
            TokenKind::Float { value, .. } => assert_eq!(*value, 2.5),
            other => panic!("expected float, got {other:?}"),
        }
        match &kinds("1e10")[0] {
            TokenKind::Float { value, .. } => assert_eq!(*value, 1e10),
            other => panic!("expected float, got {other:?}"),
        }
        match &kinds("1.5e-3")[0] {
            TokenKind::Float { value, .. } => assert_eq!(*value, 1.5e-3),
            other => panic!("expected float, got {other:?}"),
        }
    }

    #[test]
    fn lexes_numeric_bases_and_separators() {
        use TokenKind::*;
        assert_eq!(kinds("0xFF")[0], Int { value: 255, base: NumBase::Hex, suffix: None });
        assert_eq!(kinds("0b1010")[0], Int { value: 10, base: NumBase::Binary, suffix: None });
        assert_eq!(kinds("0o17")[0], Int { value: 15, base: NumBase::Octal, suffix: None });
        assert_eq!(
            kinds("1_000_000")[0],
            Int { value: 1_000_000, base: NumBase::Decimal, suffix: None }
        );
    }

    #[test]
    fn lexes_numeric_suffixes() {
        use TokenKind::*;
        assert_eq!(
            kinds("10i64")[0],
            Int { value: 10, base: NumBase::Decimal, suffix: Some("i64".into()) }
        );
        assert!(errors("10q9")[0].contains("invalid numeric suffix"));
    }

    #[test]
    fn reports_integer_overflow() {
        let msgs = errors("999999999999999999999999999999999999999999");
        assert!(msgs[0].contains("too large"), "{msgs:?}");
    }

    #[test]
    fn lexes_plain_strings_with_escapes() {
        match &kinds(r#""a\nb\t\\ \" \0""#)[0] {
            TokenKind::Str(parts) => {
                assert_eq!(parts, &vec![StrPart::Literal("a\nb\t\\ \" \0".into())]);
            }
            other => panic!("expected string, got {other:?}"),
        }
    }

    #[test]
    fn lexes_ident_interpolation() {
        // §11: print("Hello, $name")
        match &kinds(r#""Hello, $name""#)[0] {
            TokenKind::Str(parts) => {
                assert_eq!(parts.len(), 2);
                assert_eq!(parts[0], StrPart::Literal("Hello, ".into()));
                match &parts[1] {
                    StrPart::Ident { name, .. } => assert_eq!(name, "name"),
                    other => panic!("expected ident part, got {other:?}"),
                }
            }
            other => panic!("expected string, got {other:?}"),
        }
    }

    #[test]
    fn ident_interpolation_takes_field_access() {
        // §27: print("Hello, $self.name")
        match &kinds(r#""Hello, $self.name!""#)[0] {
            TokenKind::Str(parts) => match &parts[1] {
                StrPart::Ident { name, .. } => assert_eq!(name, "self.name"),
                other => panic!("expected ident part, got {other:?}"),
            },
            other => panic!("expected string, got {other:?}"),
        }
    }

    #[test]
    fn lexes_expression_interpolation() {
        // §11: print("Age: ${age + 1}")
        match &kinds(r#""Age: ${age + 1}""#)[0] {
            TokenKind::Str(parts) => {
                assert_eq!(parts[0], StrPart::Literal("Age: ".into()));
                match &parts[1] {
                    StrPart::Expr { tokens, .. } => {
                        let kinds: Vec<_> = tokens.iter().map(|t| t.kind.clone()).collect();
                        assert_eq!(
                            kinds,
                            vec![
                                TokenKind::Ident("age".into()),
                                TokenKind::Plus,
                                TokenKind::Int {
                                    value: 1,
                                    base: NumBase::Decimal,
                                    suffix: None
                                },
                                TokenKind::Eof,
                            ]
                        );
                    }
                    other => panic!("expected expr part, got {other:?}"),
                }
            }
            other => panic!("expected string, got {other:?}"),
        }
    }

    #[test]
    fn interpolation_spans_point_at_original_source() {
        let src = r#"let s := "x ${age}""#;
        let out = lex(FileId(0), src);
        let tok = out.tokens.iter().find(|t| matches!(t.kind, TokenKind::Str(_))).unwrap();
        let TokenKind::Str(parts) = &tok.kind else { unreachable!() };
        let StrPart::Expr { tokens, .. } = &parts[1] else { panic!("expected expr part") };
        let age = &tokens[0];
        assert_eq!(&src[age.span.lo as usize..age.span.hi as usize], "age");
    }

    #[test]
    fn lone_dollar_is_literal() {
        match &kinds(r#""cost: $5""#)[0] {
            TokenKind::Str(parts) => assert_eq!(parts, &vec![StrPart::Literal("cost: $5".into())]),
            other => panic!("expected string, got {other:?}"),
        }
    }

    #[test]
    fn reports_unterminated_string() {
        assert!(errors("\"abc").iter().any(|m| m.contains("unterminated string")));
        assert!(errors("\"abc\ndef\"").iter().any(|m| m.contains("unterminated string")));
    }

    #[test]
    fn lexes_char_literals() {
        assert_eq!(kinds("'S'")[0], TokenKind::Char('S'));
        assert_eq!(kinds(r"'\n'")[0], TokenKind::Char('\n'));
        assert_eq!(kinds(r"'\''")[0], TokenKind::Char('\''));
    }

    #[test]
    fn rejects_multi_char_literals() {
        let msgs = errors("'ab'");
        assert!(msgs[0].contains("more than one character"), "{msgs:?}");
    }

    #[test]
    fn skips_comments_but_keeps_doc_comments() {
        use TokenKind::*;
        let src = "// line\n/* block */\n/// doc\nfn";
        assert_eq!(
            kinds(src),
            vec![DocComment("doc".into()), Keyword(super::Keyword::Fn), Eof]
        );
    }

    #[test]
    fn block_comments_nest() {
        use TokenKind::*;
        assert_eq!(kinds("/* a /* b */ c */ fn"), vec![Keyword(super::Keyword::Fn), Eof]);
        assert!(errors("/* a /* b */").iter().any(|m| m.contains("unterminated block comment")));
    }

    #[test]
    fn lexes_optional_operators() {
        use TokenKind::*;
        // §30
        assert_eq!(kinds("str? x := name?.length ?? 0")[1], Question);
        assert_eq!(kinds("name?.length")[1], QuestionDot);
        assert_eq!(kinds("a ?? b")[1], QuestionQuestion);
    }

    #[test]
    fn lexes_keywords_from_the_spec() {
        use Keyword::*;
        let src = "let const fn struct enum interface impl module use as pub extern if else \
                   match for in while loop break continue return try catch defer unsafe async \
                   await spawn call self true false null";
        let got: Vec<Keyword> = kinds(src)
            .into_iter()
            .filter_map(|k| match k {
                TokenKind::Keyword(kw) => Some(kw),
                _ => None,
            })
            .collect();
        assert_eq!(
            got,
            vec![
                Let, Const, Fn, Struct, Enum, Interface, Impl, Module, Use, As, Pub, Extern, If,
                Else, Match, For, In, While, Loop, Break, Continue, Return, Try, Catch, Defer,
                Unsafe, Async, Await, Spawn, Call, Self_, True, False, Null,
            ]
        );
    }

    #[test]
    fn underscore_is_its_own_token() {
        // §26 wildcard match arm
        assert_eq!(kinds("_")[0], TokenKind::Underscore);
        assert_eq!(kinds("_x")[0], TokenKind::Ident("_x".into()));
    }

    #[test]
    fn reserved_words_are_rejected_but_recovered() {
        let out = lex(FileId(0), "let type := 1");
        assert!(out.diagnostics.iter().any(|d| d.message.contains("reserved word")));
        assert_eq!(out.tokens[1].kind, TokenKind::Ident("type".into()));
    }

    #[test]
    fn lexes_attributes_and_arrows() {
        use TokenKind::*;
        // §73 and §16
        assert_eq!(kinds("@test")[0], At);
        assert_eq!(kinds("-> int")[0], Arrow);
        assert_eq!(kinds("...")[0], Ellipsis);
    }

    #[test]
    fn recovers_from_unknown_characters() {
        let out = lex(FileId(0), "let a\u{7} := 1");
        assert!(out.diagnostics.iter().any(|d| d.message.contains("unexpected character")));
        // Lexing continued past the bad character.
        assert!(out.tokens.iter().any(|t| t.kind == TokenKind::Assign));
    }

    #[test]
    fn spans_cover_the_token_text() {
        let src = "let name := \"Sasha\"";
        let out = lex(FileId(0), src);
        for tok in &out.tokens {
            if tok.kind == TokenKind::Eof {
                continue;
            }
            let text = &src[tok.span.lo as usize..tok.span.hi as usize];
            assert!(!text.is_empty(), "empty span for {:?}", tok.kind);
        }
        let name = &out.tokens[1];
        assert_eq!(&src[name.span.lo as usize..name.span.hi as usize], "name");
    }

    #[test]
    fn lexes_the_spec_107_example() {
        let src = r#"
use http

struct User {
    str name
    int age
}

fn greet(User user) {
    print("Hello, $user.name!")
}

fn main() {
    let str name := "Sasha"
    let int times := 7

    let User user := User {
        name: name,
        age: 20
    }

    for i in times {
        call greet(user)
    }
}
"#;
        let out = lex(FileId(0), src);
        assert!(
            !out.diagnostics.has_errors(),
            "{:?}",
            out.diagnostics.iter().map(|d| &d.message).collect::<Vec<_>>()
        );
        assert!(out.tokens.len() > 60);
        assert_eq!(out.tokens.last().unwrap().kind, TokenKind::Eof);
    }
}
