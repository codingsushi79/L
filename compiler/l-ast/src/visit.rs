//! A read-only AST visitor.
//!
//! `llint`, `ldoc` and `llsp` all need to walk the tree looking for a handful
//! of node kinds. Implement only the methods you care about and call the
//! matching `walk_*` to continue the traversal.

use crate::*;

#[allow(unused_variables)]
pub trait Visitor<'ast>: Sized {
    fn visit_source_unit(&mut self, unit: &'ast SourceUnit) {
        walk_source_unit(self, unit);
    }
    fn visit_use(&mut self, use_decl: &'ast Use) {
        walk_use(self, use_decl);
    }
    fn visit_item(&mut self, item: &'ast Item) {
        walk_item(self, item);
    }
    fn visit_fn(&mut self, decl: &'ast FnDecl, item: &'ast Item) {
        walk_fn(self, decl);
    }
    fn visit_param(&mut self, param: &'ast Param) {
        walk_param(self, param);
    }
    fn visit_field(&mut self, field: &'ast Field) {
        walk_field(self, field);
    }
    fn visit_variant(&mut self, variant: &'ast Variant) {
        walk_variant(self, variant);
    }
    fn visit_block(&mut self, block: &'ast Block) {
        walk_block(self, block);
    }
    fn visit_stmt(&mut self, stmt: &'ast Stmt) {
        walk_stmt(self, stmt);
    }
    fn visit_expr(&mut self, expr: &'ast Expr) {
        walk_expr(self, expr);
    }
    fn visit_pattern(&mut self, pat: &'ast Pattern) {
        walk_pattern(self, pat);
    }
    fn visit_type(&mut self, ty: &'ast Type) {
        walk_type(self, ty);
    }
    fn visit_path(&mut self, path: &'ast Path) {}
    fn visit_ident(&mut self, ident: &'ast Ident) {}
    fn visit_attribute(&mut self, attr: &'ast Attribute) {
        for arg in &attr.args {
            self.visit_expr(arg);
        }
    }
}

pub fn walk_source_unit<'a, V: Visitor<'a>>(v: &mut V, unit: &'a SourceUnit) {
    if let Some(m) = &unit.module {
        v.visit_ident(&m.name);
    }
    for u in &unit.uses {
        v.visit_use(u);
    }
    for item in &unit.items {
        v.visit_item(item);
    }
}

pub fn walk_use<'a, V: Visitor<'a>>(v: &mut V, use_decl: &'a Use) {
    for tree in &use_decl.trees {
        v.visit_path(&tree.path);
        if let Some(alias) = &tree.alias {
            v.visit_ident(alias);
        }
    }
}

pub fn walk_item<'a, V: Visitor<'a>>(v: &mut V, item: &'a Item) {
    for attr in &item.attrs {
        v.visit_attribute(attr);
    }
    match &item.kind {
        ItemKind::Fn(decl) => v.visit_fn(decl, item),
        ItemKind::Struct(decl) => {
            v.visit_ident(&decl.name);
            for field in &decl.fields {
                v.visit_field(field);
            }
        }
        ItemKind::Enum(decl) => {
            v.visit_ident(&decl.name);
            for variant in &decl.variants {
                v.visit_variant(variant);
            }
        }
        ItemKind::Interface(decl) => {
            v.visit_ident(&decl.name);
            for method in &decl.methods {
                v.visit_item(method);
            }
        }
        ItemKind::Impl(block) => {
            v.visit_path(&block.interface);
            v.visit_type(&block.target);
            for method in &block.methods {
                v.visit_item(method);
            }
        }
        ItemKind::Const(decl) => {
            v.visit_ident(&decl.name);
            if let Some(ty) = &decl.ty {
                v.visit_type(ty);
            }
            v.visit_expr(&decl.value);
        }
    }
}

pub fn walk_fn<'a, V: Visitor<'a>>(v: &mut V, decl: &'a FnDecl) {
    if let Some(recv) = &decl.receiver {
        v.visit_ident(recv);
    }
    v.visit_ident(&decl.name);
    for param in &decl.params {
        v.visit_param(param);
    }
    if let Some(ret) = &decl.ret {
        v.visit_type(ret);
    }
    if let Some(body) = &decl.body {
        v.visit_block(body);
    }
}

pub fn walk_param<'a, V: Visitor<'a>>(v: &mut V, param: &'a Param) {
    v.visit_type(&param.ty);
    v.visit_ident(&param.name);
}

pub fn walk_field<'a, V: Visitor<'a>>(v: &mut V, field: &'a Field) {
    v.visit_type(&field.ty);
    v.visit_ident(&field.name);
    if let Some(default) = &field.default {
        v.visit_expr(default);
    }
}

pub fn walk_variant<'a, V: Visitor<'a>>(v: &mut V, variant: &'a Variant) {
    v.visit_ident(&variant.name);
    for ty in &variant.payload {
        v.visit_type(ty);
    }
}

pub fn walk_block<'a, V: Visitor<'a>>(v: &mut V, block: &'a Block) {
    for stmt in &block.stmts {
        v.visit_stmt(stmt);
    }
}

pub fn walk_stmt<'a, V: Visitor<'a>>(v: &mut V, stmt: &'a Stmt) {
    match &stmt.kind {
        StmtKind::Let(let_stmt) => {
            if let Some(ty) = &let_stmt.ty {
                v.visit_type(ty);
            }
            v.visit_ident(&let_stmt.name);
            v.visit_expr(&let_stmt.value);
        }
        StmtKind::Const(decl) => {
            if let Some(ty) = &decl.ty {
                v.visit_type(ty);
            }
            v.visit_ident(&decl.name);
            v.visit_expr(&decl.value);
        }
        StmtKind::Assign(assign) => {
            v.visit_expr(&assign.target);
            v.visit_expr(&assign.value);
        }
        StmtKind::Expr(expr) | StmtKind::Tail(expr) => v.visit_expr(expr),
        StmtKind::Return(Some(expr)) => v.visit_expr(expr),
        StmtKind::Defer(block) => v.visit_block(block),
        StmtKind::Item(item) => v.visit_item(item),
        StmtKind::Break(Some(label)) | StmtKind::Continue(Some(label)) => v.visit_ident(label),
        StmtKind::Return(None)
        | StmtKind::Break(None)
        | StmtKind::Continue(None)
        | StmtKind::Err => {}
    }
}

pub fn walk_expr<'a, V: Visitor<'a>>(v: &mut V, expr: &'a Expr) {
    match &expr.kind {
        ExprKind::Str(segments) => {
            for seg in segments {
                if let StrSegment::Interp(inner) = seg {
                    v.visit_expr(inner);
                }
            }
        }
        ExprKind::Path(path) => v.visit_path(path),
        ExprKind::Array(items) | ExprKind::Set(items) | ExprKind::Tuple(items) => {
            for item in items {
                v.visit_expr(item);
            }
        }
        ExprKind::Map(pairs) => {
            for (k, val) in pairs {
                v.visit_expr(k);
                v.visit_expr(val);
            }
        }
        ExprKind::StructLit { path, generics, fields } => {
            v.visit_path(path);
            for ty in generics {
                v.visit_type(ty);
            }
            for field in fields {
                v.visit_ident(&field.name);
                v.visit_expr(&field.value);
            }
        }
        ExprKind::Unary { operand, .. } => v.visit_expr(operand),
        ExprKind::Binary { lhs, rhs, .. } | ExprKind::Coalesce { lhs, rhs } => {
            v.visit_expr(lhs);
            v.visit_expr(rhs);
        }
        ExprKind::Range { start, end, .. } => {
            if let Some(s) = start {
                v.visit_expr(s);
            }
            if let Some(e) = end {
                v.visit_expr(e);
            }
        }
        ExprKind::Field { base, name } | ExprKind::OptionalField { base, name } => {
            v.visit_expr(base);
            v.visit_ident(name);
        }
        ExprKind::TupleField { base, .. } => v.visit_expr(base),
        ExprKind::Index { base, index } => {
            v.visit_expr(base);
            v.visit_expr(index);
        }
        ExprKind::Call { callee, args, .. } => {
            v.visit_expr(callee);
            for arg in args {
                v.visit_expr(arg);
            }
        }
        ExprKind::VariantCtor { path, args } => {
            v.visit_path(path);
            for arg in args {
                v.visit_expr(arg);
            }
        }
        ExprKind::If { cond, then, else_branch } => {
            v.visit_expr(cond);
            v.visit_block(then);
            match else_branch.as_deref() {
                Some(ElseBranch::Block(block)) => v.visit_block(block),
                Some(ElseBranch::If(inner)) => v.visit_expr(inner),
                None => {}
            }
        }
        ExprKind::Match { scrutinee, arms } => {
            v.visit_expr(scrutinee);
            for arm in arms {
                v.visit_pattern(&arm.pat);
                v.visit_block(&arm.body);
            }
        }
        ExprKind::Block(block) | ExprKind::Unsafe(block) => v.visit_block(block),
        ExprKind::For { pat, iter, body, .. } => {
            v.visit_pattern(pat);
            v.visit_expr(iter);
            v.visit_block(body);
        }
        ExprKind::While { cond, body, .. } => {
            v.visit_expr(cond);
            v.visit_block(body);
        }
        ExprKind::Loop { body, .. } => v.visit_block(body),
        ExprKind::Try { body, catches } => {
            v.visit_block(body);
            for catch in catches {
                if let Some(ty) = &catch.ty {
                    v.visit_type(ty);
                }
                if let Some(binding) = &catch.binding {
                    v.visit_ident(binding);
                }
                v.visit_block(&catch.body);
            }
        }
        ExprKind::Await(inner) | ExprKind::Spawn(inner) => v.visit_expr(inner),
        ExprKind::Int { .. }
        | ExprKind::Float { .. }
        | ExprKind::Bool(_)
        | ExprKind::Char(_)
        | ExprKind::Null
        | ExprKind::SelfExpr
        | ExprKind::Err => {}
    }
}

pub fn walk_pattern<'a, V: Visitor<'a>>(v: &mut V, pat: &'a Pattern) {
    match &pat.kind {
        PatternKind::Binding(ident) => v.visit_ident(ident),
        PatternKind::Variant { path, fields } => {
            v.visit_path(path);
            for field in fields {
                v.visit_pattern(field);
            }
        }
        PatternKind::Literal(expr) => v.visit_expr(expr),
        PatternKind::Tuple(fields) => {
            for field in fields {
                v.visit_pattern(field);
            }
        }
        PatternKind::Wildcard | PatternKind::Err => {}
    }
}

pub fn walk_type<'a, V: Visitor<'a>>(v: &mut V, ty: &'a Type) {
    match &ty.kind {
        TypeKind::Named { path, generics } => {
            v.visit_path(path);
            for g in generics {
                v.visit_type(g);
            }
        }
        TypeKind::Array(inner) | TypeKind::Set(inner) | TypeKind::Optional(inner) => {
            v.visit_type(inner)
        }
        TypeKind::Map(k, val) => {
            v.visit_type(k);
            v.visit_type(val);
        }
        TypeKind::Tuple(items) => {
            for item in items {
                v.visit_type(item);
            }
        }
        TypeKind::Fn { params, ret } => {
            for p in params {
                v.visit_type(p);
            }
            if let Some(r) = ret {
                v.visit_type(r);
            }
        }
        TypeKind::Void | TypeKind::Err => {}
    }
}
