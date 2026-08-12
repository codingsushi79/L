//! `lsc`, the L compiler command line (SPEC §4).
//!
//! ```text
//! lsc main.lsh        compile one file
//! lsc build           build the package in the current directory
//! lsc run             build it and run it
//! ```
//!
//! For projects `lpm` is the preferred interface (SPEC §4); `lsc build` and
//! `lsc run` exist so that the compiler alone is enough to work with a package.

use l_driver::{Config, Profile, Stage};
use l_span::{Diagnostic, Emitter, Severity, SourceMap};
use std::path::PathBuf;
use std::process::Command;

/// The compiler version, kept in step with the language version (SPEC §101).
pub const VERSION: &str = env!("CARGO_PKG_VERSION");

/// The language version this compiler implements (SPEC §101).
pub const LANGUAGE_VERSION: &str = "1.0";

const USAGE: &str = "\
lsc — the L compiler

Usage:
    lsc <file.lsh>...        Compile source files into an executable
    lsc build                Build the package in the current directory
    lsc run [-- args...]     Build the package and run it
    lsc check                Check the package without generating code
    lsc explain <code>       Explain a diagnostic code

Options:
    -o, --output <path>      Where to write the executable
        --release            Optimise the build (SPEC §82)
        --debug              Build without optimisation (the default)
        --emit-c             Keep the generated C next to the output
        --check              Stop after type checking
    -h, --help               Show this message
    -V, --version            Show the version
";

/// Run the command line. Returns the process exit code.
pub fn main(args: Vec<String>) -> i32 {
    let mut inputs: Vec<PathBuf> = Vec::new();
    let mut output: Option<PathBuf> = None;
    let mut profile = Profile::Debug;
    let mut emit_c = false;
    let mut check_only = false;
    let mut command: Option<String> = None;
    let mut run_args: Vec<String> = Vec::new();

    let mut it = args.into_iter().peekable();
    while let Some(arg) = it.next() {
        match arg.as_str() {
            "-h" | "--help" => {
                print!("{USAGE}");
                return 0;
            }
            "-V" | "--version" => {
                println!("lsc {VERSION} (L {LANGUAGE_VERSION})");
                return 0;
            }
            "--release" => profile = Profile::Release,
            "--debug" => profile = Profile::Debug,
            "--emit-c" => emit_c = true,
            "--check" => check_only = true,
            "-o" | "--output" => match it.next() {
                Some(path) => output = Some(PathBuf::from(path)),
                None => return fail("`-o` needs a path"),
            },
            "--" => {
                run_args.extend(it.by_ref());
                break;
            }
            "build" | "run" | "check" | "explain" if command.is_none() && inputs.is_empty() => {
                command = Some(arg);
            }
            other if other.starts_with('-') => {
                return fail(format!("unknown option `{other}`"));
            }
            other => {
                if command.as_deref() == Some("explain") {
                    return explain(other);
                }
                inputs.push(PathBuf::from(other));
            }
        }
    }

    if check_only {
        command = Some("check".to_string());
    }

    match command.as_deref() {
        Some("explain") => fail("`lsc explain` needs a diagnostic code, e.g. `lsc explain E3001`"),
        Some("build") => build_project(profile, emit_c, false, Vec::new()),
        Some("run") => build_project(profile, emit_c, true, run_args),
        Some("check") if inputs.is_empty() => check_project(profile),
        Some("check") => compile_files(inputs, output, profile, emit_c, Stage::Check, false, vec![]),
        _ if inputs.is_empty() => {
            print!("{USAGE}");
            0
        }
        _ => compile_files(inputs, output, profile, emit_c, Stage::Link, false, run_args),
    }
}

fn compile_files(
    inputs: Vec<PathBuf>,
    output: Option<PathBuf>,
    profile: Profile,
    emit_c: bool,
    stage: Stage,
    then_run: bool,
    run_args: Vec<String>,
) -> i32 {
    let config = Config {
        inputs,
        stage,
        profile,
        output,
        emit_c,
        tests: false,
        require_main: stage == Stage::Link,
    };

    let result = l_driver::compile(&config);
    if !result.report() {
        return 1;
    }

    match (then_run, &result.executable) {
        (true, Some(exe)) => run(exe, &run_args),
        _ => 0,
    }
}

fn build_project(profile: Profile, emit_c: bool, then_run: bool, run_args: Vec<String>) -> i32 {
    let project = match l_driver::open_project(".") {
        Ok(p) => p,
        Err(diag) => return report_bare(&diag),
    };

    let mut config = project.build_config(profile);
    config.emit_c = emit_c;

    let result = l_driver::compile(&config);
    if !result.report() {
        return 1;
    }

    if let Some(exe) = &result.executable {
        eprintln!("    built {}", exe.display());
        if then_run {
            return run(exe, &run_args);
        }
    }
    0
}

fn check_project(profile: Profile) -> i32 {
    let project = match l_driver::open_project(".") {
        Ok(p) => p,
        Err(diag) => return report_bare(&diag),
    };
    let mut config = project.build_config(profile);
    config.stage = Stage::Check;

    let result = l_driver::compile(&config);
    if !result.report() {
        return 1;
    }
    eprintln!("    checked {}", project.name());
    0
}

fn run(exe: &PathBuf, args: &[String]) -> i32 {
    // A relative path needs `./` to be executable on Unix.
    let program = if exe.is_absolute() { exe.clone() } else { PathBuf::from(".").join(exe) };
    match Command::new(&program).args(args).status() {
        Ok(status) => status.code().unwrap_or(1),
        Err(e) => fail(format!("cannot run `{}`: {e}", program.display())),
    }
}

/// `lsc explain <code>` (SPEC §103: diagnostics carry stable codes).
fn explain(code: &str) -> i32 {
    let text = match code.to_uppercase().as_str() {
        "E1001" | "E1002" | "E1003" | "E1004" => {
            "A syntax error. The parser expected a different token here. L statements end at a \
             line break, so a construct split across lines may need its operator at the end of \
             the first line."
        }
        "E1007" => {
            "`call` must be followed by a function call. SPEC §2.3 uses `call` to make invocation \
             visually distinct, as in `call print(\"hi\")`."
        }
        "E1008" => {
            "L has no `from x import y` form. Use `use module.symbol` instead (SPEC §34)."
        }
        "E2001" => "A name is defined twice in the same module. Each name may be defined once.",
        "E2002" => {
            "No such module. A module comes from a `.lsh` file in `src/`, or from the standard \
             library (SPEC §33, §35, §67)."
        }
        "E2004" => {
            "The symbol exists but is private. Declarations are private unless marked `pub` \
             (SPEC §33)."
        }
        "E3001" => {
            "Type mismatch. L does not convert between types implicitly — not even between \
             numeric types — so a conversion such as `call float(x)` must be written out."
        }
        "E3013" => {
            "A `match` must be exhaustive (SPEC §26). Either list every variant of the enum, or \
             add a `_` arm."
        }
        "E3014" => {
            "A function that declares a return type must produce a value on every path. Either \
             `return` a value, or end the block with the value itself (SPEC §17)."
        }
        "E3020" => {
            "Only an optional type may hold `null` (SPEC §30). Write the type as `T?` to allow it."
        }
        "E3025" => {
            "A condition must be `bool`. L never treats a number as a truth value (SPEC §10)."
        }
        "E5002" => {
            "The C compiler rejected the code this compiler generated. That is a bug in `lsc`, \
             not in your program; please report it."
        }
        other => {
            eprintln!("error: `{other}` is not a diagnostic code this compiler knows about");
            eprintln!("note: codes look like `E3001` and appear in brackets after `error`");
            return 1;
        }
    };
    println!("{}: {text}", code.to_uppercase());
    0
}

fn report_bare(diag: &Diagnostic) -> i32 {
    let sm = SourceMap::new();
    let mut emitter = Emitter::default();
    emitter.emit(&sm, diag);
    1
}

fn fail(message: impl Into<String>) -> i32 {
    report_bare(&Diagnostic::bare(Severity::Error, message))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn run_args(args: &[&str]) -> i32 {
        main(args.iter().map(|s| s.to_string()).collect())
    }

    #[test]
    fn help_and_version_succeed() {
        assert_eq!(run_args(&["--help"]), 0);
        assert_eq!(run_args(&["-V"]), 0);
    }

    #[test]
    fn no_arguments_prints_usage() {
        assert_eq!(run_args(&[]), 0);
    }

    #[test]
    fn unknown_options_fail() {
        assert_eq!(run_args(&["--nope"]), 1);
    }

    #[test]
    fn explain_knows_its_own_codes() {
        assert_eq!(explain("E3001"), 0);
        assert_eq!(explain("e3013"), 0);
        assert_eq!(explain("E9999"), 1);
    }

    #[test]
    fn explain_without_a_code_is_an_error() {
        assert_eq!(run_args(&["explain"]), 1);
    }
}
