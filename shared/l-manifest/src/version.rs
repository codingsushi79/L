//! Semantic versioning and dependency ranges (SPEC §42).

use std::cmp::Ordering;
use std::fmt;

/// A semantic version, `MAJOR.MINOR.PATCH` with optional pre-release and build
/// metadata.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct Version {
    pub major: u64,
    pub minor: u64,
    pub patch: u64,
    /// `1.0.0-beta.1` — ordered before the matching release.
    pub pre: Option<String>,
    /// `1.0.0+build.5` — ignored when comparing.
    pub build: Option<String>,
}

impl Version {
    pub fn new(major: u64, minor: u64, patch: u64) -> Self {
        Version { major, minor, patch, pre: None, build: None }
    }

    pub fn parse(s: &str) -> Result<Version, String> {
        let s = s.trim();
        if s.is_empty() {
            return Err("version is empty".into());
        }

        let (rest, build) = match s.split_once('+') {
            Some((r, b)) => {
                if b.is_empty() {
                    return Err("build metadata is empty".into());
                }
                (r, Some(b.to_string()))
            }
            None => (s, None),
        };
        let (core, pre) = match rest.split_once('-') {
            Some((c, p)) => {
                if p.is_empty() {
                    return Err("pre-release identifier is empty".into());
                }
                (c, Some(p.to_string()))
            }
            None => (rest, None),
        };

        let parts: Vec<&str> = core.split('.').collect();
        if parts.len() != 3 {
            return Err(format!(
                "`{s}` is not a version; expected three parts, as in `1.4.2`"
            ));
        }

        let parse_part = |p: &str, what: &str| -> Result<u64, String> {
            if p.is_empty() {
                return Err(format!("{what} version is missing"));
            }
            if p.len() > 1 && p.starts_with('0') {
                return Err(format!("{what} version `{p}` has a leading zero"));
            }
            p.parse::<u64>().map_err(|_| format!("{what} version `{p}` is not a number"))
        };

        Ok(Version {
            major: parse_part(parts[0], "major")?,
            minor: parse_part(parts[1], "minor")?,
            patch: parse_part(parts[2], "patch")?,
            pre,
            build,
        })
    }

    /// Parse a version that may omit trailing components, as dependency
    /// requirements do: SPEC §91 writes `version = "^2.0"`.
    ///
    /// Omitted components read as zero, so `^2.0` is `^2.0.0` and `^1` is
    /// `^1.0.0`. A released version must still be complete, so this is used
    /// only by [`VersionReq::parse`].
    pub fn parse_partial(s: &str) -> Result<Version, String> {
        let s = s.trim();
        let core_len = s
            .split(['-', '+'])
            .next()
            .unwrap_or(s)
            .split('.')
            .count();
        match core_len {
            0 => Version::parse(s),
            1 | 2 => {
                let padding = ".0".repeat(3 - core_len);
                // Insert the padding before any pre-release or build suffix.
                let idx = s.find(['-', '+']).unwrap_or(s.len());
                let padded = format!("{}{}{}", &s[..idx], padding, &s[idx..]);
                Version::parse(&padded)
            }
            _ => Version::parse(s),
        }
    }

    /// Whether this is a pre-release, which ranges exclude by default.
    pub fn is_prerelease(&self) -> bool {
        self.pre.is_some()
    }

    /// The next version that would be a breaking change from this one.
    ///
    /// Below 1.0.0 the minor position carries breaking changes, matching how
    /// `^0.2.1` is understood across package ecosystems.
    pub fn next_breaking(&self) -> Version {
        if self.major > 0 {
            Version::new(self.major + 1, 0, 0)
        } else if self.minor > 0 {
            Version::new(0, self.minor + 1, 0)
        } else {
            Version::new(0, 0, self.patch + 1)
        }
    }
}

impl fmt::Display for Version {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}.{}.{}", self.major, self.minor, self.patch)?;
        if let Some(pre) = &self.pre {
            write!(f, "-{pre}")?;
        }
        if let Some(build) = &self.build {
            write!(f, "+{build}")?;
        }
        Ok(())
    }
}

impl PartialOrd for Version {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

impl Ord for Version {
    fn cmp(&self, other: &Self) -> Ordering {
        // Build metadata is not part of the ordering.
        self.major
            .cmp(&other.major)
            .then(self.minor.cmp(&other.minor))
            .then(self.patch.cmp(&other.patch))
            .then_with(|| match (&self.pre, &other.pre) {
                // A release outranks any of its pre-releases.
                (None, None) => Ordering::Equal,
                (None, Some(_)) => Ordering::Greater,
                (Some(_), None) => Ordering::Less,
                (Some(a), Some(b)) => compare_prerelease(a, b),
            })
    }
}

/// Compare pre-release identifiers dot-segment by dot-segment: numeric
/// segments compare numerically and rank below alphanumeric ones.
fn compare_prerelease(a: &str, b: &str) -> Ordering {
    let mut a_parts = a.split('.');
    let mut b_parts = b.split('.');
    loop {
        match (a_parts.next(), b_parts.next()) {
            (None, None) => return Ordering::Equal,
            // A shorter set of identifiers ranks lower.
            (None, Some(_)) => return Ordering::Less,
            (Some(_), None) => return Ordering::Greater,
            (Some(x), Some(y)) => {
                let ord = match (x.parse::<u64>(), y.parse::<u64>()) {
                    (Ok(nx), Ok(ny)) => nx.cmp(&ny),
                    (Ok(_), Err(_)) => Ordering::Less,
                    (Err(_), Ok(_)) => Ordering::Greater,
                    (Err(_), Err(_)) => x.cmp(y),
                };
                if ord != Ordering::Equal {
                    return ord;
                }
            }
        }
    }
}

/// A dependency version requirement (SPEC §42).
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum VersionReq {
    /// `*` — any version.
    Any,
    /// `^1.4.0` — compatible releases, up to the next breaking version.
    Caret(Version),
    /// `~1.4.0` — patch releases of the given minor version.
    Tilde(Version),
    /// `=1.4.0` — exactly this version.
    Exact(Version),
    /// `>=1.2.0`, `>1.2.0`, `<2.0.0`, `<=2.0.0`.
    Comparator { op: Op, version: Version },
    /// Several requirements, all of which must hold: `>=1.2, <2.0`.
    All(Vec<VersionReq>),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Op {
    Greater,
    GreaterEq,
    Less,
    LessEq,
}

impl VersionReq {
    /// Parse a requirement string.
    ///
    /// A bare version such as `"1.2.0"` means `^1.2.0`, matching how the
    /// manifest examples in SPEC §41 are written.
    pub fn parse(s: &str) -> Result<VersionReq, String> {
        let s = s.trim();
        if s.is_empty() {
            return Err("version requirement is empty".into());
        }
        if s == "*" {
            return Ok(VersionReq::Any);
        }

        if s.contains(',') {
            let parts: Result<Vec<_>, _> =
                s.split(',').map(|p| VersionReq::parse(p.trim())).collect();
            return Ok(VersionReq::All(parts?));
        }

        let (prefix, rest) = split_operator(s);
        let version = Version::parse_partial(rest)?;

        Ok(match prefix {
            "^" | "" => VersionReq::Caret(version),
            "~" => VersionReq::Tilde(version),
            "=" | "==" => VersionReq::Exact(version),
            ">=" => VersionReq::Comparator { op: Op::GreaterEq, version },
            ">" => VersionReq::Comparator { op: Op::Greater, version },
            "<=" => VersionReq::Comparator { op: Op::LessEq, version },
            "<" => VersionReq::Comparator { op: Op::Less, version },
            other => return Err(format!("unknown version operator `{other}`")),
        })
    }

    /// Whether `version` satisfies this requirement.
    ///
    /// Pre-releases only match when the requirement itself names one, so
    /// `^1.0.0` never silently resolves to `2.0.0-beta.1`.
    pub fn matches(&self, version: &Version) -> bool {
        if version.is_prerelease() && !self.allows_prerelease(version) {
            return false;
        }
        match self {
            VersionReq::Any => true,
            VersionReq::Caret(base) => version >= base && *version < base.next_breaking(),
            VersionReq::Tilde(base) => {
                version >= base
                    && (version.major, version.minor) == (base.major, base.minor)
            }
            VersionReq::Exact(base) => version == base,
            VersionReq::Comparator { op, version: base } => match op {
                Op::Greater => version > base,
                Op::GreaterEq => version >= base,
                Op::Less => version < base,
                Op::LessEq => version <= base,
            },
            VersionReq::All(reqs) => reqs.iter().all(|r| r.matches(version)),
        }
    }

    /// A pre-release is eligible only against a requirement naming the same
    /// major/minor/patch triple.
    fn allows_prerelease(&self, version: &Version) -> bool {
        let same_core = |base: &Version| {
            base.is_prerelease()
                && (base.major, base.minor, base.patch)
                    == (version.major, version.minor, version.patch)
        };
        match self {
            VersionReq::Any => false,
            VersionReq::Caret(b) | VersionReq::Tilde(b) | VersionReq::Exact(b) => same_core(b),
            VersionReq::Comparator { version: b, .. } => same_core(b),
            VersionReq::All(reqs) => reqs.iter().any(|r| r.allows_prerelease(version)),
        }
    }

    /// Pick the highest version from `candidates` that satisfies this
    /// requirement.
    pub fn best_match<'a, I>(&self, candidates: I) -> Option<&'a Version>
    where
        I: IntoIterator<Item = &'a Version>,
    {
        candidates.into_iter().filter(|v| self.matches(v)).max()
    }
}

fn split_operator(s: &str) -> (&str, &str) {
    for op in ["^", "~", ">=", "<=", "==", "=", ">", "<"] {
        if let Some(rest) = s.strip_prefix(op) {
            return (op, rest.trim());
        }
    }
    ("", s)
}

impl fmt::Display for VersionReq {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            VersionReq::Any => f.write_str("*"),
            VersionReq::Caret(v) => write!(f, "^{v}"),
            VersionReq::Tilde(v) => write!(f, "~{v}"),
            VersionReq::Exact(v) => write!(f, "={v}"),
            VersionReq::Comparator { op, version } => {
                let op = match op {
                    Op::Greater => ">",
                    Op::GreaterEq => ">=",
                    Op::Less => "<",
                    Op::LessEq => "<=",
                };
                write!(f, "{op}{version}")
            }
            VersionReq::All(reqs) => {
                let parts: Vec<String> = reqs.iter().map(|r| r.to_string()).collect();
                f.write_str(&parts.join(", "))
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn v(s: &str) -> Version {
        Version::parse(s).expect("valid version")
    }

    fn req(s: &str) -> VersionReq {
        VersionReq::parse(s).expect("valid requirement")
    }

    #[test]
    fn parses_versions() {
        assert_eq!(v("1.0.0"), Version::new(1, 0, 0));
        assert_eq!(v("1.4.2"), Version::new(1, 4, 2));
        let pre = v("1.0.0-beta.1");
        assert_eq!(pre.pre.as_deref(), Some("beta.1"));
        assert_eq!(v("1.0.0+build.5").build.as_deref(), Some("build.5"));
    }

    #[test]
    fn rejects_bad_versions() {
        assert!(Version::parse("1.0").is_err());
        assert!(Version::parse("1.0.0.0").is_err());
        assert!(Version::parse("a.b.c").is_err());
        assert!(Version::parse("01.0.0").is_err());
        assert!(Version::parse("").is_err());
    }

    #[test]
    fn orders_versions() {
        assert!(v("1.0.0") < v("1.0.1"));
        assert!(v("1.0.1") < v("1.1.0"));
        assert!(v("1.1.0") < v("2.0.0"));
        assert!(v("1.0.0-alpha") < v("1.0.0"));
        assert!(v("1.0.0-alpha") < v("1.0.0-beta"));
        assert!(v("1.0.0-alpha.1") < v("1.0.0-alpha.2"));
        assert!(v("1.0.0-alpha.9") < v("1.0.0-alpha.10"));
        // Numeric identifiers rank below alphanumeric ones.
        assert!(v("1.0.0-1") < v("1.0.0-alpha"));
        // Build metadata does not affect ordering.
        assert_eq!(v("1.0.0+a").cmp(&v("1.0.0+b")), Ordering::Equal);
    }

    #[test]
    fn caret_allows_compatible_releases() {
        // SPEC §42: `^1.4.0` allows compatible 1.x releases.
        let r = req("^1.4.0");
        assert!(r.matches(&v("1.4.0")));
        assert!(r.matches(&v("1.4.9")));
        assert!(r.matches(&v("1.9.0")));
        assert!(!r.matches(&v("1.3.9")));
        assert!(!r.matches(&v("2.0.0")));
    }

    #[test]
    fn caret_below_one_is_conservative() {
        let r = req("^0.2.1");
        assert!(r.matches(&v("0.2.9")));
        assert!(!r.matches(&v("0.3.0")));
    }

    #[test]
    fn tilde_allows_patch_releases() {
        // SPEC §42: `~1.4.0` allows compatible 1.4.x releases.
        let r = req("~1.4.0");
        assert!(r.matches(&v("1.4.0")));
        assert!(r.matches(&v("1.4.7")));
        assert!(!r.matches(&v("1.5.0")));
        assert!(!r.matches(&v("1.3.0")));
    }

    #[test]
    fn exact_matches_one_version() {
        let r = req("=1.4.0");
        assert!(r.matches(&v("1.4.0")));
        assert!(!r.matches(&v("1.4.1")));
    }

    #[test]
    fn bare_version_means_caret() {
        // SPEC §41 writes `http = "1.2.0"` under `[dependencies]`.
        assert_eq!(req("1.2.0"), req("^1.2.0"));
        assert!(req("1.2.0").matches(&v("1.5.0")));
    }

    #[test]
    fn comparators_and_conjunctions() {
        assert!(req(">=1.2.0").matches(&v("1.2.0")));
        assert!(!req(">1.2.0").matches(&v("1.2.0")));
        assert!(req("<2.0.0").matches(&v("1.9.9")));

        let r = req(">=1.2.0, <2.0.0");
        assert!(r.matches(&v("1.5.0")));
        assert!(!r.matches(&v("2.0.0")));
        assert!(!r.matches(&v("1.1.0")));
    }

    #[test]
    fn requirements_accept_partial_versions() {
        // SPEC §91: `version = "^2.0"`
        assert!(req("^2.0").matches(&v("2.4.1")));
        assert!(!req("^2.0").matches(&v("3.0.0")));
        assert!(req("^1").matches(&v("1.9.9")));
        assert!(req("~1.4").matches(&v("1.4.7")));
        assert!(!req("~1.4").matches(&v("1.5.0")));
        // A released version must still be complete.
        assert!(Version::parse("2.0").is_err());
    }

    #[test]
    fn any_matches_all_releases() {
        assert!(req("*").matches(&v("0.0.1")));
        assert!(req("*").matches(&v("9.9.9")));
    }

    #[test]
    fn prereleases_are_excluded_unless_requested() {
        // `^1.0.0` must not silently pick up `2.0.0-beta.1`, or `1.1.0-rc.1`.
        assert!(!req("^1.0.0").matches(&v("1.1.0-rc.1")));
        assert!(!req("*").matches(&v("1.0.0-rc.1")));
        // Naming the pre-release explicitly opts in.
        assert!(req("^1.1.0-rc.1").matches(&v("1.1.0-rc.1")));
        assert!(req(">=1.0.0-rc.1").matches(&v("1.0.0-rc.1")));
    }

    #[test]
    fn picks_the_highest_matching_version() {
        let available = vec![v("1.2.0"), v("1.4.2"), v("1.9.0"), v("2.0.0")];
        assert_eq!(req("^1.4.0").best_match(&available), Some(&v("1.9.0")));
        assert_eq!(req("~1.4.0").best_match(&available), Some(&v("1.4.2")));
        assert_eq!(req("^3.0.0").best_match(&available), None);
    }

    #[test]
    fn requirements_round_trip_through_display() {
        for s in ["^1.4.0", "~1.4.0", "=1.4.0", ">=1.2.0", "<2.0.0", "*"] {
            assert_eq!(req(s).to_string(), s, "round trip failed for {s}");
        }
        assert_eq!(req(">=1.2.0, <2.0.0").to_string(), ">=1.2.0, <2.0.0");
    }

    #[test]
    fn next_breaking_follows_semver_zero_rules() {
        assert_eq!(v("1.4.2").next_breaking(), Version::new(2, 0, 0));
        assert_eq!(v("0.4.2").next_breaking(), Version::new(0, 5, 0));
        assert_eq!(v("0.0.2").next_breaking(), Version::new(0, 0, 3));
    }
}
