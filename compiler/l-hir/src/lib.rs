//! The L high-level intermediate representation (SPEC §80).
//!
//! HIR is what the AST becomes once names are resolved and types are known. It
//! differs from the AST in three ways:
//!
//! * every expression carries a resolved [`Ty`];
//! * names are gone — variables are [`LocalId`]s and items are [`DefId`]s;
//! * the sugar of the surface language has been expanded. `for` loops, `while`
//!   loops, compound assignment, string interpolation, method calls and `?.`
//!   are all desugared here so that MIR lowering has fewer cases to handle.
//!
//! HIR is still a tree. The control-flow graph appears in MIR.

pub mod ty;

pub use ty::{DefId, LocalId, Prim, Ty, TyVar};

use l_span::Span;
use std::collections::HashMap;

/// Identifies one loop, so `break`/`continue` know which loop they leave
/// (SPEC §22, including labelled loops).
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug, PartialOrd, Ord)]
pub struct LoopId(pub u32);

// ===========================================================================
// Program
// ===========================================================================

/// A whole compilation: every definition, from every module, already resolved.
#[derive(Debug, Default)]
pub struct Hir {
    pub structs: HashMap<DefId, StructDef>,
    pub enums: HashMap<DefId, EnumDef>,
    pub fns: HashMap<DefId, FnDef>,
    /// `fn main()` — the program entry point, absent for libraries.
    pub entry: Option<DefId>,
    /// Definitions in declaration order, so output is deterministic.
    pub order: Vec<DefId>,
}

impl Hir {
    /// The display name of a definition, for diagnostics.
    pub fn name_of(&self, def: DefId) -> String {
        if let Some(s) = self.structs.get(&def) {
            return s.name.clone();
        }
        if let Some(e) = self.enums.get(&def) {
            return e.name.clone();
        }
        if let Some(f) = self.fns.get(&def) {
            return f.name.clone();
        }
        format!("#{}", def.0)
    }

    /// Render a type using this program's definition names.
    pub fn render(&self, ty: &Ty) -> String {
        ty.render(&|d| self.name_of(d))
    }

    /// Functions in declaration order.
    pub fn fns_in_order(&self) -> impl Iterator<Item = &FnDef> {
        self.order.iter().filter_map(move |d| self.fns.get(d))
    }

    /// Tests, in declaration order (SPEC §74).
    pub fn tests(&self) -> Vec<&FnDef> {
        self.fns_in_order().filter(|f| f.is_test).collect()
    }

    /// Benchmarks, in declaration order (SPEC §75).
    pub fn benchmarks(&self) -> Vec<&FnDef> {
        self.fns_in_order().filter(|f| f.is_benchmark).collect()
    }
}

/// A struct definition (SPEC §23, §24).
#[derive(Debug, Clone)]
pub struct StructDef {
    pub id: DefId,
    pub name: String,
    pub fields: Vec<FieldDef>,
    pub span: Span,
}

impl StructDef {
    pub fn field_index(&self, name: &str) -> Option<usize> {
        self.fields.iter().position(|f| f.name == name)
    }
}

#[derive(Debug, Clone)]
pub struct FieldDef {
    pub name: String,
    pub ty: Ty,
    /// A field default (SPEC §24), evaluated at each construction site.
    pub default: Option<Expr>,
    pub public: bool,
    pub docs: String,
    pub span: Span,
}

/// An enum definition (SPEC §25).
#[derive(Debug, Clone)]
pub struct EnumDef {
    pub id: DefId,
    pub name: String,
    pub variants: Vec<VariantDef>,
    pub span: Span,
}

impl EnumDef {
    pub fn variant_index(&self, name: &str) -> Option<usize> {
        self.variants.iter().position(|v| v.name == name)
    }

    /// Whether every variant is payload-free, which lets the backend represent
    /// the enum as a plain integer tag.
    pub fn is_fieldless(&self) -> bool {
        self.variants.iter().all(|v| v.payload.is_empty())
    }
}

#[derive(Debug, Clone)]
pub struct VariantDef {
    pub name: String,
    pub payload: Vec<Ty>,
    pub docs: String,
    pub span: Span,
}

/// A function, method or constant initialiser (SPEC §16, §27).
#[derive(Debug, Clone)]
pub struct FnDef {
    pub id: DefId,
    /// The bare name, e.g. `greet`.
    pub name: String,
    /// The fully qualified name, e.g. `users.User.greet`, used for symbol
    /// naming in the backend and for documentation.
    pub qualified: String,
    /// Parameters, as locals. A method's receiver is parameter 0 (`self`).
    pub params: Vec<LocalId>,
    pub ret: Ty,
    /// Absent for `extern` declarations and interface signatures (SPEC §72).
    pub body: Option<Block>,
    /// Every local in the body, indexed by [`LocalId`].
    pub locals: Vec<LocalInfo>,
    pub is_extern: bool,
    pub is_variadic: bool,
    pub is_method: bool,
    pub is_test: bool,
    pub is_benchmark: bool,
    pub is_public: bool,
    /// `@deprecated("...")` (SPEC §73).
    pub deprecated: Option<String>,
    pub docs: String,
    pub span: Span,
}

impl FnDef {
    pub fn ty(&self, locals: &[LocalInfo]) -> Ty {
        Ty::Fn {
            params: self.params.iter().map(|p| locals[p.0 as usize].ty.clone()).collect(),
            ret: Box::new(self.ret.clone()),
        }
    }

    pub fn local(&self, id: LocalId) -> &LocalInfo {
        &self.locals[id.0 as usize]
    }
}

/// One local variable slot.
#[derive(Debug, Clone)]
pub struct LocalInfo {
    pub id: LocalId,
    /// The source name, or a compiler-generated one such as `__iter0`.
    pub name: String,
    pub ty: Ty,
    /// False for `const` bindings (SPEC §8) and loop counters.
    pub mutable: bool,
    /// True for names the compiler invented, which `llint` must not report.
    pub synthetic: bool,
    pub span: Span,
}

// ===========================================================================
// Statements and blocks
// ===========================================================================

/// A block. Its value is that of its trailing expression (SPEC §17).
#[derive(Debug, Clone)]
pub struct Block {
    pub stmts: Vec<Stmt>,
    pub tail: Option<Box<Expr>>,
    pub ty: Ty,
    pub span: Span,
}

impl Block {
    pub fn empty(span: Span) -> Block {
        Block { stmts: Vec::new(), tail: None, ty: Ty::Void, span }
    }
}

#[derive(Debug, Clone)]
pub struct Stmt {
    pub kind: StmtKind,
    pub span: Span,
}

#[derive(Debug, Clone)]
pub enum StmtKind {
    /// Initialise a local (SPEC §7).
    Let { local: LocalId, init: Box<Expr> },
    /// Evaluate for effect.
    Expr(Box<Expr>),
    /// Store to a place. Compound assignment is already expanded (SPEC §7).
    Assign { place: Box<Expr>, value: Box<Expr> },
    Return(Option<Box<Expr>>),
    Break(LoopId),
    Continue(LoopId),
    /// `defer { ... }` (SPEC §32). MIR replays these at every scope exit.
    Defer(Box<Block>),
}

// ===========================================================================
// Expressions
// ===========================================================================

#[derive(Debug, Clone)]
pub struct Expr {
    pub kind: ExprKind,
    pub ty: Ty,
    pub span: Span,
}

impl Expr {
    pub fn new(kind: ExprKind, ty: Ty, span: Span) -> Expr {
        Expr { kind, ty, span }
    }

    pub fn unit(span: Span) -> Expr {
        Expr { kind: ExprKind::Unit, ty: Ty::Void, span }
    }

    /// Whether this expression denotes a storage location that can be assigned.
    pub fn is_place(&self) -> bool {
        matches!(
            self.kind,
            ExprKind::Local(_)
                | ExprKind::Field { .. }
                | ExprKind::TupleField { .. }
                | ExprKind::Index { .. }
        )
    }
}

#[derive(Debug, Clone)]
pub enum ExprKind {
    // ---- Constants (SPEC §9–§11) ----
    Int(i128),
    Float(f64),
    Bool(bool),
    Char(char),
    Str(String),
    /// `null` (SPEC §30).
    Null,
    /// The value of a void expression.
    Unit,

    /// A local variable or parameter.
    Local(LocalId),
    /// A reference to a function, used when one is passed as a value.
    FnRef(DefId),
    /// A constant defined at item level (SPEC §8); already folded where
    /// possible, but kept as a reference so `ldoc` can name it.
    ConstRef(DefId),

    // ---- Aggregates ----
    /// `[1, 2, 3]` (SPEC §12).
    Array(Vec<Expr>),
    /// `{ "a": 1 }` (SPEC §13).
    Map(Vec<(Expr, Expr)>),
    /// `{ "a", "b" }` (SPEC §14).
    Set(Vec<Expr>),
    /// `(10, 20)` (SPEC §15).
    Tuple(Vec<Expr>),
    /// A struct literal. Fields are in declaration order and complete: any
    /// field left out at the call site has had its default substituted here
    /// (SPEC §23, §24).
    StructLit { def: DefId, fields: Vec<Expr> },
    /// An enum value, with its payload (SPEC §25).
    EnumLit { def: DefId, variant: usize, args: Vec<Expr> },

    // ---- Access ----
    /// `user.name`, by field index.
    Field { base: Box<Expr>, index: usize },
    /// `point.0` (SPEC §15).
    TupleField { base: Box<Expr>, index: usize },
    /// `numbers[0]`, `users["alice"]` (SPEC §12, §13).
    Index { base: Box<Expr>, index: Box<Expr> },
    /// `arr.length` and friends — a property with no arguments.
    Property { base: Box<Expr>, prop: Property },

    // ---- Operators ----
    Unary { op: UnOp, operand: Box<Expr> },
    Binary { op: BinOp, lhs: Box<Expr>, rhs: Box<Expr> },
    /// `&&` and `||`, kept separate because they short-circuit (SPEC §10).
    Logical { and: bool, lhs: Box<Expr>, rhs: Box<Expr> },
    /// A numeric or string conversion inserted by the type checker.
    Cast { expr: Box<Expr>, to: Ty },
    /// `a ?? b` (SPEC §30) — short-circuiting.
    Coalesce { lhs: Box<Expr>, rhs: Box<Expr> },
    /// `a..b` (SPEC §19).
    ///
    /// Always half-open by the time it reaches HIR: `a..=b` is normalised to
    /// `a..(b + 1)`, so everything downstream has one case to handle.
    Range { start: Box<Expr>, end: Box<Expr> },
    /// The start of a range value.
    RangeStart(Box<Expr>),
    /// The exclusive end of a range value.
    RangeEnd(Box<Expr>),
    /// The nth entry of a map or set, in insertion order. Produced only by
    /// desugared `for` loops, which need positional access to a hashed
    /// container (SPEC §19).
    NthEntry { base: Box<Expr>, index: Box<Expr> },

    // ---- Strings ----
    /// String interpolation, expanded from `"a $b c"` (SPEC §11). Each part is
    /// already converted to `str`.
    Concat(Vec<Expr>),

    // ---- Invocation ----
    /// A direct call to a known function (SPEC §2.3, §16). Method calls are
    /// lowered to a direct call with the receiver as the first argument.
    Call { def: DefId, args: Vec<Expr> },
    /// A call to a compiler-known function such as `print` (SPEC §67).
    Builtin { builtin: Builtin, args: Vec<Expr> },

    // ---- Control flow (SPEC §17–§22, §26) ----
    If { cond: Box<Expr>, then: Block, els: Option<Block> },
    Match { scrutinee: Box<Expr>, arms: Vec<Arm> },
    Block(Block),
    /// The one looping form in HIR. `while` and `for` are desugared into it.
    ///
    /// `step` runs at the end of every iteration *and* on `continue`, which is
    /// what makes a desugared `for` loop advance correctly (SPEC §19, §22).
    Loop { id: LoopId, body: Block, step: Option<Box<Stmt>> },

    // ---- Effects ----
    /// `try { ... } catch e { ... }` (SPEC §31).
    Try { body: Block, catches: Vec<Catch> },
    /// `unsafe { ... }` (SPEC §71).
    Unsafe(Block),

    /// A subexpression that failed to check. Suppresses later errors.
    Err,
}

/// A zero-argument property, the small set the language knows intrinsically.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Property {
    /// `array.length` (SPEC §12), and the same on `str`, `map` and `set`.
    Length,
}

#[derive(Debug, Clone)]
pub struct Arm {
    pub pat: Pat,
    pub body: Block,
    pub span: Span,
}

#[derive(Debug, Clone)]
pub struct Catch {
    /// The local the error message binds to, if the clause named one.
    pub binding: Option<LocalId>,
    pub body: Block,
    pub span: Span,
}

/// A pattern, after resolution (SPEC §26).
#[derive(Debug, Clone)]
pub struct Pat {
    pub kind: PatKind,
    pub ty: Ty,
    pub span: Span,
}

#[derive(Debug, Clone)]
pub enum PatKind {
    /// `_`
    Wild,
    /// A binding, which introduces a local.
    Bind(LocalId),
    /// `Message.TEXT(text)` — a variant with sub-patterns for its payload.
    Variant { def: DefId, variant: usize, subs: Vec<Pat> },
    /// `(a, b)` (SPEC §15).
    Tuple(Vec<Pat>),
    Int(i128),
    Str(String),
    Bool(bool),
    Char(char),
    /// `null` (SPEC §30).
    Null,
}

impl Pat {
    /// Whether this pattern matches every value of its type, which is what
    /// makes a `match` exhaustive without listing variants (SPEC §26).
    pub fn is_irrefutable(&self) -> bool {
        match &self.kind {
            PatKind::Wild | PatKind::Bind(_) => true,
            PatKind::Tuple(subs) => subs.iter().all(Pat::is_irrefutable),
            _ => false,
        }
    }

    /// Every local this pattern binds.
    pub fn bindings(&self) -> Vec<LocalId> {
        let mut out = Vec::new();
        self.collect(&mut out);
        out
    }

    fn collect(&self, out: &mut Vec<LocalId>) {
        match &self.kind {
            PatKind::Bind(id) => out.push(*id),
            PatKind::Variant { subs, .. } | PatKind::Tuple(subs) => {
                for s in subs {
                    s.collect(out);
                }
            }
            _ => {}
        }
    }
}

// ===========================================================================
// Operators and builtins
// ===========================================================================

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UnOp {
    Neg,
    Not,
    BitNot,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
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
    BitAnd,
    BitOr,
    BitXor,
    Shl,
    Shr,
}

impl BinOp {
    pub fn is_comparison(self) -> bool {
        use BinOp::*;
        matches!(self, Eq | Ne | Lt | Le | Gt | Ge)
    }

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
            BitAnd => "&",
            BitOr => "|",
            BitXor => "^",
            Shl => "<<",
            Shr => ">>",
        }
    }
}

/// The functions the compiler knows about directly.
///
/// These are the parts of the standard library that need compiler support —
/// either because they are variadic, generic over any type, or map to a
/// runtime primitive (SPEC §67).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Builtin {
    /// `print(x)` — writes a line to standard output.
    Print,
    /// `println(x)` — an alias for `print`, accepted because SPEC §28 uses it.
    Println,
    /// `eprint(x)` — writes a line to standard error.
    EPrint,
    /// `assert(cond)` (SPEC §74).
    Assert,
    /// `panic(message)` — aborts, catchable by `try` (SPEC §31).
    Panic,
    /// `str(x)` — conversion to text, also used by interpolation.
    ToStr,
    /// `int(x)` — numeric or string conversion.
    ToInt,
    /// `float(x)`.
    ToFloat,
    /// `len(x)`, the function form of `.length`.
    Len,
    /// `array.push(v)`.
    Push,
    /// `array.pop()`.
    Pop,
    /// `set.add(v)` / `map` insertion (SPEC §14).
    Add,
    /// `set.remove(v)` / `map.remove(k)` (SPEC §14).
    Remove,
    /// `map.has(k)` / `set.has(v)`.
    Has,
    /// `map.keys()`.
    Keys,
    /// `map.values()`.
    Values,
    /// `str.split(sep)`.
    Split,
    /// `str.join(parts)`.
    Join,
    /// `str.contains(needle)`.
    Contains,
    /// `str.trim()`.
    Trim,
    /// `str.upper()` / `str.lower()`.
    Upper,
    Lower,
    /// `str.substr(start, end)`.
    Substr,
    /// `str.replace(from, to)`.
    Replace,
    /// `sqrt(x)` and friends from `math` (SPEC §67).
    Sqrt,
    Abs,
    Min,
    Max,
    Pow,
    Floor,
    Ceil,
    /// `time.now()` — milliseconds since the epoch.
    Now,
    /// `env.args()` — command-line arguments.
    Args,
    /// `io.read_line()`.
    ReadLine,
    /// `fs.read_file(path)` / `fs.write_file(path, text)`.
    ReadFile,
    WriteFile,
    /// `process.exit(code)`.
    Exit,
}

impl Builtin {
    /// The name written in source, for diagnostics and `ldoc`.
    pub fn name(self) -> &'static str {
        use Builtin::*;
        match self {
            Print => "print",
            Println => "println",
            EPrint => "eprint",
            Assert => "assert",
            Panic => "panic",
            ToStr => "str",
            ToInt => "int",
            ToFloat => "float",
            Len => "len",
            Push => "push",
            Pop => "pop",
            Add => "add",
            Remove => "remove",
            Has => "has",
            Keys => "keys",
            Values => "values",
            Split => "split",
            Join => "join",
            Contains => "contains",
            Trim => "trim",
            Upper => "upper",
            Lower => "lower",
            Substr => "substr",
            Replace => "replace",
            Sqrt => "sqrt",
            Abs => "abs",
            Min => "min",
            Max => "max",
            Pow => "pow",
            Floor => "floor",
            Ceil => "ceil",
            Now => "now",
            Args => "args",
            ReadLine => "read_line",
            ReadFile => "read_file",
            WriteFile => "write_file",
            Exit => "exit",
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

    fn pat(kind: PatKind) -> Pat {
        Pat { kind, ty: Ty::INT, span: sp() }
    }

    #[test]
    fn wildcards_and_bindings_are_irrefutable() {
        assert!(pat(PatKind::Wild).is_irrefutable());
        assert!(pat(PatKind::Bind(LocalId(0))).is_irrefutable());
        assert!(!pat(PatKind::Int(3)).is_irrefutable());
        assert!(!pat(PatKind::Variant { def: DefId(0), variant: 0, subs: vec![] }).is_irrefutable());
    }

    #[test]
    fn tuple_is_irrefutable_only_if_all_parts_are() {
        let all = pat(PatKind::Tuple(vec![pat(PatKind::Wild), pat(PatKind::Bind(LocalId(1)))]));
        assert!(all.is_irrefutable());

        let some = pat(PatKind::Tuple(vec![pat(PatKind::Wild), pat(PatKind::Int(0))]));
        assert!(!some.is_irrefutable());
    }

    #[test]
    fn pattern_collects_nested_bindings() {
        let p = pat(PatKind::Variant {
            def: DefId(0),
            variant: 1,
            subs: vec![
                pat(PatKind::Bind(LocalId(2))),
                pat(PatKind::Tuple(vec![pat(PatKind::Bind(LocalId(5)))])),
            ],
        });
        assert_eq!(p.bindings(), vec![LocalId(2), LocalId(5)]);
    }

    #[test]
    fn fieldless_enums_are_detected() {
        let unit = |name: &str| VariantDef {
            name: name.into(),
            payload: vec![],
            docs: String::new(),
            span: sp(),
        };
        let mut e = EnumDef {
            id: DefId(0),
            name: "Color".into(),
            variants: vec![unit("RED"), unit("GREEN")],
            span: sp(),
        };
        assert!(e.is_fieldless());
        assert_eq!(e.variant_index("GREEN"), Some(1));
        assert_eq!(e.variant_index("BLUE"), None);

        e.variants[1].payload.push(Ty::INT);
        assert!(!e.is_fieldless());
    }

    #[test]
    fn struct_fields_are_found_by_name() {
        let field = |name: &str| FieldDef {
            name: name.into(),
            ty: Ty::STR,
            default: None,
            public: true,
            docs: String::new(),
            span: sp(),
        };
        let s = StructDef {
            id: DefId(1),
            name: "User".into(),
            fields: vec![field("name"), field("age")],
            span: sp(),
        };
        assert_eq!(s.field_index("age"), Some(1));
        assert_eq!(s.field_index("nope"), None);
    }
}
