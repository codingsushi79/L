//! Code generation for L (SPEC §80, §81).
//!
//! The reference backend emits C99 and hands it to the platform's C compiler.
//! That choice is deliberate: SPEC §81 requires x86-64 and ARM64 across Linux,
//! Windows and macOS, and going through C reaches all six combinations, with
//! the system linker and debug information, without this project maintaining
//! its own register allocator and object-file writers. The MIR that feeds this
//! backend is already a control-flow graph, so a native backend can be added
//! later without disturbing anything upstream of here.
//!
//! The generated file is self-contained: the runtime is embedded in the
//! compiler and prepended to the translation unit, so a built program has no
//! runtime library to find at load time.

use l_hir::{self as hir, Builtin, DefId, Prim, Ty};
use l_mir::{
    BasicBlock, Body, Const, Operand, Place, Program, Proj, Rvalue, Stmt, Terminator,
};
use l_span::{DiagCode, Diagnostic};
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::process::Command;

/// The C runtime, compiled into every program.
const RUNTIME: &str = include_str!("../runtime/lrt.c");

const E_CC_MISSING: DiagCode = DiagCode("E5001");
const E_CC_FAILED: DiagCode = DiagCode("E5002");
const E_IO: DiagCode = DiagCode("E5003");

/// How much the C compiler should optimise (SPEC §82).
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub enum Profile {
    #[default]
    Debug,
    Release,
}

impl Profile {
    fn cc_flags(self) -> &'static [&'static str] {
        match self {
            // Debug builds keep the C compiler from reordering across the
            // `setjmp` used by `try` (SPEC §31), and keep line info.
            Profile::Debug => &["-O0", "-g"],
            Profile::Release => &["-O2"],
        }
    }
}

/// Options for a build.
pub struct Options {
    pub profile: Profile,
    /// Where the executable goes.
    pub output: PathBuf,
    /// Keep the generated C next to the output, for inspection.
    pub emit_c: bool,
    /// Compile tests rather than `main` (SPEC §74).
    pub test_harness: bool,
}

/// Generate the complete C translation unit for a program.
pub fn emit_c(program: &Program, test_harness: bool) -> String {
    let mut e = Emitter::new(program);
    e.emit_program(test_harness);
    e.finish()
}

/// Compile a program to an executable.
pub fn build(program: &Program, options: &Options) -> Result<PathBuf, Diagnostic> {
    let source = emit_c(program, options.test_harness);

    let c_path = options.output.with_extension("c");
    if let Some(parent) = c_path.parent() {
        std::fs::create_dir_all(parent).map_err(|e| {
            Diagnostic::error(E_IO, format!("cannot create `{}`: {e}", parent.display()))
        })?;
    }
    std::fs::write(&c_path, &source).map_err(|e| {
        Diagnostic::error(E_IO, format!("cannot write `{}`: {e}", c_path.display()))
    })?;

    let cc = find_cc().ok_or_else(|| {
        Diagnostic::error(E_CC_MISSING, "no C compiler was found")
            .with_note(
                "the reference backend emits C and needs `cc`, `gcc` or `clang` on PATH \
                 (SPEC §81)",
            )
            .with_note("set the `CC` environment variable to choose one explicitly")
    })?;

    let mut cmd = Command::new(&cc);
    cmd.arg(&c_path);
    cmd.args(options.profile.cc_flags());
    cmd.arg("-o").arg(&options.output);
    // The runtime uses only the C standard library and libm.
    cmd.arg("-lm");

    let out = cmd.output().map_err(|e| {
        Diagnostic::error(E_CC_FAILED, format!("could not run `{cc}`: {e}"))
    })?;

    if !out.status.success() {
        let stderr = String::from_utf8_lossy(&out.stderr);
        return Err(Diagnostic::error(E_CC_FAILED, "the C compiler rejected the generated code")
            .with_note(format!("`{cc}` said:\n{}", stderr.trim()))
            .with_note(format!("the generated code is at `{}`", c_path.display()))
            .with_note("this is a compiler bug — please report it with the source that caused it"));
    }

    if !options.emit_c {
        let _ = std::fs::remove_file(&c_path);
    }

    Ok(options.output.clone())
}

/// The C compiler to use.
fn find_cc() -> Option<String> {
    if let Ok(cc) = std::env::var("CC") {
        if !cc.is_empty() {
            return Some(cc);
        }
    }
    for candidate in ["cc", "gcc", "clang"] {
        if which(candidate) {
            return Some(candidate.to_string());
        }
    }
    None
}

fn which(name: &str) -> bool {
    let Ok(path) = std::env::var("PATH") else { return false };
    let sep = if cfg!(windows) { ';' } else { ':' };
    path.split(sep).any(|dir| {
        let p = Path::new(dir).join(name);
        p.is_file() || p.with_extension("exe").is_file()
    })
}

// ===========================================================================
// Emitter
// ===========================================================================

struct Emitter<'a> {
    p: &'a Program,
    /// Type definitions, in dependency order.
    types: String,
    /// Generated helper functions (value-to-text conversions).
    helpers: String,
    helper_protos: String,
    /// Function prototypes.
    protos: String,
    /// Function bodies.
    code: String,
    /// Types already defined, by mangled name.
    defined: HashSet<String>,
    /// Text conversions already generated, by mangled type name.
    to_str_fns: HashMap<String, String>,
    /// Counter for C temporaries.
    tmp: u32,
}

impl<'a> Emitter<'a> {
    fn new(p: &'a Program) -> Self {
        Emitter {
            p,
            types: String::new(),
            helpers: String::new(),
            helper_protos: String::new(),
            protos: String::new(),
            code: String::new(),
            defined: HashSet::new(),
            to_str_fns: HashMap::new(),
            tmp: 0,
        }
    }

    fn finish(self) -> String {
        let mut out = String::with_capacity(RUNTIME.len() + self.code.len() + 4096);
        out.push_str("/* Generated by lsc, the L compiler. Do not edit. */\n\n");
        out.push_str(RUNTIME);
        out.push_str("\n/* ---- program types ---- */\n\n");
        out.push_str(&self.types);
        out.push_str("\n/* ---- helpers ---- */\n\n");
        out.push_str(&self.helper_protos);
        out.push('\n');
        out.push_str(&self.protos);
        out.push('\n');
        out.push_str(&self.helpers);
        out.push_str("\n/* ---- program ---- */\n\n");
        out.push_str(&self.code);
        out
    }

    fn fresh(&mut self) -> String {
        self.tmp += 1;
        format!("__c{}", self.tmp)
    }

    fn emit_program(&mut self, test_harness: bool) {
        // Types first: everything else refers to them.
        let defs: Vec<DefId> = self.p.hir.order.clone();
        for def in &defs {
            if self.p.hir.structs.contains_key(def) || self.p.hir.enums.contains_key(def) {
                self.define_adt(*def);
            }
        }

        // Then every type mentioned in a signature or a local slot, which is
        // what pulls in optionals and tuples.
        for i in 0..self.p.bodies.len() {
            let tys: Vec<Ty> = {
                let b = &self.p.bodies[i];
                let mut v: Vec<Ty> = b.locals.iter().map(|l| l.ty.clone()).collect();
                v.push(b.ret.clone());
                v
            };
            for ty in tys {
                self.cty(&ty);
            }
        }

        for i in 0..self.p.bodies.len() {
            let proto = self.signature(&self.p.bodies[i]);
            self.protos.push_str(&proto);
            self.protos.push_str(";\n");
        }

        for i in 0..self.p.bodies.len() {
            self.emit_body(i);
        }

        if test_harness {
            self.emit_test_main();
        } else {
            self.emit_main();
        }
    }

    // -----------------------------------------------------------------
    // Types
    // -----------------------------------------------------------------

    /// The C type for an L type, defining any composite it needs.
    fn cty(&mut self, ty: &Ty) -> String {
        match ty {
            Ty::Void => "void".to_string(),
            Ty::Err => "int64_t".to_string(),
            Ty::Prim(p) => prim_cty(*p).to_string(),
            Ty::Array(_) => "l_array".to_string(),
            Ty::Map(_, _) => "l_map".to_string(),
            Ty::Set(_) => "l_set".to_string(),
            Ty::Range(_) => "l_range".to_string(),
            Ty::Adt { def, .. } => {
                self.define_adt(*def);
                adt_cty(self.p, *def)
            }
            Ty::Optional(inner) => {
                let name = format!("Opt_{}", mangle(self.p, ty));
                if self.defined.insert(name.clone()) {
                    let inner_c = self.cty(inner);
                    // A `void?` can only come from an earlier error.
                    let inner_c = if inner_c == "void" { "int64_t".into() } else { inner_c };
                    self.types.push_str(&format!(
                        "typedef struct {{ int8_t has; {inner_c} value; }} {name};\n"
                    ));
                }
                name
            }
            Ty::Tuple(items) => {
                let name = format!("Tup_{}", mangle(self.p, ty));
                if self.defined.insert(name.clone()) {
                    let fields: Vec<String> = items
                        .iter()
                        .enumerate()
                        .map(|(i, t)| {
                            let c = self.cty(t);
                            format!("{c} t{i};")
                        })
                        .collect();
                    self.types
                        .push_str(&format!("typedef struct {{ {} }} {name};\n", fields.join(" ")));
                }
                name
            }
            // Function values and interfaces are not implemented; the checker
            // rejects them, and this keeps the C valid if one slips through.
            Ty::Fn { .. } | Ty::Interface { .. } | Ty::Param { .. } | Ty::Task(_)
            | Ty::Channel(_) | Ty::Infer(_) => "int64_t".to_string(),
        }
    }

    /// Define a struct or enum, and anything it contains, once.
    fn define_adt(&mut self, def: DefId) {
        let name = adt_cty(self.p, def);
        if !self.defined.insert(name.clone()) {
            return;
        }

        if let Some(s) = self.p.hir.structs.get(&def).cloned() {
            let mut fields = String::new();
            for (i, f) in s.fields.iter().enumerate() {
                let c = self.cty(&f.ty);
                fields.push_str(&format!("    {c} f{i}; /* {} */\n", f.name));
            }
            self.types.push_str(&format!(
                "typedef struct {{\n{fields}}} {name}; /* struct {} */\n",
                s.name
            ));
            return;
        }

        if let Some(e) = self.p.hir.enums.get(&def).cloned() {
            // A fieldless enum is just its tag (SPEC §25).
            if e.is_fieldless() {
                self.types.push_str(&format!(
                    "typedef struct {{ int32_t tag; }} {name}; /* enum {} */\n",
                    e.name
                ));
                return;
            }
            let mut variants = String::new();
            for (vi, v) in e.variants.iter().enumerate() {
                if v.payload.is_empty() {
                    continue;
                }
                let mut fields = String::new();
                for (pi, ty) in v.payload.iter().enumerate() {
                    let c = self.cty(ty);
                    fields.push_str(&format!("{c} p{pi}; "));
                }
                variants.push_str(&format!("        struct {{ {fields}}} v{vi};\n"));
            }
            self.types.push_str(&format!(
                "typedef struct {{\n    int32_t tag;\n    union {{\n{variants}    }} u;\n}} {name}; \
                 /* enum {} */\n",
                e.name
            ));
        }
    }

    // -----------------------------------------------------------------
    // Functions
    // -----------------------------------------------------------------

    fn signature(&mut self, body: &Body) -> String {
        let ret = self.cty(&body.ret);
        let name = fn_name(body);

        // `extern fn` follows the C ABI (SPEC §72), so it keeps its own name
        // and takes raw C types.
        if body.is_extern {
            let params: Vec<String> = body
                .params
                .iter()
                .map(|p| {
                    let ty = body.local_ty(*p).clone();
                    extern_cty(&ty)
                })
                .collect();
            let mut list = params.join(", ");
            if body.is_variadic {
                if list.is_empty() {
                    list = "...".into();
                } else {
                    list.push_str(", ...");
                }
            } else if list.is_empty() {
                list = "void".into();
            }
            return format!("extern {ret} {}({list})", body.name);
        }

        let params: Vec<String> = body
            .params
            .iter()
            .map(|p| {
                let ty = body.local_ty(*p).clone();
                let c = self.cty(&ty);
                format!("{c} v{}", p.0)
            })
            .collect();
        let list = if params.is_empty() { "void".to_string() } else { params.join(", ") };
        format!("static {ret} {name}({list})", )
    }

    fn emit_body(&mut self, index: usize) {
        if self.p.bodies[index].is_extern {
            return;
        }

        let sig = self.signature(&self.p.bodies[index]);
        let mut out = String::new();
        out.push_str(&sig);
        out.push_str(" {\n");

        // Locals that are not parameters.
        let (locals, params): (Vec<_>, Vec<_>) = {
            let b = &self.p.bodies[index];
            (
                b.locals.iter().map(|l| (l.id, l.ty.clone(), l.name.clone())).collect::<Vec<_>>(),
                b.params.clone(),
            )
        };
        for (id, ty, name) in &locals {
            if params.contains(id) || ty.is_void() {
                continue;
            }
            let c = self.cty(ty);
            out.push_str(&format!("    {c} v{} = {{0}}; /* {name} */\n", id.0));
        }

        let block_count = self.p.bodies[index].blocks.len();
        for b in 0..block_count {
            out.push_str(&format!("L{b}:;\n"));
            let block = &self.p.bodies[index].blocks[b] as *const BasicBlock;
            // The emitter appends to `self`, so the block is read through a
            // raw pointer to avoid holding a borrow across those calls. The
            // program is immutable for the whole of code generation.
            let block: &BasicBlock = unsafe { &*block };
            for stmt in &block.stmts {
                let text = self.stmt(index, stmt);
                out.push_str(&text);
            }
            let term = self.terminator(index, &block.term);
            out.push_str(&term);
        }

        out.push_str("}\n\n");
        self.code.push_str(&out);
    }

    fn emit_main(&mut self) {
        let Some(entry) = self.p.entry else {
            // A library has no entry point; nothing to emit.
            return;
        };
        let Some(body) = self.p.body(entry) else { return };
        let name = fn_name(body);
        self.code.push_str(&format!(
            "int main(int argc, char **argv) {{\n\
             \x20   l_argc = argc;\n\
             \x20   l_argv = argv;\n\
             \x20   {name}();\n\
             \x20   return 0;\n\
             }}\n"
        ));
    }

    /// The `lpm test` harness (SPEC §74).
    fn emit_test_main(&mut self) {
        let tests: Vec<(String, String)> = self
            .p
            .bodies
            .iter()
            .filter(|b| b.is_test)
            .map(|b| (fn_name(b), b.qualified.clone()))
            .collect();

        let mut calls = String::new();
        for (name, qualified) in &tests {
            calls.push_str(&format!(
                "    printf(\"test {qualified} ... \");\n\
                 \x20   fflush(stdout);\n\
                 \x20   if (setjmp(*l_try_push())) {{\n\
                 \x20       failed++;\n\
                 \x20       printf(\"FAILED\\n      %s\\n\", l_caught().data);\n\
                 \x20   }} else {{\n\
                 \x20       {name}();\n\
                 \x20       l_try_pop();\n\
                 \x20       passed++;\n\
                 \x20       printf(\"ok\\n\");\n\
                 \x20   }}\n"
            ));
        }

        self.code.push_str(&format!(
            "int main(int argc, char **argv) {{\n\
             \x20   int passed = 0, failed = 0;\n\
             \x20   l_argc = argc;\n\
             \x20   l_argv = argv;\n\
             \x20   printf(\"running {} test%s\\n\\n\", {} == 1 ? \"\" : \"s\");\n\
             {calls}\
             \x20   printf(\"\\ntest result: %s. %d passed; %d failed\\n\",\n\
             \x20          failed == 0 ? \"ok\" : \"FAILED\", passed, failed);\n\
             \x20   return failed == 0 ? 0 : 1;\n\
             }}\n",
            tests.len(),
            tests.len()
        ));
    }

    // -----------------------------------------------------------------
    // Statements
    // -----------------------------------------------------------------

    fn stmt(&mut self, body: usize, stmt: &Stmt) -> String {
        let mut pre = String::new();
        let text = match stmt {
            Stmt::Nop => String::new(),
            Stmt::PopTry => "    l_try_pop();\n".to_string(),

            Stmt::Eval(rv) => {
                let e = self.rvalue(body, rv, &mut pre);
                if e.is_empty() {
                    String::new()
                } else {
                    format!("    (void)({e});\n")
                }
            }

            Stmt::Assign(place, rv) => {
                // Writing through a map key is a call, not an assignment
                // (SPEC §13).
                if let Some(Proj::MapIndex(key)) = place.proj.last() {
                    let base = Place { local: place.local, proj: place.proj[..place.proj.len() - 1].to_vec() };
                    let (base_expr, base_ty) = self.place(body, &base, &mut pre);
                    let (kt, vt) = match &base_ty {
                        Ty::Map(k, v) => ((**k).clone(), (**v).clone()),
                        _ => (Ty::Err, Ty::Err),
                    };
                    let key_expr = self.operand(body, key, &mut pre);
                    let key_c = self.key_of(&key_expr, &kt);
                    let value = self.rvalue(body, rv, &mut pre);
                    let vc = self.cty(&vt);
                    let tmp = self.fresh();
                    return format!(
                        "{pre}    {{ {vc} {tmp} = {value}; l_map_set({base_expr}, {key_c}, &{tmp}); }}\n"
                    );
                }

                let (lhs, lhs_ty) = self.place(body, place, &mut pre);
                if lhs_ty.is_void() {
                    let e = self.rvalue(body, rv, &mut pre);
                    return format!("{pre}    (void)({e});\n");
                }
                let value = self.rvalue(body, rv, &mut pre);
                format!("    {lhs} = {value};\n")
            }
        };
        format!("{pre}{text}")
    }

    fn terminator(&mut self, body: usize, term: &Terminator) -> String {
        let mut pre = String::new();
        let text = match term {
            Terminator::Goto(b) => format!("    goto L{};\n", b.0),

            Terminator::If { cond, then, els } => {
                let c = self.operand(body, cond, &mut pre);
                format!("    if ({c}) goto L{}; else goto L{};\n", then.0, els.0)
            }

            Terminator::Switch { value, targets, default } => {
                let v = self.operand(body, value, &mut pre);
                let mut arms = String::new();
                for (k, b) in targets {
                    arms.push_str(&format!("        case {k}: goto L{};\n", b.0));
                }
                format!(
                    "    switch ((long long)({v})) {{\n{arms}        default: goto L{};\n    }}\n",
                    default.0
                )
            }

            Terminator::Try { handler, body: b } => format!(
                "    if (setjmp(*l_try_push())) goto L{}; else goto L{};\n",
                handler.0, b.0
            ),

            Terminator::Return => {
                let b = &self.p.bodies[body];
                match b.locals.iter().find(|l| l.name == "__return") {
                    Some(l) => format!("    return v{};\n", l.id.0),
                    None => "    return;\n".to_string(),
                }
            }

            Terminator::Unreachable => {
                let b = &self.p.bodies[body];
                if b.ret.is_void() {
                    "    return;\n".to_string()
                } else {
                    let ret = self.cty(&self.p.bodies[body].ret.clone());
                    format!("    return ({ret}){{0}};\n")
                }
            }
        };
        format!("{pre}{text}")
    }

    // -----------------------------------------------------------------
    // Places and operands
    // -----------------------------------------------------------------

    /// A C lvalue for a place, plus its L type.
    fn place(&mut self, body: usize, place: &Place, pre: &mut String) -> (String, Ty) {
        let mut expr = format!("v{}", place.local.0);
        let mut ty = self.p.bodies[body].local_ty(place.local).clone();

        for proj in &place.proj {
            match proj {
                Proj::Field(i) => {
                    let next = match &ty {
                        Ty::Adt { def, .. } => self
                            .p
                            .hir
                            .structs
                            .get(def)
                            .and_then(|s| s.fields.get(*i))
                            .map(|f| f.ty.clone())
                            .unwrap_or(Ty::Err),
                        _ => Ty::Err,
                    };
                    expr = format!("({expr}).f{i}");
                    ty = next;
                }
                Proj::TupleField(i) => {
                    let next = match &ty {
                        Ty::Tuple(items) => items.get(*i).cloned().unwrap_or(Ty::Err),
                        _ => Ty::Err,
                    };
                    expr = format!("({expr}).t{i}");
                    ty = next;
                }
                Proj::Index(idx) => {
                    let i = self.operand(body, idx, pre);
                    match ty.clone() {
                        Ty::Array(elem) => {
                            let c = self.cty(&elem);
                            expr = format!("(*({c} *)l_array_at({expr}, {i}))");
                            ty = *elem;
                        }
                        Ty::Prim(Prim::Str) => {
                            expr = format!("l_str_index({expr}, {i})");
                            ty = Ty::CHAR;
                        }
                        _ => {
                            expr = format!("({expr})");
                            ty = Ty::Err;
                        }
                    }
                }
                Proj::MapIndex(key) => {
                    let (kt, vt) = match &ty {
                        Ty::Map(k, v) => ((**k).clone(), (**v).clone()),
                        _ => (Ty::Err, Ty::Err),
                    };
                    let k = self.operand(body, key, pre);
                    let key_c = self.key_of(&k, &kt);
                    let vc = self.cty(&vt);
                    expr = format!("(*({vc} *)l_map_get({expr}, {key_c}))");
                    ty = vt;
                }
            }
        }
        (expr, ty)
    }

    fn operand(&mut self, body: usize, op: &Operand, pre: &mut String) -> String {
        match op {
            Operand::Copy(p) => self.place(body, p, pre).0,
            Operand::Const(c) => self.constant(c),
        }
    }

    fn constant(&mut self, c: &Const) -> String {
        match c {
            Const::Int(v) => format!("INT64_C({v})"),
            Const::Float(v) => {
                if v.is_nan() {
                    "(0.0/0.0)".to_string()
                } else if v.is_infinite() {
                    if *v > 0.0 { "(1.0/0.0)".into() } else { "(-1.0/0.0)".into() }
                } else {
                    format!("{v:?}")
                }
            }
            Const::Bool(v) => (if *v { "1" } else { "0" }).to_string(),
            Const::Char(v) => format!("INT32_C({})", *v as u32),
            Const::Str(s) => format!("l_str_lit({})", c_string(s)),
            Const::Null => "0".to_string(),
            Const::Unit => "0".to_string(),
        }
    }

    /// Build an `l_key` from a value of a primitive type (SPEC §13, §14).
    fn key_of(&mut self, expr: &str, ty: &Ty) -> String {
        match ty {
            Ty::Prim(Prim::Str) => format!("l_key_str({expr})"),
            Ty::Prim(Prim::Bool) => format!("l_key_bool({expr})"),
            Ty::Prim(Prim::Char) => format!("l_key_char({expr})"),
            Ty::Prim(p) if p.is_float() => format!("l_key_float({expr})"),
            _ => format!("l_key_int((int64_t)({expr}))"),
        }
    }

    /// Read a value of type `ty` back out of an `l_key`.
    fn key_back(&mut self, expr: &str, ty: &Ty) -> String {
        match ty {
            Ty::Prim(Prim::Str) => format!("({expr}).s"),
            Ty::Prim(p) if p.is_float() => format!("({expr}).f"),
            _ => {
                let c = self.cty(ty);
                format!("({c})({expr}).i")
            }
        }
    }

    // -----------------------------------------------------------------
    // Rvalues
    // -----------------------------------------------------------------

    fn rvalue(&mut self, body: usize, rv: &Rvalue, pre: &mut String) -> String {
        match rv {
            Rvalue::Use(op) => self.operand(body, op, pre),

            Rvalue::Unary(op, o, ty) => {
                let e = self.operand(body, o, pre);
                match op {
                    hir::UnOp::Neg => format!("(-({e}))"),
                    hir::UnOp::Not => format!("(!({e}))"),
                    hir::UnOp::BitNot => {
                        let c = self.cty(ty);
                        format!("(({c})~({e}))")
                    }
                }
            }

            Rvalue::Binary(op, l, r, ty) => self.binary(body, *op, l, r, ty, pre),

            Rvalue::Array(items, elem) => {
                let c = self.cty(elem);
                let name = self.fresh();
                pre.push_str(&format!(
                    "    l_array {name} = l_array_new(sizeof({c}), {});\n",
                    items.len().max(1)
                ));
                for item in items {
                    let e = self.operand(body, item, pre);
                    let t = self.fresh();
                    pre.push_str(&format!(
                        "    {{ {c} {t} = {e}; l_array_push({name}, &{t}); }}\n"
                    ));
                }
                name
            }

            Rvalue::Map(entries, kt, vt) => {
                let vc = self.cty(vt);
                let name = self.fresh();
                pre.push_str(&format!("    l_map {name} = l_map_new(sizeof({vc}));\n"));
                for (k, v) in entries {
                    let ke = self.operand(body, k, pre);
                    let key = self.key_of(&ke, kt);
                    let ve = self.operand(body, v, pre);
                    let t = self.fresh();
                    pre.push_str(&format!(
                        "    {{ {vc} {t} = {ve}; l_map_set({name}, {key}, &{t}); }}\n"
                    ));
                }
                name
            }

            Rvalue::Set(items, elem) => {
                let name = self.fresh();
                pre.push_str(&format!("    l_set {name} = l_set_new();\n"));
                for item in items {
                    let e = self.operand(body, item, pre);
                    let key = self.key_of(&e, elem);
                    pre.push_str(&format!("    l_set_add({name}, {key});\n"));
                }
                name
            }

            Rvalue::Tuple(items, tys) => {
                let ty = Ty::Tuple(tys.clone());
                let c = self.cty(&ty);
                let parts: Vec<String> = items
                    .iter()
                    .enumerate()
                    .map(|(i, o)| {
                        let e = self.operand(body, o, pre);
                        format!(".t{i} = {e}")
                    })
                    .collect();
                format!("(({c}){{ {} }})", parts.join(", "))
            }

            Rvalue::StructLit(def, fields) => {
                let c = self.cty(&Ty::Adt { def: *def, args: vec![] });
                if fields.is_empty() {
                    return format!("(({c}){{0}})");
                }
                let parts: Vec<String> = fields
                    .iter()
                    .enumerate()
                    .map(|(i, o)| {
                        let e = self.operand(body, o, pre);
                        format!(".f{i} = {e}")
                    })
                    .collect();
                format!("(({c}){{ {} }})", parts.join(", "))
            }

            Rvalue::EnumLit(def, variant, args) => {
                let c = self.cty(&Ty::Adt { def: *def, args: vec![] });
                if args.is_empty() {
                    return format!("(({c}){{ .tag = {variant} }})");
                }
                let parts: Vec<String> = args
                    .iter()
                    .enumerate()
                    .map(|(i, o)| {
                        let e = self.operand(body, o, pre);
                        format!(".p{i} = {e}")
                    })
                    .collect();
                format!(
                    "(({c}){{ .tag = {variant}, .u.v{variant} = {{ {} }} }})",
                    parts.join(", ")
                )
            }

            Rvalue::Concat(parts) => {
                let exprs: Vec<String> =
                    parts.iter().map(|p| self.operand(body, p, pre)).collect();
                format!(
                    "l_str_concat({}, (l_str[]){{ {} }})",
                    exprs.len(),
                    exprs.join(", ")
                )
            }

            Rvalue::Len(o, ty) => {
                let e = self.operand(body, o, pre);
                match ty {
                    Ty::Array(_) => format!("l_array_len({e})"),
                    Ty::Map(_, _) => format!("l_map_len({e})"),
                    Ty::Set(_) => format!("l_set_len({e})"),
                    Ty::Prim(Prim::Str) => format!("l_str_chars({e})"),
                    _ => "0".to_string(),
                }
            }

            Rvalue::Range(a, b) => {
                let s = self.operand(body, a, pre);
                let e = self.operand(body, b, pre);
                format!("l_range_new((int64_t)({s}), (int64_t)({e}))")
            }
            Rvalue::RangeStart(o) => {
                let e = self.operand(body, o, pre);
                format!("({e}).start")
            }
            Rvalue::RangeEnd(o) => {
                let e = self.operand(body, o, pre);
                format!("({e}).end")
            }

            Rvalue::NthEntry(base, idx, ty) => {
                let b = self.operand(body, base, pre);
                let i = self.operand(body, idx, pre);
                match ty {
                    // Iterating a map walks its keys (SPEC §19).
                    Ty::Map(k, _) => {
                        let expr = format!("l_map_key_at({b}, {i})");
                        self.key_back(&expr, k)
                    }
                    Ty::Set(k) => {
                        let expr = format!("l_set_at({b}, {i})");
                        self.key_back(&expr, k)
                    }
                    _ => "0".to_string(),
                }
            }

            Rvalue::Call(def, args) => {
                let Some(callee) = self.p.body(*def) else {
                    return "0".to_string();
                };
                let is_extern = callee.is_extern;
                let name = fn_name(callee);
                let param_tys: Vec<Ty> =
                    callee.params.iter().map(|p| callee.local_ty(*p).clone()).collect();
                let exprs: Vec<String> = args
                    .iter()
                    .enumerate()
                    .map(|(i, a)| {
                        let e = self.operand(body, a, pre);
                        // A C function takes a plain pointer for text.
                        match param_tys.get(i) {
                            Some(t) if is_extern && t.is_str() => format!("({e}).data"),
                            _ => e,
                        }
                    })
                    .collect();
                format!("{name}({})", exprs.join(", "))
            }

            Rvalue::Builtin(b, args, tys) => self.builtin(body, *b, args, tys, pre),

            Rvalue::Discriminant(o, _) => {
                let e = self.operand(body, o, pre);
                format!("(int64_t)({e}).tag")
            }

            Rvalue::Payload(o, _, variant, field) => {
                let e = self.operand(body, o, pre);
                format!("({e}).u.v{variant}.p{field}")
            }

            Rvalue::MakeOptional(o, inner) => {
                let e = self.operand(body, o, pre);
                let opt = Ty::Optional(Box::new(inner.clone()));
                let c = self.cty(&opt);
                format!("(({c}){{ .has = 1, .value = {e} }})")
            }

            Rvalue::UnwrapOptional(o, _) => {
                let e = self.operand(body, o, pre);
                // Reading past a null is a failure, catchable by `try`.
                format!("(({e}).has ? ({e}).value : (l_fail_cstr(\"this value is null\"), ({e}).value))")
            }

            Rvalue::IsNull(o) => {
                let e = self.operand(body, o, pre);
                format!("(!({e}).has)")
            }

            Rvalue::Cast(o, from, to) => {
                let e = self.operand(body, o, pre);
                let c = self.cty(to);
                if from == to || c == "void" {
                    e
                } else {
                    format!("(({c})({e}))")
                }
            }

            Rvalue::NullOptional(ty) => {
                let c = self.cty(ty);
                format!("(({c}){{ .has = 0 }})")
            }

            Rvalue::CaughtError => "l_caught()".to_string(),
        }
    }

    fn binary(
        &mut self,
        body: usize,
        op: hir::BinOp,
        l: &Operand,
        r: &Operand,
        ty: &Ty,
        pre: &mut String,
    ) -> String {
        use hir::BinOp::*;
        let a = self.operand(body, l, pre);
        let b = self.operand(body, r, pre);

        // Text compares by content, not by pointer (SPEC §11).
        if ty.is_str() {
            return match op {
                Eq => format!("l_str_eq({a}, {b})"),
                Ne => format!("(!l_str_eq({a}, {b}))"),
                Lt => format!("(l_str_cmp({a}, {b}) < 0)"),
                Le => format!("(l_str_cmp({a}, {b}) <= 0)"),
                Gt => format!("(l_str_cmp({a}, {b}) > 0)"),
                Ge => format!("(l_str_cmp({a}, {b}) >= 0)"),
                _ => format!("(({a}), ({b}), 0)"),
            };
        }

        // Enums compare by tag; a struct compares field by field, which for
        // now means comparing its representation.
        if let Ty::Adt { def, .. } = ty {
            if self.p.hir.enums.contains_key(def) {
                let cmp = format!("(({a}).tag == ({b}).tag)");
                return match op {
                    Eq => cmp,
                    Ne => format!("(!{cmp})"),
                    _ => "0".to_string(),
                };
            }
            let c = self.cty(ty);
            let ta = self.fresh();
            let tb = self.fresh();
            pre.push_str(&format!("    {c} {ta} = {a}; {c} {tb} = {b};\n"));
            let cmp = format!("(memcmp(&{ta}, &{tb}, sizeof({c})) == 0)");
            return match op {
                Eq => cmp,
                Ne => format!("(!{cmp})"),
                _ => "0".to_string(),
            };
        }

        // Integer division traps on zero rather than being undefined.
        if ty.is_integer() {
            let c = self.cty(ty);
            match op {
                Div => return format!("(({c})l_idiv((int64_t)({a}), (int64_t)({b})))"),
                Rem => return format!("(({c})l_irem((int64_t)({a}), (int64_t)({b})))"),
                _ => {}
            }
        }

        let sym = match op {
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
        };
        format!("(({a}) {sym} ({b}))")
    }

    fn builtin(
        &mut self,
        body: usize,
        b: Builtin,
        args: &[Operand],
        tys: &[Ty],
        pre: &mut String,
    ) -> String {
        use Builtin::*;
        let arg = |e: &mut Self, i: usize, pre: &mut String| -> String {
            match args.get(i) {
                Some(o) => e.operand(body, o, pre),
                None => "0".to_string(),
            }
        };
        let ty_of = |i: usize| tys.get(i).cloned().unwrap_or(Ty::Err);

        match b {
            Print | Println => {
                let a = arg(self, 0, pre);
                format!("l_print({a})")
            }
            EPrint => {
                let a = arg(self, 0, pre);
                format!("l_eprint({a})")
            }
            Panic => {
                let a = arg(self, 0, pre);
                format!("l_fail({a})")
            }
            Assert => {
                let cond = arg(self, 0, pre);
                let msg = if args.len() > 1 {
                    arg(self, 1, pre)
                } else {
                    "l_str_lit(\"assertion failed\")".to_string()
                };
                format!("l_assert({cond}, {msg})")
            }

            ToStr => {
                let ty = ty_of(0);
                let a = arg(self, 0, pre);
                self.to_str(&a, &ty)
            }

            ToInt => {
                let ty = ty_of(0);
                let a = arg(self, 0, pre);
                if ty.is_str() {
                    format!("l_str_to_int({a})")
                } else {
                    format!("((int64_t)({a}))")
                }
            }

            ToFloat => {
                let ty = ty_of(0);
                let a = arg(self, 0, pre);
                if ty.is_str() {
                    format!("l_str_to_float({a})")
                } else {
                    format!("((double)({a}))")
                }
            }

            Len => {
                let ty = ty_of(0);
                let a = arg(self, 0, pre);
                match ty {
                    Ty::Array(_) => format!("l_array_len({a})"),
                    Ty::Map(_, _) => format!("l_map_len({a})"),
                    Ty::Set(_) => format!("l_set_len({a})"),
                    Ty::Prim(Prim::Str) => format!("l_str_chars({a})"),
                    _ => "0".to_string(),
                }
            }

            Push => {
                let elem = match ty_of(0) {
                    Ty::Array(t) => *t,
                    _ => Ty::Err,
                };
                let a = arg(self, 0, pre);
                let v = arg(self, 1, pre);
                let c = self.cty(&elem);
                let t = self.fresh();
                pre.push_str(&format!("    {{ {c} {t} = {v}; l_array_push({a}, &{t}); }}\n"));
                String::new()
            }

            Pop => {
                let elem = match ty_of(0) {
                    Ty::Array(t) => *t,
                    _ => Ty::Err,
                };
                let a = arg(self, 0, pre);
                let c = self.cty(&elem);
                let t = self.fresh();
                pre.push_str(&format!("    {c} {t} = {{0}}; l_array_pop({a}, &{t});\n"));
                t
            }

            Add => {
                let elem = match ty_of(0) {
                    Ty::Set(t) => *t,
                    _ => Ty::Err,
                };
                let a = arg(self, 0, pre);
                let v = arg(self, 1, pre);
                let key = self.key_of(&v, &elem);
                format!("l_set_add({a}, {key})")
            }

            Remove => {
                let recv = ty_of(0);
                let a = arg(self, 0, pre);
                let v = arg(self, 1, pre);
                match recv {
                    Ty::Set(t) => {
                        let key = self.key_of(&v, &t);
                        format!("l_set_remove({a}, {key})")
                    }
                    Ty::Map(k, _) => {
                        let key = self.key_of(&v, &k);
                        format!("l_map_remove({a}, {key})")
                    }
                    _ => "0".to_string(),
                }
            }

            Has => {
                let recv = ty_of(0);
                let a = arg(self, 0, pre);
                let v = arg(self, 1, pre);
                match recv {
                    Ty::Set(t) => {
                        let key = self.key_of(&v, &t);
                        format!("l_set_has({a}, {key})")
                    }
                    Ty::Map(k, _) => {
                        let key = self.key_of(&v, &k);
                        format!("l_map_has({a}, {key})")
                    }
                    _ => "0".to_string(),
                }
            }

            Keys | Values => {
                let recv = ty_of(0);
                let (kt, vt) = match &recv {
                    Ty::Map(k, v) => ((**k).clone(), (**v).clone()),
                    _ => (Ty::Err, Ty::Err),
                };
                let a = arg(self, 0, pre);
                let want = if b == Keys { kt.clone() } else { vt.clone() };
                let c = self.cty(&want);
                let out = self.fresh();
                let i = self.fresh();
                let item = self.fresh();
                let read = if b == Keys {
                    let expr = format!("l_map_key_at({a}, {i})");
                    self.key_back(&expr, &kt)
                } else {
                    let vc = self.cty(&vt);
                    format!("(*({vc} *)l_map_val_at({a}, {i}))")
                };
                pre.push_str(&format!(
                    "    l_array {out} = l_array_new(sizeof({c}), l_map_len({a}) + 1);\n\
                     \x20   for (int64_t {i} = 0; {i} < l_map_len({a}); {i}++) {{\n\
                     \x20       {c} {item} = {read};\n\
                     \x20       l_array_push({out}, &{item});\n\
                     \x20   }}\n"
                ));
                out
            }

            Contains => {
                let recv = ty_of(0);
                let a = arg(self, 0, pre);
                let v = arg(self, 1, pre);
                match recv {
                    Ty::Prim(Prim::Str) => format!("l_str_contains({a}, {v})"),
                    Ty::Array(elem) => {
                        let c = self.cty(&elem);
                        let found = self.fresh();
                        let i = self.fresh();
                        let needle = self.fresh();
                        let cmp = if elem.is_str() {
                            format!("l_str_eq((({c} *)({a})->data)[{i}], {needle})")
                        } else {
                            format!("(({c} *)({a})->data)[{i}] == {needle}")
                        };
                        pre.push_str(&format!(
                            "    int8_t {found} = 0;\n\
                             \x20   {c} {needle} = {v};\n\
                             \x20   for (int64_t {i} = 0; {i} < l_array_len({a}); {i}++) {{\n\
                             \x20       if ({cmp}) {{ {found} = 1; break; }}\n\
                             \x20   }}\n"
                        ));
                        found
                    }
                    _ => "0".to_string(),
                }
            }

            Split => {
                let a = arg(self, 0, pre);
                let v = arg(self, 1, pre);
                format!("l_str_split({a}, {v})")
            }
            Join => {
                let a = arg(self, 0, pre);
                let v = arg(self, 1, pre);
                format!("l_str_join({a}, {v})")
            }
            Trim => {
                let a = arg(self, 0, pre);
                format!("l_str_trim({a})")
            }
            Upper => {
                let a = arg(self, 0, pre);
                format!("l_str_upper({a})")
            }
            Lower => {
                let a = arg(self, 0, pre);
                format!("l_str_lower({a})")
            }
            Substr => {
                let a = arg(self, 0, pre);
                let x = arg(self, 1, pre);
                let y = arg(self, 2, pre);
                format!("l_str_substr({a}, {x}, {y})")
            }
            Replace => {
                let a = arg(self, 0, pre);
                let x = arg(self, 1, pre);
                let y = arg(self, 2, pre);
                format!("l_str_replace({a}, {x}, {y})")
            }

            Sqrt => {
                let a = arg(self, 0, pre);
                format!("sqrt({a})")
            }
            Floor => {
                let a = arg(self, 0, pre);
                format!("floor({a})")
            }
            Ceil => {
                let a = arg(self, 0, pre);
                format!("ceil({a})")
            }
            Pow => {
                let a = arg(self, 0, pre);
                let x = arg(self, 1, pre);
                format!("pow({a}, {x})")
            }
            Abs => {
                let ty = ty_of(0);
                let a = arg(self, 0, pre);
                if ty.is_float() {
                    format!("fabs({a})")
                } else {
                    format!("((int64_t)llabs((long long)({a})))")
                }
            }
            Min | Max => {
                let ty = ty_of(0);
                let c = self.cty(&ty);
                let a = arg(self, 0, pre);
                let x = arg(self, 1, pre);
                let ta = self.fresh();
                let tb = self.fresh();
                pre.push_str(&format!("    {c} {ta} = {a}; {c} {tb} = {x};\n"));
                let sym = if b == Min { "<" } else { ">" };
                format!("(({ta} {sym} {tb}) ? {ta} : {tb})")
            }

            Now => "l_now()".to_string(),
            Args => "l_args()".to_string(),
            ReadLine => "l_read_line()".to_string(),
            ReadFile => {
                let a = arg(self, 0, pre);
                format!("l_read_file({a})")
            }
            WriteFile => {
                let a = arg(self, 0, pre);
                let x = arg(self, 1, pre);
                format!("l_write_file({a}, {x})")
            }
            Exit => {
                let a = arg(self, 0, pre);
                format!("exit((int)({a}))")
            }
        }
    }

    // -----------------------------------------------------------------
    // Value to text (SPEC §11 interpolation, and `print` of any value)
    // -----------------------------------------------------------------

    fn to_str(&mut self, expr: &str, ty: &Ty) -> String {
        match ty {
            Ty::Prim(Prim::Str) => expr.to_string(),
            Ty::Prim(Prim::Bool) => format!("l_str_from_bool({expr})"),
            Ty::Prim(Prim::Char) => format!("l_str_from_char({expr})"),
            Ty::Prim(p) if p.is_float() => format!("l_str_from_float({expr})"),
            Ty::Prim(p) if p.is_unsigned_int() => {
                format!("l_str_from_uint((uint64_t)({expr}))")
            }
            Ty::Prim(p) if p.is_integer() => format!("l_str_from_int((int64_t)({expr}))"),
            Ty::Void | Ty::Err => "l_str_lit(\"\")".to_string(),
            _ => {
                let f = self.to_str_fn(ty);
                format!("{f}({expr})")
            }
        }
    }

    /// Generate — once per type — a function that renders a value as text.
    fn to_str_fn(&mut self, ty: &Ty) -> String {
        let key = mangle(self.p, ty);
        if let Some(name) = self.to_str_fns.get(&key) {
            return name.clone();
        }

        let name = format!("l_ts_{key}");
        // Registered before the body is built, so a self-referential type
        // does not recurse forever.
        self.to_str_fns.insert(key.clone(), name.clone());
        let cty = self.cty(ty);
        self.helper_protos.push_str(&format!("static l_str {name}({cty} v);\n"));

        let mut b = String::new();
        b.push_str(&format!("static l_str {name}({cty} v) {{\n"));

        match ty {
            Ty::Adt { def, .. } => {
                if let Some(s) = self.p.hir.structs.get(def).cloned() {
                    // `User { name: "Sasha", age: 20 }`
                    let n = s.fields.len() * 2 + 2;
                    b.push_str(&format!("    l_str parts[{}];\n", n.max(1)));
                    b.push_str(&format!(
                        "    parts[0] = l_str_lit(\"{} {{\");\n",
                        s.name
                    ));
                    let mut at = 1;
                    for (i, f) in s.fields.iter().enumerate() {
                        let sep = if i == 0 { " " } else { ", " };
                        b.push_str(&format!(
                            "    parts[{at}] = l_str_lit(\"{sep}{}: \");\n",
                            f.name
                        ));
                        at += 1;
                        let inner = self.to_str(&format!("v.f{i}"), &f.ty);
                        b.push_str(&format!("    parts[{at}] = {inner};\n"));
                        at += 1;
                    }
                    b.push_str(&format!("    parts[{at}] = l_str_lit(\" }}\");\n"));
                    at += 1;
                    b.push_str(&format!("    return l_str_concat({at}, parts);\n"));
                } else if let Some(e) = self.p.hir.enums.get(def).cloned() {
                    // `Color.RED`, or `Message.TEXT("hi")`
                    b.push_str("    switch (v.tag) {\n");
                    for (vi, variant) in e.variants.iter().enumerate() {
                        b.push_str(&format!("    case {vi}: {{\n"));
                        if variant.payload.is_empty() {
                            b.push_str(&format!(
                                "        return l_str_lit(\"{}.{}\");\n",
                                e.name, variant.name
                            ));
                        } else {
                            let n = variant.payload.len() * 2 + 1;
                            b.push_str(&format!("        l_str parts[{n}];\n"));
                            b.push_str(&format!(
                                "        parts[0] = l_str_lit(\"{}.{}(\");\n",
                                e.name, variant.name
                            ));
                            let mut at = 1;
                            for (pi, pty) in variant.payload.iter().enumerate() {
                                if pi > 0 {
                                    b.push_str(&format!(
                                        "        parts[{at}] = l_str_lit(\", \");\n"
                                    ));
                                    at += 1;
                                }
                                let inner = self.to_str(&format!("v.u.v{vi}.p{pi}"), pty);
                                b.push_str(&format!("        parts[{at}] = {inner};\n"));
                                at += 1;
                            }
                            b.push_str(&format!("        parts[{at}] = l_str_lit(\")\");\n"));
                            at += 1;
                            b.push_str(&format!(
                                "        return l_str_concat({at}, parts);\n"
                            ));
                        }
                        b.push_str("    }\n");
                    }
                    b.push_str("    }\n    return l_str_lit(\"?\");\n");
                } else {
                    b.push_str("    (void)v;\n    return l_str_lit(\"?\");\n");
                }
            }

            Ty::Array(elem) => {
                let ec = self.cty(elem);
                let inner = self.to_str(&format!("(({ec} *)v->data)[i]"), elem);
                b.push_str(&format!(
                    "    int64_t n = l_array_len(v);\n\
                     \x20   l_str *parts = (l_str *)l_alloc(sizeof(l_str) * (size_t)(n * 2 + 2));\n\
                     \x20   int at = 0;\n\
                     \x20   parts[at++] = l_str_lit(\"[\");\n\
                     \x20   for (int64_t i = 0; i < n; i++) {{\n\
                     \x20       if (i) parts[at++] = l_str_lit(\", \");\n\
                     \x20       parts[at++] = {inner};\n\
                     \x20   }}\n\
                     \x20   parts[at++] = l_str_lit(\"]\");\n\
                     \x20   return l_str_concat(at, parts);\n"
                ));
            }

            Ty::Map(k, v) => {
                let vc = self.cty(v);
                let key_expr = {
                    let e = "l_map_key_at(v, i)".to_string();
                    self.key_back(&e, k)
                };
                let key_str = self.to_str(&key_expr, k);
                let val_str = self.to_str(&format!("(*({vc} *)l_map_val_at(v, i))"), v);
                b.push_str(&format!(
                    "    int64_t n = l_map_len(v);\n\
                     \x20   l_str *parts = (l_str *)l_alloc(sizeof(l_str) * (size_t)(n * 4 + 2));\n\
                     \x20   int at = 0;\n\
                     \x20   parts[at++] = l_str_lit(\"{{\");\n\
                     \x20   for (int64_t i = 0; i < n; i++) {{\n\
                     \x20       if (i) parts[at++] = l_str_lit(\", \");\n\
                     \x20       parts[at++] = {key_str};\n\
                     \x20       parts[at++] = l_str_lit(\": \");\n\
                     \x20       parts[at++] = {val_str};\n\
                     \x20   }}\n\
                     \x20   parts[at++] = l_str_lit(\"}}\");\n\
                     \x20   return l_str_concat(at, parts);\n"
                ));
            }

            Ty::Set(elem) => {
                let e = "l_set_at(v, i)".to_string();
                let back = self.key_back(&e, elem);
                let inner = self.to_str(&back, elem);
                b.push_str(&format!(
                    "    int64_t n = l_set_len(v);\n\
                     \x20   l_str *parts = (l_str *)l_alloc(sizeof(l_str) * (size_t)(n * 2 + 2));\n\
                     \x20   int at = 0;\n\
                     \x20   parts[at++] = l_str_lit(\"{{\");\n\
                     \x20   for (int64_t i = 0; i < n; i++) {{\n\
                     \x20       if (i) parts[at++] = l_str_lit(\", \");\n\
                     \x20       parts[at++] = {inner};\n\
                     \x20   }}\n\
                     \x20   parts[at++] = l_str_lit(\"}}\");\n\
                     \x20   return l_str_concat(at, parts);\n"
                ));
            }

            Ty::Tuple(items) => {
                let n = items.len() * 2 + 1;
                b.push_str(&format!("    l_str parts[{n}];\n"));
                b.push_str("    parts[0] = l_str_lit(\"(\");\n");
                let mut at = 1;
                for (i, t) in items.iter().enumerate() {
                    if i > 0 {
                        b.push_str(&format!("    parts[{at}] = l_str_lit(\", \");\n"));
                        at += 1;
                    }
                    let inner = self.to_str(&format!("v.t{i}"), t);
                    b.push_str(&format!("    parts[{at}] = {inner};\n"));
                    at += 1;
                }
                b.push_str(&format!("    parts[{at}] = l_str_lit(\")\");\n"));
                at += 1;
                b.push_str(&format!("    return l_str_concat({at}, parts);\n"));
            }

            // `null` prints as `null` (SPEC §30).
            Ty::Optional(inner) => {
                let i = self.to_str("v.value", inner);
                b.push_str(&format!(
                    "    if (!v.has) return l_str_lit(\"null\");\n    return {i};\n"
                ));
            }

            Ty::Range(_) => {
                b.push_str(
                    "    l_str parts[3];\n\
                     \x20   parts[0] = l_str_from_int(v.start);\n\
                     \x20   parts[1] = l_str_lit(\"..\");\n\
                     \x20   parts[2] = l_str_from_int(v.end);\n\
                     \x20   return l_str_concat(3, parts);\n",
                );
            }

            _ => {
                b.push_str("    (void)v;\n    return l_str_lit(\"?\");\n");
            }
        }

        b.push_str("}\n\n");
        self.helpers.push_str(&b);
        name
    }
}

// ===========================================================================
// Naming
// ===========================================================================

fn prim_cty(p: Prim) -> &'static str {
    match p {
        Prim::Bool => "int8_t",
        Prim::Char => "int32_t",
        Prim::Str => "l_str",
        Prim::Byte | Prim::Uint8 => "uint8_t",
        Prim::Int8 => "int8_t",
        Prim::Int16 => "int16_t",
        Prim::Int32 => "int32_t",
        Prim::Int | Prim::Int64 => "int64_t",
        Prim::Int128 => "__int128",
        Prim::Uint16 => "uint16_t",
        Prim::Uint32 => "uint32_t",
        Prim::Uint | Prim::Uint64 => "uint64_t",
        Prim::Uint128 => "unsigned __int128",
        Prim::Float32 => "float",
        Prim::Float | Prim::Float64 => "double",
    }
}

/// The C type used at an `extern` boundary (SPEC §72).
fn extern_cty(ty: &Ty) -> String {
    match ty {
        Ty::Prim(Prim::Str) => "const char *".to_string(),
        Ty::Prim(p) => prim_cty(*p).to_string(),
        Ty::Void => "void".to_string(),
        _ => "void *".to_string(),
    }
}

fn adt_cty(p: &Program, def: DefId) -> String {
    let name = p.hir.name_of(def);
    let kind = if p.hir.enums.contains_key(&def) { "E" } else { "S" };
    format!("{kind}_{}_{}", def.0, sanitize(&name))
}

fn fn_name(body: &Body) -> String {
    if body.is_extern {
        return body.name.clone();
    }
    format!("l_{}_{}", body.def.0, sanitize(&body.qualified))
}

/// A structural name for a type, used to name generated helpers.
fn mangle(p: &Program, ty: &Ty) -> String {
    match ty {
        Ty::Prim(prim) => prim.name().to_string(),
        Ty::Void => "void".into(),
        Ty::Err => "err".into(),
        Ty::Array(t) => format!("arr_{}", mangle(p, t)),
        Ty::Map(k, v) => format!("map_{}_{}", mangle(p, k), mangle(p, v)),
        Ty::Set(t) => format!("set_{}", mangle(p, t)),
        Ty::Range(t) => format!("rng_{}", mangle(p, t)),
        Ty::Optional(t) => format!("opt_{}", mangle(p, t)),
        Ty::Tuple(items) => {
            let parts: Vec<String> = items.iter().map(|t| mangle(p, t)).collect();
            format!("tup{}_{}", items.len(), parts.join("_"))
        }
        Ty::Adt { def, .. } => format!("{}_{}", def.0, sanitize(&p.hir.name_of(*def))),
        _ => "other".into(),
    }
}

/// Make a name safe to use as a C identifier.
fn sanitize(name: &str) -> String {
    name.chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '_' })
        .collect()
}

/// Render a string as a C literal, escaping anything that needs it.
fn c_string(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 2);
    out.push('"');
    for b in s.bytes() {
        match b {
            b'"' => out.push_str("\\\""),
            b'\\' => out.push_str("\\\\"),
            b'\n' => out.push_str("\\n"),
            b'\r' => out.push_str("\\r"),
            b'\t' => out.push_str("\\t"),
            0x20..=0x7e => out.push(b as char),
            // Anything else, including UTF-8 continuation bytes, goes out as
            // an octal escape so the literal stays plain ASCII.
            other => out.push_str(&format!("\\{other:03o}")),
        }
    }
    out.push('"');
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn c_strings_are_escaped() {
        assert_eq!(c_string("hi"), "\"hi\"");
        assert_eq!(c_string("a\"b"), "\"a\\\"b\"");
        assert_eq!(c_string("a\nb"), "\"a\\nb\"");
        // Non-ASCII becomes octal escapes, so the literal stays portable.
        assert_eq!(c_string("é"), "\"\\303\\251\"");
    }

    #[test]
    fn identifiers_are_sanitised() {
        assert_eq!(sanitize("users.User.greet"), "users_User_greet");
        assert_eq!(sanitize("main"), "main");
    }

    #[test]
    fn primitives_map_to_fixed_width_c_types() {
        assert_eq!(prim_cty(Prim::Int), "int64_t");
        assert_eq!(prim_cty(Prim::Uint8), "uint8_t");
        assert_eq!(prim_cty(Prim::Float32), "float");
        assert_eq!(prim_cty(Prim::Str), "l_str");
    }

    #[test]
    fn the_runtime_is_embedded() {
        assert!(RUNTIME.contains("l_str_concat"));
        assert!(RUNTIME.contains("l_array_push"));
    }

    #[test]
    fn profiles_choose_optimisation_flags() {
        assert!(Profile::Release.cc_flags().contains(&"-O2"));
        assert!(Profile::Debug.cc_flags().contains(&"-O0"));
    }
}
