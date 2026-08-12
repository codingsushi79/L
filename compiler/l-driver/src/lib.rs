//! The L compiler driver (SPEC §80).
//!
//! This crate is the pipeline of SPEC §80 written out once, so that every tool
//! that needs to understand L source — `lsc`, `lpm`, `llint`, `llsp`, `ldoc` —
//! drives the compiler the same way and reports the same diagnostics.
//!
//! ```text
//! source -> lexer -> parser -> AST -> resolver -> type checker
//!        -> HIR -> MIR -> optimiser -> C -> executable
//! ```
//!
//! Each stage is allowed to fail without stopping the ones that can still run
//! usefully: parse errors in one file do not prevent the others being parsed,
//! and resolution still runs so that a file with a syntax error still gets its
//! names checked. Code generation is the exception — it runs only when nothing
//! has reported an error.

use l_hir::Hir;
use l_manifest::{Manifest, MANIFEST_FILE};
use l_resolver::{Resolution, Unit};
use l_span::{DiagCode, Diagnostic, Diagnostics, Emitter, Severity, SourceMap};
use std::path::{Path, PathBuf};

pub use l_backend::{Options as BackendOptions, Profile};
pub use l_opt::Level as OptLevel;

const E_IO: DiagCode = DiagCode("E0001");
const E_NO_SOURCES: DiagCode = DiagCode("E0002");
const E_BAD_MANIFEST: DiagCode = DiagCode("E0003");

/// The file extension of L source (SPEC §5).
pub const SOURCE_EXT: &str = "lsh";

/// How far to run the pipeline.
#[derive(Clone, Copy, PartialEq, Eq, Debug, PartialOrd, Ord)]
pub enum Stage {
    /// Stop after parsing. Used by `lformat`.
    Parse,
    /// Stop after type checking. Used by `llint`, `llsp` and `ldoc`.
    Check,
    /// Generate C but do not compile it.
    Codegen,
    /// Produce an executable.
    Link,
}

/// What to compile and how.
pub struct Config {
    /// The source files, in the order they should be compiled.
    pub inputs: Vec<PathBuf>,
    pub stage: Stage,
    pub profile: Profile,
    /// Where the executable goes; defaults to the stem of the first input.
    pub output: Option<PathBuf>,
    /// Keep the generated C alongside the executable.
    pub emit_c: bool,
    /// Build the test harness instead of `main` (SPEC §74).
    pub tests: bool,
    /// Require a `fn main`. False for libraries (SPEC §39).
    pub require_main: bool,
}

impl Config {
    /// A configuration for compiling a list of files to an executable.
    pub fn build(inputs: Vec<PathBuf>) -> Config {
        Config {
            inputs,
            stage: Stage::Link,
            profile: Profile::Debug,
            output: None,
            emit_c: false,
            tests: false,
            require_main: true,
        }
    }

    /// A configuration that stops after type checking, for the tools.
    pub fn check(inputs: Vec<PathBuf>) -> Config {
        Config { stage: Stage::Check, require_main: false, ..Config::build(inputs) }
    }
}

/// Everything a run of the pipeline produced.
///
/// Analysis results are kept even when compilation failed, because the tools
/// want whatever was successfully worked out about a broken file.
pub struct Compilation {
    pub sources: SourceMap,
    pub units: Vec<Unit>,
    pub resolution: Option<Resolution>,
    pub hir: Option<Hir>,
    /// The generated C, when the run reached code generation.
    pub c_source: Option<String>,
    /// The executable, when the run linked one.
    pub executable: Option<PathBuf>,
    pub diagnostics: Diagnostics,
}

impl Compilation {
    pub fn failed(&self) -> bool {
        self.diagnostics.has_errors()
    }

    /// Print every diagnostic, then a summary line, and report whether the run
    /// succeeded (SPEC §103).
    pub fn report(&self) -> bool {
        let mut emitter = Emitter::default();
        for diag in self.diagnostics.iter() {
            emitter.emit(&self.sources, diag);
        }
        emitter.emit_summary();
        !emitter.has_errors()
    }
}

/// Run the pipeline.
pub fn compile(config: &Config) -> Compilation {
    let mut c = Compilation {
        sources: SourceMap::new(),
        units: Vec::new(),
        resolution: None,
        hir: None,
        c_source: None,
        executable: None,
        diagnostics: Diagnostics::new(),
    };

    if config.inputs.is_empty() {
        c.diagnostics.push(
            Diagnostic::error(E_NO_SOURCES, "no source files to compile")
                .with_note(format!("L source files end in `.{SOURCE_EXT}` (SPEC §5)")),
        );
        return c;
    }

    // ---- lex and parse ----
    for path in &config.inputs {
        let file = match c.sources.load(path) {
            Ok(f) => f,
            Err(e) => {
                c.diagnostics.push(Diagnostic::error(
                    E_IO,
                    format!("cannot read `{}`: {e}", path.display()),
                ));
                continue;
            }
        };
        let src = c.sources.get(file).src.clone();
        let parsed = l_parser::parse_source(file, &src);
        c.diagnostics.extend(parsed.diagnostics);
        c.units.push(Unit {
            file,
            module: module_name(path, &parsed.unit),
            unit: parsed.unit,
        });
    }

    if config.stage == Stage::Parse {
        return c;
    }

    // ---- resolve ----
    let resolution = l_resolver::resolve(&c.units);
    c.diagnostics.extend(resolution.diagnostics.clone());

    if config.require_main && resolution.entry().is_none() && !c.diagnostics.has_errors() {
        c.diagnostics.push(resolution.missing_main_error());
    }

    // ---- type check and lower to HIR ----
    let checked = l_typeck::check(&c.units, &resolution);
    c.diagnostics.extend(checked.diagnostics);
    c.hir = Some(checked.hir);
    c.resolution = Some(resolution);

    if config.stage == Stage::Check || c.diagnostics.has_errors() {
        return c;
    }

    // ---- MIR, optimisation, code generation ----
    let hir = c.hir.take().expect("hir was just set");
    let mut program = l_mir::lower(hir);

    let level = match config.profile {
        Profile::Release => OptLevel::Full,
        Profile::Debug => OptLevel::None,
    };
    l_opt::optimise(&mut program, level);

    c.c_source = Some(l_backend::emit_c(&program, config.tests));

    if config.stage == Stage::Codegen {
        c.hir = Some(program.hir);
        return c;
    }

    // ---- link ----
    let output = config.output.clone().unwrap_or_else(|| default_output(&config.inputs));
    let options = BackendOptions {
        profile: config.profile,
        output,
        emit_c: config.emit_c,
        test_harness: config.tests,
    };
    match l_backend::build(&program, &options) {
        Ok(path) => c.executable = Some(path),
        Err(diag) => c.diagnostics.push(diag),
    }

    c.hir = Some(program.hir);
    c
}

/// The module a file belongs to (SPEC §33, §35).
///
/// A `module x` declaration wins; otherwise the file's stem names the module,
/// which is what makes `users.lsh` importable as `use users`.
fn module_name(path: &Path, unit: &l_ast::SourceUnit) -> String {
    if let Some(decl) = &unit.module {
        return decl.name.name.clone();
    }
    path.file_stem()
        .map(|s| s.to_string_lossy().to_string())
        .unwrap_or_else(|| "main".to_string())
}

fn default_output(inputs: &[PathBuf]) -> PathBuf {
    // Prefer `main.lsh` if it is present, since that is where `fn main` lives.
    let chosen = inputs
        .iter()
        .find(|p| p.file_stem().is_some_and(|s| s == "main"))
        .or_else(|| inputs.first());
    match chosen {
        Some(p) => PathBuf::from(p.file_stem().unwrap_or_default()),
        None => PathBuf::from("a.out"),
    }
}

// ===========================================================================
// Projects
// ===========================================================================

/// A package on disk (SPEC §36).
pub struct Project {
    pub root: PathBuf,
    pub manifest: Manifest,
    pub sources: Vec<PathBuf>,
    pub tests: Vec<PathBuf>,
}

impl Project {
    pub fn name(&self) -> String {
        self.manifest
            .package
            .as_ref()
            .map(|p| p.name.clone())
            .unwrap_or_else(|| "package".to_string())
    }

    pub fn is_lib(&self) -> bool {
        self.manifest.package.as_ref().is_some_and(|p| p.is_lib)
    }

    /// Where build output goes.
    pub fn target_dir(&self, profile: Profile) -> PathBuf {
        let sub = match profile {
            Profile::Debug => "debug",
            Profile::Release => "release",
        };
        self.root.join("target").join(sub)
    }

    /// A configuration that builds this package.
    pub fn build_config(&self, profile: Profile) -> Config {
        let out = self.target_dir(profile).join(self.name());
        Config {
            inputs: self.sources.clone(),
            stage: Stage::Link,
            profile,
            output: Some(out),
            emit_c: false,
            tests: false,
            require_main: !self.is_lib(),
        }
    }

    /// A configuration that builds this package's tests (SPEC §74).
    pub fn test_config(&self, profile: Profile) -> Config {
        let mut inputs = self.sources.clone();
        inputs.extend(self.tests.iter().cloned());
        Config {
            inputs,
            stage: Stage::Link,
            profile,
            output: Some(self.target_dir(profile).join(format!("{}-tests", self.name()))),
            emit_c: false,
            tests: true,
            require_main: false,
        }
    }
}

/// Find the package containing `start` and read its manifest (SPEC §36).
pub fn open_project(start: impl AsRef<Path>) -> Result<Project, Diagnostic> {
    let root = l_manifest::find_manifest_dir(&start).ok_or_else(|| {
        Diagnostic::error(E_BAD_MANIFEST, format!("no `{MANIFEST_FILE}` found"))
            .with_note(format!(
                "a package is a directory containing `{MANIFEST_FILE}` and `src/` (SPEC §36)"
            ))
            .with_note("create one with `lpm new <name>`")
    })?;

    let manifest = Manifest::load(root.join(MANIFEST_FILE)).map_err(|e| {
        Diagnostic::error(E_BAD_MANIFEST, format!("cannot read `{MANIFEST_FILE}`: {e}"))
    })?;

    let sources = collect_sources(&root.join("src"));
    let tests = collect_sources(&root.join("tests"));

    Ok(Project { root, manifest, sources, tests })
}

/// Every `.lsh` file in a directory, sorted, with `main.lsh` last.
///
/// The ordering matters only for determinism; resolution is order-independent.
pub fn collect_sources(dir: &Path) -> Vec<PathBuf> {
    let mut out = Vec::new();
    collect_into(dir, &mut out);
    out.sort();
    // `main.lsh` compiles last so that the entry point is easy to find in the
    // generated C.
    out.sort_by_key(|p| p.file_stem().is_some_and(|s| s == "main"));
    out
}

fn collect_into(dir: &Path, out: &mut Vec<PathBuf>) {
    let Ok(entries) = std::fs::read_dir(dir) else { return };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            collect_into(&path, out);
        } else if path.extension().is_some_and(|e| e == SOURCE_EXT) {
            out.push(path);
        }
    }
}

/// A diagnostic with no source location, for driver-level failures.
pub fn bare_error(message: impl Into<String>) -> Diagnostic {
    Diagnostic::bare(Severity::Error, message)
}

#[cfg(test)]
mod tests {
    use super::*;
    use l_span::FileId;
    use std::fs;

    fn temp_dir(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("l-driver-test-{name}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).expect("create temp dir");
        dir
    }

    fn write(path: &Path, contents: &str) {
        if let Some(p) = path.parent() {
            fs::create_dir_all(p).expect("create parent");
        }
        fs::write(path, contents).expect("write file");
    }

    #[test]
    fn a_module_declaration_names_the_module() {
        let parsed = l_parser::parse_source(FileId(0), "module users\n");
        assert_eq!(module_name(Path::new("/tmp/whatever.lsh"), &parsed.unit), "users");
    }

    #[test]
    fn without_a_declaration_the_file_stem_names_the_module() {
        let parsed = l_parser::parse_source(FileId(0), "fn main() {\n}\n");
        assert_eq!(module_name(Path::new("/tmp/users.lsh"), &parsed.unit), "users");
    }

    #[test]
    fn missing_input_is_reported_not_panicked() {
        let config = Config::check(vec![PathBuf::from("/does/not/exist.lsh")]);
        let out = compile(&config);
        assert!(out.failed());
        assert!(out.diagnostics.iter().any(|d| d.message.contains("cannot read")));
    }

    #[test]
    fn no_inputs_is_an_error() {
        let out = compile(&Config::check(vec![]));
        assert!(out.failed());
        assert!(out.diagnostics.iter().any(|d| d.message.contains("no source files")));
    }

    #[test]
    fn checking_reaches_hir_for_a_valid_program() {
        let dir = temp_dir("check");
        let main = dir.join("main.lsh");
        write(&main, "fn main() {\n    print(\"hi\")\n}\n");

        let out = compile(&Config::check(vec![main]));
        assert!(!out.failed(), "{:?}", out.diagnostics.iter().map(|d| &d.message).collect::<Vec<_>>());
        let hir = out.hir.expect("hir");
        assert!(hir.entry.is_some());
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_program_without_main_is_reported_when_one_is_required() {
        let dir = temp_dir("nomain");
        let f = dir.join("lib.lsh");
        write(&f, "pub fn helper() {\n}\n");

        let out = compile(&Config::build(vec![f.clone()]));
        assert!(out.diagnostics.iter().any(|d| d.message.contains("no `fn main`")));

        // A library is allowed to have none (SPEC §39).
        let mut lib = Config::build(vec![f]);
        lib.require_main = false;
        lib.stage = Stage::Check;
        let out = compile(&lib);
        assert!(!out.failed());
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn sources_are_collected_with_main_last() {
        let dir = temp_dir("collect");
        write(&dir.join("src/users.lsh"), "module users\n");
        write(&dir.join("src/main.lsh"), "fn main() {\n}\n");
        write(&dir.join("src/notes.txt"), "ignored");

        let found = collect_sources(&dir.join("src"));
        assert_eq!(found.len(), 2);
        assert!(found[1].ends_with("main.lsh"));
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_project_is_found_from_a_subdirectory() {
        let dir = temp_dir("project");
        write(
            &dir.join("lsharp.toml"),
            "[package]\nname = \"demo\"\nversion = \"1.0.0\"\n",
        );
        write(&dir.join("src/main.lsh"), "fn main() {\n}\n");

        let project = open_project(dir.join("src")).expect("project");
        assert_eq!(project.name(), "demo");
        assert_eq!(project.sources.len(), 1);
        assert!(!project.is_lib());
        assert!(project.target_dir(Profile::Debug).ends_with("target/debug"));
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn opening_a_project_outside_one_explains_how_to_make_one() {
        let dir = temp_dir("noproject");
        let Err(err) = open_project(&dir) else { panic!("should fail") };
        assert!(err.message.contains("lsharp.toml"));
        let _ = fs::remove_dir_all(&dir);
    }
}
