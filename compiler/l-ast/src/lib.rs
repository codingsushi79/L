//! The L abstract syntax tree.
//!
//! The AST is a faithful record of what was written: it keeps spans, doc
//! comments and attributes so that `lformat`, `llint`, `ldoc` and `llsp` can
//! all work from the same tree. Desugaring happens later, in HIR lowering.

pub mod visit;

use l_span::Span;
use std::fmt;

/// A node identifier, unique within one parsed crate.
///
/// Assigned by the parser and used by later stages as a side-table key.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug, PartialOrd, Ord)]
pub struct NodeId(pub u32);

impl NodeId {
    /// Placeholder for nodes the compiler synthesises.
    pub const DUMMY: NodeId = NodeId(u32::MAX);
}

/// An identifier together with where it was written.
#[derive(Clone, PartialEq, Eq, Hash)]
pub struct Ident {
    pub name: String,
    pub span: Span,
}

impl Ident {
    pub fn new(name: impl Into<String>, span: Span) -> Self {
        Ident { name: name.into(), span }
    }
}

impl fmt::Debug for Ident {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.name)
    }
}

impl fmt::Display for Ident {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.name)
    }
}

/// A dotted path such as `math.sqrt` or `Message.TEXT` (SPEC §34, §26).
#[derive(Clone, Debug, PartialEq)]
pub struct Path {
    pub segments: Vec<Ident>,
    pub span: Span,
}

impl Path {
    pub fn single(ident: Ident) -> Self {
        let span = ident.span;
        Path { segments: vec![ident], span }
    }

    /// The final segment, which names the item itself.
    pub fn last(&self) -> &Ident {
        self.segments.last().expect("path has at least one segment")
    }

    pub fn is_single(&self) -> bool {
        self.segments.len() == 1
    }

    pub fn to_string_dotted(&self) -> String {
        self.segments
            .iter()
            .map(|s| s.name.as_str())
            .collect::<Vec<_>>()
            .join(".")
    }
}

/// Visibility. Private is the default (SPEC §33).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum Visibility {
    #[default]
    Private,
    Public(Span),
}

impl Visibility {
    pub fn is_public(&self) -> bool {
        matches!(self, Visibility::Public(_))
    }
}

/// An attribute such as `@test`, `@inline` or `@deprecated("...")` (SPEC §73).
#[derive(Clone, Debug, PartialEq)]
pub struct Attribute {
    pub name: Ident,
    /// Arguments, if written in parentheses.
    pub args: Vec<Expr>,
    pub span: Span,
}

impl Attribute {
    pub fn is(&self, name: &str) -> bool {
        self.name.name == name
    }
}

/// Documentation comments attached to an item (SPEC §6, §79).
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Docs {
    pub lines: Vec<String>,
    pub span: Option<Span>,
}

impl Docs {
    pub fn is_empty(&self) -> bool {
        self.lines.is_empty()
    }

    /// The doc text as a single string, one line per `///` comment.
    pub fn text(&self) -> String {
        self.lines.join("\n")
    }

    /// The first paragraph, used as a summary in `ldoc` and registry listings.
    pub fn summary(&self) -> String {
        let mut out = Vec::new();
        for line in &self.lines {
            if line.trim().is_empty() && !out.is_empty() {
                break;
            }
            out.push(line.trim());
        }
        out.join(" ").trim().to_string()
    }
}

// ===========================================================================
// Types
// ===========================================================================

/// A type as written in source.
#[derive(Clone, Debug, PartialEq)]
pub struct Type {
    pub id: NodeId,
    pub kind: TypeKind,
    pub span: Span,
}

#[derive(Clone, Debug, PartialEq)]
pub enum TypeKind {
    /// A named type, possibly generic: `int`, `User`, `Box<int>`, `math.Vec`.
    Named { path: Path, generics: Vec<Type> },
    /// `T[]` (SPEC §12).
    Array(Box<Type>),
    /// `map<K, V>` (SPEC §13).
    Map(Box<Type>, Box<Type>),
    /// `set<T>` (SPEC §14).
    Set(Box<Type>),
    /// `(A, B, C)` (SPEC §15).
    Tuple(Vec<Type>),
    /// `T?` (SPEC §30).
    Optional(Box<Type>),
    /// `fn(A, B) -> C`, for higher-order functions.
    Fn { params: Vec<Type>, ret: Option<Box<Type>> },
    /// The unit type, written as an omitted return type (SPEC §16).
    Void,
    /// A type that failed to parse; suppresses downstream errors.
    Err,
}

impl Type {
    pub fn is_err(&self) -> bool {
        matches!(self.kind, TypeKind::Err)
    }

    /// Render the type roughly as written, for diagnostics and `ldoc`.
    pub fn render(&self) -> String {
        match &self.kind {
            TypeKind::Named { path, generics } => {
                let base = path.to_string_dotted();
                if generics.is_empty() {
                    base
                } else {
                    let args: Vec<_> = generics.iter().map(|t| t.render()).collect();
                    format!("{base}<{}>", args.join(", "))
                }
            }
            TypeKind::Array(inner) => format!("{}[]", inner.render()),
            TypeKind::Map(k, v) => format!("map<{}, {}>", k.render(), v.render()),
            TypeKind::Set(t) => format!("set<{}>", t.render()),
            TypeKind::Tuple(items) => {
                let parts: Vec<_> = items.iter().map(|t| t.render()).collect();
                format!("({})", parts.join(", "))
            }
            TypeKind::Optional(inner) => format!("{}?", inner.render()),
            TypeKind::Fn { params, ret } => {
                let parts: Vec<_> = params.iter().map(|t| t.render()).collect();
                match ret {
                    Some(r) => format!("fn({}) -> {}", parts.join(", "), r.render()),
                    None => format!("fn({})", parts.join(", ")),
                }
            }
            TypeKind::Void => "void".to_string(),
            TypeKind::Err => "<error>".to_string(),
        }
    }
}

/// A generic parameter declaration, `<T>` (SPEC §29).
#[derive(Clone, Debug, PartialEq)]
pub struct GenericParam {
    pub id: NodeId,
    pub name: Ident,
    /// Interface bounds, e.g. `<T: Printable>`.
    pub bounds: Vec<Path>,
    pub span: Span,
}

// ===========================================================================
// Items
// ===========================================================================

/// A parsed source file (SPEC §5, §33).
#[derive(Clone, Debug, PartialEq)]
pub struct SourceUnit {
    pub id: NodeId,
    /// The `module x` declaration, if the file has one.
    pub module: Option<ModuleDecl>,
    pub uses: Vec<Use>,
    pub items: Vec<Item>,
    pub span: Span,
}

/// `module users` (SPEC §33).
#[derive(Clone, Debug, PartialEq)]
pub struct ModuleDecl {
    pub id: NodeId,
    pub name: Ident,
    pub docs: Docs,
    pub span: Span,
}

/// A `use` declaration (SPEC §34).
///
/// `use math.sqrt, math.sin` parses to one `Use` with two trees.
#[derive(Clone, Debug, PartialEq)]
pub struct Use {
    pub id: NodeId,
    pub trees: Vec<UseTree>,
    pub span: Span,
}

#[derive(Clone, Debug, PartialEq)]
pub struct UseTree {
    pub id: NodeId,
    pub path: Path,
    /// `use database as db` (SPEC §34).
    pub alias: Option<Ident>,
    pub span: Span,
}

impl UseTree {
    /// The name this import binds in the current scope.
    pub fn bound_name(&self) -> &Ident {
        self.alias.as_ref().unwrap_or_else(|| self.path.last())
    }
}

/// A top-level or nested declaration.
#[derive(Clone, Debug, PartialEq)]
pub struct Item {
    pub id: NodeId,
    pub kind: ItemKind,
    pub vis: Visibility,
    pub attrs: Vec<Attribute>,
    pub docs: Docs,
    pub span: Span,
}

impl Item {
    /// The name the item declares, if it has one.
    pub fn name(&self) -> Option<&Ident> {
        match &self.kind {
            ItemKind::Fn(f) => Some(&f.name),
            ItemKind::Struct(s) => Some(&s.name),
            ItemKind::Enum(e) => Some(&e.name),
            ItemKind::Interface(i) => Some(&i.name),
            ItemKind::Const(c) => Some(&c.name),
            ItemKind::Impl(_) => None,
        }
    }

    pub fn has_attr(&self, name: &str) -> bool {
        self.attrs.iter().any(|a| a.is(name))
    }

    /// `@test` (SPEC §73, §74).
    pub fn is_test(&self) -> bool {
        self.has_attr("test")
    }

    /// `@benchmark` (SPEC §75).
    pub fn is_benchmark(&self) -> bool {
        self.has_attr("benchmark")
    }
}

#[derive(Clone, Debug, PartialEq)]
pub enum ItemKind {
    /// `fn name(...) -> T { ... }`, including methods `fn User.greet()`.
    Fn(Box<FnDecl>),
    /// `struct User { ... }` (SPEC §23).
    Struct(Box<StructDecl>),
    /// `enum Color { ... }` (SPEC §25).
    Enum(Box<EnumDecl>),
    /// `interface Printable { ... }` (SPEC §28).
    Interface(Box<InterfaceDecl>),
    /// `impl Printable for User { ... }` (SPEC §28).
    Impl(Box<ImplBlock>),
    /// `const int MAX := 100` (SPEC §8).
    Const(Box<ConstDecl>),
}

/// A function or method (SPEC §16, §27, §68, §72).
#[derive(Clone, Debug, PartialEq)]
pub struct FnDecl {
    /// For a method `fn User.greet()`, the receiver type name `User`.
    pub receiver: Option<Ident>,
    pub name: Ident,
    pub generics: Vec<GenericParam>,
    pub params: Vec<Param>,
    /// `None` means void (SPEC §16).
    pub ret: Option<Type>,
    /// `None` for an interface method signature or an `extern` declaration.
    pub body: Option<Block>,
    pub is_async: bool,
    /// `extern fn printf(...)` (SPEC §72).
    pub is_extern: bool,
    /// Trailing `...` in an extern declaration (SPEC §72).
    pub is_variadic: bool,
}

impl FnDecl {
    pub fn is_method(&self) -> bool {
        self.receiver.is_some()
    }

    /// A display name such as `User.greet`.
    pub fn qualified_name(&self) -> String {
        match &self.receiver {
            Some(r) => format!("{}.{}", r.name, self.name.name),
            None => self.name.name.clone(),
        }
    }
}

/// A parameter, written type-first: `int a` (SPEC §16).
#[derive(Clone, Debug, PartialEq)]
pub struct Param {
    pub id: NodeId,
    pub ty: Type,
    pub name: Ident,
    pub span: Span,
}

/// `struct User { str name  int age := 0 }` (SPEC §23, §24, §29).
#[derive(Clone, Debug, PartialEq)]
pub struct StructDecl {
    pub name: Ident,
    pub generics: Vec<GenericParam>,
    pub fields: Vec<Field>,
}

/// A struct field, with an optional default (SPEC §24).
#[derive(Clone, Debug, PartialEq)]
pub struct Field {
    pub id: NodeId,
    pub vis: Visibility,
    pub ty: Type,
    pub name: Ident,
    pub default: Option<Expr>,
    pub docs: Docs,
    pub span: Span,
}

/// `enum Message { TEXT(str)  QUIT }` (SPEC §25).
#[derive(Clone, Debug, PartialEq)]
pub struct EnumDecl {
    pub name: Ident,
    pub generics: Vec<GenericParam>,
    pub variants: Vec<Variant>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Variant {
    pub id: NodeId,
    pub name: Ident,
    /// Payload types for a data variant, e.g. `TEXT(str)`.
    pub payload: Vec<Type>,
    pub docs: Docs,
    pub span: Span,
}

impl Variant {
    pub fn has_payload(&self) -> bool {
        !self.payload.is_empty()
    }
}

/// `interface Printable { fn print() }` (SPEC §28).
#[derive(Clone, Debug, PartialEq)]
pub struct InterfaceDecl {
    pub name: Ident,
    pub generics: Vec<GenericParam>,
    /// Method signatures, and optionally default bodies.
    pub methods: Vec<Item>,
}

/// `impl Printable for User { ... }` (SPEC §28).
#[derive(Clone, Debug, PartialEq)]
pub struct ImplBlock {
    /// The interface being implemented.
    pub interface: Path,
    /// The implementing type.
    pub target: Type,
    pub generics: Vec<GenericParam>,
    pub methods: Vec<Item>,
}

/// `const int MAX_USERS := 100` (SPEC §8).
#[derive(Clone, Debug, PartialEq)]
pub struct ConstDecl {
    pub ty: Option<Type>,
    pub name: Ident,
    pub value: Expr,
}

// ===========================================================================
// Statements
// ===========================================================================

/// A brace-delimited block. Its value is that of its trailing expression, if
/// any (SPEC §17).
#[derive(Clone, Debug, PartialEq)]
pub struct Block {
    pub id: NodeId,
    pub stmts: Vec<Stmt>,
    pub span: Span,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Stmt {
    pub id: NodeId,
    pub kind: StmtKind,
    pub span: Span,
}

#[derive(Clone, Debug, PartialEq)]
pub enum StmtKind {
    /// `let int age := 20` / `let age := 20` (SPEC §7).
    Let(Box<LetStmt>),
    /// `const int MAX := 100` in statement position (SPEC §8).
    Const(Box<ConstDecl>),
    /// `age := 21`, `age += 1`, `numbers[0] := 10` (SPEC §7, §12).
    Assign(Box<AssignStmt>),
    /// An expression evaluated for its effect.
    Expr(Box<Expr>),
    /// The trailing expression of a block, whose value the block takes (§17).
    Tail(Box<Expr>),
    /// `return`, optionally with a value (SPEC §16).
    Return(Option<Box<Expr>>),
    /// `break`, optionally to a label (SPEC §22).
    Break(Option<Ident>),
    /// `continue`, optionally to a label (SPEC §22).
    Continue(Option<Ident>),
    /// `defer { ... }` (SPEC §32).
    Defer(Box<Block>),
    /// An item declared inside a function body.
    Item(Box<Item>),
    /// A statement that failed to parse.
    Err,
}

/// `let int age := 20` (SPEC §7).
#[derive(Clone, Debug, PartialEq)]
pub struct LetStmt {
    /// `None` when the type is inferred (SPEC §7).
    pub ty: Option<Type>,
    pub name: Ident,
    pub value: Expr,
}

/// An assignment or compound assignment (SPEC §7).
#[derive(Clone, Debug, PartialEq)]
pub struct AssignStmt {
    pub target: Expr,
    pub op: AssignOp,
    pub value: Expr,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AssignOp {
    /// `:=`
    Assign,
    /// `+=`
    Add,
    /// `-=`
    Sub,
    /// `*=`
    Mul,
    /// `/=`
    Div,
    /// `%=`
    Rem,
}

impl AssignOp {
    pub fn as_str(self) -> &'static str {
        match self {
            AssignOp::Assign => ":=",
            AssignOp::Add => "+=",
            AssignOp::Sub => "-=",
            AssignOp::Mul => "*=",
            AssignOp::Div => "/=",
            AssignOp::Rem => "%=",
        }
    }

    /// The binary operator a compound assignment expands to.
    pub fn to_binop(self) -> Option<BinOp> {
        Some(match self {
            AssignOp::Assign => return None,
            AssignOp::Add => BinOp::Add,
            AssignOp::Sub => BinOp::Sub,
            AssignOp::Mul => BinOp::Mul,
            AssignOp::Div => BinOp::Div,
            AssignOp::Rem => BinOp::Rem,
        })
    }
}

// ===========================================================================
// Expressions
// ===========================================================================

#[derive(Clone, Debug, PartialEq)]
pub struct Expr {
    pub id: NodeId,
    pub kind: ExprKind,
    pub span: Span,
}

impl Expr {
    pub fn is_err(&self) -> bool {
        matches!(self.kind, ExprKind::Err)
    }

    /// Whether the expression may appear on the left of an assignment.
    pub fn is_place(&self) -> bool {
        match &self.kind {
            ExprKind::Path(_) | ExprKind::SelfExpr => true,
            ExprKind::Field { .. } | ExprKind::Index { .. } | ExprKind::TupleField { .. } => true,
            _ => false,
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub enum ExprKind {
    // ---- Literals ----
    Int { value: u128, suffix: Option<String> },
    Float { value: f64, suffix: Option<String> },
    Bool(bool),
    Char(char),
    /// A string literal, possibly interpolated (SPEC §11).
    Str(Vec<StrSegment>),
    /// `null` (SPEC §30).
    Null,

    // ---- Names ----
    /// A variable, function or path such as `users.create`.
    Path(Path),
    /// `self` (SPEC §27).
    SelfExpr,

    // ---- Collections ----
    /// `[1, 2, 3]` (SPEC §12).
    Array(Vec<Expr>),
    /// `{ "a": 1 }` (SPEC §13).
    Map(Vec<(Expr, Expr)>),
    /// `{ "a", "b" }` (SPEC §14).
    Set(Vec<Expr>),
    /// `(10, 20)` (SPEC §15).
    Tuple(Vec<Expr>),
    /// `User { name: "Sasha" }` (SPEC §23).
    StructLit { path: Path, generics: Vec<Type>, fields: Vec<FieldInit> },

    // ---- Operators ----
    Unary { op: UnOp, operand: Box<Expr> },
    Binary { op: BinOp, lhs: Box<Expr>, rhs: Box<Expr> },
    /// `a ?? b` (SPEC §30).
    Coalesce { lhs: Box<Expr>, rhs: Box<Expr> },
    /// `0..10` and `0..=10` (SPEC §19).
    Range { start: Option<Box<Expr>>, end: Option<Box<Expr>>, inclusive: bool },

    // ---- Access ----
    /// `user.name`.
    Field { base: Box<Expr>, name: Ident },
    /// `name?.length` (SPEC §30).
    OptionalField { base: Box<Expr>, name: Ident },
    /// `point.0` (SPEC §15).
    TupleField { base: Box<Expr>, index: u32, span: Span },
    /// `numbers[0]` (SPEC §12, §13).
    Index { base: Box<Expr>, index: Box<Expr> },

    // ---- Invocation ----
    /// `call print("Hello")` (SPEC §2.3).
    ///
    /// The `call` keyword is required at statement level; the parser records
    /// whether it was written so `llint` can enforce house style.
    Call { callee: Box<Expr>, args: Vec<Expr>, has_call_keyword: bool },
    /// `Message.TEXT("hi")` — constructing a data variant (SPEC §25).
    /// Produced by the resolver; the parser emits `Call` and it is reclassified.
    VariantCtor { path: Path, args: Vec<Expr> },

    // ---- Control flow used as an expression ----
    /// `if c { a } else { b }` (SPEC §18).
    If { cond: Box<Expr>, then: Block, else_branch: Option<Box<ElseBranch>> },
    /// `match x { ... }` (SPEC §26).
    Match { scrutinee: Box<Expr>, arms: Vec<MatchArm> },
    /// A bare block used as an expression.
    Block(Block),
    /// `for x in xs { ... }` (SPEC §19).
    For { label: Option<Ident>, pat: Pattern, iter: Box<Expr>, body: Block },
    /// `while c { ... }` (SPEC §20).
    While { label: Option<Ident>, cond: Box<Expr>, body: Block },
    /// `loop { ... }` (SPEC §21).
    Loop { label: Option<Ident>, body: Block },

    // ---- Effects ----
    /// `try { ... } catch e { ... }` (SPEC §31).
    Try { body: Block, catches: Vec<CatchClause> },
    /// `await expr` (SPEC §68).
    Await(Box<Expr>),
    /// `spawn expr` or `spawn { ... }` (SPEC §68, §69).
    Spawn(Box<Expr>),
    /// `unsafe { ... }` (SPEC §71).
    Unsafe(Block),

    /// An expression that failed to parse.
    Err,
}

/// One piece of an interpolated string (SPEC §11).
#[derive(Clone, Debug, PartialEq)]
pub enum StrSegment {
    Literal(String),
    /// `$name` or `${expr}`; both become an expression to evaluate and format.
    Interp(Box<Expr>),
}

/// `name: value` inside a struct literal (SPEC §23).
#[derive(Clone, Debug, PartialEq)]
pub struct FieldInit {
    pub id: NodeId,
    pub name: Ident,
    pub value: Expr,
    pub span: Span,
}

/// The tail of an `if` (SPEC §18).
#[derive(Clone, Debug, PartialEq)]
pub enum ElseBranch {
    Block(Block),
    /// `else if ...`
    If(Expr),
}

/// One arm of a `match` (SPEC §26).
#[derive(Clone, Debug, PartialEq)]
pub struct MatchArm {
    pub id: NodeId,
    pub pat: Pattern,
    pub body: Block,
    pub span: Span,
}

/// One `catch` clause (SPEC §31).
#[derive(Clone, Debug, PartialEq)]
pub struct CatchClause {
    pub id: NodeId,
    /// `catch FileError error` — the error type, if narrowed.
    pub ty: Option<Type>,
    pub binding: Option<Ident>,
    pub body: Block,
    pub span: Span,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum UnOp {
    /// `-x`
    Neg,
    /// `!x` (SPEC §10)
    Not,
    /// `~x`
    BitNot,
}

impl UnOp {
    pub fn as_str(self) -> &'static str {
        match self {
            UnOp::Neg => "-",
            UnOp::Not => "!",
            UnOp::BitNot => "~",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BinOp {
    Add,
    Sub,
    Mul,
    Div,
    Rem,
    Eq,
    Ne,
    Lt,
    Le,
    Gt,
    Ge,
    /// `&&` (SPEC §10) — short-circuiting.
    And,
    /// `||` (SPEC §10) — short-circuiting.
    Or,
    BitAnd,
    BitOr,
    BitXor,
    Shl,
    Shr,
}

impl BinOp {
    pub fn as_str(self) -> &'static str {
        use BinOp::*;
        match self {
            Add => "+",
            Sub => "-",
            Mul => "*",
            Div => "/",
            Rem => "%",
            Eq => "==",
            Ne => "!=",
            Lt => "<",
            Le => "<=",
            Gt => ">",
            Ge => ">=",
            And => "&&",
            Or => "||",
            BitAnd => "&",
            BitOr => "|",
            BitXor => "^",
            Shl => "<<",
            Shr => ">>",
        }
    }

    pub fn is_comparison(self) -> bool {
        use BinOp::*;
        matches!(self, Eq | Ne | Lt | Le | Gt | Ge)
    }

    pub fn is_logical(self) -> bool {
        matches!(self, BinOp::And | BinOp::Or)
    }

    pub fn is_arithmetic(self) -> bool {
        use BinOp::*;
        matches!(self, Add | Sub | Mul | Div | Rem)
    }
}

// ===========================================================================
// Patterns
// ===========================================================================

/// A pattern, as used by `match` arms and `for` bindings (SPEC §19, §26).
#[derive(Clone, Debug, PartialEq)]
pub struct Pattern {
    pub id: NodeId,
    pub kind: PatternKind,
    pub span: Span,
}

#[derive(Clone, Debug, PartialEq)]
pub enum PatternKind {
    /// `_` (SPEC §26).
    Wildcard,
    /// A new binding, e.g. `user` in `for user in users`.
    Binding(Ident),
    /// `Color.RED`, or `Message.TEXT(text)` with sub-patterns (SPEC §26).
    Variant { path: Path, fields: Vec<Pattern> },
    /// A literal pattern, e.g. `0` or `"quit"`.
    ///
    /// Boxed: `ExprKind::For` holds a `Pattern`, so an inline `Expr` here would
    /// make both types infinitely sized.
    Literal(Box<Expr>),
    /// `(a, b)` (SPEC §15).
    Tuple(Vec<Pattern>),
    /// A pattern that failed to parse.
    Err,
}

impl Pattern {
    /// Every name this pattern binds.
    pub fn bindings(&self) -> Vec<&Ident> {
        let mut out = Vec::new();
        self.collect_bindings(&mut out);
        out
    }

    fn collect_bindings<'a>(&'a self, out: &mut Vec<&'a Ident>) {
        match &self.kind {
            PatternKind::Binding(ident) => out.push(ident),
            PatternKind::Variant { fields, .. } | PatternKind::Tuple(fields) => {
                for f in fields {
                    f.collect_bindings(out);
                }
            }
            PatternKind::Wildcard | PatternKind::Literal(_) | PatternKind::Err => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use l_span::{FileId, Span};

    fn sp() -> Span {
        Span::new(FileId(0), 0, 1)
    }

    fn ident(name: &str) -> Ident {
        Ident::new(name, sp())
    }

    fn ty(kind: TypeKind) -> Type {
        Type { id: NodeId(0), kind, span: sp() }
    }

    #[test]
    fn renders_types_as_written() {
        let int = || ty(TypeKind::Named { path: Path::single(ident("int")), generics: vec![] });
        assert_eq!(int().render(), "int");
        assert_eq!(ty(TypeKind::Array(Box::new(int()))).render(), "int[]");
        assert_eq!(ty(TypeKind::Optional(Box::new(int()))).render(), "int?");
        assert_eq!(
            ty(TypeKind::Map(Box::new(int()), Box::new(int()))).render(),
            "map<int, int>"
        );
        assert_eq!(ty(TypeKind::Set(Box::new(int()))).render(), "set<int>");
        assert_eq!(ty(TypeKind::Tuple(vec![int(), int()])).render(), "(int, int)");
        assert_eq!(
            ty(TypeKind::Named {
                path: Path::single(ident("Box")),
                generics: vec![int()]
            })
            .render(),
            "Box<int>"
        );
    }

    #[test]
    fn use_tree_binds_alias_when_present() {
        let tree = UseTree {
            id: NodeId(0),
            path: Path::single(ident("database")),
            alias: Some(ident("db")),
            span: sp(),
        };
        assert_eq!(tree.bound_name().name, "db");

        let plain = UseTree { alias: None, ..tree };
        assert_eq!(plain.bound_name().name, "database");
    }

    #[test]
    fn path_renders_dotted() {
        let path = Path {
            segments: vec![ident("math"), ident("sqrt")],
            span: sp(),
        };
        assert_eq!(path.to_string_dotted(), "math.sqrt");
        assert_eq!(path.last().name, "sqrt");
        assert!(!path.is_single());
    }

    #[test]
    fn pattern_collects_nested_bindings() {
        let pat = Pattern {
            id: NodeId(0),
            kind: PatternKind::Variant {
                path: Path::single(ident("TEXT")),
                fields: vec![
                    Pattern { id: NodeId(1), kind: PatternKind::Binding(ident("a")), span: sp() },
                    Pattern { id: NodeId(2), kind: PatternKind::Wildcard, span: sp() },
                    Pattern {
                        id: NodeId(3),
                        kind: PatternKind::Tuple(vec![Pattern {
                            id: NodeId(4),
                            kind: PatternKind::Binding(ident("b")),
                            span: sp(),
                        }]),
                        span: sp(),
                    },
                ],
            },
            span: sp(),
        };
        let names: Vec<_> = pat.bindings().iter().map(|i| i.name.as_str()).collect();
        assert_eq!(names, vec!["a", "b"]);
    }

    #[test]
    fn docs_summary_stops_at_blank_line() {
        let docs = Docs {
            lines: vec![
                "Adds two numbers.".into(),
                "".into(),
                "More detail here.".into(),
            ],
            span: None,
        };
        assert_eq!(docs.summary(), "Adds two numbers.");
        assert_eq!(docs.text().lines().count(), 3);
    }

    #[test]
    fn compound_assignment_maps_to_binop() {
        assert_eq!(AssignOp::Add.to_binop(), Some(BinOp::Add));
        assert_eq!(AssignOp::Assign.to_binop(), None);
    }

    #[test]
    fn place_expressions_are_assignable() {
        let path = Expr {
            id: NodeId(0),
            kind: ExprKind::Path(Path::single(ident("age"))),
            span: sp(),
        };
        assert!(path.is_place());

        let lit = Expr { id: NodeId(1), kind: ExprKind::Int { value: 1, suffix: None }, span: sp() };
        assert!(!lit.is_place());

        let index = Expr {
            id: NodeId(2),
            kind: ExprKind::Index { base: Box::new(path), index: Box::new(lit) },
            span: sp(),
        };
        assert!(index.is_place());
    }
}
