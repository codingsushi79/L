//! The L mid-level intermediate representation (SPEC §80).
//!
//! MIR is a control-flow graph. Each function becomes a list of basic blocks;
//! each block is a straight run of statements ending in one terminator. The
//! tree structure of HIR — `if`, `match`, `loop`, short-circuiting operators —
//! is gone, replaced by explicit branches.
//!
//! This is the representation the optimiser rewrites and the backend walks.
//! Two things are worth knowing about its shape:
//!
//! * A call is an [`Rvalue`], not a terminator. The reference backend emits C,
//!   which has no unwinding through calls, so splitting a block at every call
//!   would cost clarity and buy nothing.
//! * `defer` blocks (SPEC §32) are not represented. They are replayed inline
//!   at each exit during lowering, which is what makes them run on `return`
//!   and on ordinary fall-out alike.

use l_hir as hir;
use l_hir::{Builtin, DefId, Hir, LocalId, LocalInfo, LoopId, Ty};
use l_span::Span;
use std::collections::HashMap;

/// An index into a body's block list.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug, PartialOrd, Ord)]
pub struct BlockId(pub u32);

/// A whole program, lowered.
pub struct Program {
    /// Type definitions are still needed by the backend for layout.
    pub hir: Hir,
    pub bodies: Vec<Body>,
    pub entry: Option<DefId>,
}

impl Program {
    pub fn body(&self, def: DefId) -> Option<&Body> {
        self.bodies.iter().find(|b| b.def == def)
    }
}

/// One function, as a control-flow graph.
pub struct Body {
    pub def: DefId,
    pub name: String,
    pub qualified: String,
    pub params: Vec<LocalId>,
    pub ret: Ty,
    pub locals: Vec<LocalInfo>,
    pub blocks: Vec<BasicBlock>,
    pub is_extern: bool,
    pub is_variadic: bool,
    pub is_test: bool,
    pub is_benchmark: bool,
    pub span: Span,
}

impl Body {
    pub const ENTRY: BlockId = BlockId(0);

    pub fn local_ty(&self, id: LocalId) -> &Ty {
        &self.locals[id.0 as usize].ty
    }

    pub fn block(&self, id: BlockId) -> &BasicBlock {
        &self.blocks[id.0 as usize]
    }
}

#[derive(Debug, Default)]
pub struct BasicBlock {
    pub stmts: Vec<Stmt>,
    pub term: Terminator,
}

#[derive(Debug, Clone)]
pub enum Stmt {
    /// `place = rvalue`
    Assign(Place, Rvalue),
    /// Evaluate for effect — a call whose result is discarded.
    Eval(Rvalue),
    /// Leave the innermost `try` (SPEC §31).
    PopTry,
    /// Removed by the optimiser; kept so block indices stay stable.
    Nop,
}

#[derive(Debug, Clone)]
pub enum Terminator {
    Goto(BlockId),
    /// Two-way branch on a `bool`.
    If { cond: Operand, then: BlockId, els: BlockId },
    /// Multi-way branch on an integer, used for enum tags (SPEC §26).
    Switch { value: Operand, targets: Vec<(i128, BlockId)>, default: BlockId },
    /// Install an error handler, then continue (SPEC §31).
    Try { handler: BlockId, body: BlockId },
    Return,
    /// Control cannot reach here.
    Unreachable,
}

impl Default for Terminator {
    fn default() -> Self {
        Terminator::Unreachable
    }
}

impl Terminator {
    /// The blocks this terminator may transfer control to.
    pub fn successors(&self) -> Vec<BlockId> {
        match self {
            Terminator::Goto(b) => vec![*b],
            Terminator::If { then, els, .. } => vec![*then, *els],
            Terminator::Switch { targets, default, .. } => {
                let mut v: Vec<BlockId> = targets.iter().map(|(_, b)| *b).collect();
                v.push(*default);
                v
            }
            Terminator::Try { handler, body } => vec![*handler, *body],
            Terminator::Return | Terminator::Unreachable => Vec::new(),
        }
    }
}

/// A storage location: a local, possibly with a path into it.
#[derive(Debug, Clone, PartialEq)]
pub struct Place {
    pub local: LocalId,
    pub proj: Vec<Proj>,
}

impl Place {
    pub fn local(id: LocalId) -> Place {
        Place { local: id, proj: Vec::new() }
    }

    pub fn is_local(&self) -> bool {
        self.proj.is_empty()
    }

    pub fn with(&self, p: Proj) -> Place {
        let mut out = self.clone();
        out.proj.push(p);
        out
    }
}

#[derive(Debug, Clone, PartialEq)]
pub enum Proj {
    /// A struct field, by index.
    Field(usize),
    /// A tuple element (SPEC §15).
    TupleField(usize),
    /// `xs[i]` on an array (SPEC §12).
    Index(Operand),
    /// `m[k]` on a map (SPEC §13).
    MapIndex(Operand),
}

#[derive(Debug, Clone, PartialEq)]
pub enum Operand {
    Copy(Place),
    Const(Const),
}

impl Operand {
    pub fn unit() -> Operand {
        Operand::Const(Const::Unit)
    }
}

#[derive(Debug, Clone, PartialEq)]
pub enum Const {
    Int(i128),
    Float(f64),
    Bool(bool),
    Char(char),
    Str(String),
    Null,
    Unit,
}

#[derive(Debug, Clone)]
pub enum Rvalue {
    Use(Operand),
    Unary(hir::UnOp, Operand, Ty),
    /// The type is the operand type, which decides how the backend emits it.
    Binary(hir::BinOp, Operand, Operand, Ty),
    /// `[a, b]` with its element type (SPEC §12).
    Array(Vec<Operand>, Ty),
    /// `{k: v}` with key and value types (SPEC §13).
    Map(Vec<(Operand, Operand)>, Ty, Ty),
    /// `{a, b}` with its element type (SPEC §14).
    Set(Vec<Operand>, Ty),
    Tuple(Vec<Operand>, Vec<Ty>),
    StructLit(DefId, Vec<Operand>),
    EnumLit(DefId, usize, Vec<Operand>),
    /// String interpolation and `+` on strings (SPEC §11).
    Concat(Vec<Operand>),
    /// `xs.length` (SPEC §12).
    Len(Operand, Ty),
    /// `a..b`, half-open (SPEC §19).
    Range(Operand, Operand),
    RangeStart(Operand),
    RangeEnd(Operand),
    /// The nth entry of a map or set, for desugared `for` loops.
    NthEntry(Operand, Operand, Ty),
    /// A direct call (SPEC §16).
    Call(DefId, Vec<Operand>),
    /// A compiler-known function (SPEC §67). Argument types are carried so the
    /// backend can pick the right runtime entry point.
    Builtin(Builtin, Vec<Operand>, Vec<Ty>),
    /// The tag of an enum value (SPEC §25).
    Discriminant(Operand, DefId),
    /// One value out of a data variant's payload.
    Payload(Operand, DefId, usize, usize),
    /// Wrap in an optional (SPEC §30).
    MakeOptional(Operand, Ty),
    /// Read the value out of an optional, which must not be null.
    UnwrapOptional(Operand, Ty),
    /// Whether an optional is null.
    IsNull(Operand),
    /// A numeric conversion.
    Cast(Operand, Ty, Ty),
    /// A null optional of a known type (SPEC §30).
    NullOptional(Ty),
    /// The message of the error currently being handled (SPEC §31).
    CaughtError,
}

// ===========================================================================
// Lowering
// ===========================================================================

/// Lower a checked program to MIR.
pub fn lower(hir: Hir) -> Program {
    let mut bodies = Vec::new();
    let order = hir.order.clone();
    let mut seen = Vec::new();

    for def in order {
        if seen.contains(&def) {
            continue;
        }
        let Some(f) = hir.fns.get(&def) else { continue };
        seen.push(def);
        bodies.push(lower_fn(&hir, f));
    }

    let entry = hir.entry;
    Program { hir, bodies, entry }
}

fn lower_fn(hir: &Hir, f: &hir::FnDef) -> Body {
    let mut b = Builder {
        hir,
        locals: f.locals.clone(),
        blocks: vec![BasicBlock::default()],
        current: BlockId(0),
        loops: HashMap::new(),
        defers: vec![Vec::new()],
    };

    if let Some(body) = &f.body {
        // The return slot is allocated before the body is lowered, so every
        // `return` writes to the same local and the backend has one place to
        // read the result from.
        let ret_place = if f.ret.is_void() {
            None
        } else {
            Some(Place::local(b.alloc_return(f.ret.clone(), f.span)))
        };

        // A function's value is its trailing expression (SPEC §17).
        b.lower_block(body, ret_place);

        // Anything deferred in the function's own scope runs before the
        // implicit return (SPEC §32).
        b.emit_defers(0);
        b.terminate(Terminator::Return);
    } else {
        b.terminate(Terminator::Return);
    }

    Body {
        def: f.id,
        name: f.name.clone(),
        qualified: f.qualified.clone(),
        params: f.params.clone(),
        ret: f.ret.clone(),
        locals: b.locals,
        blocks: b.blocks,
        is_extern: f.is_extern,
        is_variadic: f.is_variadic,
        is_test: f.is_test,
        is_benchmark: f.is_benchmark,
        span: f.span,
    }
}

/// The local that holds a function's return value.
///
/// Rather than invent a return slot, lowering assigns the value to a temporary
/// and the backend reads it at `return`. The temporary is recorded by writing
/// it into the body's last local, which the backend recognises.
const RETURN_MARKER: &str = "__return";

struct Builder<'a> {
    hir: &'a Hir,
    locals: Vec<LocalInfo>,
    blocks: Vec<BasicBlock>,
    current: BlockId,
    /// Where `break` and `continue` go, per loop (SPEC §22).
    loops: HashMap<LoopId, LoopTargets>,
    /// Deferred blocks, innermost scope last (SPEC §32).
    defers: Vec<Vec<hir::Block>>,
}

#[derive(Clone, Copy)]
struct LoopTargets {
    brk: BlockId,
    cont: BlockId,
    /// How deep the defer stack was when the loop was entered, so leaving it
    /// runs the right deferred blocks.
    defer_depth: usize,
}

impl<'a> Builder<'a> {
    // ---- block and local plumbing ----

    fn new_block(&mut self) -> BlockId {
        let id = BlockId(self.blocks.len() as u32);
        self.blocks.push(BasicBlock::default());
        id
    }

    fn switch_to(&mut self, id: BlockId) {
        self.current = id;
    }

    fn push(&mut self, stmt: Stmt) {
        self.blocks[self.current.0 as usize].stmts.push(stmt);
    }

    /// Set the current block's terminator, unless it already has one.
    fn terminate(&mut self, term: Terminator) {
        let block = &mut self.blocks[self.current.0 as usize];
        if matches!(block.term, Terminator::Unreachable) {
            block.term = term;
        }
    }

    /// Whether the current block has already been closed by a `return`,
    /// `break` or `continue`.
    fn closed(&self) -> bool {
        !matches!(self.blocks[self.current.0 as usize].term, Terminator::Unreachable)
    }

    fn temp(&mut self, ty: Ty, span: Span) -> LocalId {
        let id = LocalId(self.locals.len() as u32);
        self.locals.push(LocalInfo {
            id,
            name: format!("__t{}", id.0),
            ty,
            mutable: true,
            synthetic: true,
            span,
        });
        id
    }

    /// Reserve the local that holds this function's return value.
    fn alloc_return(&mut self, ty: Ty, span: Span) -> LocalId {
        let id = LocalId(self.locals.len() as u32);
        self.locals.push(LocalInfo {
            id,
            name: RETURN_MARKER.to_string(),
            ty,
            mutable: true,
            synthetic: true,
            span,
        });
        id
    }

    /// The local holding the return value, if the body produces one.
    fn return_local(&self) -> Option<LocalId> {
        self.locals.iter().find(|l| l.name == RETURN_MARKER).map(|l| l.id)
    }

    // ---- defer (SPEC §32) ----

    fn push_defer_scope(&mut self) {
        self.defers.push(Vec::new());
    }

    fn pop_defer_scope(&mut self) {
        self.defers.pop();
    }

    /// Emit every deferred block from the innermost scope down to `depth`,
    /// in reverse order of registration.
    fn emit_defers(&mut self, depth: usize) {
        let scopes: Vec<Vec<hir::Block>> =
            self.defers.iter().skip(depth).cloned().collect();
        for scope in scopes.iter().rev() {
            for block in scope.iter().rev() {
                self.lower_block(block, None);
            }
        }
    }

    // ---- blocks and statements ----

    fn lower_block(&mut self, block: &hir::Block, dest: Option<Place>) {
        self.push_defer_scope();
        let depth = self.defers.len() - 1;

        for stmt in &block.stmts {
            if self.closed() {
                break;
            }
            self.lower_stmt(stmt);
        }

        if !self.closed() {
            // A block with no trailing expression leaves `dest` alone. That
            // happens when every path out of the block already returned, and
            // when it does not, the checker has reported it (SPEC §16).
            if let Some(tail) = &block.tail {
                self.lower_into(tail, dest);
            }
            // Ordinary fall-out of the block runs its defers.
            self.emit_defers(depth);
        }

        self.pop_defer_scope();
    }

    fn lower_stmt(&mut self, stmt: &hir::Stmt) {
        match &stmt.kind {
            hir::StmtKind::Let { local, init } => {
                self.lower_into(init, Some(Place::local(*local)));
            }

            hir::StmtKind::Assign { place, value } => {
                let p = self.lower_place(place);
                self.lower_into(value, Some(p));
            }

            hir::StmtKind::Expr(e) => {
                self.lower_into(e, None);
            }

            hir::StmtKind::Return(value) => {
                if let Some(v) = value {
                    // A `return` in a void function is a type error that has
                    // already been reported; evaluate the value for its
                    // effects and drop it.
                    let slot = self.return_local().map(Place::local);
                    self.lower_into(v, slot);
                }
                // Deferred blocks run before the value leaves the function.
                self.emit_defers(0);
                self.terminate(Terminator::Return);
            }

            hir::StmtKind::Break(id) => {
                if let Some(t) = self.loops.get(id).copied() {
                    self.emit_defers(t.defer_depth);
                    self.terminate(Terminator::Goto(t.brk));
                }
            }

            hir::StmtKind::Continue(id) => {
                if let Some(t) = self.loops.get(id).copied() {
                    self.emit_defers(t.defer_depth);
                    self.terminate(Terminator::Goto(t.cont));
                }
            }

            hir::StmtKind::Defer(block) => {
                if let Some(scope) = self.defers.last_mut() {
                    scope.push((**block).clone());
                }
            }
        }
    }

    // ---- places ----

    fn lower_place(&mut self, e: &hir::Expr) -> Place {
        match &e.kind {
            hir::ExprKind::Local(id) => Place::local(*id),
            hir::ExprKind::Field { base, index } => {
                let b = self.lower_place(base);
                b.with(Proj::Field(*index))
            }
            hir::ExprKind::TupleField { base, index } => {
                let b = self.lower_place(base);
                b.with(Proj::TupleField(*index))
            }
            hir::ExprKind::Index { base, index } => {
                let b = self.lower_place(base);
                let i = self.lower_operand(index);
                let proj = if matches!(base.ty, Ty::Map(_, _)) {
                    Proj::MapIndex(i)
                } else {
                    Proj::Index(i)
                };
                b.with(proj)
            }
            // Not a place: evaluate into a temporary so lowering can continue
            // after a type error.
            _ => {
                let t = self.temp(e.ty.clone(), e.span);
                let p = Place::local(t);
                self.lower_into(e, Some(p.clone()));
                p
            }
        }
    }

    // ---- operands ----

    /// Lower to something that can be used directly as an argument.
    fn lower_operand(&mut self, e: &hir::Expr) -> Operand {
        match &e.kind {
            hir::ExprKind::Int(v) => Operand::Const(Const::Int(*v)),
            hir::ExprKind::Float(v) => Operand::Const(Const::Float(*v)),
            hir::ExprKind::Bool(v) => Operand::Const(Const::Bool(*v)),
            hir::ExprKind::Char(v) => Operand::Const(Const::Char(*v)),
            hir::ExprKind::Str(v) => Operand::Const(Const::Str(v.clone())),
            hir::ExprKind::Unit => Operand::unit(),
            hir::ExprKind::Local(id) => Operand::Copy(Place::local(*id)),
            _ => {
                let t = self.temp(e.ty.clone(), e.span);
                let p = Place::local(t);
                self.lower_into(e, Some(p.clone()));
                Operand::Copy(p)
            }
        }
    }

    // ---- expressions ----

    /// Lower `e`, writing its value to `dest` if one is wanted.
    fn lower_into(&mut self, e: &hir::Expr, dest: Option<Place>) {
        use hir::ExprKind as K;

        // Control flow needs its own blocks; everything else becomes an
        // rvalue assigned in the current block.
        match &e.kind {
            K::If { cond, then, els } => return self.lower_if(cond, then, els.as_ref(), dest),
            K::Match { scrutinee, arms } => return self.lower_match(scrutinee, arms, dest),
            K::Loop { id, body, step } => return self.lower_loop(*id, body, step.as_deref()),
            K::Block(b) | K::Unsafe(b) => return self.lower_block(b, dest),
            K::Logical { and, lhs, rhs } => return self.lower_logical(*and, lhs, rhs, dest, e.span),
            K::Coalesce { lhs, rhs } => return self.lower_coalesce(lhs, rhs, dest, e.span),
            K::Try { body, catches } => return self.lower_try(body, catches, dest),
            _ => {}
        }

        let rvalue = self.lower_rvalue(e);
        match dest {
            Some(p) => self.push(Stmt::Assign(p, rvalue)),
            None => self.push(Stmt::Eval(rvalue)),
        }
    }

    fn lower_rvalue(&mut self, e: &hir::Expr) -> Rvalue {
        use hir::ExprKind as K;
        match &e.kind {
            // `null` carries the optional's type, which the backend needs in
            // order to build the value (SPEC §30).
            K::Null => Rvalue::NullOptional(e.ty.clone()),

            K::Int(_) | K::Float(_) | K::Bool(_) | K::Char(_) | K::Str(_) | K::Unit
            | K::Local(_) => Rvalue::Use(self.lower_operand(e)),

            K::FnRef(_) | K::ConstRef(_) | K::Err => Rvalue::Use(Operand::unit()),

            K::Array(items) => {
                let ops = items.iter().map(|i| self.lower_operand(i)).collect();
                let elem = match &e.ty {
                    Ty::Array(t) => (**t).clone(),
                    _ => Ty::Err,
                };
                Rvalue::Array(ops, elem)
            }

            K::Map(entries) => {
                let ops = entries
                    .iter()
                    .map(|(k, v)| (self.lower_operand(k), self.lower_operand(v)))
                    .collect();
                let (kt, vt) = match &e.ty {
                    Ty::Map(k, v) => ((**k).clone(), (**v).clone()),
                    _ => (Ty::Err, Ty::Err),
                };
                Rvalue::Map(ops, kt, vt)
            }

            K::Set(items) => {
                let ops = items.iter().map(|i| self.lower_operand(i)).collect();
                let elem = match &e.ty {
                    Ty::Set(t) => (**t).clone(),
                    _ => Ty::Err,
                };
                Rvalue::Set(ops, elem)
            }

            K::Tuple(items) => {
                let tys = items.iter().map(|i| i.ty.clone()).collect();
                let ops = items.iter().map(|i| self.lower_operand(i)).collect();
                Rvalue::Tuple(ops, tys)
            }

            K::StructLit { def, fields } => {
                let ops = fields.iter().map(|f| self.lower_operand(f)).collect();
                Rvalue::StructLit(*def, ops)
            }

            K::EnumLit { def, variant, args } => {
                let ops = args.iter().map(|a| self.lower_operand(a)).collect();
                Rvalue::EnumLit(*def, *variant, ops)
            }

            K::Field { .. } | K::TupleField { .. } | K::Index { .. } => {
                let p = self.lower_place(e);
                Rvalue::Use(Operand::Copy(p))
            }

            K::Property { base, prop } => {
                let ty = base.ty.clone();
                let op = self.lower_operand(base);
                match prop {
                    hir::Property::Length => Rvalue::Len(op, ty),
                }
            }

            K::Unary { op, operand } => {
                let ty = operand.ty.clone();
                let o = self.lower_operand(operand);
                Rvalue::Unary(*op, o, ty)
            }

            K::Binary { op, lhs, rhs } => {
                // Comparing an optional with `null` is a null test, not a
                // value comparison (SPEC §30).
                let null_test = match (op, &lhs.ty, &rhs.kind) {
                    (hir::BinOp::Eq | hir::BinOp::Ne, Ty::Optional(_), hir::ExprKind::Null) => true,
                    _ => false,
                };
                if null_test {
                    let o = self.lower_operand(lhs);
                    let is_null = Rvalue::IsNull(o);
                    if *op == hir::BinOp::Eq {
                        return is_null;
                    }
                    let t = self.temp(Ty::BOOL, e.span);
                    self.push(Stmt::Assign(Place::local(t), is_null));
                    return Rvalue::Unary(
                        hir::UnOp::Not,
                        Operand::Copy(Place::local(t)),
                        Ty::BOOL,
                    );
                }

                let ty = lhs.ty.clone();
                // `+` on strings is concatenation (SPEC §11).
                if *op == hir::BinOp::Add && ty.is_str() {
                    let l = self.lower_operand(lhs);
                    let r = self.lower_operand(rhs);
                    return Rvalue::Concat(vec![l, r]);
                }
                let l = self.lower_operand(lhs);
                let r = self.lower_operand(rhs);
                Rvalue::Binary(*op, l, r, ty)
            }

            K::Concat(parts) => {
                let ops = parts.iter().map(|p| self.lower_operand(p)).collect();
                Rvalue::Concat(ops)
            }

            K::Cast { expr, to } => {
                let from = expr.ty.clone();
                let op = self.lower_operand(expr);
                match (&from, to) {
                    (f, Ty::Optional(inner)) if !f.is_optional() => {
                        Rvalue::MakeOptional(op, (**inner).clone())
                    }
                    (Ty::Optional(inner), t) if !t.is_optional() => {
                        Rvalue::UnwrapOptional(op, (**inner).clone())
                    }
                    (f, t) if f == t => Rvalue::Use(op),
                    (f, t) => Rvalue::Cast(op, f.clone(), t.clone()),
                }
            }

            K::Range { start, end } => {
                let s = self.lower_operand(start);
                let en = self.lower_operand(end);
                Rvalue::Range(s, en)
            }
            K::RangeStart(r) => {
                let op = self.lower_operand(r);
                Rvalue::RangeStart(op)
            }
            K::RangeEnd(r) => {
                let op = self.lower_operand(r);
                Rvalue::RangeEnd(op)
            }
            K::NthEntry { base, index } => {
                let ty = base.ty.clone();
                let b = self.lower_operand(base);
                let i = self.lower_operand(index);
                Rvalue::NthEntry(b, i, ty)
            }

            K::Call { def, args } => {
                let ops = args.iter().map(|a| self.lower_operand(a)).collect();
                Rvalue::Call(*def, ops)
            }

            K::Builtin { builtin, args } => {
                let tys = args.iter().map(|a| a.ty.clone()).collect();
                let ops = args.iter().map(|a| self.lower_operand(a)).collect();
                Rvalue::Builtin(*builtin, ops, tys)
            }

            // Handled by `lower_into`.
            K::If { .. }
            | K::Match { .. }
            | K::Loop { .. }
            | K::Block(_)
            | K::Unsafe(_)
            | K::Logical { .. }
            | K::Coalesce { .. }
            | K::Try { .. } => {
                let t = self.temp(e.ty.clone(), e.span);
                let p = Place::local(t);
                self.lower_into(e, Some(p.clone()));
                Rvalue::Use(Operand::Copy(p))
            }
        }
    }

    // ---- control flow ----

    fn lower_if(
        &mut self,
        cond: &hir::Expr,
        then: &hir::Block,
        els: Option<&hir::Block>,
        dest: Option<Place>,
    ) {
        let c = self.lower_operand(cond);
        let then_block = self.new_block();
        let else_block = self.new_block();
        let join = self.new_block();

        self.terminate(Terminator::If { cond: c, then: then_block, els: else_block });

        self.switch_to(then_block);
        self.lower_block(then, dest.clone());
        self.terminate(Terminator::Goto(join));

        self.switch_to(else_block);
        match els {
            Some(b) => self.lower_block(b, dest),
            None => {}
        }
        self.terminate(Terminator::Goto(join));

        self.switch_to(join);
    }

    /// `&&` and `||`, which must not evaluate the right operand needlessly
    /// (SPEC §10).
    fn lower_logical(
        &mut self,
        and: bool,
        lhs: &hir::Expr,
        rhs: &hir::Expr,
        dest: Option<Place>,
        span: Span,
    ) {
        let slot = match dest {
            Some(p) => p,
            None => Place::local(self.temp(Ty::BOOL, span)),
        };

        let l = self.lower_operand(lhs);
        self.push(Stmt::Assign(slot.clone(), Rvalue::Use(l.clone())));

        let rhs_block = self.new_block();
        let join = self.new_block();

        // For `&&`, evaluate the right side only when the left is true.
        let (then, els) = if and { (rhs_block, join) } else { (join, rhs_block) };
        self.terminate(Terminator::If { cond: l, then, els });

        self.switch_to(rhs_block);
        self.lower_into(rhs, Some(slot));
        self.terminate(Terminator::Goto(join));

        self.switch_to(join);
    }

    /// `a ?? b` (SPEC §30).
    fn lower_coalesce(
        &mut self,
        lhs: &hir::Expr,
        rhs: &hir::Expr,
        dest: Option<Place>,
        span: Span,
    ) {
        let inner = lhs.ty.unwrap_optional().clone();
        let slot = match dest {
            Some(p) => p,
            None => Place::local(self.temp(inner.clone(), span)),
        };

        let l = self.lower_operand(lhs);
        let is_null = self.temp(Ty::BOOL, span);
        self.push(Stmt::Assign(Place::local(is_null), Rvalue::IsNull(l.clone())));

        let fallback = self.new_block();
        let value = self.new_block();
        let join = self.new_block();
        self.terminate(Terminator::If {
            cond: Operand::Copy(Place::local(is_null)),
            then: fallback,
            els: value,
        });

        self.switch_to(value);
        self.push(Stmt::Assign(slot.clone(), Rvalue::UnwrapOptional(l, inner)));
        self.terminate(Terminator::Goto(join));

        self.switch_to(fallback);
        self.lower_into(rhs, Some(slot));
        self.terminate(Terminator::Goto(join));

        self.switch_to(join);
    }

    fn lower_loop(&mut self, id: LoopId, body: &hir::Block, step: Option<&hir::Stmt>) {
        let head = self.new_block();
        let step_block = self.new_block();
        let exit = self.new_block();

        self.terminate(Terminator::Goto(head));

        self.loops.insert(
            id,
            LoopTargets { brk: exit, cont: step_block, defer_depth: self.defers.len() },
        );

        self.switch_to(head);
        self.lower_block(body, None);
        self.terminate(Terminator::Goto(step_block));

        // The step runs at the end of each iteration and on `continue`, which
        // is what makes a desugared `for` advance (SPEC §19, §22).
        self.switch_to(step_block);
        if let Some(s) = step {
            self.lower_stmt(s);
        }
        self.terminate(Terminator::Goto(head));

        self.loops.remove(&id);
        self.switch_to(exit);
    }

    fn lower_match(&mut self, scrutinee: &hir::Expr, arms: &[hir::Arm], dest: Option<Place>) {
        let value = self.lower_operand(scrutinee);
        let scrut_ty = scrutinee.ty.clone();
        let join = self.new_block();

        for arm in arms {
            let body_block = self.new_block();
            let next_block = self.new_block();

            let matched = self.test_pattern(&arm.pat, &value, &scrut_ty, arm.span);
            match matched {
                // An irrefutable arm always runs; nothing after it can.
                None => {
                    self.terminate(Terminator::Goto(body_block));
                }
                Some(cond) => {
                    self.terminate(Terminator::If {
                        cond,
                        then: body_block,
                        els: next_block,
                    });
                }
            }

            self.switch_to(body_block);
            self.bind_pattern(&arm.pat, &value, &scrut_ty);
            self.lower_block(&arm.body, dest.clone());
            self.terminate(Terminator::Goto(join));

            self.switch_to(next_block);
        }

        // Falling off the end means no arm matched. The checker requires
        // exhaustiveness (SPEC §26), so this is only reachable if checking
        // already failed.
        self.terminate(Terminator::Goto(join));
        self.switch_to(join);
    }

    /// The test that decides whether `pat` matches, or `None` when it always
    /// does.
    fn test_pattern(
        &mut self,
        pat: &hir::Pat,
        value: &Operand,
        scrut_ty: &Ty,
        span: Span,
    ) -> Option<Operand> {
        use hir::PatKind as P;
        match &pat.kind {
            P::Wild | P::Bind(_) => None,

            P::Variant { def, variant, .. } => {
                let tag = self.temp(Ty::INT, span);
                self.push(Stmt::Assign(
                    Place::local(tag),
                    Rvalue::Discriminant(value.clone(), *def),
                ));
                let cmp = self.temp(Ty::BOOL, span);
                self.push(Stmt::Assign(
                    Place::local(cmp),
                    Rvalue::Binary(
                        hir::BinOp::Eq,
                        Operand::Copy(Place::local(tag)),
                        Operand::Const(Const::Int(*variant as i128)),
                        Ty::INT,
                    ),
                ));
                Some(Operand::Copy(Place::local(cmp)))
            }

            P::Int(v) => self.compare_const(value, Const::Int(*v), scrut_ty, span),
            P::Str(s) => self.compare_const(value, Const::Str(s.clone()), scrut_ty, span),
            P::Bool(b) => self.compare_const(value, Const::Bool(*b), scrut_ty, span),
            P::Char(c) => self.compare_const(value, Const::Char(*c), scrut_ty, span),

            P::Null => {
                let t = self.temp(Ty::BOOL, span);
                self.push(Stmt::Assign(Place::local(t), Rvalue::IsNull(value.clone())));
                Some(Operand::Copy(Place::local(t)))
            }

            // A tuple pattern matches when every element does.
            P::Tuple(subs) => {
                let Ty::Tuple(item_tys) = scrut_ty else { return None };
                let mut cond: Option<Operand> = None;
                for (i, sub) in subs.iter().enumerate() {
                    let field = self.temp(item_tys[i].clone(), span);
                    self.push(Stmt::Assign(
                        Place::local(field),
                        Rvalue::Use(Operand::Copy(
                            self.place_of(value).with(Proj::TupleField(i)),
                        )),
                    ));
                    let sub_value = Operand::Copy(Place::local(field));
                    if let Some(c) = self.test_pattern(sub, &sub_value, &item_tys[i], span) {
                        cond = Some(match cond {
                            None => c,
                            Some(prev) => {
                                let t = self.temp(Ty::BOOL, span);
                                self.push(Stmt::Assign(
                                    Place::local(t),
                                    Rvalue::Binary(hir::BinOp::BitAnd, prev, c, Ty::BOOL),
                                ));
                                Operand::Copy(Place::local(t))
                            }
                        });
                    }
                }
                cond
            }
        }
    }

    fn compare_const(
        &mut self,
        value: &Operand,
        c: Const,
        ty: &Ty,
        span: Span,
    ) -> Option<Operand> {
        let t = self.temp(Ty::BOOL, span);
        self.push(Stmt::Assign(
            Place::local(t),
            Rvalue::Binary(hir::BinOp::Eq, value.clone(), Operand::Const(c), ty.clone()),
        ));
        Some(Operand::Copy(Place::local(t)))
    }

    /// The place an operand refers to, spilling a constant to a temporary.
    fn place_of(&self, value: &Operand) -> Place {
        match value {
            Operand::Copy(p) => p.clone(),
            // Constants are never projected into by generated code.
            Operand::Const(_) => Place::local(LocalId(0)),
        }
    }

    /// Assign each binding in a matched pattern.
    fn bind_pattern(&mut self, pat: &hir::Pat, value: &Operand, scrut_ty: &Ty) {
        use hir::PatKind as P;
        match &pat.kind {
            P::Bind(local) => {
                self.push(Stmt::Assign(Place::local(*local), Rvalue::Use(value.clone())));
            }

            P::Variant { def, variant, subs } => {
                let Some(en) = self.hir.enums.get(def) else { return };
                let payload = en.variants[*variant].payload.clone();
                for (i, sub) in subs.iter().enumerate() {
                    let Some(ty) = payload.get(i) else { continue };
                    let slot = self.temp(ty.clone(), pat.span);
                    self.push(Stmt::Assign(
                        Place::local(slot),
                        Rvalue::Payload(value.clone(), *def, *variant, i),
                    ));
                    let sub_value = Operand::Copy(Place::local(slot));
                    self.bind_pattern(sub, &sub_value, ty);
                }
            }

            P::Tuple(subs) => {
                let Ty::Tuple(item_tys) = scrut_ty else { return };
                for (i, sub) in subs.iter().enumerate() {
                    let Some(ty) = item_tys.get(i) else { continue };
                    let slot = self.temp(ty.clone(), pat.span);
                    let src = self.place_of(value).with(Proj::TupleField(i));
                    self.push(Stmt::Assign(
                        Place::local(slot),
                        Rvalue::Use(Operand::Copy(src)),
                    ));
                    let sub_value = Operand::Copy(Place::local(slot));
                    self.bind_pattern(sub, &sub_value, ty);
                }
            }

            P::Wild | P::Int(_) | P::Str(_) | P::Bool(_) | P::Char(_) | P::Null => {}
        }
    }

    /// `try { ... } catch e { ... }` (SPEC §31).
    fn lower_try(&mut self, body: &hir::Block, catches: &[hir::Catch], dest: Option<Place>) {
        let body_block = self.new_block();
        let handler = self.new_block();
        let join = self.new_block();

        self.terminate(Terminator::Try { handler, body: body_block });

        self.switch_to(body_block);
        self.lower_block(body, dest.clone());
        self.push(Stmt::PopTry);
        self.terminate(Terminator::Goto(join));

        self.switch_to(handler);
        for catch in catches {
            if let Some(binding) = catch.binding {
                self.push(Stmt::Assign(Place::local(binding), Rvalue::CaughtError));
            }
            self.lower_block(&catch.body, dest.clone());
        }
        self.terminate(Terminator::Goto(join));

        self.switch_to(join);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Count how many blocks a body ends up with, as a rough check that
    /// control flow produced branches at all.
    fn block_count(body: &Body) -> usize {
        body.blocks.len()
    }

    fn empty_hir_fn(ret: Ty) -> hir::FnDef {
        hir::FnDef {
            id: DefId(0),
            name: "f".into(),
            qualified: "m.f".into(),
            params: vec![],
            ret,
            body: Some(hir::Block::empty(Span::dummy())),
            locals: vec![],
            is_extern: false,
            is_variadic: false,
            is_method: false,
            is_test: false,
            is_benchmark: false,
            is_public: false,
            deprecated: None,
            docs: String::new(),
            span: Span::dummy(),
        }
    }

    #[test]
    fn an_empty_body_returns_immediately() {
        let hir = Hir::default();
        let body = lower_fn(&hir, &empty_hir_fn(Ty::Void));
        assert_eq!(block_count(&body), 1);
        assert!(matches!(body.blocks[0].term, Terminator::Return));
    }

    #[test]
    fn terminators_report_their_successors() {
        let goto = Terminator::Goto(BlockId(3));
        assert_eq!(goto.successors(), vec![BlockId(3)]);

        let branch = Terminator::If {
            cond: Operand::Const(Const::Bool(true)),
            then: BlockId(1),
            els: BlockId(2),
        };
        assert_eq!(branch.successors(), vec![BlockId(1), BlockId(2)]);

        assert!(Terminator::Return.successors().is_empty());

        let switch = Terminator::Switch {
            value: Operand::Const(Const::Int(0)),
            targets: vec![(0, BlockId(4)), (1, BlockId(5))],
            default: BlockId(6),
        };
        assert_eq!(switch.successors(), vec![BlockId(4), BlockId(5), BlockId(6)]);
    }

    #[test]
    fn places_project() {
        let p = Place::local(LocalId(2));
        assert!(p.is_local());
        let f = p.with(Proj::Field(1));
        assert!(!f.is_local());
        assert_eq!(f.local, LocalId(2));
        assert_eq!(f.proj.len(), 1);
    }
}
