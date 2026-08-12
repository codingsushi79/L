//! Type checking and HIR lowering for L (SPEC §80).
//!
//! Checking and lowering are one pass. Every expression is checked against an
//! expected type where the context supplies one, and each checked expression is
//! returned as HIR, already typed. Fusing the two avoids walking each body
//! twice and keeps inference and desugaring in agreement: an expression is
//! lowered by the same code that decided what its type is.
//!
//! # Inference
//!
//! L infers types where the annotation is omitted (SPEC §7), but it is not a
//! Hindley-Milner language: there is no constraint solver and no unification
//! variables outside of literals. Checking is bidirectional. When an expected
//! type is available it is pushed down — that is what makes `let float x := 1`
//! and `let int[] xs := []` work. When one is not, the expression's own shape
//! decides, and an unsuffixed integer literal becomes `int` (SPEC §9).
//!
//! # Desugaring
//!
//! What leaves this pass is smaller than what entered it:
//!
//! * `for` and `while` become [`ExprKind::Loop`] (SPEC §19, §20);
//! * compound assignment becomes assignment of a binary expression (SPEC §7);
//! * string interpolation becomes [`ExprKind::Concat`] (SPEC §11);
//! * method calls become direct calls with the receiver first (SPEC §27);
//! * struct literals are completed with their field defaults (SPEC §24);
//! * `a?.b` becomes a conditional on the optional (SPEC §30).

use l_ast as ast;
use l_hir::*;
use l_resolver::{Def, DefKind, Res, Resolution, Unit};
use l_span::{DiagCode, Diagnostic, Diagnostics, Span};
use std::collections::HashMap;

// E3xxx is reserved for type errors.
const E_MISMATCH: DiagCode = DiagCode("E3001");
const E_UNKNOWN_TYPE: DiagCode = DiagCode("E3002");
const E_UNKNOWN_NAME: DiagCode = DiagCode("E3003");
const E_NOT_CALLABLE: DiagCode = DiagCode("E3004");
const E_ARITY: DiagCode = DiagCode("E3005");
const E_NO_FIELD: DiagCode = DiagCode("E3006");
const E_NO_METHOD: DiagCode = DiagCode("E3007");
const E_NOT_INDEXABLE: DiagCode = DiagCode("E3008");
const E_BAD_OPERAND: DiagCode = DiagCode("E3009");
const E_NOT_ITERABLE: DiagCode = DiagCode("E3010");
const E_MISSING_FIELD: DiagCode = DiagCode("E3011");
const E_UNKNOWN_FIELD: DiagCode = DiagCode("E3012");
const E_NOT_EXHAUSTIVE: DiagCode = DiagCode("E3013");
const E_MISSING_RETURN: DiagCode = DiagCode("E3014");
const E_IMMUTABLE: DiagCode = DiagCode("E3015");
const E_NOT_PLACE: DiagCode = DiagCode("E3016");
const E_BREAK_OUTSIDE: DiagCode = DiagCode("E3017");
const E_UNKNOWN_LABEL: DiagCode = DiagCode("E3018");
const E_NOT_OPTIONAL: DiagCode = DiagCode("E3019");
const E_NULL_TYPE: DiagCode = DiagCode("E3020");
const E_SELF_OUTSIDE: DiagCode = DiagCode("E3021");
const E_UNSUPPORTED: DiagCode = DiagCode("E3022");
const E_DUP_FIELD: DiagCode = DiagCode("E3023");
const E_INT_RANGE: DiagCode = DiagCode("E3024");
const E_CONDITION: DiagCode = DiagCode("E3025");

/// The output of checking a compilation.
pub struct Checked {
    pub hir: Hir,
    pub diagnostics: Diagnostics,
}

/// A function's declared signature, known before any body is checked so that
/// calls can be checked regardless of declaration order.
#[derive(Debug, Clone)]
struct Signature {
    params: Vec<Ty>,
    ret: Ty,
    receiver: Option<Ty>,
}

/// One entry on the loop stack, so `break`/`continue` find their target
/// (SPEC §22).
struct LoopFrame {
    id: LoopId,
    label: Option<String>,
}

/// Check and lower a whole compilation.
pub fn check(units: &[Unit], res: &Resolution) -> Checked {
    let mut cx = Checker {
        units,
        res,
        hir: Hir::default(),
        diags: Diagnostics::new(),
        sigs: HashMap::new(),
        consts: HashMap::new(),
        unit: 0,
        locals: Vec::new(),
        scopes: Vec::new(),
        loops: Vec::new(),
        next_loop: 0,
        ret_ty: Ty::Void,
        self_local: None,
    };
    cx.run();
    cx.diags.sort();
    Checked { hir: cx.hir, diagnostics: cx.diags }
}

struct Checker<'a> {
    units: &'a [Unit],
    res: &'a Resolution,
    hir: Hir,
    diags: Diagnostics,
    sigs: HashMap<DefId, Signature>,
    /// Item-level constants, inlined at each use site (SPEC §8).
    consts: HashMap<DefId, Expr>,

    // ---- per-function state ----
    unit: usize,
    locals: Vec<LocalInfo>,
    scopes: Vec<HashMap<String, LocalId>>,
    loops: Vec<LoopFrame>,
    next_loop: u32,
    ret_ty: Ty,
    self_local: Option<LocalId>,
}

impl<'a> Checker<'a> {
    fn run(&mut self) {
        self.collect_type_defs();
        self.collect_signatures();
        self.check_consts();
        self.check_bodies();
        self.hir.entry = self.res.entry();
    }

    fn error(&mut self, diag: Diagnostic) {
        self.diags.push(diag);
    }

    // =======================================================================
    // Phase 1: struct and enum shapes
    // =======================================================================

    fn collect_type_defs(&mut self) {
        // Field and payload types may mention any other type, so shapes are
        // registered first and filled in afterwards.
        for def in self.res.defs().to_vec() {
            match def.kind {
                DefKind::Struct => {
                    self.hir.structs.insert(
                        def.id,
                        StructDef {
                            id: def.id,
                            name: def.name.clone(),
                            fields: Vec::new(),
                            span: def.span,
                        },
                    );
                    self.hir.order.push(def.id);
                }
                DefKind::Enum => {
                    self.hir.enums.insert(
                        def.id,
                        EnumDef {
                            id: def.id,
                            name: def.name.clone(),
                            variants: Vec::new(),
                            span: def.span,
                        },
                    );
                    self.hir.order.push(def.id);
                }
                _ => {}
            }
        }

        for def in self.res.defs().to_vec() {
            match def.kind {
                DefKind::Struct => {
                    let Some(ast::ItemKind::Struct(s)) = self.item_kind(&def).cloned() else {
                        continue;
                    };
                    self.reject_generics(&s.generics);
                    self.unit = def.loc.unit;
                    let mut fields = Vec::new();
                    for f in &s.fields {
                        let ty = self.lower_ty(&f.ty);
                        let default = f.default.as_ref().map(|d| {
                            self.in_empty_scope(|cx| cx.check_expr(d, Some(&ty.clone())))
                        });
                        fields.push(FieldDef {
                            name: f.name.name.clone(),
                            ty,
                            default,
                            public: f.vis.is_public(),
                            docs: f.docs.text(),
                            span: f.span,
                        });
                    }
                    if let Some(sd) = self.hir.structs.get_mut(&def.id) {
                        sd.fields = fields;
                    }
                }
                DefKind::Enum => {
                    let Some(ast::ItemKind::Enum(e)) = self.item_kind(&def).cloned() else {
                        continue;
                    };
                    self.reject_generics(&e.generics);
                    self.unit = def.loc.unit;
                    let variants = e
                        .variants
                        .iter()
                        .map(|v| VariantDef {
                            name: v.name.name.clone(),
                            payload: v.payload.iter().map(|t| self.lower_ty(t)).collect(),
                            docs: v.docs.text(),
                            span: v.span,
                        })
                        .collect();
                    if let Some(ed) = self.hir.enums.get_mut(&def.id) {
                        ed.variants = variants;
                    }
                }
                DefKind::Interface => {
                    let Some(ast::ItemKind::Interface(i)) = self.item_kind(&def).cloned() else {
                        continue;
                    };
                    self.reject_generics(&i.generics);
                }
                _ => {}
            }
        }
    }

    fn reject_generics(&mut self, generics: &[ast::GenericParam]) {
        if let Some(g) = generics.first() {
            self.error(
                Diagnostic::error(E_UNSUPPORTED, "generics are not implemented in this preview")
                    .with_primary(g.span, "generic parameter")
                    .with_note("SPEC §29 defines generics; the reference compiler does not yet \
                                monomorphise them"),
            );
        }
    }

    // =======================================================================
    // Phase 2: function signatures
    // =======================================================================

    fn collect_signatures(&mut self) {
        for def in self.res.defs().to_vec() {
            if def.kind != DefKind::Fn {
                continue;
            }
            let Some(f) = self.fn_decl(&def).cloned() else { continue };
            self.reject_generics(&f.generics);
            self.unit = def.loc.unit;

            if f.is_async {
                self.error(
                    Diagnostic::error(E_UNSUPPORTED, "`async` is not implemented in this preview")
                        .with_primary(f.name.span, "async function")
                        .with_note("SPEC §68 defines async; the reference compiler is synchronous"),
                );
            }

            let receiver = def.receiver.as_ref().and_then(|r| {
                let ty = self.named_ty(r, f.name.span);
                if ty.is_err() {
                    None
                } else {
                    Some(ty)
                }
            });
            let params = f.params.iter().map(|p| self.lower_ty(&p.ty)).collect();
            let ret = match &f.ret {
                Some(t) => self.lower_ty(t),
                None => Ty::Void,
            };
            self.sigs.insert(def.id, Signature { params, ret, receiver });
        }
    }

    // =======================================================================
    // Phase 3: constants (SPEC §8)
    // =======================================================================

    fn check_consts(&mut self) {
        for def in self.res.defs().to_vec() {
            if def.kind != DefKind::Const {
                continue;
            }
            let Some(ast::ItemKind::Const(c)) = self.item_kind(&def).cloned() else { continue };
            self.unit = def.loc.unit;
            let want = c.ty.as_ref().map(|t| self.lower_ty(t));
            let value = self.in_empty_scope(|cx| cx.check_expr(&c.value, want.as_ref()));
            self.consts.insert(def.id, value);
        }
    }

    // =======================================================================
    // Phase 4: bodies
    // =======================================================================

    fn check_bodies(&mut self) {
        for def in self.res.defs().to_vec() {
            if def.kind != DefKind::Fn {
                continue;
            }
            if let Some(fd) = self.check_fn(&def) {
                self.hir.order.push(def.id);
                self.hir.fns.insert(def.id, fd);
            }
        }
    }

    fn check_fn(&mut self, def: &Def) -> Option<FnDef> {
        let decl = self.fn_decl(def).cloned()?;
        let sig = self.sigs.get(&def.id).cloned()?;
        let item = self.item(def)?.clone();

        self.unit = def.loc.unit;
        self.locals = Vec::new();
        self.scopes = vec![HashMap::new()];
        self.loops = Vec::new();
        self.ret_ty = sig.ret.clone();
        self.self_local = None;

        let mut params = Vec::new();
        if let Some(recv) = &sig.receiver {
            let id = self.declare_local("self", recv.clone(), false, true, decl.name.span);
            self.self_local = Some(id);
            params.push(id);
        }
        for (p, ty) in decl.params.iter().zip(sig.params.iter()) {
            let id = self.declare_local(&p.name.name, ty.clone(), true, false, p.name.span);
            params.push(id);
        }

        let body = match &decl.body {
            Some(b) => {
                let want = if sig.ret.is_void() { None } else { Some(sig.ret.clone()) };
                let block = self.check_block(b, want.as_ref());

                // A function that promises a value must produce one on every
                // path (SPEC §16, §17).
                if !sig.ret.is_void() && !sig.ret.is_err() && block.tail.is_none() {
                    if !block_diverges(&block) {
                        self.error(
                            Diagnostic::error(
                                E_MISSING_RETURN,
                                format!(
                                    "`{}` must return `{}` on every path",
                                    decl.qualified_name(),
                                    self.hir.render(&sig.ret)
                                ),
                            )
                            .with_primary(b.span.shrink_to_hi(), "this path returns nothing")
                            .with_note(
                                "either `return` a value, or end the block with the value itself \
                                 (SPEC §17)",
                            ),
                        );
                    }
                }
                Some(block)
            }
            None if decl.is_extern => None,
            None => None,
        };

        let deprecated = item.attrs.iter().find(|a| a.is("deprecated")).map(|a| {
            match a.args.first().map(|e| &e.kind) {
                Some(ast::ExprKind::Str(parts)) => match parts.first() {
                    Some(ast::StrSegment::Literal(s)) => s.clone(),
                    _ => String::new(),
                },
                _ => String::new(),
            }
        });

        Some(FnDef {
            id: def.id,
            name: def.name.clone(),
            qualified: def.qualified.clone(),
            params,
            ret: sig.ret,
            body,
            locals: std::mem::take(&mut self.locals),
            is_extern: decl.is_extern,
            is_variadic: decl.is_variadic,
            is_method: sig.receiver.is_some(),
            is_test: item.is_test(),
            is_benchmark: item.is_benchmark(),
            is_public: def.public,
            deprecated,
            docs: item.docs.text(),
            span: decl.name.span,
        })
    }

    // =======================================================================
    // AST lookup helpers
    // =======================================================================

    fn item(&self, def: &Def) -> Option<&'a ast::Item> {
        let unit = self.units.get(def.loc.unit)?;
        let item = unit.unit.items.get(def.loc.item)?;
        match def.loc.sub {
            None => Some(item),
            Some(sub) => match &item.kind {
                ast::ItemKind::Impl(b) => b.methods.get(sub),
                ast::ItemKind::Interface(i) => i.methods.get(sub),
                _ => None,
            },
        }
    }

    fn item_kind(&self, def: &Def) -> Option<&'a ast::ItemKind> {
        self.item(def).map(|i| &i.kind)
    }

    fn fn_decl(&self, def: &Def) -> Option<&'a ast::FnDecl> {
        match self.item_kind(def)? {
            ast::ItemKind::Fn(f) => Some(f),
            _ => None,
        }
    }

    // =======================================================================
    // Types
    // =======================================================================

    fn lower_ty(&mut self, t: &ast::Type) -> Ty {
        match &t.kind {
            ast::TypeKind::Named { path, generics } => {
                // `map<K, V>` and `set<T>` are written like named generics but
                // are built-in shapes (SPEC §13, §14).
                let name = path.last().name.as_str();
                match (name, generics.len()) {
                    ("map", 2) => {
                        let k = self.lower_ty(&generics[0]);
                        let v = self.lower_ty(&generics[1]);
                        return Ty::Map(Box::new(k), Box::new(v));
                    }
                    ("set", 1) => {
                        let inner = self.lower_ty(&generics[0]);
                        return Ty::Set(Box::new(inner));
                    }
                    ("map", _) | ("set", _) => {
                        self.error(
                            Diagnostic::error(
                                E_UNKNOWN_TYPE,
                                format!("`{name}` needs the right number of type arguments"),
                            )
                            .with_primary(t.span, "wrong number of type arguments")
                            .with_note("`map<K, V>` takes two, `set<T>` takes one"),
                        );
                        return Ty::Err;
                    }
                    _ => {}
                }
                if !generics.is_empty() {
                    self.error(
                        Diagnostic::error(
                            E_UNSUPPORTED,
                            "generic types are not implemented in this preview",
                        )
                        .with_primary(t.span, "generic arguments")
                        .with_note("SPEC §29 defines generics"),
                    );
                    return Ty::Err;
                }
                if path.is_single() {
                    if name == "void" {
                        return Ty::Void;
                    }
                    self.named_ty(name, t.span)
                } else {
                    match self.res.lookup_path(self.unit, path) {
                        Some(Res::Def(id)) => self.adt_ty(id, t.span),
                        _ => {
                            self.error(
                                Diagnostic::error(
                                    E_UNKNOWN_TYPE,
                                    format!("cannot find type `{}`", path.to_string_dotted()),
                                )
                                .with_primary(t.span, "not a type"),
                            );
                            Ty::Err
                        }
                    }
                }
            }
            ast::TypeKind::Array(inner) => Ty::Array(Box::new(self.lower_ty(inner))),
            ast::TypeKind::Map(k, v) => {
                let k = self.lower_ty(k);
                let v = self.lower_ty(v);
                Ty::Map(Box::new(k), Box::new(v))
            }
            ast::TypeKind::Set(inner) => Ty::Set(Box::new(self.lower_ty(inner))),
            ast::TypeKind::Tuple(items) => {
                Ty::Tuple(items.iter().map(|t| self.lower_ty(t)).collect())
            }
            ast::TypeKind::Optional(inner) => self.lower_ty(inner).into_optional(),
            ast::TypeKind::Fn { params, ret } => Ty::Fn {
                params: params.iter().map(|t| self.lower_ty(t)).collect(),
                ret: Box::new(match ret {
                    Some(r) => self.lower_ty(r),
                    None => Ty::Void,
                }),
            },
            ast::TypeKind::Void => Ty::Void,
            ast::TypeKind::Err => Ty::Err,
        }
    }

    /// Resolve a single type name.
    fn named_ty(&mut self, name: &str, span: Span) -> Ty {
        if let Some(p) = Prim::from_name(name) {
            return Ty::Prim(p);
        }
        match self.res.lookup(self.unit, name) {
            Some(Res::Prim(p)) => Ty::Prim(p),
            Some(Res::Def(id)) => self.adt_ty(id, span),
            _ => {
                self.error(
                    Diagnostic::error(E_UNKNOWN_TYPE, format!("cannot find type `{name}`"))
                        .with_primary(span, "not found")
                        .with_note("types are the primitives of SPEC §9, or a `struct`/`enum` \
                                    in scope"),
                );
                Ty::Err
            }
        }
    }

    fn adt_ty(&mut self, id: DefId, span: Span) -> Ty {
        let def = self.res.def(id);
        match def.kind {
            DefKind::Struct | DefKind::Enum => Ty::Adt { def: id, args: Vec::new() },
            DefKind::Interface => {
                self.error(
                    Diagnostic::error(
                        E_UNSUPPORTED,
                        "using an interface as a type is not implemented in this preview",
                    )
                    .with_primary(span, "interface type")
                    .with_note(
                        "SPEC §28 defines interfaces; the reference compiler resolves interface \
                         methods statically, so an interface cannot yet be a value's type",
                    ),
                );
                Ty::Err
            }
            other => {
                let name = def.name.clone();
                self.error(
                    Diagnostic::error(E_UNKNOWN_TYPE, format!("`{name}` is not a type"))
                        .with_primary(span, format!("this is a {}", other.describe())),
                );
                Ty::Err
            }
        }
    }

    // =======================================================================
    // Scopes and locals
    // =======================================================================

    fn declare_local(
        &mut self,
        name: &str,
        ty: Ty,
        mutable: bool,
        synthetic: bool,
        span: Span,
    ) -> LocalId {
        let id = LocalId(self.locals.len() as u32);
        self.locals.push(LocalInfo {
            id,
            name: name.to_string(),
            ty,
            mutable,
            synthetic,
            span,
        });
        if let Some(scope) = self.scopes.last_mut() {
            scope.insert(name.to_string(), id);
        }
        id
    }

    fn lookup_local(&self, name: &str) -> Option<LocalId> {
        self.scopes.iter().rev().find_map(|s| s.get(name).copied())
    }

    fn push_scope(&mut self) {
        self.scopes.push(HashMap::new());
    }

    fn pop_scope(&mut self) {
        self.scopes.pop();
    }

    /// Check an expression with no locals in scope, for struct field defaults
    /// and item-level constants.
    fn in_empty_scope<R>(&mut self, f: impl FnOnce(&mut Self) -> R) -> R {
        let scopes = std::mem::replace(&mut self.scopes, vec![HashMap::new()]);
        let locals = std::mem::take(&mut self.locals);
        let r = f(self);
        self.scopes = scopes;
        self.locals = locals;
        r
    }

    fn local_ty(&self, id: LocalId) -> Ty {
        self.locals[id.0 as usize].ty.clone()
    }

    // =======================================================================
    // Blocks and statements
    // =======================================================================

    fn check_block(&mut self, block: &ast::Block, want: Option<&Ty>) -> Block {
        self.push_scope();
        let out = self.check_block_no_scope(block, want);
        self.pop_scope();
        out
    }

    fn check_block_no_scope(&mut self, block: &ast::Block, want: Option<&Ty>) -> Block {
        let mut stmts = Vec::new();
        let mut tail = None;

        for stmt in &block.stmts {
            match &stmt.kind {
                ast::StmtKind::Tail(e) => {
                    tail = Some(Box::new(self.check_expr(e, want)));
                }
                _ => {
                    if let Some(s) = self.check_stmt(stmt) {
                        stmts.push(s);
                    }
                }
            }
        }

        let ty = tail.as_ref().map(|t: &Box<Expr>| t.ty.clone()).unwrap_or(Ty::Void);
        Block { stmts, tail, ty, span: block.span }
    }

    fn check_stmt(&mut self, stmt: &ast::Stmt) -> Option<Stmt> {
        let span = stmt.span;
        let kind = match &stmt.kind {
            ast::StmtKind::Let(l) => {
                let want = l.ty.as_ref().map(|t| self.lower_ty(t));
                let init = self.check_expr(&l.value, want.as_ref());
                let ty = want.unwrap_or_else(|| init.ty.clone());
                if ty.is_void() && !init.ty.is_err() {
                    self.error(
                        Diagnostic::error(
                            E_MISMATCH,
                            format!("`{}` would have type `void`", l.name.name),
                        )
                        .with_primary(l.value.span, "this produces no value")
                        .with_note("a variable must hold a value"),
                    );
                }
                let local = self.declare_local(&l.name.name, ty, true, false, l.name.span);
                StmtKind::Let { local, init: Box::new(init) }
            }

            ast::StmtKind::Const(c) => {
                let want = c.ty.as_ref().map(|t| self.lower_ty(t));
                let init = self.check_expr(&c.value, want.as_ref());
                let ty = want.unwrap_or_else(|| init.ty.clone());
                // A local constant is a local that may not be reassigned
                // (SPEC §8).
                let local = self.declare_local(&c.name.name, ty, false, false, c.name.span);
                StmtKind::Let { local, init: Box::new(init) }
            }

            ast::StmtKind::Assign(a) => return self.check_assign(a, span),

            ast::StmtKind::Expr(e) => {
                let expr = self.check_expr(e, None);
                StmtKind::Expr(Box::new(expr))
            }

            // Handled by the caller, which knows whether a value is wanted.
            ast::StmtKind::Tail(e) => {
                let expr = self.check_expr(e, None);
                StmtKind::Expr(Box::new(expr))
            }

            ast::StmtKind::Return(value) => {
                let ret = self.ret_ty.clone();
                match value {
                    Some(e) => {
                        if ret.is_void() {
                            let expr = self.check_expr(e, None);
                            self.error(
                                Diagnostic::error(
                                    E_MISMATCH,
                                    "this function returns no value, but `return` has one",
                                )
                                .with_primary(e.span, "unexpected value")
                                .with_note("declare a return type with `-> T` (SPEC §16)"),
                            );
                            StmtKind::Return(Some(Box::new(expr)))
                        } else {
                            let expr = self.check_expr(e, Some(&ret));
                            StmtKind::Return(Some(Box::new(expr)))
                        }
                    }
                    None => {
                        if !ret.is_void() && !ret.is_err() {
                            let rendered = self.hir.render(&ret);
                            self.error(
                                Diagnostic::error(
                                    E_MISMATCH,
                                    format!("this function must return `{rendered}`"),
                                )
                                .with_primary(span, "`return` with no value"),
                            );
                        }
                        StmtKind::Return(None)
                    }
                }
            }

            ast::StmtKind::Break(label) => match self.find_loop(label.as_ref(), span, "break") {
                Some(id) => StmtKind::Break(id),
                None => return None,
            },

            ast::StmtKind::Continue(label) => {
                match self.find_loop(label.as_ref(), span, "continue") {
                    Some(id) => StmtKind::Continue(id),
                    None => return None,
                }
            }

            ast::StmtKind::Defer(block) => {
                let b = self.check_block(block, None);
                StmtKind::Defer(Box::new(b))
            }

            ast::StmtKind::Item(item) => {
                self.error(
                    Diagnostic::error(
                        E_UNSUPPORTED,
                        "declarations inside a function body are not supported",
                    )
                    .with_primary(item.span, "move this to the top level of the file"),
                );
                return None;
            }

            ast::StmtKind::Err => return None,
        };
        Some(Stmt { kind, span })
    }

    fn check_assign(&mut self, a: &ast::AssignStmt, span: Span) -> Option<Stmt> {
        let place = self.check_expr(&a.target, None);

        if !place.is_place() && !place.ty.is_err() {
            self.error(
                Diagnostic::error(E_NOT_PLACE, "this cannot be assigned to")
                    .with_primary(a.target.span, "not a place")
                    .with_note("assign to a variable, a field, or an element (SPEC §7, §12)"),
            );
            return None;
        }

        // A `const` binding, or `self`, cannot be reassigned (SPEC §8).
        if let ExprKind::Local(id) = &place.kind {
            let info = &self.locals[id.0 as usize];
            if !info.mutable {
                let name = info.name.clone();
                self.error(
                    Diagnostic::error(E_IMMUTABLE, format!("`{name}` cannot be reassigned"))
                        .with_primary(a.target.span, "assignment to a constant")
                        .with_secondary(info.span, "declared here")
                        .with_note("`const` bindings are fixed; use `let` for a variable"),
                );
                return None;
            }
        }

        let want = place.ty.clone();
        let value = match a.op {
            ast::AssignOp::Assign => self.check_expr(&a.value, Some(&want)),
            other => {
                // `x += 1` becomes `x := x + 1` (SPEC §7).
                let op = match other.to_binop().expect("compound op") {
                    ast::BinOp::Add => BinOp::Add,
                    ast::BinOp::Sub => BinOp::Sub,
                    ast::BinOp::Mul => BinOp::Mul,
                    ast::BinOp::Div => BinOp::Div,
                    ast::BinOp::Rem => BinOp::Rem,
                    _ => unreachable!("compound assignment is arithmetic only"),
                };
                let rhs = self.check_expr(&a.value, Some(&want));
                let ty = self.check_binary_operands(op, &place, &rhs, span);
                Expr::new(
                    ExprKind::Binary { op, lhs: Box::new(place.clone()), rhs: Box::new(rhs) },
                    ty,
                    span,
                )
            }
        };

        Some(Stmt {
            kind: StmtKind::Assign { place: Box::new(place), value: Box::new(value) },
            span,
        })
    }

    fn find_loop(&mut self, label: Option<&ast::Ident>, span: Span, what: &str) -> Option<LoopId> {
        match label {
            None => match self.loops.last() {
                Some(f) => Some(f.id),
                None => {
                    self.error(
                        Diagnostic::error(
                            E_BREAK_OUTSIDE,
                            format!("`{what}` is only valid inside a loop"),
                        )
                        .with_primary(span, format!("`{what}` outside a loop")),
                    );
                    None
                }
            },
            Some(name) => {
                match self.loops.iter().rev().find(|f| f.label.as_deref() == Some(&name.name)) {
                    Some(f) => Some(f.id),
                    None => {
                        self.error(
                            Diagnostic::error(
                                E_UNKNOWN_LABEL,
                                format!("no loop is labelled `{}`", name.name),
                            )
                            .with_primary(name.span, "unknown label")
                            .with_note("label a loop by writing `name: for ...` (SPEC §22)"),
                        );
                        None
                    }
                }
            }
        }
    }

    // =======================================================================
    // Expressions
    // =======================================================================

    fn check_expr(&mut self, e: &ast::Expr, want: Option<&Ty>) -> Expr {
        let out = self.check_expr_inner(e, want);
        match want {
            Some(w) => self.coerce(out, w, e.span),
            None => out,
        }
    }

    fn err_expr(&self, span: Span) -> Expr {
        Expr::new(ExprKind::Err, Ty::Err, span)
    }

    fn check_expr_inner(&mut self, e: &ast::Expr, want: Option<&Ty>) -> Expr {
        let span = e.span;
        match &e.kind {
            // ---- literals (SPEC §9–§11) ----
            ast::ExprKind::Int { value, suffix } => {
                let ty = match suffix {
                    Some(s) => match Prim::from_suffix(s) {
                        Some(p) => Ty::Prim(p),
                        None => {
                            self.error(
                                Diagnostic::error(
                                    E_UNKNOWN_TYPE,
                                    format!("`{s}` is not a numeric type"),
                                )
                                .with_primary(span, "unknown suffix"),
                            );
                            Ty::Err
                        }
                    },
                    // An unsuffixed literal takes the expected numeric type,
                    // and `int` when there is none (SPEC §9).
                    None => match want.map(|w| w.unwrap_optional()) {
                        Some(Ty::Prim(p)) if p.is_numeric() => Ty::Prim(*p),
                        _ => Ty::INT,
                    },
                };
                if let Ty::Prim(p) = &ty {
                    if p.is_float() {
                        return Expr::new(ExprKind::Float(*value as f64), ty, span);
                    }
                    self.check_int_range(*value, *p, span);
                }
                Expr::new(ExprKind::Int(*value as i128), ty, span)
            }

            ast::ExprKind::Float { value, suffix } => {
                let ty = match suffix {
                    Some(s) => match Prim::from_suffix(s).filter(|p| p.is_float()) {
                        Some(p) => Ty::Prim(p),
                        None => {
                            self.error(
                                Diagnostic::error(
                                    E_UNKNOWN_TYPE,
                                    format!("`{s}` is not a float type"),
                                )
                                .with_primary(span, "unknown suffix"),
                            );
                            Ty::Err
                        }
                    },
                    None => match want.map(|w| w.unwrap_optional()) {
                        Some(Ty::Prim(p)) if p.is_float() => Ty::Prim(*p),
                        _ => Ty::FLOAT,
                    },
                };
                Expr::new(ExprKind::Float(*value), ty, span)
            }

            ast::ExprKind::Bool(b) => Expr::new(ExprKind::Bool(*b), Ty::BOOL, span),
            ast::ExprKind::Char(c) => Expr::new(ExprKind::Char(*c), Ty::CHAR, span),

            ast::ExprKind::Str(parts) => self.check_str(parts, span),

            ast::ExprKind::Null => match want {
                Some(Ty::Optional(_)) => Expr::new(ExprKind::Null, want.unwrap().clone(), span),
                Some(other) if !other.is_err() => {
                    let rendered = self.hir.render(other);
                    self.error(
                        Diagnostic::error(
                            E_NULL_TYPE,
                            format!("`null` is not a value of type `{rendered}`"),
                        )
                        .with_primary(span, "null here")
                        .with_note("only an optional type may hold null (SPEC §30)")
                        .with_suggestion(
                            "make the type optional",
                            span,
                            format!("{rendered}?"),
                        ),
                    );
                    self.err_expr(span)
                }
                _ => {
                    self.error(
                        Diagnostic::error(E_NULL_TYPE, "the type of this `null` is unknown")
                            .with_primary(span, "cannot infer a type")
                            .with_note("annotate the variable, e.g. `let str? name := null`"),
                    );
                    self.err_expr(span)
                }
            },

            // ---- names ----
            ast::ExprKind::Path(path) => self.check_path_expr(path, span),

            ast::ExprKind::SelfExpr => match self.self_local {
                Some(id) => Expr::new(ExprKind::Local(id), self.local_ty(id), span),
                None => {
                    self.error(
                        Diagnostic::error(E_SELF_OUTSIDE, "`self` is only valid inside a method")
                            .with_primary(span, "no receiver here")
                            .with_note("write a method as `fn Type.name()` (SPEC §27)"),
                    );
                    self.err_expr(span)
                }
            },

            // ---- collections ----
            ast::ExprKind::Array(items) => self.check_array(items, want, span),
            ast::ExprKind::Map(entries) => self.check_map(entries, want, span),
            ast::ExprKind::Set(items) => self.check_set(items, want, span),
            ast::ExprKind::Tuple(items) => self.check_tuple(items, want, span),
            ast::ExprKind::StructLit { path, generics, fields } => {
                self.check_struct_lit(path, generics, fields, span)
            }

            // ---- operators ----
            ast::ExprKind::Unary { op, operand } => self.check_unary(*op, operand, span),
            ast::ExprKind::Binary { op, lhs, rhs } => self.check_binary(*op, lhs, rhs, span),
            ast::ExprKind::Coalesce { lhs, rhs } => self.check_coalesce(lhs, rhs, want, span),
            ast::ExprKind::Range { start, end, inclusive } => {
                self.check_range(start.as_deref(), end.as_deref(), *inclusive, span)
            }

            // ---- access ----
            ast::ExprKind::Field { base, name } => self.check_field(e, base, name, span),
            ast::ExprKind::OptionalField { base, name } => {
                self.check_optional_field(base, name, span)
            }
            ast::ExprKind::TupleField { base, index, .. } => {
                self.check_tuple_field(base, *index, span)
            }
            ast::ExprKind::Index { base, index } => self.check_index(base, index, span),

            // ---- calls ----
            ast::ExprKind::Call { callee, args, .. } => self.check_call(callee, args, span),
            ast::ExprKind::VariantCtor { .. } => {
                // Only the resolver produces this, and this compiler builds it
                // directly in `check_call`.
                self.err_expr(span)
            }

            // ---- control flow ----
            ast::ExprKind::If { cond, then, else_branch } => {
                self.check_if(cond, then, else_branch.as_deref(), want, span)
            }
            ast::ExprKind::Match { scrutinee, arms } => {
                self.check_match(scrutinee, arms, want, span)
            }
            ast::ExprKind::Block(b) => {
                let block = self.check_block(b, want);
                let ty = block.ty.clone();
                Expr::new(ExprKind::Block(block), ty, span)
            }
            ast::ExprKind::For { label, pat, iter, body } => {
                self.check_for(label.as_ref(), pat, iter, body, span)
            }
            ast::ExprKind::While { label, cond, body } => {
                self.check_while(label.as_ref(), cond, body, span)
            }
            ast::ExprKind::Loop { label, body } => self.check_loop(label.as_ref(), body, span),

            // ---- effects ----
            ast::ExprKind::Try { body, catches } => self.check_try(body, catches, want, span),
            ast::ExprKind::Unsafe(b) => {
                let block = self.check_block(b, want);
                let ty = block.ty.clone();
                Expr::new(ExprKind::Unsafe(block), ty, span)
            }
            ast::ExprKind::Await(_) | ast::ExprKind::Spawn(_) => {
                self.error(
                    Diagnostic::error(
                        E_UNSUPPORTED,
                        "`async`, `await` and `spawn` are not implemented in this preview",
                    )
                    .with_primary(span, "concurrency is not available")
                    .with_note("SPEC §68 and §69 define the concurrency model"),
                );
                self.err_expr(span)
            }

            ast::ExprKind::Err => self.err_expr(span),
        }
    }

    fn check_int_range(&mut self, value: u128, p: Prim, span: Span) {
        let too_big = if p.is_signed_int() {
            p.signed_range().is_some_and(|(_, max)| value > max as u128)
        } else {
            p.unsigned_max().is_some_and(|max| value > max)
        };
        if too_big {
            self.error(
                Diagnostic::error(
                    E_INT_RANGE,
                    format!("`{value}` does not fit in `{}`", p.name()),
                )
                .with_primary(span, "value out of range")
                .with_note(format!("`{}` is {} bits wide", p.name(), p.bit_width().unwrap_or(0))),
            );
        }
    }

    /// String literals, including interpolation (SPEC §11).
    fn check_str(&mut self, parts: &[ast::StrSegment], span: Span) -> Expr {
        // A literal with no interpolation stays a plain constant.
        if parts.len() == 1 {
            if let ast::StrSegment::Literal(s) = &parts[0] {
                return Expr::new(ExprKind::Str(s.clone()), Ty::STR, span);
            }
        }
        if parts.is_empty() {
            return Expr::new(ExprKind::Str(String::new()), Ty::STR, span);
        }

        let mut pieces = Vec::new();
        for part in parts {
            match part {
                ast::StrSegment::Literal(s) => {
                    pieces.push(Expr::new(ExprKind::Str(s.clone()), Ty::STR, span));
                }
                ast::StrSegment::Interp(e) => {
                    let checked = self.check_expr(e, None);
                    pieces.push(self.to_str(checked));
                }
            }
        }
        Expr::new(ExprKind::Concat(pieces), Ty::STR, span)
    }

    /// Convert any value to `str`, as interpolation and `print` require.
    fn to_str(&mut self, e: Expr) -> Expr {
        if e.ty.is_str() || e.ty.is_err() {
            return e;
        }
        let span = e.span;
        Expr::new(ExprKind::Builtin { builtin: Builtin::ToStr, args: vec![e] }, Ty::STR, span)
    }

    fn check_path_expr(&mut self, path: &ast::Path, span: Span) -> Expr {
        if path.is_single() {
            let name = &path.last().name;
            if let Some(id) = self.lookup_local(name) {
                return Expr::new(ExprKind::Local(id), self.local_ty(id), span);
            }
        }
        match self.res.lookup_path(self.unit, path) {
            Some(Res::Def(id)) => self.def_as_value(id, span),
            Some(Res::Variant(def, idx)) => self.variant_value(def, idx, &[], span),
            Some(Res::Builtin(_)) => {
                self.error(
                    Diagnostic::error(
                        E_UNSUPPORTED,
                        format!("`{}` must be called", path.to_string_dotted()),
                    )
                    .with_primary(span, "a builtin cannot be used as a value")
                    .with_suggestion(
                        "call it",
                        span,
                        format!("call {}(...)", path.to_string_dotted()),
                    ),
                );
                self.err_expr(span)
            }
            Some(Res::Module(_)) | Some(Res::StdModule(_)) => {
                self.error(
                    Diagnostic::error(
                        E_UNKNOWN_NAME,
                        format!("`{}` is a module, not a value", path.to_string_dotted()),
                    )
                    .with_primary(span, "module used as a value"),
                );
                self.err_expr(span)
            }
            Some(Res::Prim(p)) => {
                self.error(
                    Diagnostic::error(
                        E_UNKNOWN_NAME,
                        format!("`{}` is a type, not a value", p.name()),
                    )
                    .with_primary(span, "type used as a value"),
                );
                self.err_expr(span)
            }
            None => {
                let name = path.to_string_dotted();
                let diag = self.res.unknown_name_error(self.unit, &name, span);
                self.error(diag);
                self.err_expr(span)
            }
        }
    }

    fn def_as_value(&mut self, id: DefId, span: Span) -> Expr {
        let def = self.res.def(id).clone();
        match def.kind {
            DefKind::Const => match self.consts.get(&id) {
                Some(v) => {
                    let mut v = v.clone();
                    v.span = span;
                    v
                }
                None => {
                    self.error(
                        Diagnostic::error(
                            E_UNKNOWN_NAME,
                            format!("`{}` is used before it is defined", def.name),
                        )
                        .with_primary(span, "constant not yet available")
                        .with_secondary(def.span, "declared here")
                        .with_note("a constant may only refer to constants declared before it"),
                    );
                    self.err_expr(span)
                }
            },
            DefKind::Fn => {
                let sig = self.sigs.get(&id).cloned();
                match sig {
                    Some(s) => Expr::new(
                        ExprKind::FnRef(id),
                        Ty::Fn { params: s.params, ret: Box::new(s.ret) },
                        span,
                    ),
                    None => self.err_expr(span),
                }
            }
            other => {
                self.error(
                    Diagnostic::error(
                        E_UNKNOWN_NAME,
                        format!("`{}` is a {}, not a value", def.name, other.describe()),
                    )
                    .with_primary(span, "not a value"),
                );
                self.err_expr(span)
            }
        }
    }

    /// `Color.RED`, or `Message.TEXT("hi")` when arguments are supplied.
    fn variant_value(
        &mut self,
        def: DefId,
        idx: usize,
        args: &[ast::Expr],
        span: Span,
    ) -> Expr {
        let Some(en) = self.hir.enums.get(&def).cloned() else {
            return self.err_expr(span);
        };
        let variant = &en.variants[idx];
        let ty = Ty::Adt { def, args: Vec::new() };

        if variant.payload.len() != args.len() {
            self.error(
                Diagnostic::error(
                    E_ARITY,
                    format!(
                        "`{}.{}` takes {} value{}, but {} {} given",
                        en.name,
                        variant.name,
                        variant.payload.len(),
                        if variant.payload.len() == 1 { "" } else { "s" },
                        args.len(),
                        if args.len() == 1 { "was" } else { "were" }
                    ),
                )
                .with_primary(span, "wrong number of values")
                .with_secondary(variant.span, "variant declared here"),
            );
            return self.err_expr(span);
        }

        let payload = variant.payload.clone();
        let lowered: Vec<Expr> = args
            .iter()
            .zip(payload.iter())
            .map(|(a, want)| self.check_expr(a, Some(want)))
            .collect();

        Expr::new(ExprKind::EnumLit { def, variant: idx, args: lowered }, ty, span)
    }

    // ---- collections ----

    fn check_array(&mut self, items: &[ast::Expr], want: Option<&Ty>, span: Span) -> Expr {
        let elem_want = match want.map(|w| w.unwrap_optional()) {
            Some(Ty::Array(t)) => Some((**t).clone()),
            _ => None,
        };

        if items.is_empty() {
            return match elem_want {
                Some(t) => Expr::new(ExprKind::Array(Vec::new()), Ty::Array(Box::new(t)), span),
                None => {
                    self.error(
                        Diagnostic::error(
                            E_MISMATCH,
                            "the element type of this empty array is unknown",
                        )
                        .with_primary(span, "cannot infer a type")
                        .with_note("annotate the variable, e.g. `let int[] xs := []`"),
                    );
                    self.err_expr(span)
                }
            };
        }

        let mut lowered = Vec::with_capacity(items.len());
        // The first element fixes the element type when none was expected.
        let first = self.check_expr(&items[0], elem_want.as_ref());
        let elem = elem_want.unwrap_or_else(|| first.ty.clone());
        lowered.push(first);
        for item in &items[1..] {
            lowered.push(self.check_expr(item, Some(&elem)));
        }
        Expr::new(ExprKind::Array(lowered), Ty::Array(Box::new(elem)), span)
    }

    fn check_map(&mut self, entries: &[(ast::Expr, ast::Expr)], want: Option<&Ty>, span: Span) -> Expr {
        let (kw, vw) = match want.map(|w| w.unwrap_optional()) {
            Some(Ty::Map(k, v)) => (Some((**k).clone()), Some((**v).clone())),
            _ => (None, None),
        };

        if entries.is_empty() {
            return match (kw, vw) {
                (Some(k), Some(v)) => Expr::new(
                    ExprKind::Map(Vec::new()),
                    Ty::Map(Box::new(k), Box::new(v)),
                    span,
                ),
                _ => {
                    self.error(
                        Diagnostic::error(E_MISMATCH, "the type of this empty map is unknown")
                            .with_primary(span, "cannot infer a type")
                            .with_note("annotate it, e.g. `let map<str, int> m := {}`"),
                    );
                    self.err_expr(span)
                }
            };
        }

        let (k0, v0) = &entries[0];
        let fk = self.check_expr(k0, kw.as_ref());
        let fv = self.check_expr(v0, vw.as_ref());
        let kt = kw.unwrap_or_else(|| fk.ty.clone());
        let vt = vw.unwrap_or_else(|| fv.ty.clone());
        self.require_hashable(&kt, k0.span);

        let mut lowered = vec![(fk, fv)];
        for (k, v) in &entries[1..] {
            let k = self.check_expr(k, Some(&kt));
            let v = self.check_expr(v, Some(&vt));
            lowered.push((k, v));
        }
        Expr::new(ExprKind::Map(lowered), Ty::Map(Box::new(kt), Box::new(vt)), span)
    }

    fn check_set(&mut self, items: &[ast::Expr], want: Option<&Ty>, span: Span) -> Expr {
        let elem_want = match want.map(|w| w.unwrap_optional()) {
            Some(Ty::Set(t)) => Some((**t).clone()),
            _ => None,
        };

        if items.is_empty() {
            return match elem_want {
                Some(t) => Expr::new(ExprKind::Set(Vec::new()), Ty::Set(Box::new(t)), span),
                None => {
                    self.error(
                        Diagnostic::error(E_MISMATCH, "the element type of this set is unknown")
                            .with_primary(span, "cannot infer a type")
                            .with_note("annotate it, e.g. `let set<str> s := {}`"),
                    );
                    self.err_expr(span)
                }
            };
        }

        let first = self.check_expr(&items[0], elem_want.as_ref());
        let elem = elem_want.unwrap_or_else(|| first.ty.clone());
        self.require_hashable(&elem, items[0].span);

        let mut lowered = vec![first];
        for item in &items[1..] {
            lowered.push(self.check_expr(item, Some(&elem)));
        }
        Expr::new(ExprKind::Set(lowered), Ty::Set(Box::new(elem)), span)
    }

    /// Map keys and set elements must be comparable by value.
    fn require_hashable(&mut self, ty: &Ty, span: Span) {
        let ok = matches!(ty, Ty::Prim(_)) || ty.is_err();
        if !ok {
            let rendered = self.hir.render(ty);
            self.error(
                Diagnostic::error(
                    E_MISMATCH,
                    format!("`{rendered}` cannot be a map key or set element"),
                )
                .with_primary(span, "not comparable by value")
                .with_note("keys must be a primitive type (SPEC §9, §13, §14)"),
            );
        }
    }

    fn check_tuple(&mut self, items: &[ast::Expr], want: Option<&Ty>, span: Span) -> Expr {
        let wants: Option<Vec<Ty>> = match want.map(|w| w.unwrap_optional()) {
            Some(Ty::Tuple(ts)) if ts.len() == items.len() => Some(ts.clone()),
            _ => None,
        };
        let mut lowered = Vec::with_capacity(items.len());
        for (i, item) in items.iter().enumerate() {
            let w = wants.as_ref().map(|ts| ts[i].clone());
            lowered.push(self.check_expr(item, w.as_ref()));
        }
        let ty = Ty::Tuple(lowered.iter().map(|e| e.ty.clone()).collect());
        Expr::new(ExprKind::Tuple(lowered), ty, span)
    }

    /// A struct literal, completed with field defaults (SPEC §23, §24).
    fn check_struct_lit(
        &mut self,
        path: &ast::Path,
        generics: &[ast::Type],
        inits: &[ast::FieldInit],
        span: Span,
    ) -> Expr {
        if !generics.is_empty() {
            self.error(
                Diagnostic::error(
                    E_UNSUPPORTED,
                    "generic types are not implemented in this preview",
                )
                .with_primary(span, "generic arguments"),
            );
            return self.err_expr(span);
        }

        let res = self.res.lookup_path(self.unit, path);
        let Some(Res::Def(id)) = res else {
            let name = path.to_string_dotted();
            let diag = self.res.unknown_name_error(self.unit, &name, path.span);
            self.error(diag);
            return self.err_expr(span);
        };
        let Some(sd) = self.hir.structs.get(&id).cloned() else {
            let name = self.res.def(id).name.clone();
            self.error(
                Diagnostic::error(E_UNKNOWN_TYPE, format!("`{name}` is not a struct"))
                    .with_primary(path.span, "not a struct"),
            );
            return self.err_expr(span);
        };

        let mut given: Vec<Option<Expr>> = vec![None; sd.fields.len()];
        let mut seen: Vec<Option<Span>> = vec![None; sd.fields.len()];

        for init in inits {
            let Some(idx) = sd.field_index(&init.name.name) else {
                let mut diag = Diagnostic::error(
                    E_UNKNOWN_FIELD,
                    format!("`{}` has no field named `{}`", sd.name, init.name.name),
                )
                .with_primary(init.name.span, "unknown field")
                .with_secondary(sd.span, "struct declared here");
                if let Some(close) = closest(
                    &init.name.name,
                    sd.fields.iter().map(|f| f.name.as_str()),
                ) {
                    diag = diag.with_suggestion("a field with a similar name exists", init.name.span, close);
                }
                self.error(diag);
                continue;
            };
            if let Some(prev) = seen[idx] {
                self.error(
                    Diagnostic::error(
                        E_DUP_FIELD,
                        format!("field `{}` is given twice", init.name.name),
                    )
                    .with_primary(init.name.span, "second value")
                    .with_secondary(prev, "first value"),
                );
                continue;
            }
            seen[idx] = Some(init.name.span);
            let want = sd.fields[idx].ty.clone();
            given[idx] = Some(self.check_expr(&init.value, Some(&want)));
        }

        // Fill in defaults, and report anything still missing (SPEC §24).
        let mut missing = Vec::new();
        let mut fields = Vec::with_capacity(sd.fields.len());
        for (idx, f) in sd.fields.iter().enumerate() {
            match given[idx].take() {
                Some(v) => fields.push(v),
                None => match &f.default {
                    Some(d) => fields.push(d.clone()),
                    None => {
                        missing.push(f.name.clone());
                        fields.push(self.err_expr(span));
                    }
                },
            }
        }

        if !missing.is_empty() {
            let list = missing
                .iter()
                .map(|m| format!("`{m}`"))
                .collect::<Vec<_>>()
                .join(", ");
            self.error(
                Diagnostic::error(
                    E_MISSING_FIELD,
                    format!(
                        "missing field{} {list} in `{}`",
                        if missing.len() == 1 { "" } else { "s" },
                        sd.name
                    ),
                )
                .with_primary(span, "incomplete struct literal")
                .with_secondary(sd.span, "struct declared here")
                .with_note("give the field a value, or a default in the struct (SPEC §24)"),
            );
        }

        Expr::new(
            ExprKind::StructLit { def: id, fields },
            Ty::Adt { def: id, args: Vec::new() },
            span,
        )
    }

    // ---- operators ----

    fn check_unary(&mut self, op: ast::UnOp, operand: &ast::Expr, span: Span) -> Expr {
        let want = match op {
            ast::UnOp::Not => Some(Ty::BOOL),
            _ => None,
        };
        let e = self.check_expr(operand, want.as_ref());
        if e.ty.is_err() {
            return self.err_expr(span);
        }

        let (hop, ty) = match op {
            ast::UnOp::Neg => {
                if !e.ty.is_numeric() {
                    let rendered = self.hir.render(&e.ty);
                    self.error(
                        Diagnostic::error(E_BAD_OPERAND, format!("cannot negate `{rendered}`"))
                            .with_primary(span, "not a number"),
                    );
                    return self.err_expr(span);
                }
                (UnOp::Neg, e.ty.clone())
            }
            ast::UnOp::Not => (UnOp::Not, Ty::BOOL),
            ast::UnOp::BitNot => {
                if !e.ty.is_integer() {
                    let rendered = self.hir.render(&e.ty);
                    self.error(
                        Diagnostic::error(
                            E_BAD_OPERAND,
                            format!("cannot apply `~` to `{rendered}`"),
                        )
                        .with_primary(span, "not an integer"),
                    );
                    return self.err_expr(span);
                }
                (UnOp::BitNot, e.ty.clone())
            }
        };
        Expr::new(ExprKind::Unary { op: hop, operand: Box::new(e) }, ty, span)
    }

    fn check_binary(
        &mut self,
        op: ast::BinOp,
        lhs: &ast::Expr,
        rhs: &ast::Expr,
        span: Span,
    ) -> Expr {
        // `&&` and `||` short-circuit, so they are their own node (SPEC §10).
        if op.is_logical() {
            let l = self.check_expr(lhs, Some(&Ty::BOOL));
            let r = self.check_expr(rhs, Some(&Ty::BOOL));
            return Expr::new(
                ExprKind::Logical {
                    and: op == ast::BinOp::And,
                    lhs: Box::new(l),
                    rhs: Box::new(r),
                },
                Ty::BOOL,
                span,
            );
        }

        let l = self.check_expr(lhs, None);
        // The left operand's type guides the right, so `1.5 * 2` works.
        let want = if l.ty.is_err() { None } else { Some(l.ty.clone()) };
        let r = self.check_expr(rhs, want.as_ref());

        let hop = match op {
            ast::BinOp::Add => BinOp::Add,
            ast::BinOp::Sub => BinOp::Sub,
            ast::BinOp::Mul => BinOp::Mul,
            ast::BinOp::Div => BinOp::Div,
            ast::BinOp::Rem => BinOp::Rem,
            ast::BinOp::Eq => BinOp::Eq,
            ast::BinOp::Ne => BinOp::Ne,
            ast::BinOp::Lt => BinOp::Lt,
            ast::BinOp::Le => BinOp::Le,
            ast::BinOp::Gt => BinOp::Gt,
            ast::BinOp::Ge => BinOp::Ge,
            ast::BinOp::BitAnd => BinOp::BitAnd,
            ast::BinOp::BitOr => BinOp::BitOr,
            ast::BinOp::BitXor => BinOp::BitXor,
            ast::BinOp::Shl => BinOp::Shl,
            ast::BinOp::Shr => BinOp::Shr,
            ast::BinOp::And | ast::BinOp::Or => unreachable!("handled above"),
        };

        let ty = self.check_binary_operands(hop, &l, &r, span);
        Expr::new(ExprKind::Binary { op: hop, lhs: Box::new(l), rhs: Box::new(r) }, ty, span)
    }

    /// Decide the result type of a binary operator, reporting bad operands.
    fn check_binary_operands(&mut self, op: BinOp, l: &Expr, r: &Expr, span: Span) -> Ty {
        if l.ty.is_err() || r.ty.is_err() {
            return Ty::Err;
        }

        use BinOp::*;
        match op {
            // `+` also concatenates strings (SPEC §11).
            Add if l.ty.is_str() && r.ty.is_str() => Ty::STR,

            Add | Sub | Mul | Div | Rem => {
                if !l.ty.is_numeric() || !r.ty.is_numeric() {
                    self.operand_error(op, l, r, span, "numbers");
                    return Ty::Err;
                }
                if l.ty != r.ty {
                    self.mixed_numeric_error(op, l, r, span);
                    return Ty::Err;
                }
                if matches!(op, Rem) && l.ty.is_float() {
                    self.error(
                        Diagnostic::error(E_BAD_OPERAND, "`%` requires integers")
                            .with_primary(span, "float remainder is not defined"),
                    );
                    return Ty::Err;
                }
                l.ty.clone()
            }

            Eq | Ne => {
                if l.ty != r.ty {
                    self.mixed_numeric_error(op, l, r, span);
                    return Ty::Err;
                }
                Ty::BOOL
            }

            Lt | Le | Gt | Ge => {
                let ordered = l.ty.is_numeric() || l.ty.is_str() || matches!(l.ty, Ty::Prim(Prim::Char));
                if !ordered {
                    self.operand_error(op, l, r, span, "numbers, strings or characters");
                    return Ty::Err;
                }
                if l.ty != r.ty {
                    self.mixed_numeric_error(op, l, r, span);
                    return Ty::Err;
                }
                Ty::BOOL
            }

            BitAnd | BitOr | BitXor | Shl | Shr => {
                if !l.ty.is_integer() || !r.ty.is_integer() {
                    self.operand_error(op, l, r, span, "integers");
                    return Ty::Err;
                }
                l.ty.clone()
            }
        }
    }

    fn operand_error(&mut self, op: BinOp, l: &Expr, r: &Expr, span: Span, need: &str) {
        let lt = self.hir.render(&l.ty);
        let rt = self.hir.render(&r.ty);
        self.error(
            Diagnostic::error(
                E_BAD_OPERAND,
                format!("`{}` cannot be applied to `{lt}` and `{rt}`", op.as_str()),
            )
            .with_primary(span, format!("`{}` needs {need}", op.as_str()))
            .with_secondary(l.span, format!("this is `{lt}`"))
            .with_secondary(r.span, format!("this is `{rt}`")),
        );
    }

    fn mixed_numeric_error(&mut self, op: BinOp, l: &Expr, r: &Expr, span: Span) {
        let lt = self.hir.render(&l.ty);
        let rt = self.hir.render(&r.ty);
        let mut diag = Diagnostic::error(
            E_MISMATCH,
            format!("`{}` needs both sides to have the same type", op.as_str()),
        )
        .with_primary(span, format!("`{lt}` and `{rt}`"))
        .with_secondary(l.span, format!("this is `{lt}`"))
        .with_secondary(r.span, format!("this is `{rt}`"));

        if l.ty.is_numeric() && r.ty.is_numeric() {
            diag = diag
                .with_note("L does not convert between numeric types implicitly")
                .with_suggestion(
                    "convert explicitly",
                    r.span,
                    format!("call {lt}(...)"),
                );
        }
        self.error(diag);
    }

    /// `a ?? b` (SPEC §30).
    fn check_coalesce(
        &mut self,
        lhs: &ast::Expr,
        rhs: &ast::Expr,
        want: Option<&Ty>,
        span: Span,
    ) -> Expr {
        let l = self.check_expr(lhs, None);
        if l.ty.is_err() {
            return self.err_expr(span);
        }
        let Ty::Optional(inner) = l.ty.clone() else {
            let rendered = self.hir.render(&l.ty);
            self.error(
                Diagnostic::error(
                    E_NOT_OPTIONAL,
                    format!("`??` needs an optional on the left, but this is `{rendered}`"),
                )
                .with_primary(lhs.span, "never null")
                .with_note("an optional type is written `T?` (SPEC §30)"),
            );
            return self.err_expr(span);
        };
        let want = want.cloned().unwrap_or((*inner).clone());
        let r = self.check_expr(rhs, Some(&want));
        Expr::new(
            ExprKind::Coalesce { lhs: Box::new(l), rhs: Box::new(r) },
            want,
            span,
        )
    }

    /// `0..10` and `0..=10` (SPEC §19).
    fn check_range(
        &mut self,
        start: Option<&ast::Expr>,
        end: Option<&ast::Expr>,
        inclusive: bool,
        span: Span,
    ) -> Expr {
        let (Some(start), Some(end)) = (start, end) else {
            self.error(
                Diagnostic::error(E_UNSUPPORTED, "a range needs both a start and an end")
                    .with_primary(span, "open-ended range")
                    .with_note("SPEC §19 defines `a..b` and `a..=b`"),
            );
            return self.err_expr(span);
        };
        let s = self.check_expr(start, None);
        let e = self.check_expr(end, Some(&s.ty));
        if !s.ty.is_integer() && !s.ty.is_err() {
            let rendered = self.hir.render(&s.ty);
            self.error(
                Diagnostic::error(E_BAD_OPERAND, format!("a range needs integers, not `{rendered}`"))
                    .with_primary(start.span, "not an integer"),
            );
            return self.err_expr(span);
        }
        let ty = Ty::Range(Box::new(s.ty.clone()));

        // `a..=b` is normalised to the half-open `a..(b + 1)` so that nothing
        // downstream has to carry the inclusive flag.
        let end = if inclusive {
            let one = Expr::new(ExprKind::Int(1), e.ty.clone(), span);
            Expr::new(
                ExprKind::Binary { op: BinOp::Add, lhs: Box::new(e), rhs: Box::new(one) },
                s.ty.clone(),
                span,
            )
        } else {
            e
        };

        Expr::new(
            ExprKind::Range { start: Box::new(s), end: Box::new(end) },
            ty,
            span,
        )
    }

    // ---- access ----

    /// `a.b` — which may be a field, a property, an enum variant, or a name in
    /// another module. The parser cannot tell them apart, so this does.
    fn check_field(
        &mut self,
        whole: &ast::Expr,
        base: &ast::Expr,
        name: &ast::Ident,
        span: Span,
    ) -> Expr {
        // If the whole expression is a chain of plain names, it may be a path.
        if let Some(path) = flatten_path(whole) {
            let leading = &path.segments[0].name;
            if self.lookup_local(leading).is_none() {
                match self.res.lookup_path(self.unit, &path) {
                    Some(Res::Variant(def, idx)) => return self.variant_value(def, idx, &[], span),
                    Some(Res::Def(id)) => return self.def_as_value(id, span),
                    _ => {}
                }
            }
        }

        let b = self.check_expr(base, None);
        self.field_of(b, name, span)
    }

    fn field_of(&mut self, b: Expr, name: &ast::Ident, span: Span) -> Expr {
        if b.ty.is_err() {
            return self.err_expr(span);
        }

        // `.length` on every sized type (SPEC §12).
        if name.name == "length" {
            if matches!(b.ty, Ty::Array(_) | Ty::Map(_, _) | Ty::Set(_)) || b.ty.is_str() {
                return Expr::new(
                    ExprKind::Property { base: Box::new(b), prop: Property::Length },
                    Ty::INT,
                    span,
                );
            }
        }

        if let Ty::Adt { def, .. } = &b.ty {
            if let Some(sd) = self.hir.structs.get(def).cloned() {
                if let Some(idx) = sd.field_index(&name.name) {
                    let ty = sd.fields[idx].ty.clone();
                    return Expr::new(
                        ExprKind::Field { base: Box::new(b), index: idx },
                        ty,
                        span,
                    );
                }
                let mut diag = Diagnostic::error(
                    E_NO_FIELD,
                    format!("`{}` has no field named `{}`", sd.name, name.name),
                )
                .with_primary(name.span, "unknown field")
                .with_secondary(sd.span, "struct declared here");
                if let Some(close) =
                    closest(&name.name, sd.fields.iter().map(|f| f.name.as_str()))
                {
                    diag = diag.with_suggestion("a field with a similar name exists", name.span, close);
                }
                if self.res.method(&sd.name, &name.name).is_some() {
                    diag = diag
                        .with_note(format!("`{}` is a method — call it with `()`", name.name));
                }
                self.error(diag);
                return self.err_expr(span);
            }
        }

        let rendered = self.hir.render(&b.ty);
        self.error(
            Diagnostic::error(
                E_NO_FIELD,
                format!("`{rendered}` has no field named `{}`", name.name),
            )
            .with_primary(name.span, "unknown field"),
        );
        self.err_expr(span)
    }

    /// `name?.length` (SPEC §30) — evaluates to an optional.
    fn check_optional_field(&mut self, base: &ast::Expr, name: &ast::Ident, span: Span) -> Expr {
        let b = self.check_expr(base, None);
        if b.ty.is_err() {
            return self.err_expr(span);
        }
        let Ty::Optional(inner) = b.ty.clone() else {
            let rendered = self.hir.render(&b.ty);
            self.error(
                Diagnostic::error(
                    E_NOT_OPTIONAL,
                    format!("`?.` needs an optional, but this is `{rendered}`"),
                )
                .with_primary(base.span, "never null")
                .with_suggestion("use plain field access", name.span.shrink_to_lo(), "."),
            );
            return self.err_expr(span);
        };

        // Desugars to: if the optional holds a value, the field of that value,
        // otherwise null. MIR gets a plain `If`.
        let unwrapped = Expr::new(
            ExprKind::Cast { expr: Box::new(b.clone()), to: (*inner).clone() },
            (*inner).clone(),
            span,
        );
        let field = self.field_of(unwrapped, name, span);
        if field.ty.is_err() {
            return self.err_expr(span);
        }
        let result_ty = field.ty.clone().into_optional();

        let has_value = Expr::new(
            ExprKind::Binary {
                op: BinOp::Ne,
                lhs: Box::new(b),
                rhs: Box::new(Expr::new(ExprKind::Null, Ty::Void.into_optional(), span)),
            },
            Ty::BOOL,
            span,
        );
        let then = Block {
            stmts: Vec::new(),
            tail: Some(Box::new(Expr::new(
                ExprKind::Cast { expr: Box::new(field), to: result_ty.clone() },
                result_ty.clone(),
                span,
            ))),
            ty: result_ty.clone(),
            span,
        };
        let els = Block {
            stmts: Vec::new(),
            tail: Some(Box::new(Expr::new(ExprKind::Null, result_ty.clone(), span))),
            ty: result_ty.clone(),
            span,
        };
        Expr::new(
            ExprKind::If { cond: Box::new(has_value), then, els: Some(els) },
            result_ty,
            span,
        )
    }

    fn check_tuple_field(&mut self, base: &ast::Expr, index: u32, span: Span) -> Expr {
        let b = self.check_expr(base, None);
        if b.ty.is_err() {
            return self.err_expr(span);
        }
        let Ty::Tuple(items) = b.ty.clone() else {
            let rendered = self.hir.render(&b.ty);
            self.error(
                Diagnostic::error(E_NO_FIELD, format!("`{rendered}` is not a tuple"))
                    .with_primary(span, "numbered access needs a tuple")
                    .with_note("tuples are written `(a, b)` (SPEC §15)"),
            );
            return self.err_expr(span);
        };
        let idx = index as usize;
        if idx >= items.len() {
            self.error(
                Diagnostic::error(
                    E_NO_FIELD,
                    format!("this tuple has {} elements, so `.{index}` is out of range", items.len()),
                )
                .with_primary(span, "index out of range"),
            );
            return self.err_expr(span);
        }
        let ty = items[idx].clone();
        Expr::new(ExprKind::TupleField { base: Box::new(b), index: idx }, ty, span)
    }

    fn check_index(&mut self, base: &ast::Expr, index: &ast::Expr, span: Span) -> Expr {
        let b = self.check_expr(base, None);
        if b.ty.is_err() {
            return self.err_expr(span);
        }
        match b.ty.clone() {
            Ty::Array(elem) => {
                let i = self.check_expr(index, Some(&Ty::INT));
                Expr::new(
                    ExprKind::Index { base: Box::new(b), index: Box::new(i) },
                    *elem,
                    span,
                )
            }
            Ty::Map(k, v) => {
                let i = self.check_expr(index, Some(&k));
                Expr::new(
                    ExprKind::Index { base: Box::new(b), index: Box::new(i) },
                    *v,
                    span,
                )
            }
            Ty::Prim(Prim::Str) => {
                let i = self.check_expr(index, Some(&Ty::INT));
                Expr::new(
                    ExprKind::Index { base: Box::new(b), index: Box::new(i) },
                    Ty::CHAR,
                    span,
                )
            }
            other => {
                let rendered = self.hir.render(&other);
                self.error(
                    Diagnostic::error(E_NOT_INDEXABLE, format!("`{rendered}` cannot be indexed"))
                        .with_primary(span, "not indexable")
                        .with_note("arrays, maps and strings support `[...]` (SPEC §12, §13)"),
                );
                self.err_expr(span)
            }
        }
    }

    // ---- calls ----

    fn check_call(&mut self, callee: &ast::Expr, args: &[ast::Expr], span: Span) -> Expr {
        // A call whose callee is a chain of names may be a free function, a
        // module function, a builtin, or an enum variant constructor.
        if let Some(path) = flatten_path(callee) {
            let leading = &path.segments[0].name;
            if self.lookup_local(leading).is_none() {
                match self.res.lookup_path(self.unit, &path) {
                    Some(Res::Def(id)) if self.res.def(id).kind == DefKind::Fn => {
                        return self.call_fn(id, None, args, span, path.span);
                    }
                    Some(Res::Builtin(b)) => return self.call_builtin(b, args, span),
                    Some(Res::Variant(def, idx)) => {
                        return self.variant_value(def, idx, args, span)
                    }
                    _ => {}
                }
            }
        }

        // Otherwise it is a method call `receiver.name(args)` (SPEC §27).
        if let ast::ExprKind::Field { base, name } = &callee.kind {
            let recv = self.check_expr(base, None);
            return self.call_method(recv, name, args, span);
        }

        let c = self.check_expr(callee, None);
        if c.ty.is_err() {
            return self.err_expr(span);
        }
        let rendered = self.hir.render(&c.ty);
        self.error(
            Diagnostic::error(E_NOT_CALLABLE, format!("`{rendered}` is not a function"))
                .with_primary(callee.span, "cannot be called")
                .with_note("indirect calls through function values are not implemented yet"),
        );
        self.err_expr(span)
    }

    fn call_fn(
        &mut self,
        id: DefId,
        receiver: Option<Expr>,
        args: &[ast::Expr],
        span: Span,
        callee_span: Span,
    ) -> Expr {
        let Some(sig) = self.sigs.get(&id).cloned() else {
            return self.err_expr(span);
        };
        let def = self.res.def(id).clone();

        if sig.receiver.is_some() && receiver.is_none() {
            self.error(
                Diagnostic::error(
                    E_NOT_CALLABLE,
                    format!("`{}` is a method and needs a receiver", def.name),
                )
                .with_primary(callee_span, "called without a value")
                .with_note("call it as `value.{name}(...)` (SPEC §27)".replace("{name}", &def.name)),
            );
            return self.err_expr(span);
        }

        if args.len() != sig.params.len() {
            self.error(
                Diagnostic::error(
                    E_ARITY,
                    format!(
                        "`{}` takes {} argument{}, but {} {} given",
                        def.name,
                        sig.params.len(),
                        if sig.params.len() == 1 { "" } else { "s" },
                        args.len(),
                        if args.len() == 1 { "was" } else { "were" }
                    ),
                )
                .with_primary(span, "wrong number of arguments")
                .with_secondary(def.span, "function declared here"),
            );
            // Check what was written anyway, to surface any further errors.
            for a in args {
                self.check_expr(a, None);
            }
            return self.err_expr(span);
        }

        let mut lowered = Vec::new();
        if let Some(r) = receiver {
            lowered.push(r);
        }
        for (a, want) in args.iter().zip(sig.params.iter()) {
            lowered.push(self.check_expr(a, Some(want)));
        }

        if let Some(msg) = self.deprecation(id) {
            let note = if msg.is_empty() {
                format!("`{}` is deprecated", def.name)
            } else {
                format!("`{}` is deprecated: {msg}", def.name)
            };
            self.diags.push(
                Diagnostic::warning(DiagCode("W0001"), note)
                    .with_primary(callee_span, "deprecated function")
                    .with_secondary(def.span, "declared here"),
            );
        }

        Expr::new(ExprKind::Call { def: id, args: lowered }, sig.ret, span)
    }

    fn deprecation(&self, id: DefId) -> Option<String> {
        let def = self.res.def(id);
        let item = self.item(def)?;
        let attr = item.attrs.iter().find(|a| a.is("deprecated"))?;
        match attr.args.first().map(|e| &e.kind) {
            Some(ast::ExprKind::Str(parts)) => match parts.first() {
                Some(ast::StrSegment::Literal(s)) => Some(s.clone()),
                _ => Some(String::new()),
            },
            _ => Some(String::new()),
        }
    }

    /// `value.name(args)` — a user method, or a built-in one on a collection.
    fn call_method(
        &mut self,
        recv: Expr,
        name: &ast::Ident,
        args: &[ast::Expr],
        span: Span,
    ) -> Expr {
        if recv.ty.is_err() {
            return self.err_expr(span);
        }

        // A user-declared method on a struct or enum (SPEC §27, §28).
        if let Ty::Adt { def, .. } = &recv.ty {
            let type_name = self.hir.name_of(*def);
            if let Some(m) = self.res.method(&type_name, &name.name) {
                return self.call_fn(m, Some(recv), args, span, name.span);
            }
            let mut diag = Diagnostic::error(
                E_NO_METHOD,
                format!("`{type_name}` has no method named `{}`", name.name),
            )
            .with_primary(name.span, "unknown method");
            let methods = self.res.methods_of(&type_name);
            let names: Vec<String> =
                methods.iter().map(|m| self.res.def(*m).name.clone()).collect();
            if let Some(close) = closest(&name.name, names.iter().map(|s| s.as_str())) {
                diag = diag.with_suggestion("a method with a similar name exists", name.span, close);
            }
            self.error(diag);
            return self.err_expr(span);
        }

        // Built-in methods on primitives and collections (SPEC §12–§14).
        match self.builtin_method(&recv.ty, &name.name) {
            Some(spec) => self.call_builtin_method(recv, spec, args, span, name.span),
            None => {
                let rendered = self.hir.render(&recv.ty);
                self.error(
                    Diagnostic::error(
                        E_NO_METHOD,
                        format!("`{rendered}` has no method named `{}`", name.name),
                    )
                    .with_primary(name.span, "unknown method"),
                );
                self.err_expr(span)
            }
        }
    }

    /// The signature of a built-in method: which builtin, its parameter types
    /// and its result type, given the receiver.
    fn builtin_method(&self, recv: &Ty, name: &str) -> Option<(Builtin, Vec<Ty>, Ty)> {
        use Builtin::*;
        Some(match (recv, name) {
            (Ty::Array(t), "push") => (Push, vec![(**t).clone()], Ty::Void),
            (Ty::Array(t), "pop") => (Pop, vec![], (**t).clone()),
            (Ty::Array(t), "contains") => (Contains, vec![(**t).clone()], Ty::BOOL),
            (Ty::Array(t), "join") if t.is_str() => (Join, vec![Ty::STR], Ty::STR),
            (Ty::Array(_), "length") => (Len, vec![], Ty::INT),

            (Ty::Set(t), "add") => (Add, vec![(**t).clone()], Ty::Void),
            (Ty::Set(t), "remove") => (Remove, vec![(**t).clone()], Ty::Void),
            (Ty::Set(t), "has") => (Has, vec![(**t).clone()], Ty::BOOL),
            (Ty::Set(t), "contains") => (Has, vec![(**t).clone()], Ty::BOOL),
            (Ty::Set(_), "length") => (Len, vec![], Ty::INT),

            (Ty::Map(k, _), "has") => (Has, vec![(**k).clone()], Ty::BOOL),
            (Ty::Map(k, _), "contains") => (Has, vec![(**k).clone()], Ty::BOOL),
            (Ty::Map(k, _), "remove") => (Remove, vec![(**k).clone()], Ty::Void),
            (Ty::Map(k, _), "keys") => (Keys, vec![], Ty::Array(k.clone())),
            (Ty::Map(_, v), "values") => (Values, vec![], Ty::Array(v.clone())),
            (Ty::Map(_, _), "length") => (Len, vec![], Ty::INT),

            (Ty::Prim(Prim::Str), "split") => {
                (Split, vec![Ty::STR], Ty::Array(Box::new(Ty::STR)))
            }
            (Ty::Prim(Prim::Str), "contains") => (Contains, vec![Ty::STR], Ty::BOOL),
            (Ty::Prim(Prim::Str), "trim") => (Trim, vec![], Ty::STR),
            (Ty::Prim(Prim::Str), "upper") => (Upper, vec![], Ty::STR),
            (Ty::Prim(Prim::Str), "lower") => (Lower, vec![], Ty::STR),
            (Ty::Prim(Prim::Str), "substr") => (Substr, vec![Ty::INT, Ty::INT], Ty::STR),
            (Ty::Prim(Prim::Str), "replace") => (Replace, vec![Ty::STR, Ty::STR], Ty::STR),
            (Ty::Prim(Prim::Str), "length") => (Len, vec![], Ty::INT),
            _ => return None,
        })
    }

    fn call_builtin_method(
        &mut self,
        recv: Expr,
        spec: (Builtin, Vec<Ty>, Ty),
        args: &[ast::Expr],
        span: Span,
        name_span: Span,
    ) -> Expr {
        let (builtin, params, ret) = spec;
        if args.len() != params.len() {
            self.error(
                Diagnostic::error(
                    E_ARITY,
                    format!(
                        "`{}` takes {} argument{}, but {} {} given",
                        builtin.name(),
                        params.len(),
                        if params.len() == 1 { "" } else { "s" },
                        args.len(),
                        if args.len() == 1 { "was" } else { "were" }
                    ),
                )
                .with_primary(name_span, "wrong number of arguments"),
            );
            return self.err_expr(span);
        }
        let mut lowered = vec![recv];
        for (a, want) in args.iter().zip(params.iter()) {
            lowered.push(self.check_expr(a, Some(want)));
        }
        Expr::new(ExprKind::Builtin { builtin, args: lowered }, ret, span)
    }

    /// A free builtin such as `print(x)` or `math.sqrt(x)` (SPEC §67).
    fn call_builtin(&mut self, b: Builtin, args: &[ast::Expr], span: Span) -> Expr {
        use Builtin::*;

        // (minimum arity, maximum arity)
        let (min, max) = match b {
            Now | Args | ReadLine => (0, 0),
            Print | Println | EPrint | Panic | ToStr | ToInt | ToFloat | Len | Sqrt | Abs
            | Floor | Ceil | ReadFile | Exit => (1, 1),
            Assert => (1, 2),
            Min | Max | Pow | WriteFile => (2, 2),
            _ => (1, 1),
        };
        if args.len() < min || args.len() > max {
            let want = if min == max {
                format!("{min}")
            } else {
                format!("{min} or {max}")
            };
            self.error(
                Diagnostic::error(
                    E_ARITY,
                    format!(
                        "`{}` takes {want} argument{}, but {} {} given",
                        b.name(),
                        if max == 1 { "" } else { "s" },
                        args.len(),
                        if args.len() == 1 { "was" } else { "were" }
                    ),
                )
                .with_primary(span, "wrong number of arguments"),
            );
            return self.err_expr(span);
        }

        let mut lowered: Vec<Expr>;
        let ret: Ty;

        match b {
            // Print anything: the argument is converted to text (SPEC §11).
            Print | Println | EPrint => {
                let a = self.check_expr(&args[0], None);
                lowered = vec![self.to_str(a)];
                ret = Ty::Void;
            }
            ToStr => {
                let a = self.check_expr(&args[0], None);
                lowered = vec![a];
                ret = Ty::STR;
            }
            Assert => {
                let cond = self.check_expr(&args[0], Some(&Ty::BOOL));
                lowered = vec![cond];
                if let Some(msg) = args.get(1) {
                    let m = self.check_expr(msg, Some(&Ty::STR));
                    lowered.push(m);
                }
                ret = Ty::Void;
            }
            Panic => {
                let a = self.check_expr(&args[0], Some(&Ty::STR));
                lowered = vec![a];
                ret = Ty::Void;
            }
            ToInt => {
                let a = self.check_expr(&args[0], None);
                self.require_convertible(&a, "int", span);
                lowered = vec![a];
                ret = Ty::INT;
            }
            ToFloat => {
                let a = self.check_expr(&args[0], None);
                self.require_convertible(&a, "float", span);
                lowered = vec![a];
                ret = Ty::FLOAT;
            }
            Len => {
                let a = self.check_expr(&args[0], None);
                let ok = matches!(a.ty, Ty::Array(_) | Ty::Map(_, _) | Ty::Set(_))
                    || a.ty.is_str()
                    || a.ty.is_err();
                if !ok {
                    let rendered = self.hir.render(&a.ty);
                    self.error(
                        Diagnostic::error(E_BAD_OPERAND, format!("`{rendered}` has no length"))
                            .with_primary(args[0].span, "not a sized value"),
                    );
                }
                lowered = vec![a];
                ret = Ty::INT;
            }
            Sqrt | Floor | Ceil => {
                let a = self.check_expr(&args[0], Some(&Ty::FLOAT));
                lowered = vec![a];
                ret = Ty::FLOAT;
            }
            Abs => {
                let a = self.check_expr(&args[0], None);
                if !a.ty.is_numeric() && !a.ty.is_err() {
                    let rendered = self.hir.render(&a.ty);
                    self.error(
                        Diagnostic::error(E_BAD_OPERAND, format!("`abs` needs a number, not `{rendered}`"))
                            .with_primary(args[0].span, "not a number"),
                    );
                }
                ret = a.ty.clone();
                lowered = vec![a];
            }
            Min | Max => {
                let a = self.check_expr(&args[0], None);
                let bb = self.check_expr(&args[1], Some(&a.ty));
                if !a.ty.is_numeric() && !a.ty.is_err() {
                    let rendered = self.hir.render(&a.ty);
                    self.error(
                        Diagnostic::error(
                            E_BAD_OPERAND,
                            format!("`{}` needs numbers, not `{rendered}`", b.name()),
                        )
                        .with_primary(args[0].span, "not a number"),
                    );
                }
                ret = a.ty.clone();
                lowered = vec![a, bb];
            }
            Pow => {
                let a = self.check_expr(&args[0], Some(&Ty::FLOAT));
                let bb = self.check_expr(&args[1], Some(&Ty::FLOAT));
                lowered = vec![a, bb];
                ret = Ty::FLOAT;
            }
            Now => {
                lowered = vec![];
                ret = Ty::INT;
            }
            Args => {
                lowered = vec![];
                ret = Ty::Array(Box::new(Ty::STR));
            }
            ReadLine => {
                lowered = vec![];
                ret = Ty::STR;
            }
            ReadFile => {
                let a = self.check_expr(&args[0], Some(&Ty::STR));
                lowered = vec![a];
                ret = Ty::STR;
            }
            WriteFile => {
                let a = self.check_expr(&args[0], Some(&Ty::STR));
                let bb = self.check_expr(&args[1], Some(&Ty::STR));
                lowered = vec![a, bb];
                ret = Ty::Void;
            }
            Exit => {
                let a = self.check_expr(&args[0], Some(&Ty::INT));
                lowered = vec![a];
                ret = Ty::Void;
            }
            // Method-only builtins never reach here.
            other => {
                self.error(
                    Diagnostic::error(
                        E_NOT_CALLABLE,
                        format!("`{}` is a method, not a free function", other.name()),
                    )
                    .with_primary(span, "call it on a value instead"),
                );
                return self.err_expr(span);
            }
        }

        Expr::new(ExprKind::Builtin { builtin: b, args: lowered }, ret, span)
    }

    fn require_convertible(&mut self, e: &Expr, to: &str, span: Span) {
        let ok = e.ty.is_numeric()
            || e.ty.is_str()
            || e.ty.is_err()
            || matches!(e.ty, Ty::Prim(Prim::Bool) | Ty::Prim(Prim::Char));
        if !ok {
            let rendered = self.hir.render(&e.ty);
            self.error(
                Diagnostic::error(
                    E_MISMATCH,
                    format!("`{rendered}` cannot be converted to `{to}`"),
                )
                .with_primary(span, "no conversion available"),
            );
        }
    }

    // ---- control flow ----

    fn check_if(
        &mut self,
        cond: &ast::Expr,
        then: &ast::Block,
        els: Option<&ast::ElseBranch>,
        want: Option<&Ty>,
        span: Span,
    ) -> Expr {
        let c = self.check_condition(cond);
        let then_block = self.check_block(then, want);

        let else_block = match els {
            None => None,
            Some(ast::ElseBranch::Block(b)) => Some(self.check_block(b, want)),
            Some(ast::ElseBranch::If(e)) => {
                // `else if` is an `if` in a block of its own.
                let inner = self.check_expr(e, want);
                let ty = inner.ty.clone();
                let span = inner.span;
                Some(Block {
                    stmts: Vec::new(),
                    tail: Some(Box::new(inner)),
                    ty,
                    span,
                })
            }
        };

        // An `if` used for its value must have both halves, and they must
        // agree (SPEC §17, §18).
        let ty = match (&else_block, want) {
            (Some(e), _) if e.ty == then_block.ty => then_block.ty.clone(),
            (Some(e), Some(w)) if !w.is_void() => {
                if !e.ty.is_err() && !then_block.ty.is_err() {
                    let tt = self.hir.render(&then_block.ty);
                    let et = self.hir.render(&e.ty);
                    self.error(
                        Diagnostic::error(
                            E_MISMATCH,
                            "the two branches of this `if` have different types",
                        )
                        .with_primary(span, format!("`{tt}` and `{et}`"))
                        .with_secondary(then_block.span, format!("this is `{tt}`"))
                        .with_secondary(e.span, format!("this is `{et}`")),
                    );
                }
                Ty::Err
            }
            (Some(_), _) => Ty::Void,
            (None, Some(w)) if !w.is_void() && !then_block.ty.is_void() => {
                self.error(
                    Diagnostic::error(E_MISMATCH, "this `if` produces a value but has no `else`")
                        .with_primary(span, "no `else` branch")
                        .with_note("without `else` the `if` has no value when the test fails"),
                );
                Ty::Err
            }
            (None, _) => Ty::Void,
        };

        Expr::new(
            ExprKind::If { cond: Box::new(c), then: then_block, els: else_block },
            ty,
            span,
        )
    }

    /// A condition must be `bool`; L never treats a number as a truth value
    /// (SPEC §10).
    fn check_condition(&mut self, cond: &ast::Expr) -> Expr {
        let c = self.check_expr_inner(cond, Some(&Ty::BOOL));
        if c.ty.is_bool() || c.ty.is_err() {
            return c;
        }
        let rendered = self.hir.render(&c.ty);
        let mut diag = Diagnostic::error(
            E_CONDITION,
            format!("a condition must be `bool`, but this is `{rendered}`"),
        )
        .with_primary(cond.span, "not a boolean");
        if c.ty.is_numeric() {
            diag = diag
                .with_note("L does not convert numbers to booleans (SPEC §10)")
                .with_suggestion("compare explicitly", cond.span, "... != 0");
        } else if c.ty.is_optional() {
            diag = diag.with_suggestion("test for null", cond.span, "... != null");
        }
        self.error(diag);
        self.err_expr(cond.span)
    }

    fn check_match(
        &mut self,
        scrutinee: &ast::Expr,
        arms: &[ast::MatchArm],
        want: Option<&Ty>,
        span: Span,
    ) -> Expr {
        let s = self.check_expr(scrutinee, None);
        let scrut_ty = s.ty.clone();

        let mut lowered = Vec::new();
        let mut covered_variants: Vec<usize> = Vec::new();
        let mut has_catch_all = false;

        for arm in arms {
            self.push_scope();
            let pat = self.check_pattern(&arm.pat, &scrut_ty);
            if pat.is_irrefutable() {
                has_catch_all = true;
            }
            if let PatKind::Variant { variant, subs, .. } = &pat.kind {
                if subs.iter().all(Pat::is_irrefutable) {
                    covered_variants.push(*variant);
                }
            }
            let body = self.check_block_no_scope(&arm.body, want);
            self.pop_scope();
            lowered.push(Arm { pat, body, span: arm.span });
        }

        // Matches must be exhaustive (SPEC §26).
        if !has_catch_all && !scrut_ty.is_err() {
            if let Ty::Adt { def, .. } = &scrut_ty {
                if let Some(en) = self.hir.enums.get(def).cloned() {
                    let missing: Vec<String> = en
                        .variants
                        .iter()
                        .enumerate()
                        .filter(|(i, _)| !covered_variants.contains(i))
                        .map(|(_, v)| format!("`{}.{}`", en.name, v.name))
                        .collect();
                    if !missing.is_empty() {
                        self.error(
                            Diagnostic::error(E_NOT_EXHAUSTIVE, "this `match` is not exhaustive")
                                .with_primary(span, format!("{} not covered", missing.join(", ")))
                                .with_secondary(en.span, "enum declared here")
                                .with_note("add the missing arms, or a `_` arm (SPEC §26)"),
                        );
                    }
                }
            } else {
                self.error(
                    Diagnostic::error(E_NOT_EXHAUSTIVE, "this `match` is not exhaustive")
                        .with_primary(span, "no arm matches every value")
                        .with_note("add a `_` arm to cover the rest (SPEC §26)"),
                );
            }
        }

        // Every arm must agree on a type when the match yields a value.
        let ty = match want {
            Some(w) if !w.is_void() => w.clone(),
            _ => {
                let first = lowered.first().map(|a| a.body.ty.clone()).unwrap_or(Ty::Void);
                if lowered.iter().all(|a| a.body.ty == first) {
                    first
                } else {
                    Ty::Void
                }
            }
        };

        Expr::new(ExprKind::Match { scrutinee: Box::new(s), arms: lowered }, ty, span)
    }

    fn check_pattern(&mut self, pat: &ast::Pattern, scrut: &Ty) -> Pat {
        let span = pat.span;
        let kind = match &pat.kind {
            ast::PatternKind::Wildcard => PatKind::Wild,

            ast::PatternKind::Binding(name) => {
                // A bare name may be an enum variant rather than a binding.
                if let Ty::Adt { def, .. } = scrut {
                    if let Some(en) = self.hir.enums.get(def) {
                        if let Some(idx) = en.variant_index(&name.name) {
                            return Pat {
                                kind: PatKind::Variant { def: *def, variant: idx, subs: vec![] },
                                ty: scrut.clone(),
                                span,
                            };
                        }
                    }
                }
                let id =
                    self.declare_local(&name.name, scrut.clone(), true, false, name.span);
                PatKind::Bind(id)
            }

            ast::PatternKind::Variant { path, fields } => {
                return self.check_variant_pattern(path, fields, scrut, span)
            }

            ast::PatternKind::Literal(e) => {
                let checked = self.check_expr(e, Some(scrut));
                match &checked.kind {
                    ExprKind::Int(v) => PatKind::Int(*v),
                    ExprKind::Str(s) => PatKind::Str(s.clone()),
                    ExprKind::Bool(b) => PatKind::Bool(*b),
                    ExprKind::Char(c) => PatKind::Char(*c),
                    ExprKind::Null => PatKind::Null,
                    ExprKind::Err => PatKind::Wild,
                    _ => {
                        self.error(
                            Diagnostic::error(
                                E_UNSUPPORTED,
                                "only literal values may be used as patterns",
                            )
                            .with_primary(span, "not a literal"),
                        );
                        PatKind::Wild
                    }
                }
            }

            ast::PatternKind::Tuple(subs) => match scrut {
                Ty::Tuple(items) if items.len() == subs.len() => PatKind::Tuple(
                    subs.iter()
                        .zip(items.iter())
                        .map(|(p, t)| self.check_pattern(p, t))
                        .collect(),
                ),
                _ => {
                    let rendered = self.hir.render(scrut);
                    self.error(
                        Diagnostic::error(
                            E_MISMATCH,
                            format!("this pattern does not match `{rendered}`"),
                        )
                        .with_primary(span, "tuple pattern"),
                    );
                    PatKind::Wild
                }
            },

            ast::PatternKind::Err => PatKind::Wild,
        };
        Pat { kind, ty: scrut.clone(), span }
    }

    fn check_variant_pattern(
        &mut self,
        path: &ast::Path,
        fields: &[ast::Pattern],
        scrut: &Ty,
        span: Span,
    ) -> Pat {
        let wild = |ty: &Ty| Pat { kind: PatKind::Wild, ty: ty.clone(), span };

        let res = self.res.lookup_path(self.unit, path);
        let Some(Res::Variant(def, idx)) = res else {
            // A single-segment path may still be a variant of the scrutinee.
            if path.is_single() {
                if let Ty::Adt { def, .. } = scrut {
                    if let Some(en) = self.hir.enums.get(def) {
                        if let Some(i) = en.variant_index(&path.last().name) {
                            return self.variant_pattern(*def, i, fields, scrut, span);
                        }
                    }
                }
            }
            self.error(
                Diagnostic::error(
                    E_UNKNOWN_NAME,
                    format!("`{}` is not an enum variant", path.to_string_dotted()),
                )
                .with_primary(path.span, "not a variant")
                .with_note("patterns match variants such as `Message.TEXT(text)` (SPEC §26)"),
            );
            return wild(scrut);
        };

        if let Ty::Adt { def: sdef, .. } = scrut {
            if *sdef != def {
                let want = self.hir.render(scrut);
                let got = self.hir.name_of(def);
                self.error(
                    Diagnostic::error(
                        E_MISMATCH,
                        format!("this pattern matches `{got}`, but the value is `{want}`"),
                    )
                    .with_primary(path.span, "wrong enum"),
                );
                return wild(scrut);
            }
        }

        self.variant_pattern(def, idx, fields, scrut, span)
    }

    fn variant_pattern(
        &mut self,
        def: DefId,
        idx: usize,
        fields: &[ast::Pattern],
        scrut: &Ty,
        span: Span,
    ) -> Pat {
        let Some(en) = self.hir.enums.get(&def).cloned() else {
            return Pat { kind: PatKind::Wild, ty: scrut.clone(), span };
        };
        let variant = &en.variants[idx];

        if !fields.is_empty() && fields.len() != variant.payload.len() {
            self.error(
                Diagnostic::error(
                    E_ARITY,
                    format!(
                        "`{}.{}` holds {} value{}, but the pattern binds {}",
                        en.name,
                        variant.name,
                        variant.payload.len(),
                        if variant.payload.len() == 1 { "" } else { "s" },
                        fields.len()
                    ),
                )
                .with_primary(span, "wrong number of bindings")
                .with_secondary(variant.span, "variant declared here"),
            );
            return Pat { kind: PatKind::Wild, ty: scrut.clone(), span };
        }

        let payload = variant.payload.clone();
        let subs = fields
            .iter()
            .zip(payload.iter())
            .map(|(p, t)| self.check_pattern(p, t))
            .collect();

        Pat {
            kind: PatKind::Variant { def, variant: idx, subs },
            ty: Ty::Adt { def, args: Vec::new() },
            span,
        }
    }

    // ---- loops ----

    fn push_loop(&mut self, label: Option<&ast::Ident>) -> LoopId {
        let id = LoopId(self.next_loop);
        self.next_loop += 1;
        self.loops.push(LoopFrame { id, label: label.map(|l| l.name.clone()) });
        id
    }

    fn check_loop(&mut self, label: Option<&ast::Ident>, body: &ast::Block, span: Span) -> Expr {
        let id = self.push_loop(label);
        let b = self.check_block(body, None);
        self.loops.pop();
        Expr::new(ExprKind::Loop { id, body: b, step: None }, Ty::Void, span)
    }

    /// `while c { ... }` becomes `loop { if !c { break } ... }` (SPEC §20).
    fn check_while(
        &mut self,
        label: Option<&ast::Ident>,
        cond: &ast::Expr,
        body: &ast::Block,
        span: Span,
    ) -> Expr {
        let id = self.push_loop(label);
        let c = self.check_condition(cond);
        let mut b = self.check_block(body, None);
        self.loops.pop();

        let guard = self.break_unless(c, id, span);
        b.stmts.insert(0, guard);
        Expr::new(ExprKind::Loop { id, body: b, step: None }, Ty::Void, span)
    }

    /// `if !cond { break }`, as a statement.
    fn break_unless(&mut self, cond: Expr, loop_id: LoopId, span: Span) -> Stmt {
        let negated = Expr::new(
            ExprKind::Unary { op: UnOp::Not, operand: Box::new(cond) },
            Ty::BOOL,
            span,
        );
        let brk = Block {
            stmts: vec![Stmt { kind: StmtKind::Break(loop_id), span }],
            tail: None,
            ty: Ty::Void,
            span,
        };
        Stmt {
            kind: StmtKind::Expr(Box::new(Expr::new(
                ExprKind::If { cond: Box::new(negated), then: brk, els: None },
                Ty::Void,
                span,
            ))),
            span,
        }
    }

    /// The unified `for ... in` of SPEC §19.
    ///
    /// Every form becomes a counted loop over an index, with the loop variable
    /// bound at the top of the body:
    ///
    /// ```text
    /// for x in xs { body }
    /// =>
    /// let __seq := xs
    /// let __i := 0
    /// loop {
    ///     if !(__i < length(__seq)) { break }
    ///     let x := __seq[__i]
    ///     body
    /// } step { __i := __i + 1 }
    /// ```
    fn check_for(
        &mut self,
        label: Option<&ast::Ident>,
        pat: &ast::Pattern,
        iter: &ast::Expr,
        body: &ast::Block,
        span: Span,
    ) -> Expr {
        let seq = self.check_expr(iter, None);
        if seq.ty.is_err() {
            return self.err_expr(span);
        }

        let Some(elem_ty) = seq.ty.iter_element() else {
            let rendered = self.hir.render(&seq.ty);
            self.error(
                Diagnostic::error(E_NOT_ITERABLE, format!("`{rendered}` cannot be iterated"))
                    .with_primary(iter.span, "not iterable")
                    .with_note(
                        "`for` iterates arrays, sets, maps, strings, ranges, and integer counts \
                         (SPEC §19)",
                    ),
            );
            return self.err_expr(span);
        };

        self.push_scope();

        // The sequence is evaluated once, into a hidden local.
        let seq_local =
            self.declare_local("__seq", seq.ty.clone(), false, true, span);
        let idx_local = self.declare_local("__index", Ty::INT, true, true, span);

        let mut outer = Vec::new();
        outer.push(Stmt {
            kind: StmtKind::Let { local: seq_local, init: Box::new(seq.clone()) },
            span,
        });

        // The starting index, and the bound to compare against.
        let seq_ref = || Expr::new(ExprKind::Local(seq_local), seq.ty.clone(), span);
        let idx_ref = || Expr::new(ExprKind::Local(idx_local), Ty::INT, span);

        let length_of = |base: Expr| {
            Expr::new(
                ExprKind::Property { base: Box::new(base), prop: Property::Length },
                Ty::INT,
                span,
            )
        };

        // Where the index starts, where it stops, and what the loop variable is
        // on each iteration.
        let (init_index, bound, elem_of): (Expr, Expr, ElemSource) = match &seq.ty {
            // `for i in 0..10` walks the range's own bounds (SPEC §19).
            Ty::Range(_) => (
                Expr::new(ExprKind::RangeStart(Box::new(seq_ref())), Ty::INT, span),
                Expr::new(ExprKind::RangeEnd(Box::new(seq_ref())), Ty::INT, span),
                ElemSource::Range,
            ),
            // `for i in 10` counts 0, 1, ... 9 (SPEC §19).
            Ty::Prim(p) if p.is_integer() => (
                Expr::new(ExprKind::Int(0), Ty::INT, span),
                seq_ref(),
                ElemSource::Counter,
            ),
            // Everything else is walked by position.
            _ => (
                Expr::new(ExprKind::Int(0), Ty::INT, span),
                length_of(seq_ref()),
                ElemSource::Indexed,
            ),
        };

        outer.push(Stmt {
            kind: StmtKind::Let { local: idx_local, init: Box::new(init_index) },
            span,
        });

        let loop_id = self.push_loop(label);

        // `__index < bound`. Inclusive ranges were normalised at construction,
        // so the test is the same for every iteration form.
        let test = Expr::new(
            ExprKind::Binary {
                op: BinOp::Lt,
                lhs: Box::new(idx_ref()),
                rhs: Box::new(bound),
            },
            Ty::BOOL,
            span,
        );
        let guard = self.break_unless(test, loop_id, span);

        // The element for this iteration.
        let element = match elem_of {
            ElemSource::Range | ElemSource::Counter => idx_ref(),
            ElemSource::Indexed => match &seq.ty {
                // Iterating a map or set walks its entries in order.
                Ty::Map(_, _) | Ty::Set(_) => Expr::new(
                    ExprKind::NthEntry { base: Box::new(seq_ref()), index: Box::new(idx_ref()) },
                    elem_ty.clone(),
                    span,
                ),
                _ => Expr::new(
                    ExprKind::Index { base: Box::new(seq_ref()), index: Box::new(idx_ref()) },
                    elem_ty.clone(),
                    span,
                ),
            },
        };

        self.push_scope();
        let bind_pat = self.check_pattern(pat, &elem_ty);
        let mut body_block = self.check_block_no_scope(body, None);
        self.pop_scope();

        // Bind the loop pattern at the top of the body.
        let bind_stmt = match &bind_pat.kind {
            PatKind::Bind(local) => {
                Some(Stmt { kind: StmtKind::Let { local: *local, init: Box::new(element) }, span })
            }
            PatKind::Wild => Some(Stmt { kind: StmtKind::Expr(Box::new(element)), span }),
            _ => {
                self.error(
                    Diagnostic::error(
                        E_UNSUPPORTED,
                        "a `for` loop binds a single name, not a pattern",
                    )
                    .with_primary(pat.span, "unsupported loop pattern"),
                );
                None
            }
        };
        if let Some(b) = bind_stmt {
            body_block.stmts.insert(0, b);
        }
        body_block.stmts.insert(0, guard);

        self.loops.pop();

        // `__index := __index + 1`, run at the end of each iteration and on
        // `continue`.
        let step = Stmt {
            kind: StmtKind::Assign {
                place: Box::new(idx_ref()),
                value: Box::new(Expr::new(
                    ExprKind::Binary {
                        op: BinOp::Add,
                        lhs: Box::new(idx_ref()),
                        rhs: Box::new(Expr::new(ExprKind::Int(1), Ty::INT, span)),
                    },
                    Ty::INT,
                    span,
                )),
            },
            span,
        };

        self.pop_scope();

        outer.push(Stmt {
            kind: StmtKind::Expr(Box::new(Expr::new(
                ExprKind::Loop { id: loop_id, body: body_block, step: Some(Box::new(step)) },
                Ty::Void,
                span,
            ))),
            span,
        });

        Expr::new(
            ExprKind::Block(Block { stmts: outer, tail: None, ty: Ty::Void, span }),
            Ty::Void,
            span,
        )
    }

    // ---- try / catch (SPEC §31) ----

    fn check_try(
        &mut self,
        body: &ast::Block,
        catches: &[ast::CatchClause],
        want: Option<&Ty>,
        span: Span,
    ) -> Expr {
        let b = self.check_block(body, want);
        let mut lowered = Vec::new();

        for c in catches {
            if let Some(ty) = &c.ty {
                self.error(
                    Diagnostic::error(
                        E_UNSUPPORTED,
                        "typed `catch` clauses are not implemented in this preview",
                    )
                    .with_primary(ty.span, "error type")
                    .with_note(
                        "SPEC §31 defines typed catches; this compiler binds every failure as a \
                         `str` message",
                    ),
                );
            }
            self.push_scope();
            let binding = c
                .binding
                .as_ref()
                .map(|b| self.declare_local(&b.name, Ty::STR, false, false, b.span));
            let cb = self.check_block_no_scope(&c.body, want);
            self.pop_scope();
            lowered.push(Catch { binding, body: cb, span: c.span });
        }

        Expr::new(ExprKind::Try { body: b, catches: lowered }, Ty::Void, span)
    }

    // =======================================================================
    // Coercion
    // =======================================================================

    /// Make `e` acceptable where a `want` is required, or report why not.
    fn coerce(&mut self, e: Expr, want: &Ty, span: Span) -> Expr {
        if e.ty == *want || e.ty.is_err() || want.is_err() {
            return e;
        }

        // `T` is acceptable wherever `T?` is wanted (SPEC §30).
        if let Ty::Optional(inner) = want {
            if e.ty == **inner {
                return Expr::new(
                    ExprKind::Cast { expr: Box::new(e), to: want.clone() },
                    want.clone(),
                    span,
                );
            }
            if matches!(e.kind, ExprKind::Null) {
                return Expr::new(ExprKind::Null, want.clone(), span);
            }
        }

        // Nothing else converts implicitly.
        let got = self.hir.render(&e.ty);
        let expected = self.hir.render(want);
        let mut diag = Diagnostic::error(E_MISMATCH, "type mismatch")
            .with_primary(span, format!("expected `{expected}`"))
            .with_note(format!("this expression has type `{got}`"));

        if e.ty.is_numeric() && want.is_numeric() {
            diag = diag.with_suggestion(
                "convert explicitly",
                span,
                format!("call {expected}(...)"),
            );
        }
        if e.ty.is_optional() && !want.is_optional() && e.ty.unwrap_optional() == want {
            diag = diag
                .with_note("this value may be null")
                .with_suggestion("supply a fallback", span, "... ?? default");
        }
        if want.is_str() && !e.ty.is_str() {
            diag = diag.with_suggestion("convert to text", span, "call str(...)");
        }
        self.error(diag);
        self.err_expr(span)
    }
}

/// Which value a desugared `for` loop yields on each iteration.
enum ElemSource {
    /// `for i in 0..10` — the index is the value.
    Range,
    /// `for i in 10` — the index is the value.
    Counter,
    /// `for x in xs` — the value is read out of the sequence.
    Indexed,
}

/// Whether a block always leaves the function or its loop, which is what makes
/// a function without a trailing expression still well-typed (SPEC §16).
fn block_diverges(block: &Block) -> bool {
    if let Some(tail) = &block.tail {
        if expr_diverges(tail) {
            return true;
        }
    }
    block.stmts.iter().any(stmt_diverges)
}

fn stmt_diverges(stmt: &Stmt) -> bool {
    match &stmt.kind {
        StmtKind::Return(_) | StmtKind::Break(_) | StmtKind::Continue(_) => true,
        StmtKind::Expr(e) => expr_diverges(e),
        _ => false,
    }
}

fn expr_diverges(e: &Expr) -> bool {
    match &e.kind {
        ExprKind::If { then, els: Some(els), .. } => block_diverges(then) && block_diverges(els),
        ExprKind::Match { arms, .. } => {
            !arms.is_empty() && arms.iter().all(|a| block_diverges(&a.body))
        }
        ExprKind::Block(b) => block_diverges(b),
        ExprKind::Builtin { builtin: Builtin::Panic | Builtin::Exit, .. } => true,
        // A loop with no `break` never finishes, so anything after it is
        // unreachable and the function cannot fall out of the end.
        ExprKind::Loop { id, body, .. } => !block_breaks(body, *id),
        _ => false,
    }
}

fn block_breaks(block: &Block, target: LoopId) -> bool {
    block.stmts.iter().any(|s| stmt_breaks(s, target))
        || block.tail.as_ref().is_some_and(|t| expr_breaks(t, target))
}

fn stmt_breaks(stmt: &Stmt, target: LoopId) -> bool {
    match &stmt.kind {
        StmtKind::Break(id) => *id == target,
        StmtKind::Expr(e) | StmtKind::Assign { value: e, .. } => expr_breaks(e, target),
        StmtKind::Let { init, .. } => expr_breaks(init, target),
        StmtKind::Return(Some(e)) => expr_breaks(e, target),
        _ => false,
    }
}

fn expr_breaks(e: &Expr, target: LoopId) -> bool {
    match &e.kind {
        ExprKind::If { then, els, .. } => {
            block_breaks(then, target) || els.as_ref().is_some_and(|b| block_breaks(b, target))
        }
        ExprKind::Match { arms, .. } => arms.iter().any(|a| block_breaks(&a.body, target)),
        ExprKind::Block(b) | ExprKind::Unsafe(b) => block_breaks(b, target),
        ExprKind::Loop { body, .. } => block_breaks(body, target),
        ExprKind::Try { body, catches } => {
            block_breaks(body, target) || catches.iter().any(|c| block_breaks(&c.body, target))
        }
        _ => false,
    }
}

/// Turn a chain of `a.b.c` field accesses over plain names back into a path.
fn flatten_path(e: &ast::Expr) -> Option<ast::Path> {
    match &e.kind {
        ast::ExprKind::Path(p) => Some(p.clone()),
        ast::ExprKind::Field { base, name } => {
            let mut p = flatten_path(base)?;
            p.segments.push(name.clone());
            p.span = p.span.to(name.span);
            Some(p)
        }
        _ => None,
    }
}

/// The closest candidate name, for "did you mean" suggestions.
fn closest<'s>(name: &str, candidates: impl Iterator<Item = &'s str>) -> Option<String> {
    let mut best: Option<(usize, &str)> = None;
    for c in candidates {
        let d = distance(name, c);
        if d <= (name.len() / 3).max(1) && best.as_ref().is_none_or(|(bd, _)| d < *bd) {
            best = Some((d, c));
        }
    }
    best.map(|(_, c)| c.to_string())
}

fn distance(a: &str, b: &str) -> usize {
    let a: Vec<char> = a.chars().collect();
    let b: Vec<char> = b.chars().collect();
    let mut prev: Vec<usize> = (0..=b.len()).collect();
    let mut cur = vec![0; b.len() + 1];
    for i in 1..=a.len() {
        cur[0] = i;
        for j in 1..=b.len() {
            let cost = usize::from(a[i - 1] != b[j - 1]);
            cur[j] = (prev[j] + 1).min(cur[j - 1] + 1).min(prev[j - 1] + cost);
        }
        std::mem::swap(&mut prev, &mut cur);
    }
    prev[b.len()]
}
