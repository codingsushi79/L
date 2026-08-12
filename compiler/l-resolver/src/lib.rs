//! Name resolution for L (SPEC §33, §34, §80).
//!
//! The resolver runs before type checking. It walks every source unit in the
//! compilation, gives each declaration a [`DefId`], groups declarations into
//! modules, and works out what every `use` brings into scope. What it produces
//! is a lookup table: given "the unit I am in" and "the path that was written",
//! it answers "which definition is that".
//!
//! Resolution of *local* names — parameters, `let` bindings, pattern bindings —
//! is not done here. Those scopes are inseparable from inference, so the type
//! checker handles them as it walks a body. This crate owns everything that is
//! visible across a whole module or package.

use l_ast::*;
use l_hir::{Builtin, DefId, Prim};
use l_span::{DiagCode, Diagnostic, Diagnostics, FileId, Span};
use std::collections::HashMap;

// E2xxx is reserved for resolution errors.
const E_DUPLICATE: DiagCode = DiagCode("E2001");
const E_UNKNOWN_MODULE: DiagCode = DiagCode("E2002");
const E_UNKNOWN_NAME: DiagCode = DiagCode("E2003");
const E_NOT_PUBLIC: DiagCode = DiagCode("E2004");
const E_UNKNOWN_MEMBER: DiagCode = DiagCode("E2005");
const E_NO_MAIN: DiagCode = DiagCode("E2006");
const E_UNSUPPORTED: DiagCode = DiagCode("E2007");

/// One parsed source file entering the compilation.
pub struct Unit {
    pub file: FileId,
    /// The module this file declares, or its file stem when it declares none
    /// (SPEC §33, §35).
    pub module: String,
    pub unit: SourceUnit,
}

/// What kind of thing a definition is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DefKind {
    Struct,
    Enum,
    Interface,
    Fn,
    Const,
}

impl DefKind {
    pub fn describe(self) -> &'static str {
        match self {
            DefKind::Struct => "struct",
            DefKind::Enum => "enum",
            DefKind::Interface => "interface",
            DefKind::Fn => "function",
            DefKind::Const => "constant",
        }
    }

    /// Whether the definition names a type, and so may appear in type position.
    pub fn is_type(self) -> bool {
        matches!(self, DefKind::Struct | DefKind::Enum | DefKind::Interface)
    }
}

/// Where in the input a definition's AST lives, so the type checker can find
/// the item again without another search.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ItemLoc {
    /// Index into the slice of units passed to [`resolve`].
    pub unit: usize,
    /// Index into that unit's `items`.
    pub item: usize,
    /// For a method declared inside an `impl` or `interface` block, its index
    /// within that block.
    pub sub: Option<usize>,
}

/// One resolved declaration.
#[derive(Debug, Clone)]
pub struct Def {
    pub id: DefId,
    pub name: String,
    pub module: String,
    /// `module.Type.method` or `module.name`, used for backend symbol names.
    pub qualified: String,
    pub kind: DefKind,
    /// For a method, the name of the type it is attached to (SPEC §27, §28).
    pub receiver: Option<String>,
    /// The interface this method implements, for `impl I for T` blocks.
    pub interface: Option<String>,
    pub public: bool,
    pub loc: ItemLoc,
    pub span: Span,
}

impl Def {
    pub fn is_method(&self) -> bool {
        self.receiver.is_some()
    }
}

/// What a written path resolves to.
#[derive(Debug, Clone, PartialEq)]
pub enum Res {
    /// A user definition.
    Def(DefId),
    /// An enum variant, e.g. `Color.RED` (SPEC §25).
    Variant(DefId, usize),
    /// A primitive type name (SPEC §9).
    Prim(Prim),
    /// A compiler-known function such as `print` (SPEC §67).
    Builtin(Builtin),
    /// A local module in this package.
    Module(String),
    /// A standard-library module such as `math` (SPEC §67).
    StdModule(&'static str),
}

/// The result of resolving a compilation.
pub struct Resolution {
    defs: Vec<Def>,
    /// module name -> (item name -> DefId)
    modules: HashMap<String, HashMap<String, DefId>>,
    /// unit index -> (bound name -> what it names)
    imports: Vec<HashMap<String, Res>>,
    /// unit index -> the module that unit belongs to
    unit_module: Vec<String>,
    /// (type name, method name) -> DefId (SPEC §27, §28)
    methods: HashMap<(String, String), DefId>,
    /// enum name -> variant names, for `Color.RED` lookups
    variants: HashMap<DefId, Vec<String>>,
    /// `fn main` (SPEC §85).
    entry: Option<DefId>,
    pub diagnostics: Diagnostics,
}

/// Standard-library module names (SPEC §67).
pub const STD_MODULES: &[&str] =
    &["math", "fs", "io", "net", "json", "time", "crypto", "random", "process", "env"];

/// Names available without any `use` at all.
fn prelude(name: &str) -> Option<Builtin> {
    use Builtin::*;
    Some(match name {
        "print" => Print,
        "println" => Println,
        "eprint" => EPrint,
        "assert" => Assert,
        "panic" => Panic,
        "str" => ToStr,
        "int" => ToInt,
        "float" => ToFloat,
        "len" => Len,
        _ => return None,
    })
}

/// Functions reached through a standard-library module, e.g. `math.sqrt`.
fn std_member(module: &str, name: &str) -> Option<Builtin> {
    use Builtin::*;
    Some(match (module, name) {
        ("math", "sqrt") => Sqrt,
        ("math", "abs") => Abs,
        ("math", "min") => Min,
        ("math", "max") => Max,
        ("math", "pow") => Pow,
        ("math", "floor") => Floor,
        ("math", "ceil") => Ceil,
        ("time", "now") => Now,
        ("env", "args") => Args,
        ("io", "read_line") => ReadLine,
        ("io", "print") => Print,
        ("io", "println") => Println,
        ("fs", "read_file") => ReadFile,
        ("fs", "write_file") => WriteFile,
        ("process", "exit") => Exit,
        _ => return None,
    })
}

/// Resolve a whole compilation.
pub fn resolve(units: &[Unit]) -> Resolution {
    let mut r = Resolution {
        defs: Vec::new(),
        modules: HashMap::new(),
        imports: Vec::new(),
        unit_module: Vec::new(),
        methods: HashMap::new(),
        variants: HashMap::new(),
        entry: None,
        diagnostics: Diagnostics::new(),
    };
    r.collect(units);
    r.link_imports(units);
    r
}

impl Resolution {
    // ---- construction ----

    /// Pass one: give every declaration a `DefId` and file it under its module.
    fn collect(&mut self, units: &[Unit]) {
        for (ui, unit) in units.iter().enumerate() {
            self.unit_module.push(unit.module.clone());
            self.imports.push(HashMap::new());

            for (ii, item) in unit.unit.items.iter().enumerate() {
                let loc = ItemLoc { unit: ui, item: ii, sub: None };
                match &item.kind {
                    ItemKind::Fn(f) => {
                        let kind = DefKind::Fn;
                        let id = self.declare(
                            &unit.module,
                            &f.name.name,
                            kind,
                            f.receiver.as_ref().map(|r| r.name.clone()),
                            None,
                            item.vis.is_public(),
                            loc,
                            f.name.span,
                        );
                        if let (Some(id), Some(recv)) = (id, &f.receiver) {
                            self.declare_method(&recv.name, &f.name.name, id, f.name.span);
                        }
                        if let Some(id) = id {
                            if f.receiver.is_none() && f.name.name == "main" {
                                self.entry = Some(id);
                            }
                        }
                    }
                    ItemKind::Struct(s) => {
                        self.declare(
                            &unit.module,
                            &s.name.name,
                            DefKind::Struct,
                            None,
                            None,
                            item.vis.is_public(),
                            loc,
                            s.name.span,
                        );
                    }
                    ItemKind::Enum(e) => {
                        if let Some(id) = self.declare(
                            &unit.module,
                            &e.name.name,
                            DefKind::Enum,
                            None,
                            None,
                            item.vis.is_public(),
                            loc,
                            e.name.span,
                        ) {
                            let names = e.variants.iter().map(|v| v.name.name.clone()).collect();
                            self.variants.insert(id, names);
                        }
                    }
                    ItemKind::Interface(i) => {
                        self.declare(
                            &unit.module,
                            &i.name.name,
                            DefKind::Interface,
                            None,
                            None,
                            item.vis.is_public(),
                            loc,
                            i.name.span,
                        );
                        // Interface method signatures are not definitions in
                        // their own right; an `impl` block supplies the bodies.
                    }
                    ItemKind::Const(c) => {
                        self.declare(
                            &unit.module,
                            &c.name.name,
                            DefKind::Const,
                            None,
                            None,
                            item.vis.is_public(),
                            loc,
                            c.name.span,
                        );
                    }
                    ItemKind::Impl(block) => {
                        self.collect_impl(unit, ui, ii, item, block);
                    }
                }
            }
        }

        if self.entry.is_none() {
            // Only a diagnostic for programs; the driver decides whether a
            // missing `main` matters, since libraries have none (SPEC §39).
        }
    }

    /// `impl Printable for User { fn print() { ... } }` (SPEC §28).
    fn collect_impl(
        &mut self,
        unit: &Unit,
        ui: usize,
        ii: usize,
        item: &Item,
        block: &ImplBlock,
    ) {
        let target = match &block.target.kind {
            TypeKind::Named { path, .. } => path.last().name.clone(),
            _ => {
                self.diagnostics.push(
                    Diagnostic::error(E_UNSUPPORTED, "only named types can implement interfaces")
                        .with_primary(block.target.span, "not a named type"),
                );
                return;
            }
        };
        let interface = block.interface.last().name.clone();

        for (mi, method) in block.methods.iter().enumerate() {
            let ItemKind::Fn(f) = &method.kind else { continue };
            let loc = ItemLoc { unit: ui, item: ii, sub: Some(mi) };
            let id = self.declare(
                &unit.module,
                &f.name.name,
                DefKind::Fn,
                Some(target.clone()),
                Some(interface.clone()),
                item.vis.is_public() || method.vis.is_public(),
                loc,
                f.name.span,
            );
            if let Some(id) = id {
                self.declare_method(&target, &f.name.name, id, f.name.span);
            }
        }
    }

    fn declare(
        &mut self,
        module: &str,
        name: &str,
        kind: DefKind,
        receiver: Option<String>,
        interface: Option<String>,
        public: bool,
        loc: ItemLoc,
        span: Span,
    ) -> Option<DefId> {
        let id = DefId(self.defs.len() as u32);
        let qualified = match &receiver {
            Some(r) => format!("{module}.{r}.{name}"),
            None => format!("{module}.{name}"),
        };

        // Methods live in a separate namespace keyed by their receiver, so
        // `fn User.print` and `fn Doc.print` do not collide.
        if receiver.is_none() {
            let table = self.modules.entry(module.to_string()).or_default();
            if let Some(&prev) = table.get(name) {
                let prev_span = self.defs[prev.0 as usize].span;
                self.diagnostics.push(
                    Diagnostic::error(E_DUPLICATE, format!("`{name}` is defined more than once"))
                        .with_primary(span, "redefined here")
                        .with_secondary(prev_span, "first defined here")
                        .with_note("each name may be defined only once in a module"),
                );
                return None;
            }
            table.insert(name.to_string(), id);
        }

        self.defs.push(Def {
            id,
            name: name.to_string(),
            module: module.to_string(),
            qualified,
            kind,
            receiver,
            interface,
            public,
            loc,
            span,
        });
        Some(id)
    }

    fn declare_method(&mut self, target: &str, name: &str, id: DefId, span: Span) {
        let key = (target.to_string(), name.to_string());
        if let Some(&prev) = self.methods.get(&key) {
            let prev_span = self.defs[prev.0 as usize].span;
            self.diagnostics.push(
                Diagnostic::error(
                    E_DUPLICATE,
                    format!("`{target}` already has a method named `{name}`"),
                )
                .with_primary(span, "redefined here")
                .with_secondary(prev_span, "first defined here"),
            );
            return;
        }
        self.methods.insert(key, id);
    }

    /// Pass two: work out what each `use` binds (SPEC §34).
    fn link_imports(&mut self, units: &[Unit]) {
        for (ui, unit) in units.iter().enumerate() {
            for use_decl in &unit.unit.uses {
                for tree in &use_decl.trees {
                    let bound = tree.bound_name().name.clone();
                    let Some(res) = self.resolve_use_path(&tree.path) else {
                        continue;
                    };
                    self.imports[ui].insert(bound, res);
                }
            }
        }
    }

    fn resolve_use_path(&mut self, path: &Path) -> Option<Res> {
        let segs: Vec<&str> = path.segments.iter().map(|s| s.name.as_str()).collect();
        match segs.as_slice() {
            // `use math` / `use users`
            [one] => {
                if let Some(std) = STD_MODULES.iter().find(|m| *m == one) {
                    return Some(Res::StdModule(std));
                }
                if self.modules.contains_key(*one) {
                    return Some(Res::Module((*one).to_string()));
                }
                self.diagnostics.push(
                    Diagnostic::error(E_UNKNOWN_MODULE, format!("no module named `{one}`"))
                        .with_primary(path.span, "not found")
                        .with_note(
                            "a module comes from a `.lsh` file in `src/`, or from the standard \
                             library",
                        ),
                );
                None
            }
            // `use math.sqrt` / `use users.User`
            [module, name] => {
                if STD_MODULES.contains(module) {
                    return match std_member(module, name) {
                        Some(b) => Some(Res::Builtin(b)),
                        None => {
                            self.diagnostics.push(
                                Diagnostic::error(
                                    E_UNKNOWN_MEMBER,
                                    format!("`{module}` has no member `{name}`"),
                                )
                                .with_primary(path.span, "not found")
                                .with_note(
                                    "this preview of the standard library implements only part \
                                     of SPEC §67",
                                ),
                            );
                            None
                        }
                    };
                }
                let Some(table) = self.modules.get(*module) else {
                    self.diagnostics.push(
                        Diagnostic::error(E_UNKNOWN_MODULE, format!("no module named `{module}`"))
                            .with_primary(path.span, "not found"),
                    );
                    return None;
                };
                let Some(&id) = table.get(*name) else {
                    self.diagnostics.push(
                        Diagnostic::error(
                            E_UNKNOWN_MEMBER,
                            format!("`{module}` has no member `{name}`"),
                        )
                        .with_primary(path.span, "not found"),
                    );
                    return None;
                };
                if !self.defs[id.0 as usize].public {
                    self.diagnostics.push(
                        Diagnostic::error(E_NOT_PUBLIC, format!("`{name}` is private"))
                            .with_primary(path.span, "not visible here")
                            .with_secondary(self.defs[id.0 as usize].span, "declared here")
                            .with_note("declarations are private unless marked `pub` (SPEC §33)")
                            .with_suggestion(
                                "make it public",
                                self.defs[id.0 as usize].span.shrink_to_lo(),
                                "pub ",
                            ),
                    );
                }
                Some(Res::Def(id))
            }
            _ => {
                self.diagnostics.push(
                    Diagnostic::error(E_UNSUPPORTED, "nested module paths are not supported")
                        .with_primary(path.span, "too many segments")
                        .with_note("SPEC §34 defines `use module` and `use module.symbol`"),
                );
                None
            }
        }
    }

    // ---- queries ----

    pub fn defs(&self) -> &[Def] {
        &self.defs
    }

    pub fn def(&self, id: DefId) -> &Def {
        &self.defs[id.0 as usize]
    }

    pub fn entry(&self) -> Option<DefId> {
        self.entry
    }

    pub fn module_of_unit(&self, unit: usize) -> &str {
        &self.unit_module[unit]
    }

    /// The definitions declared in a module, in declaration order.
    pub fn module_defs(&self, module: &str) -> Vec<DefId> {
        let mut ids: Vec<DefId> = self
            .modules
            .get(module)
            .map(|t| t.values().copied().collect())
            .unwrap_or_default();
        ids.sort();
        ids
    }

    /// Look up a method on a named type (SPEC §27).
    pub fn method(&self, type_name: &str, method: &str) -> Option<DefId> {
        self.methods.get(&(type_name.to_string(), method.to_string())).copied()
    }

    /// Every method attached to a type, for `llsp` completion and `ldoc`.
    pub fn methods_of(&self, type_name: &str) -> Vec<DefId> {
        let mut out: Vec<DefId> = self
            .methods
            .iter()
            .filter(|((t, _), _)| t == type_name)
            .map(|(_, id)| *id)
            .collect();
        out.sort();
        out
    }

    /// Resolve a single-segment name from inside `unit`.
    ///
    /// The order is: the enclosing module, then names brought in by `use`,
    /// then the prelude, then primitive type names (SPEC §9, §33, §34).
    pub fn lookup(&self, unit: usize, name: &str) -> Option<Res> {
        let module = &self.unit_module[unit];
        if let Some(table) = self.modules.get(module) {
            if let Some(&id) = table.get(name) {
                return Some(Res::Def(id));
            }
        }
        if let Some(res) = self.imports[unit].get(name) {
            return Some(res.clone());
        }
        if let Some(b) = prelude(name) {
            return Some(Res::Builtin(b));
        }
        if let Some(p) = Prim::from_name(name) {
            return Some(Res::Prim(p));
        }
        if STD_MODULES.contains(&name) {
            // Reachable without `use` only for diagnostics; a `use` is still
            // required to bring the module into scope.
            return None;
        }
        None
    }

    /// Resolve a dotted path written in source, from inside `unit`.
    ///
    /// Returns `Ok(None)` when the path's leading segment resolves but the rest
    /// must be interpreted as field access on a value — `user.name` and
    /// `Color.RED` are spelled identically, so the caller decides.
    pub fn lookup_path(&self, unit: usize, path: &Path) -> Option<Res> {
        let segs: Vec<&str> = path.segments.iter().map(|s| s.name.as_str()).collect();
        match segs.as_slice() {
            [one] => self.lookup(unit, one),
            [first, second] => match self.lookup(unit, first)? {
                Res::Module(m) => {
                    let id = *self.modules.get(&m)?.get(*second)?;
                    Some(Res::Def(id))
                }
                Res::StdModule(m) => std_member(m, second).map(Res::Builtin),
                Res::Def(id) => self.member_of_def(id, second),
                other => Some(other),
            },
            // `module.Type.method`
            [first, second, third] => {
                let mid = match self.lookup(unit, first)? {
                    Res::Module(m) => m,
                    _ => return None,
                };
                let id = *self.modules.get(&mid)?.get(*second)?;
                self.member_of_def(id, third)
            }
            _ => None,
        }
    }

    /// A member reached through a definition: an enum variant, or a method.
    fn member_of_def(&self, id: DefId, name: &str) -> Option<Res> {
        let def = self.def(id);
        if def.kind == DefKind::Enum {
            if let Some(vs) = self.variants.get(&id) {
                if let Some(idx) = vs.iter().position(|v| v == name) {
                    return Some(Res::Variant(id, idx));
                }
            }
        }
        if def.kind.is_type() {
            if let Some(m) = self.method(&def.name, name) {
                return Some(Res::Def(m));
            }
        }
        None
    }

    /// Report a name that could not be resolved, with a spelling suggestion.
    pub fn unknown_name_error(&self, unit: usize, name: &str, span: Span) -> Diagnostic {
        let mut diag = Diagnostic::error(E_UNKNOWN_NAME, format!("cannot find `{name}` in scope"))
            .with_primary(span, "not found");
        if let Some(similar) = self.suggest(unit, name) {
            diag = diag.with_suggestion("a similar name exists", span, similar);
        }
        diag
    }

    /// The closest name in scope, by edit distance.
    pub fn suggest(&self, unit: usize, name: &str) -> Option<String> {
        let module = &self.unit_module[unit];
        let mut candidates: Vec<String> = Vec::new();
        if let Some(table) = self.modules.get(module) {
            candidates.extend(table.keys().cloned());
        }
        candidates.extend(self.imports[unit].keys().cloned());
        candidates.extend(
            ["print", "println", "assert", "panic", "str", "int", "float", "len"]
                .iter()
                .map(|s| s.to_string()),
        );

        let mut best: Option<(usize, String)> = None;
        for cand in candidates {
            let d = edit_distance(name, &cand);
            // Only suggest names that are genuinely close.
            let limit = (name.len() / 3).max(1);
            if d <= limit && best.as_ref().is_none_or(|(bd, _)| d < *bd) {
                best = Some((d, cand));
            }
        }
        best.map(|(_, c)| c)
    }

    /// A diagnostic for a program with no `fn main` (SPEC §85).
    pub fn missing_main_error(&self) -> Diagnostic {
        Diagnostic::error(E_NO_MAIN, "this program has no `fn main`")
            .with_note("an executable package must define `fn main()` in `src/main.lsh`")
    }
}

/// Levenshtein distance, used only for "did you mean" suggestions.
fn edit_distance(a: &str, b: &str) -> usize {
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

#[cfg(test)]
mod tests {
    use super::*;
    use l_span::FileId;

    fn unit(module: &str, src: &str) -> Unit {
        let parsed = l_parser::parse_source(FileId(0), src);
        assert!(!parsed.diagnostics.has_errors(), "parse failed for test input");
        Unit { file: FileId(0), module: module.into(), unit: parsed.unit }
    }

    fn errors(r: &Resolution) -> Vec<String> {
        r.diagnostics.iter().map(|d| d.message.clone()).collect()
    }

    #[test]
    fn collects_definitions_and_finds_main() {
        let r = resolve(&[unit(
            "main",
            "struct User { str name }\nfn main() {\n}\nfn helper() {\n}\n",
        )]);
        assert!(errors(&r).is_empty(), "{:?}", errors(&r));
        assert!(r.entry.is_some());
        assert_eq!(r.defs().len(), 3);
        assert!(matches!(r.lookup(0, "User"), Some(Res::Def(_))));
        assert!(matches!(r.lookup(0, "helper"), Some(Res::Def(_))));
    }

    #[test]
    fn rejects_duplicate_definitions() {
        let r = resolve(&[unit("main", "fn a() {\n}\nfn a() {\n}\n")]);
        assert!(errors(&r).iter().any(|e| e.contains("defined more than once")));
    }

    #[test]
    fn resolves_methods_by_receiver() {
        let r = resolve(&[unit(
            "main",
            "struct User { str name }\nfn User.greet() {\n}\nfn main() {\n}\n",
        )]);
        assert!(errors(&r).is_empty(), "{:?}", errors(&r));
        assert!(r.method("User", "greet").is_some());
        assert!(r.method("User", "nope").is_none());
    }

    #[test]
    fn same_method_name_on_two_types_is_allowed() {
        let r = resolve(&[unit(
            "main",
            "struct A { int x }\nstruct B { int x }\nfn A.show() {\n}\nfn B.show() {\n}\n",
        )]);
        assert!(errors(&r).is_empty(), "{:?}", errors(&r));
        assert_ne!(r.method("A", "show"), r.method("B", "show"));
    }

    #[test]
    fn resolves_enum_variants() {
        let r = resolve(&[unit("main", "enum Color {\n RED\n GREEN\n}\nfn main() {\n}\n")]);
        let path = Path {
            segments: vec![
                Ident::new("Color", Span::dummy()),
                Ident::new("GREEN", Span::dummy()),
            ],
            span: Span::dummy(),
        };
        assert!(matches!(r.lookup_path(0, &path), Some(Res::Variant(_, 1))));
    }

    #[test]
    fn use_brings_a_local_module_into_scope() {
        let users = unit("users", "pub struct User {\n pub str name\n}\npub fn create() {\n}\n");
        let main = unit("main", "use users\nfn main() {\n}\n");
        let r = resolve(&[users, main]);
        assert!(errors(&r).is_empty(), "{:?}", errors(&r));

        let path = Path {
            segments: vec![
                Ident::new("users", Span::dummy()),
                Ident::new("create", Span::dummy()),
            ],
            span: Span::dummy(),
        };
        assert!(matches!(r.lookup_path(1, &path), Some(Res::Def(_))));
    }

    #[test]
    fn use_of_a_specific_symbol_binds_the_last_segment() {
        let users = unit("users", "pub fn create() {\n}\n");
        let main = unit("main", "use users.create\nfn main() {\n}\n");
        let r = resolve(&[users, main]);
        assert!(errors(&r).is_empty(), "{:?}", errors(&r));
        assert!(matches!(r.lookup(1, "create"), Some(Res::Def(_))));
    }

    #[test]
    fn use_alias_rebinds_the_name() {
        let db = unit("database", "pub fn open() {\n}\n");
        let main = unit("main", "use database as db\nfn main() {\n}\n");
        let r = resolve(&[db, main]);
        assert!(errors(&r).is_empty(), "{:?}", errors(&r));
        assert!(matches!(r.lookup(1, "db"), Some(Res::Module(_))));
        assert!(r.lookup(1, "database").is_none());
    }

    #[test]
    fn private_symbols_cannot_be_imported() {
        let users = unit("users", "fn secret() {\n}\n");
        let main = unit("main", "use users.secret\nfn main() {\n}\n");
        let r = resolve(&[users, main]);
        assert!(errors(&r).iter().any(|e| e.contains("is private")), "{:?}", errors(&r));
    }

    #[test]
    fn std_modules_resolve_to_builtins() {
        let r = resolve(&[unit("main", "use math\nfn main() {\n}\n")]);
        assert!(errors(&r).is_empty(), "{:?}", errors(&r));
        let path = Path {
            segments: vec![Ident::new("math", Span::dummy()), Ident::new("sqrt", Span::dummy())],
            span: Span::dummy(),
        };
        assert_eq!(r.lookup_path(0, &path), Some(Res::Builtin(Builtin::Sqrt)));
    }

    #[test]
    fn unknown_module_is_reported() {
        let r = resolve(&[unit("main", "use nowhere\nfn main() {\n}\n")]);
        assert!(errors(&r).iter().any(|e| e.contains("no module named `nowhere`")));
    }

    #[test]
    fn prelude_and_primitives_are_always_in_scope() {
        let r = resolve(&[unit("main", "fn main() {\n}\n")]);
        assert_eq!(r.lookup(0, "print"), Some(Res::Builtin(Builtin::Print)));
        assert_eq!(r.lookup(0, "int"), Some(Res::Builtin(Builtin::ToInt)));
        assert_eq!(r.lookup(0, "bool"), Some(Res::Prim(Prim::Bool)));
    }

    #[test]
    fn suggests_close_names() {
        let r = resolve(&[unit("main", "fn calculate() {\n}\nfn main() {\n}\n")]);
        assert_eq!(r.suggest(0, "calculat"), Some("calculate".to_string()));
        assert_eq!(r.suggest(0, "zzzzzzzzz"), None);
    }

    #[test]
    fn edit_distance_is_symmetric_and_zero_for_equal() {
        assert_eq!(edit_distance("abc", "abc"), 0);
        assert_eq!(edit_distance("kitten", "sitting"), 3);
        assert_eq!(edit_distance("sitting", "kitten"), 3);
    }
}
