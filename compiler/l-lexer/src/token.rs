//! Token definitions for L.

use l_span::Span;
use std::fmt;

/// A lexed token.
#[derive(Clone, Debug, PartialEq)]
pub struct Token {
    pub kind: TokenKind,
    pub span: Span,
}

impl Token {
    pub fn new(kind: TokenKind, span: Span) -> Self {
        Token { kind, span }
    }

    pub fn is(&self, kind: &TokenKind) -> bool {
        &self.kind == kind
    }

    /// Documentation comments are kept in the stream (SPEC §6, §79); ordinary
    /// comments are not.
    pub fn is_doc(&self) -> bool {
        matches!(self.kind, TokenKind::DocComment(_))
    }
}

/// A keyword of the language.
///
/// Every keyword required by the specification appears here; the parser is
/// responsible for rejecting ones used in the wrong position.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Keyword {
    // Declarations
    Let,      // §7
    Const,    // §8
    Fn,       // §16
    Struct,   // §23
    Enum,     // §25
    Interface,// §28
    Impl,     // §28
    Module,   // §33
    Use,      // §34
    As,       // §34 aliases
    Pub,      // §33
    Extern,   // §72
    // Control flow
    If,       // §18
    Else,
    Match,    // §26
    For,      // §19
    In,
    While,    // §20
    Loop,     // §21
    Break,    // §22
    Continue,
    Return,   // §16
    // Effects and error handling
    Try,      // §31
    Catch,
    Defer,    // §32
    Unsafe,   // §71
    // Concurrency (§68, §69)
    Async,
    Await,
    Spawn,
    // Expressions
    Call,     // §2.3
    Self_,    // §27
    True,
    False,
    Null,     // §30
}

impl Keyword {
    /// Map an identifier to a keyword, if it is one.
    pub fn from_str(s: &str) -> Option<Keyword> {
        use Keyword::*;
        Some(match s {
            "let" => Let,
            "const" => Const,
            "fn" => Fn,
            "struct" => Struct,
            "enum" => Enum,
            "interface" => Interface,
            "impl" => Impl,
            "module" => Module,
            "use" => Use,
            "as" => As,
            "pub" => Pub,
            "extern" => Extern,
            "if" => If,
            "else" => Else,
            "match" => Match,
            "for" => For,
            "in" => In,
            "while" => While,
            "loop" => Loop,
            "break" => Break,
            "continue" => Continue,
            "return" => Return,
            "try" => Try,
            "catch" => Catch,
            "defer" => Defer,
            "unsafe" => Unsafe,
            "async" => Async,
            "await" => Await,
            "spawn" => Spawn,
            "call" => Call,
            "self" => Self_,
            "true" => True,
            "false" => False,
            "null" => Null,
            _ => return None,
        })
    }

    pub fn as_str(self) -> &'static str {
        use Keyword::*;
        match self {
            Let => "let",
            Const => "const",
            Fn => "fn",
            Struct => "struct",
            Enum => "enum",
            Interface => "interface",
            Impl => "impl",
            Module => "module",
            Use => "use",
            As => "as",
            Pub => "pub",
            Extern => "extern",
            If => "if",
            Else => "else",
            Match => "match",
            For => "for",
            In => "in",
            While => "while",
            Loop => "loop",
            Break => "break",
            Continue => "continue",
            Return => "return",
            Try => "try",
            Catch => "catch",
            Defer => "defer",
            Unsafe => "unsafe",
            Async => "async",
            Await => "await",
            Spawn => "spawn",
            Call => "call",
            Self_ => "self",
            True => "true",
            False => "false",
            Null => "null",
        }
    }
}

impl fmt::Display for Keyword {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// Words the language does not use yet but reserves, so that adding them later
/// is not a breaking change under SPEC §102.
pub const RESERVED: &[&str] = &[
    "yield", "type", "trait", "where", "macro", "static", "mut", "ref", "move", "become", "when",
    "select", "with", "do", "then", "end", "class", "new", "delete", "goto", "switch", "case",
    "default", "throw", "throws", "finally", "public", "private", "protected", "abstract",
    "override", "virtual", "operator", "union", "sizeof", "alignof", "typeof",
];

pub fn is_reserved(s: &str) -> bool {
    RESERVED.contains(&s)
}

/// A piece of an interpolated string (SPEC §11).
#[derive(Clone, Debug, PartialEq)]
pub enum StrPart {
    /// Literal text, with escapes already resolved.
    Literal(String),
    /// `$name` — a bare identifier interpolation.
    Ident { name: String, span: Span },
    /// `${expr}` — the inner tokens are lexed but parsed later by the parser.
    Expr { tokens: Vec<Token>, span: Span },
}

/// The base a numeric literal was written in.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum NumBase {
    Binary,
    Octal,
    Decimal,
    Hex,
}

impl NumBase {
    pub fn radix(self) -> u32 {
        match self {
            NumBase::Binary => 2,
            NumBase::Octal => 8,
            NumBase::Decimal => 10,
            NumBase::Hex => 16,
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub enum TokenKind {
    // ---- Literals and names ----
    Ident(String),
    Keyword(Keyword),
    /// An integer literal, with its digits already stripped of `_` separators.
    Int { value: u128, base: NumBase, suffix: Option<String> },
    Float { value: f64, suffix: Option<String> },
    /// A string literal. A literal with no interpolation is a single
    /// `StrPart::Literal`.
    Str(Vec<StrPart>),
    Char(char),
    /// `/// ...` — retained for `ldoc` and the language server.
    DocComment(String),

    // ---- Delimiters ----
    LParen,
    RParen,
    LBrace,
    RBrace,
    LBracket,
    RBracket,

    // ---- Punctuation ----
    Comma,
    Semi,
    Colon,
    /// `::` — reserved for future path syntax; not used by L 1.0.
    ColonColon,
    Dot,
    /// `..`
    DotDot,
    /// `..=`
    DotDotEq,
    /// `->`
    Arrow,
    /// `=>` — reserved; L 1.0 match arms use blocks (§26).
    FatArrow,
    /// `@` (§73)
    At,
    /// `?` (§30)
    Question,
    /// `?.` (§30)
    QuestionDot,
    /// `??` (§30)
    QuestionQuestion,
    /// `_`
    Underscore,
    /// `...` (§72 variadics)
    Ellipsis,
    /// `$` — only meaningful inside strings, but lexed for error recovery.
    Dollar,

    // ---- Assignment ----
    /// `:=` — L uses `:=` for both binding and assignment (§7).
    Assign,
    PlusEq,
    MinusEq,
    StarEq,
    SlashEq,
    PercentEq,

    // ---- Arithmetic ----
    Plus,
    Minus,
    Star,
    Slash,
    Percent,

    // ---- Comparison ----
    EqEq,
    NotEq,
    Lt,
    Le,
    Gt,
    Ge,

    // ---- Logical (§10) ----
    AndAnd,
    OrOr,
    Bang,

    // ---- Bitwise ----
    Amp,
    Pipe,
    Caret,
    Tilde,
    Shl,
    Shr,

    /// `=` is not an operator in L; it is lexed so the parser can suggest `:=`.
    Eq,

    /// End of input.
    Eof,
}

impl TokenKind {
    /// A short human-readable description, used in parser error messages.
    pub fn describe(&self) -> String {
        use TokenKind::*;
        match self {
            Ident(name) => format!("identifier `{name}`"),
            Keyword(kw) => format!("keyword `{kw}`"),
            Int { .. } => "integer literal".into(),
            Float { .. } => "float literal".into(),
            Str(_) => "string literal".into(),
            Char(_) => "character literal".into(),
            DocComment(_) => "documentation comment".into(),
            Eof => "end of file".into(),
            other => format!("`{}`", other.punct_str()),
        }
    }

    /// The source spelling of a punctuation or delimiter token.
    pub fn punct_str(&self) -> &'static str {
        use TokenKind::*;
        match self {
            LParen => "(",
            RParen => ")",
            LBrace => "{",
            RBrace => "}",
            LBracket => "[",
            RBracket => "]",
            Comma => ",",
            Semi => ";",
            Colon => ":",
            ColonColon => "::",
            Dot => ".",
            DotDot => "..",
            DotDotEq => "..=",
            Arrow => "->",
            FatArrow => "=>",
            At => "@",
            Question => "?",
            QuestionDot => "?.",
            QuestionQuestion => "??",
            Underscore => "_",
            Ellipsis => "...",
            Dollar => "$",
            Assign => ":=",
            PlusEq => "+=",
            MinusEq => "-=",
            StarEq => "*=",
            SlashEq => "/=",
            PercentEq => "%=",
            Plus => "+",
            Minus => "-",
            Star => "*",
            Slash => "/",
            Percent => "%",
            EqEq => "==",
            NotEq => "!=",
            Lt => "<",
            Le => "<=",
            Gt => ">",
            Ge => ">=",
            AndAnd => "&&",
            OrOr => "||",
            Bang => "!",
            Amp => "&",
            Pipe => "|",
            Caret => "^",
            Tilde => "~",
            Shl => "<<",
            Shr => ">>",
            Eq => "=",
            _ => "",
        }
    }

    pub fn is_keyword(&self, kw: Keyword) -> bool {
        matches!(self, TokenKind::Keyword(k) if *k == kw)
    }
}

impl fmt::Display for TokenKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.describe())
    }
}
