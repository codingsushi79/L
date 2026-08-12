//! The `lsharp.lock` lockfile (SPEC §40).
//!
//! The lockfile records the exact resolved version of every dependency, with a
//! checksum, so that a build is reproducible (SPEC §63) and a published
//! version cannot change underneath a project (SPEC §48).

use crate::{Error, Result, Version};
use l_toml::Value;
use std::collections::BTreeMap;
use std::path::Path;

/// The lockfile format version, so that future changes can be detected.
pub const LOCK_VERSION: i64 = 1;

/// A parsed `lsharp.lock`.
#[derive(Clone, Debug, PartialEq, Default)]
pub struct Lockfile {
    pub version: i64,
    /// Resolved packages, kept sorted by name and version.
    pub packages: Vec<LockedPackage>,
}

/// One resolved dependency.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct LockedPackage {
    pub name: String,
    pub version: Version,
    /// `registry`, `git` or `path` (SPEC §41).
    pub source: String,
    /// A content hash. Absent for path dependencies, which are not immutable.
    pub checksum: Option<String>,
    /// Names of this package's own dependencies, for `lpm tree` (SPEC §98).
    pub dependencies: Vec<String>,
}

impl Lockfile {
    pub fn new() -> Self {
        Lockfile { version: LOCK_VERSION, packages: Vec::new() }
    }

    pub fn load(path: impl AsRef<Path>) -> Result<Lockfile> {
        let path = path.as_ref();
        let src = std::fs::read_to_string(path)
            .map_err(|e| Error { message: format!("could not read lockfile: {e}"), path: Some(path.into()), line: None })?;
        Lockfile::parse(&src).map_err(|e| Error { path: Some(path.to_path_buf()), ..e })
    }

    pub fn parse(src: &str) -> Result<Lockfile> {
        let doc = l_toml::parse(src).map_err(|e| Error {
            message: e.message,
            path: None,
            line: Some(e.line),
        })?;

        let version = doc.get("version").and_then(|v| v.as_integer()).unwrap_or(LOCK_VERSION);
        if version > LOCK_VERSION {
            return Err(Error {
                message: format!(
                    "lockfile version {version} is newer than this toolchain supports \
                     (expected {LOCK_VERSION}); upgrade L or delete `lsharp.lock`"
                ),
                path: None,
                line: None,
            });
        }

        let mut packages = Vec::new();
        if let Some(entries) = doc.get("package").and_then(|v| v.as_array()) {
            for entry in entries {
                let name = entry
                    .get_str("name")
                    .ok_or_else(|| Error {
                        message: "lockfile entry is missing `name`".into(),
                        path: None,
                        line: None,
                    })?
                    .to_string();
                let version_str = entry.get_str("version").ok_or_else(|| Error {
                    message: format!("lockfile entry `{name}` is missing `version`"),
                    path: None,
                    line: None,
                })?;
                let version = Version::parse(version_str).map_err(|e| Error {
                    message: format!("lockfile entry `{name}` has an invalid version: {e}"),
                    path: None,
                    line: None,
                })?;

                let dependencies = entry
                    .get("dependencies")
                    .and_then(|v| v.as_array())
                    .map(|items| {
                        items.iter().filter_map(|i| i.as_str().map(str::to_string)).collect()
                    })
                    .unwrap_or_default();

                packages.push(LockedPackage {
                    name,
                    version,
                    source: entry.get_str("source").unwrap_or("registry").to_string(),
                    checksum: entry.get_str("checksum").map(str::to_string),
                    dependencies,
                });
            }
        }

        packages.sort();
        Ok(Lockfile { version, packages })
    }

    /// Find a locked package by name.
    pub fn get(&self, name: &str) -> Option<&LockedPackage> {
        self.packages.iter().find(|p| p.name == name)
    }

    pub fn contains(&self, name: &str) -> bool {
        self.get(name).is_some()
    }

    /// Add or replace an entry, keeping the list sorted.
    pub fn insert(&mut self, package: LockedPackage) {
        match self.packages.iter().position(|p| p.name == package.name) {
            Some(i) => self.packages[i] = package,
            None => self.packages.push(package),
        }
        self.packages.sort();
    }

    pub fn remove(&mut self, name: &str) -> bool {
        let before = self.packages.len();
        self.packages.retain(|p| p.name != name);
        self.packages.len() != before
    }

    pub fn is_empty(&self) -> bool {
        self.packages.is_empty()
    }

    /// Render to TOML.
    ///
    /// Entries are sorted and formatting is fixed, so re-resolving an
    /// unchanged dependency graph produces a byte-identical file.
    pub fn to_toml(&self) -> String {
        let mut out = String::from(
            "# This file is generated by lpm.\n# It is not intended for manual editing.\n",
        );
        out.push_str(&format!("version = {}\n", self.version));

        let mut sorted = self.packages.clone();
        sorted.sort();

        for pkg in &sorted {
            out.push_str("\n[[package]]\n");
            let mut table = BTreeMap::new();
            table.insert("name".to_string(), Value::String(pkg.name.clone()));
            table.insert("version".to_string(), Value::String(pkg.version.to_string()));
            table.insert("source".to_string(), Value::String(pkg.source.clone()));
            if let Some(sum) = &pkg.checksum {
                table.insert("checksum".to_string(), Value::String(sum.clone()));
            }
            if !pkg.dependencies.is_empty() {
                let mut deps = pkg.dependencies.clone();
                deps.sort();
                table.insert(
                    "dependencies".to_string(),
                    Value::Array(deps.into_iter().map(Value::String).collect()),
                );
            }
            // Write in a fixed, readable order rather than alphabetically.
            for key in ["name", "version", "source", "checksum", "dependencies"] {
                if let Some(value) = table.get(key) {
                    out.push_str(&format!("{key} = {}\n", render_value(value)));
                }
            }
        }
        out
    }

    pub fn save(&self, path: impl AsRef<Path>) -> Result<()> {
        let path = path.as_ref();
        std::fs::write(path, self.to_toml()).map_err(|e| Error {
            message: format!("could not write lockfile: {e}"),
            path: Some(path.to_path_buf()),
            line: None,
        })
    }
}

fn render_value(value: &Value) -> String {
    match value {
        Value::String(s) => format!("{s:?}"),
        Value::Array(items) => {
            let parts: Vec<String> = items.iter().map(render_value).collect();
            format!("[{}]", parts.join(", "))
        }
        Value::Integer(i) => i.to_string(),
        Value::Boolean(b) => b.to_string(),
        Value::Float(f) => f.to_string(),
        Value::Table(_) => String::new(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const SPEC_40: &str = r#"
[[package]]
name = "http"
version = "1.2.3"
source = "registry"
checksum = "abc123"

[[package]]
name = "json"
version = "2.1.0"
source = "registry"
checksum = "def456"
"#;

    #[test]
    fn parses_the_spec_40_lockfile() {
        let lock = Lockfile::parse(SPEC_40).expect("valid lockfile");
        assert_eq!(lock.packages.len(), 2);
        assert_eq!(lock.get("http").unwrap().version, Version::new(1, 2, 3));
        assert_eq!(lock.get("json").unwrap().checksum.as_deref(), Some("def456"));
        assert!(!lock.contains("missing"));
    }

    #[test]
    fn round_trips() {
        let lock = Lockfile::parse(SPEC_40).unwrap();
        let reparsed = Lockfile::parse(&lock.to_toml()).unwrap();
        assert_eq!(lock, reparsed, "\n{}", lock.to_toml());
    }

    #[test]
    fn output_is_byte_stable_regardless_of_insertion_order() {
        let mut a = Lockfile::new();
        let mut b = Lockfile::new();

        let http = LockedPackage {
            name: "http".into(),
            version: Version::new(1, 2, 3),
            source: "registry".into(),
            checksum: Some("abc".into()),
            dependencies: vec!["tls".into(), "json".into()],
        };
        let json = LockedPackage {
            name: "json".into(),
            version: Version::new(2, 1, 0),
            source: "registry".into(),
            checksum: Some("def".into()),
            dependencies: vec![],
        };

        a.insert(http.clone());
        a.insert(json.clone());
        b.insert(json);
        b.insert(http);

        assert_eq!(a.to_toml(), b.to_toml());
        // Dependency lists are sorted too.
        assert!(a.to_toml().contains(r#"dependencies = ["json", "tls"]"#), "{}", a.to_toml());
    }

    #[test]
    fn insert_replaces_an_existing_entry() {
        let mut lock = Lockfile::new();
        lock.insert(LockedPackage {
            name: "http".into(),
            version: Version::new(1, 0, 0),
            source: "registry".into(),
            checksum: None,
            dependencies: vec![],
        });
        lock.insert(LockedPackage {
            name: "http".into(),
            version: Version::new(2, 0, 0),
            source: "registry".into(),
            checksum: None,
            dependencies: vec![],
        });
        assert_eq!(lock.packages.len(), 1);
        assert_eq!(lock.get("http").unwrap().version, Version::new(2, 0, 0));

        assert!(lock.remove("http"));
        assert!(lock.is_empty());
    }

    #[test]
    fn rejects_a_newer_lockfile_version() {
        let err = Lockfile::parse("version = 99\n").unwrap_err();
        assert!(err.message.contains("newer than this toolchain"), "{err}");
    }

    #[test]
    fn an_empty_lockfile_is_valid() {
        let lock = Lockfile::parse("version = 1\n").unwrap();
        assert!(lock.is_empty());
    }
}
