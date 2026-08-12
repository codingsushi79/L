//! The `lsharp.toml` manifest and `lsharp.lock` lockfile (SPEC §36, §37, §40–42, §64).

pub mod lockfile;
pub mod version;

pub use lockfile::{LockedPackage, Lockfile};
pub use version::{Version, VersionReq};

use l_toml::Value;
use std::collections::BTreeMap;
use std::fmt;
use std::path::{Path, PathBuf};

/// The manifest file name.
pub const MANIFEST_FILE: &str = "lsharp.toml";
/// The lockfile name.
pub const LOCKFILE: &str = "lsharp.lock";
/// The source file extension (SPEC §5).
pub const SOURCE_EXT: &str = "l";

/// A manifest error, with the file it came from.
#[derive(Clone, Debug)]
pub struct Error {
    pub message: String,
    pub path: Option<PathBuf>,
    /// The offending line, when the failure came from the TOML parser.
    pub line: Option<usize>,
}

impl Error {
    fn new(message: impl Into<String>) -> Self {
        Error { message: message.into(), path: None, line: None }
    }

    fn at(mut self, path: impl Into<PathBuf>) -> Self {
        self.path = Some(path.into());
        self
    }
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match (&self.path, self.line) {
            (Some(p), Some(l)) => write!(f, "{}:{}: {}", p.display(), l, self.message),
            (Some(p), None) => write!(f, "{}: {}", p.display(), self.message),
            (None, _) => f.write_str(&self.message),
        }
    }
}

impl std::error::Error for Error {}

pub type Result<T> = std::result::Result<T, Error>;

/// A parsed `lsharp.toml`.
#[derive(Clone, Debug, PartialEq)]
pub struct Manifest {
    /// Absent for a pure workspace root.
    pub package: Option<Package>,
    pub dependencies: BTreeMap<String, Dependency>,
    pub dev_dependencies: BTreeMap<String, Dependency>,
    /// `[features]` (SPEC §66).
    pub features: BTreeMap<String, Vec<String>>,
    /// `[workspace]` (SPEC §64).
    pub workspace: Option<Workspace>,
    /// `[registries]` (SPEC §90).
    pub registries: BTreeMap<String, String>,
    /// `[language] version` (SPEC §101).
    pub language_version: Option<String>,
}

/// The `[package]` table (SPEC §37, §84).
#[derive(Clone, Debug, PartialEq)]
pub struct Package {
    pub name: String,
    pub version: Version,
    pub description: Option<String>,
    pub license: Option<String>,
    pub authors: Vec<String>,
    /// SPEC §51 — used by the registry to link back to GitHub.
    pub repository: Option<String>,
    pub homepage: Option<String>,
    pub documentation: Option<String>,
    pub keywords: Vec<String>,
    /// A library package, created by `lpm new --lib` (SPEC §39).
    pub is_lib: bool,
}

/// The `[workspace]` table (SPEC §64).
#[derive(Clone, Debug, PartialEq, Default)]
pub struct Workspace {
    pub members: Vec<String>,
    pub exclude: Vec<String>,
}

/// Where a dependency comes from (SPEC §41).
#[derive(Clone, Debug, PartialEq)]
pub enum Dependency {
    /// `http = "^1.2.0"`, optionally from a named registry (SPEC §91).
    Registry { req: VersionReq, registry: Option<String>, features: Vec<String> },
    /// `http = { git = "...", branch = "..." }`.
    Git { url: String, reference: GitRef, features: Vec<String> },
    /// `http = { path = "../http" }` (SPEC §65).
    Path { path: PathBuf, features: Vec<String> },
}

impl Dependency {
    pub fn features(&self) -> &[String] {
        match self {
            Dependency::Registry { features, .. }
            | Dependency::Git { features, .. }
            | Dependency::Path { features, .. } => features,
        }
    }

    /// The source label recorded in the lockfile (SPEC §40).
    pub fn source_kind(&self) -> &'static str {
        match self {
            Dependency::Registry { .. } => "registry",
            Dependency::Git { .. } => "git",
            Dependency::Path { .. } => "path",
        }
    }
}

/// Which commit of a git dependency to use (SPEC §41).
#[derive(Clone, Debug, PartialEq, Default)]
pub enum GitRef {
    /// The repository's default branch.
    #[default]
    Default,
    Branch(String),
    Tag(String),
    Rev(String),
}

impl fmt::Display for GitRef {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            GitRef::Default => f.write_str("HEAD"),
            GitRef::Branch(b) => write!(f, "branch={b}"),
            GitRef::Tag(t) => write!(f, "tag={t}"),
            GitRef::Rev(r) => write!(f, "rev={r}"),
        }
    }
}

impl Manifest {
    /// Read and parse a manifest from disk.
    pub fn load(path: impl AsRef<Path>) -> Result<Manifest> {
        let path = path.as_ref();
        let src = std::fs::read_to_string(path).map_err(|e| {
            Error::new(format!("could not read manifest: {e}")).at(path)
        })?;
        Manifest::parse(&src).map_err(|e| Error { path: Some(path.to_path_buf()), ..e })
    }

    /// Parse a manifest from TOML text.
    pub fn parse(src: &str) -> Result<Manifest> {
        let doc = l_toml::parse(src).map_err(|e| Error {
            message: e.message,
            path: None,
            line: Some(e.line),
        })?;

        let package = match doc.get("package") {
            Some(table) => Some(parse_package(table)?),
            None => None,
        };

        let dependencies = parse_dep_table(doc.get("dependencies"))?;
        let dev_dependencies = parse_dep_table(doc.get("dev-dependencies"))?;

        let mut features = BTreeMap::new();
        if let Some(table) = doc.get("features").and_then(|v| v.as_table()) {
            for (name, value) in table {
                let list = value
                    .as_array()
                    .ok_or_else(|| {
                        Error::new(format!("feature `{name}` must be a list of strings"))
                    })?
                    .iter()
                    .map(|v| {
                        v.as_str().map(str::to_string).ok_or_else(|| {
                            Error::new(format!("feature `{name}` must contain only strings"))
                        })
                    })
                    .collect::<Result<Vec<_>>>()?;
                features.insert(name.clone(), list);
            }
        }

        let workspace = match doc.get("workspace") {
            Some(table) => Some(Workspace {
                members: string_list(table.get("members"), "workspace.members")?,
                exclude: string_list(table.get("exclude"), "workspace.exclude")?,
            }),
            None => None,
        };

        let mut registries = BTreeMap::new();
        if let Some(table) = doc.get("registries").and_then(|v| v.as_table()) {
            for (name, value) in table {
                // Either `name = "url"` or `name = { index = "url" }`.
                let url = value
                    .as_str()
                    .or_else(|| value.get_str("index"))
                    .ok_or_else(|| {
                        Error::new(format!("registry `{name}` must be a URL string"))
                    })?;
                registries.insert(name.clone(), url.to_string());
            }
        }

        let language_version = doc.get_str("language.version").map(str::to_string);

        if package.is_none() && workspace.is_none() {
            return Err(Error::new(
                "manifest has neither a `[package]` nor a `[workspace]` table",
            ));
        }

        Ok(Manifest {
            package,
            dependencies,
            dev_dependencies,
            features,
            workspace,
            registries,
            language_version,
        })
    }

    /// The package table, or an error naming what is missing.
    pub fn require_package(&self) -> Result<&Package> {
        self.package.as_ref().ok_or_else(|| {
            Error::new("this manifest describes a workspace and has no `[package]` table")
        })
    }

    /// Dependencies and dev-dependencies together.
    pub fn all_dependencies(&self) -> impl Iterator<Item = (&String, &Dependency)> {
        self.dependencies.iter().chain(self.dev_dependencies.iter())
    }

    pub fn is_workspace_root(&self) -> bool {
        self.workspace.is_some()
    }

    /// Render back to TOML.
    pub fn to_toml(&self) -> String {
        let mut root = BTreeMap::new();

        if let Some(pkg) = &self.package {
            let mut table = BTreeMap::new();
            table.insert("name".into(), Value::String(pkg.name.clone()));
            table.insert("version".into(), Value::String(pkg.version.to_string()));
            if let Some(d) = &pkg.description {
                table.insert("description".into(), Value::String(d.clone()));
            }
            if let Some(l) = &pkg.license {
                table.insert("license".into(), Value::String(l.clone()));
            }
            if !pkg.authors.is_empty() {
                table.insert(
                    "authors".into(),
                    Value::Array(pkg.authors.iter().cloned().map(Value::String).collect()),
                );
            }
            if let Some(r) = &pkg.repository {
                table.insert("repository".into(), Value::String(r.clone()));
            }
            if let Some(h) = &pkg.homepage {
                table.insert("homepage".into(), Value::String(h.clone()));
            }
            if let Some(d) = &pkg.documentation {
                table.insert("documentation".into(), Value::String(d.clone()));
            }
            if !pkg.keywords.is_empty() {
                table.insert(
                    "keywords".into(),
                    Value::Array(pkg.keywords.iter().cloned().map(Value::String).collect()),
                );
            }
            if pkg.is_lib {
                table.insert("lib".into(), Value::Boolean(true));
            }
            root.insert("package".to_string(), Value::Table(table));
        }

        if let Some(v) = &self.language_version {
            let mut table = BTreeMap::new();
            table.insert("version".into(), Value::String(v.clone()));
            root.insert("language".to_string(), Value::Table(table));
        }

        if let Some(ws) = &self.workspace {
            let mut table = BTreeMap::new();
            table.insert(
                "members".into(),
                Value::Array(ws.members.iter().cloned().map(Value::String).collect()),
            );
            if !ws.exclude.is_empty() {
                table.insert(
                    "exclude".into(),
                    Value::Array(ws.exclude.iter().cloned().map(Value::String).collect()),
                );
            }
            root.insert("workspace".to_string(), Value::Table(table));
        }

        if !self.dependencies.is_empty() {
            root.insert("dependencies".to_string(), deps_to_toml(&self.dependencies));
        }
        if !self.dev_dependencies.is_empty() {
            root.insert("dev-dependencies".to_string(), deps_to_toml(&self.dev_dependencies));
        }
        if !self.features.is_empty() {
            let mut table = BTreeMap::new();
            for (name, list) in &self.features {
                table.insert(
                    name.clone(),
                    Value::Array(list.iter().cloned().map(Value::String).collect()),
                );
            }
            root.insert("features".to_string(), Value::Table(table));
        }
        if !self.registries.is_empty() {
            let mut table = BTreeMap::new();
            for (name, url) in &self.registries {
                table.insert(name.clone(), Value::String(url.clone()));
            }
            root.insert("registries".to_string(), Value::Table(table));
        }

        l_toml::to_string(&Value::Table(root))
    }

    /// Write the manifest to disk.
    pub fn save(&self, path: impl AsRef<Path>) -> Result<()> {
        let path = path.as_ref();
        std::fs::write(path, self.to_toml())
            .map_err(|e| Error::new(format!("could not write manifest: {e}")).at(path))
    }
}

fn deps_to_toml(deps: &BTreeMap<String, Dependency>) -> Value {
    let mut table = BTreeMap::new();
    for (name, dep) in deps {
        let value = match dep {
            // The common case stays a plain string.
            Dependency::Registry { req, registry: None, features } if features.is_empty() => {
                Value::String(req.to_string())
            }
            Dependency::Registry { req, registry, features } => {
                let mut t = BTreeMap::new();
                t.insert("version".into(), Value::String(req.to_string()));
                if let Some(r) = registry {
                    t.insert("registry".into(), Value::String(r.clone()));
                }
                insert_features(&mut t, features);
                Value::Table(t)
            }
            Dependency::Git { url, reference, features } => {
                let mut t = BTreeMap::new();
                t.insert("git".into(), Value::String(url.clone()));
                match reference {
                    GitRef::Default => {}
                    GitRef::Branch(b) => {
                        t.insert("branch".into(), Value::String(b.clone()));
                    }
                    GitRef::Tag(tag) => {
                        t.insert("tag".into(), Value::String(tag.clone()));
                    }
                    GitRef::Rev(r) => {
                        t.insert("rev".into(), Value::String(r.clone()));
                    }
                }
                insert_features(&mut t, features);
                Value::Table(t)
            }
            Dependency::Path { path, features } => {
                let mut t = BTreeMap::new();
                t.insert(
                    "path".into(),
                    Value::String(path.to_string_lossy().replace('\\', "/")),
                );
                insert_features(&mut t, features);
                Value::Table(t)
            }
        };
        table.insert(name.clone(), value);
    }
    Value::Table(table)
}

fn insert_features(table: &mut BTreeMap<String, Value>, features: &[String]) {
    if !features.is_empty() {
        table.insert(
            "features".into(),
            Value::Array(features.iter().cloned().map(|f| Value::String(f)).collect()),
        );
    }
}

fn parse_package(table: &Value) -> Result<Package> {
    let name = table
        .get_str("name")
        .ok_or_else(|| Error::new("`package.name` is required"))?
        .to_string();
    validate_package_name(&name)?;

    let version_str = table
        .get_str("version")
        .ok_or_else(|| Error::new("`package.version` is required"))?;
    let version = Version::parse(version_str)
        .map_err(|e| Error::new(format!("`package.version` is invalid: {e}")))?;

    Ok(Package {
        name,
        version,
        description: table.get_str("description").map(str::to_string),
        license: table.get_str("license").map(str::to_string),
        authors: string_list(table.get("authors"), "package.authors")?,
        repository: table.get_str("repository").map(str::to_string),
        homepage: table.get_str("homepage").map(str::to_string),
        documentation: table.get_str("documentation").map(str::to_string),
        keywords: string_list(table.get("keywords"), "package.keywords")?,
        is_lib: table.get("lib").and_then(|v| v.as_bool()).unwrap_or(false),
    })
}

fn string_list(value: Option<&Value>, what: &str) -> Result<Vec<String>> {
    match value {
        None => Ok(Vec::new()),
        Some(v) => v
            .as_array()
            .ok_or_else(|| Error::new(format!("`{what}` must be a list of strings")))?
            .iter()
            .map(|item| {
                item.as_str()
                    .map(str::to_string)
                    .ok_or_else(|| Error::new(format!("`{what}` must contain only strings")))
            })
            .collect(),
    }
}

fn parse_dep_table(value: Option<&Value>) -> Result<BTreeMap<String, Dependency>> {
    let mut out = BTreeMap::new();
    let Some(table) = value.and_then(|v| v.as_table()) else {
        return Ok(out);
    };
    for (name, spec) in table {
        out.insert(name.clone(), parse_dependency(name, spec)?);
    }
    Ok(out)
}

fn parse_dependency(name: &str, spec: &Value) -> Result<Dependency> {
    // `http = "^1.2.0"`
    if let Some(text) = spec.as_str() {
        let req = VersionReq::parse(text).map_err(|e| {
            Error::new(format!("dependency `{name}` has an invalid version: {e}"))
        })?;
        return Ok(Dependency::Registry { req, registry: None, features: Vec::new() });
    }

    let table = spec.as_table().ok_or_else(|| {
        Error::new(format!(
            "dependency `{name}` must be a version string or a table, found {}",
            spec.type_name()
        ))
    })?;

    let features = string_list(table.get("features"), &format!("{name}.features"))?;

    // Exactly one source must be given (SPEC §41).
    let sources: Vec<&str> = ["git", "path", "version"]
        .into_iter()
        .filter(|k| table.contains_key(*k))
        .collect();
    if sources.is_empty() {
        return Err(Error::new(format!(
            "dependency `{name}` must specify `version`, `git` or `path`"
        )));
    }
    if sources.len() > 1 && !(sources.contains(&"version") && sources.len() == 2) {
        return Err(Error::new(format!(
            "dependency `{name}` specifies several sources: {}",
            sources.join(", ")
        )));
    }

    if let Some(url) = spec.get_str("git") {
        let branch = spec.get_str("branch");
        let tag = spec.get_str("tag");
        let rev = spec.get_str("rev");
        let given = [branch.is_some(), tag.is_some(), rev.is_some()]
            .iter()
            .filter(|b| **b)
            .count();
        if given > 1 {
            return Err(Error::new(format!(
                "git dependency `{name}` sets more than one of `branch`, `tag` and `rev`"
            )));
        }
        let reference = match (branch, tag, rev) {
            (Some(b), _, _) => GitRef::Branch(b.to_string()),
            (_, Some(t), _) => GitRef::Tag(t.to_string()),
            (_, _, Some(r)) => GitRef::Rev(r.to_string()),
            _ => GitRef::Default,
        };
        return Ok(Dependency::Git { url: url.to_string(), reference, features });
    }

    if let Some(path) = spec.get_str("path") {
        return Ok(Dependency::Path { path: PathBuf::from(path), features });
    }

    let version = spec
        .get_str("version")
        .ok_or_else(|| Error::new(format!("dependency `{name}` has no `version`")))?;
    let req = VersionReq::parse(version)
        .map_err(|e| Error::new(format!("dependency `{name}` has an invalid version: {e}")))?;
    Ok(Dependency::Registry {
        req,
        registry: spec.get_str("registry").map(str::to_string),
        features,
    })
}

/// Check a package name against the rules of SPEC §45.
///
/// Names are lowercase, contain letters, digits and `-`, and do not begin with
/// a digit. Scoped names such as `@org/pkg` are accepted (SPEC §55).
pub fn validate_package_name(name: &str) -> Result<()> {
    if name.is_empty() {
        return Err(Error::new("package name is empty"));
    }

    // A scoped name: `@scope/name`, both parts following the ordinary rules.
    if let Some(rest) = name.strip_prefix('@') {
        let (scope, base) = rest.split_once('/').ok_or_else(|| {
            Error::new(format!(
                "scoped package name `{name}` must be written `@scope/name`"
            ))
        })?;
        validate_name_part(scope, "scope")?;
        validate_name_part(base, "package name")?;
        return Ok(());
    }

    validate_name_part(name, "package name")
}

fn validate_name_part(part: &str, what: &str) -> Result<()> {
    if part.is_empty() {
        return Err(Error::new(format!("{what} is empty")));
    }
    if part.len() > 64 {
        return Err(Error::new(format!("{what} `{part}` is longer than 64 characters")));
    }
    if part.starts_with(|c: char| c.is_ascii_digit()) {
        return Err(Error::new(format!("{what} `{part}` may not begin with a digit")));
    }
    if part.starts_with('-') || part.ends_with('-') {
        return Err(Error::new(format!("{what} `{part}` may not begin or end with `-`")));
    }
    for c in part.chars() {
        if c.is_ascii_uppercase() {
            return Err(Error::new(format!(
                "{what} `{part}` must be lowercase"
            )));
        }
        if !(c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-' || c == '_') {
            return Err(Error::new(format!(
                "{what} `{part}` contains the invalid character `{c}`"
            )));
        }
    }
    Ok(())
}

/// Walk up from `start` looking for the directory holding an `lsharp.toml`.
pub fn find_manifest_dir(start: impl AsRef<Path>) -> Option<PathBuf> {
    let mut dir = start.as_ref().to_path_buf();
    if dir.is_relative() {
        dir = std::env::current_dir().ok()?.join(dir);
    }
    loop {
        if dir.join(MANIFEST_FILE).is_file() {
            return Some(dir);
        }
        if !dir.pop() {
            return None;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const SPEC_84: &str = r#"
[package]
name = "myapp"
version = "1.0.0"
description = "A web application written in L"
license = "MIT"
authors = ["Sasha"]
repository = "https://github.com/example/myapp"
homepage = "https://example.com"

[dependencies]
http = "^2.0.0"
json = "^2.1.0"
database = "^1.4.0"

[dev-dependencies]
test = "^1.0.0"
"#;

    #[test]
    fn parses_the_spec_84_manifest() {
        let m = Manifest::parse(SPEC_84).expect("valid manifest");
        let pkg = m.require_package().unwrap();
        assert_eq!(pkg.name, "myapp");
        assert_eq!(pkg.version, Version::new(1, 0, 0));
        assert_eq!(pkg.license.as_deref(), Some("MIT"));
        assert_eq!(pkg.authors, vec!["Sasha"]);
        assert_eq!(pkg.repository.as_deref(), Some("https://github.com/example/myapp"));
        assert_eq!(m.dependencies.len(), 3);
        assert_eq!(m.dev_dependencies.len(), 1);
    }

    #[test]
    fn parses_all_dependency_sources() {
        // SPEC §41
        let m = Manifest::parse(
            r#"
[package]
name = "app"
version = "0.1.0"

[dependencies]
registry_dep = "1.2.0"
git_plain = { git = "https://github.com/example/http" }
git_branch = { git = "https://github.com/example/http", branch = "development" }
git_tag = { git = "https://github.com/example/http", tag = "v1.2.0" }
local = { path = "../http" }
scoped = { registry = "company", version = "^2.0" }
"#,
        )
        .expect("valid manifest");

        assert!(matches!(
            &m.dependencies["registry_dep"],
            Dependency::Registry { registry: None, .. }
        ));
        assert!(matches!(
            &m.dependencies["git_plain"],
            Dependency::Git { reference: GitRef::Default, .. }
        ));
        assert!(matches!(
            &m.dependencies["git_branch"],
            Dependency::Git { reference: GitRef::Branch(b), .. } if b == "development"
        ));
        assert!(matches!(
            &m.dependencies["git_tag"],
            Dependency::Git { reference: GitRef::Tag(t), .. } if t == "v1.2.0"
        ));
        assert!(matches!(
            &m.dependencies["local"],
            Dependency::Path { path, .. } if path == Path::new("../http")
        ));
        assert!(matches!(
            &m.dependencies["scoped"],
            Dependency::Registry { registry: Some(r), .. } if r == "company"
        ));
    }

    #[test]
    fn rejects_conflicting_dependency_sources() {
        let err = Manifest::parse(
            "[package]\nname=\"a\"\nversion=\"1.0.0\"\n[dependencies]\nx = { git = \"u\", path = \"p\" }\n",
        )
        .unwrap_err();
        assert!(err.message.contains("several sources"), "{err}");

        let err = Manifest::parse(
            "[package]\nname=\"a\"\nversion=\"1.0.0\"\n[dependencies]\nx = { git = \"u\", branch = \"b\", tag = \"t\" }\n",
        )
        .unwrap_err();
        assert!(err.message.contains("more than one"), "{err}");
    }

    #[test]
    fn parses_workspaces() {
        // SPEC §64
        let m = Manifest::parse(
            "[workspace]\nmembers = [\n  \"packages/server\",\n  \"packages/client\"\n]\n",
        )
        .expect("valid workspace");
        assert!(m.is_workspace_root());
        assert!(m.package.is_none());
        assert_eq!(m.workspace.unwrap().members.len(), 2);
    }

    #[test]
    fn parses_features() {
        // SPEC §66
        let m = Manifest::parse(
            "[package]\nname=\"a\"\nversion=\"1.0.0\"\n[features]\ndefault = [\"json\"]\ntls = [\"openssl\"]\n",
        )
        .unwrap();
        assert_eq!(m.features["default"], vec!["json"]);
        assert_eq!(m.features["tls"], vec!["openssl"]);
    }

    #[test]
    fn parses_registries_and_language_version() {
        // SPEC §90, §101
        let m = Manifest::parse(
            "[package]\nname=\"a\"\nversion=\"1.0.0\"\n[language]\nversion = \"1.0\"\n[registries]\ncompany = \"https://packages.company.com\"\n",
        )
        .unwrap();
        assert_eq!(m.language_version.as_deref(), Some("1.0"));
        assert_eq!(m.registries["company"], "https://packages.company.com");
    }

    #[test]
    fn requires_a_package_or_workspace() {
        let err = Manifest::parse("[dependencies]\nhttp = \"1.0.0\"\n").unwrap_err();
        assert!(err.message.contains("neither"), "{err}");
    }

    #[test]
    fn reports_missing_required_fields() {
        let err = Manifest::parse("[package]\nversion = \"1.0.0\"\n").unwrap_err();
        assert!(err.message.contains("package.name"), "{err}");

        let err = Manifest::parse("[package]\nname = \"a\"\n").unwrap_err();
        assert!(err.message.contains("package.version"), "{err}");
    }

    #[test]
    fn validates_package_names() {
        // SPEC §45
        for good in ["http", "json", "sqlite", "web", "crypto", "my-library", "http2"] {
            assert!(validate_package_name(good).is_ok(), "{good} should be valid");
        }
        for bad in ["", "2fast", "Http", "my package", "-lead", "trail-", "a/b"] {
            assert!(validate_package_name(bad).is_err(), "{bad} should be invalid");
        }
        // Scoped names (SPEC §55).
        assert!(validate_package_name("@lsharp/http").is_ok());
        assert!(validate_package_name("@forge/tools").is_ok());
        assert!(validate_package_name("@bad").is_err());
    }

    #[test]
    fn round_trips_through_toml() {
        let m = Manifest::parse(SPEC_84).unwrap();
        let rendered = m.to_toml();
        let reparsed = Manifest::parse(&rendered).expect("re-parses");
        assert_eq!(m, reparsed, "round trip changed the manifest:\n{rendered}");
    }

    #[test]
    fn round_trips_every_dependency_form() {
        let src = r#"
[package]
name = "app"
version = "0.1.0"

[dependencies]
a = "^1.0.0"
b = { git = "https://example.com/b", tag = "v1" }
c = { path = "../c" }
d = { registry = "company", version = "^2.0.0" }
e = { version = "^1.0.0", features = ["tls"] }
"#;
        let m = Manifest::parse(src).unwrap();
        let reparsed = Manifest::parse(&m.to_toml()).unwrap();
        assert_eq!(m, reparsed, "\n{}", m.to_toml());
    }

    #[test]
    fn reports_the_toml_line_on_syntax_errors() {
        let err = Manifest::parse("[package]\nname = \"a\"\nversion =\n").unwrap_err();
        assert_eq!(err.line, Some(3));
    }
}
