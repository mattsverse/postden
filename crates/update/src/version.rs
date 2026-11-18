//! Release versions, as Postden tags them: `v` and a stable `major.minor.patch`, or a nightly
//! build of the next one, `major.minor.patch-nightly.N`.

use std::cmp::Ordering;
use std::fmt;
use std::str::FromStr;

/// What a nightly build's version adds to the release it leads up to.
const NIGHTLY: &str = "-nightly.";

/// A `major.minor.patch` version, or a nightly build leading up to it, ordered as semver orders
/// them: nightlies by their number, and all of them before the release itself.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Version {
    pub major: u64,
    pub minor: u64,
    pub patch: u64,
    /// The nightly build's number, its build time as `YYYYMMDDHHMM`. `None` for a release.
    pub nightly: Option<u64>,
}

impl Version {
    /// The version of a release tag such as `v0.2.0` or `v0.2.1-nightly.202611180300`. `None`
    /// for anything else, such as another pre-release (`v0.2.0-rc.1`).
    #[must_use]
    pub fn from_tag(tag: &str) -> Option<Self> {
        tag.strip_prefix('v')?.parse().ok()
    }

    /// Whether this is a nightly build rather than a release.
    #[must_use]
    pub const fn is_nightly(&self) -> bool {
        self.nightly.is_some()
    }
}

impl Ord for Version {
    fn cmp(&self, other: &Self) -> Ordering {
        (self.major, self.minor, self.patch)
            .cmp(&(other.major, other.minor, other.patch))
            .then_with(|| match (self.nightly, other.nightly) {
                (None, None) => Ordering::Equal,
                (None, Some(_)) => Ordering::Greater,
                (Some(_), None) => Ordering::Less,
                (Some(a), Some(b)) => a.cmp(&b),
            })
    }
}

impl PartialOrd for Version {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

impl FromStr for Version {
    type Err = InvalidVersion;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        // No sign, no leading zeros, as release.py also insists.
        let number = |part: &str| {
            let canonical = !part.is_empty()
                && part.bytes().all(|b| b.is_ascii_digit())
                && (part == "0" || !part.starts_with('0'));
            canonical.then(|| part.parse::<u64>().ok()).flatten()
        };
        let invalid = || InvalidVersion(s.to_owned());
        let (release, nightly) = match s.split_once(NIGHTLY) {
            Some((release, n)) => (release, Some(number(n).ok_or_else(invalid)?)),
            None => (s, None),
        };
        let mut parts = release.split('.').map(number);
        match (parts.next(), parts.next(), parts.next(), parts.next()) {
            (Some(Some(major)), Some(Some(minor)), Some(Some(patch)), None) => Ok(Self {
                major,
                minor,
                patch,
                nightly,
            }),
            _ => Err(invalid()),
        }
    }
}

impl fmt::Display for Version {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}.{}.{}", self.major, self.minor, self.patch)?;
        self.nightly.map_or(Ok(()), |n| write!(f, "{NIGHTLY}{n}"))
    }
}

/// A version that isn't a `major.minor.patch`, or a nightly build of one.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("{0:?} isn't a major.minor.patch version")]
pub struct InvalidVersion(String);

#[cfg(test)]
mod tests {
    use super::*;

    fn v(s: &str) -> Version {
        s.parse().unwrap()
    }

    #[test]
    fn versions_parse_and_print() {
        assert_eq!(
            v("1.22.333"),
            Version {
                major: 1,
                minor: 22,
                patch: 333,
                nightly: None,
            }
        );
        assert_eq!(v("0.10.0").to_string(), "0.10.0");
        assert_eq!(
            v("0.10.1-nightly.202611180300").nightly,
            Some(202_611_180_300)
        );
        assert_eq!(
            v("0.10.1-nightly.202611180300").to_string(),
            "0.10.1-nightly.202611180300"
        );
        for bad in [
            "",
            "1",
            "1.2",
            "1.2.3.4",
            "1.02.3",
            "1.2.x",
            "+1.2.3",
            "1.2.3-rc.1",
            "1.2.3-nightly.",
            "1.2.3-nightly.01",
            "1.2.3-nightly.1.2",
            "1.2-nightly.1",
        ] {
            assert!(bad.parse::<Version>().is_err(), "{bad:?} should be refused");
        }
    }

    #[test]
    fn versions_order_numerically() {
        assert!(v("0.10.0") > v("0.9.9"), "minor compares as a number");
        assert!(v("1.0.0") > v("0.99.99"), "major wins");
        assert!(v("0.1.1") > v("0.1.0"), "patch breaks ties");
        assert!(
            v("0.1.1-nightly.2") > v("0.1.0"),
            "a nightly leads up to the next release"
        );
        assert!(v("0.1.1-nightly.2") < v("0.1.1"), "and comes before it");
        assert!(
            v("0.1.1-nightly.10") > v("0.1.1-nightly.9"),
            "by its number"
        );
    }

    #[test]
    fn tags_need_the_v_and_a_stable_version() {
        assert_eq!(Version::from_tag("v0.2.0"), Some(v("0.2.0")));
        assert_eq!(Version::from_tag("0.2.0"), None);
        assert_eq!(Version::from_tag("v0.2.0-rc.1"), None);
        assert_eq!(
            Version::from_tag("v0.2.1-nightly.7"),
            Some(v("0.2.1-nightly.7"))
        );
    }
}
