//! Postden's releases on GitHub, and which of them are newer than the running version.

use serde::Deserialize;

use crate::Version;

/// A file attached to a release.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct Asset {
    pub name: String,
    #[serde(rename = "browser_download_url")]
    pub url: String,
}

/// A release as GitHub's REST API lists it, with only what updating needs.
#[derive(Debug, Clone, Deserialize)]
pub(crate) struct Listed {
    tag_name: String,
    #[serde(default)]
    body: Option<String>,
    html_url: String,
    #[serde(default)]
    draft: bool,
    #[serde(default)]
    prerelease: bool,
    #[serde(default)]
    assets: Vec<Asset>,
}

/// A published release.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Release {
    pub version: Version,
    /// Its changelog, in Markdown, as release-please wrote it.
    pub notes: String,
    /// Its page on GitHub, where every download is.
    pub page: String,
    pub assets: Vec<Asset>,
}

impl Release {
    /// The attached file called `name`.
    #[must_use]
    pub fn asset(&self, name: &str) -> Option<&Asset> {
        self.assets.iter().find(|a| a.name == name)
    }
}

/// Which releases to update to.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Track {
    /// Releases only.
    #[default]
    Stable,
    /// Releases, and the nightly builds leading up to the next one.
    Nightly,
}

impl Track {
    /// How the track is saved in the settings.
    #[must_use]
    pub const fn key(self) -> &'static str {
        match self {
            Self::Stable => "stable",
            Self::Nightly => "nightly",
        }
    }

    /// The track saved as `key`, if it's one.
    #[must_use]
    pub fn from_key(key: &str) -> Option<Self> {
        [Self::Stable, Self::Nightly]
            .into_iter()
            .find(|t| t.key() == key)
    }
}

/// A release newer than the running version, and what changed since that version.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Available {
    /// The version running now.
    pub current: Version,
    /// The newest release, the one to update to.
    pub latest: Release,
    /// Every release since `current`, newest first: its version and changelog.
    pub changes: Vec<(Version, String)>,
}

impl Available {
    /// Every changelog since the running version, newest first, as one Markdown document.
    #[must_use]
    pub fn changelog(&self) -> String {
        self.changes
            .iter()
            .map(|(version, notes)| {
                let notes = notes.trim();
                // release-please's notes open with their own version heading.
                if notes.is_empty() {
                    format!("## {version}\n\nNo changes were noted.")
                } else if notes.starts_with('#') {
                    notes.to_owned()
                } else {
                    format!("## {version}\n\n{notes}")
                }
            })
            .collect::<Vec<_>>()
            .join("\n\n")
    }
}

/// What, among `listed`, is newer than `current` on `track`: the newest release (or, on the
/// nightly track, nightly build), and the changelogs of every one since `current`. `None` when
/// nothing is newer.
pub(crate) fn newer(listed: Vec<Listed>, current: Version, track: Track) -> Option<Available> {
    let mut releases: Vec<Release> = listed
        .into_iter()
        .filter(|r| !r.draft)
        .filter_map(|r| {
            let version = Version::from_tag(&r.tag_name)?;
            // Nightlies are published as pre-releases; any other pre-release is skipped.
            let wanted = if r.prerelease {
                track == Track::Nightly && version.is_nightly()
            } else {
                !version.is_nightly()
            };
            wanted.then(|| Release {
                version,
                notes: r.body.unwrap_or_default(),
                page: r.html_url,
                assets: r.assets,
            })
        })
        .filter(|r| r.version > current)
        .collect();
    releases.sort_by_key(|r| std::cmp::Reverse(r.version));
    releases.dedup_by_key(|r| r.version);
    let changes = releases
        .iter()
        .map(|r| (r.version, r.notes.clone()))
        .collect();
    let latest = releases.into_iter().next()?;
    Some(Available {
        current,
        latest,
        changes,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn listed(tag: &str, body: &str) -> Listed {
        Listed {
            tag_name: tag.to_owned(),
            body: Some(body.to_owned()),
            html_url: format!("https://github.com/mattsverse/postden/releases/tag/{tag}"),
            draft: false,
            prerelease: false,
            assets: Vec::new(),
        }
    }

    fn v(s: &str) -> Version {
        s.parse().unwrap()
    }

    #[test]
    fn nothing_newer_is_no_update() {
        let releases = vec![listed("v0.1.0", ""), listed("v0.0.9", "")];
        assert_eq!(newer(releases, v("0.1.0"), Track::Stable), None);
    }

    #[test]
    fn the_newest_release_wins_and_every_skipped_changelog_is_kept() {
        let releases = vec![
            listed(
                "v0.2.0",
                "## [0.2.0](link) (2026-11-20)\n\n### Features\n\n* a",
            ),
            listed(
                "v0.3.0",
                "## [0.3.0](link) (2026-11-27)\n\n### Bug Fixes\n\n* b",
            ),
            listed("v0.1.0", "## 0.1.0\n\n* old"),
            Listed {
                prerelease: true,
                ..listed("v0.4.0", "pre")
            },
            Listed {
                draft: true,
                ..listed("v0.5.0", "draft")
            },
            listed("nightly", "not a version"),
        ];
        let available = newer(releases, v("0.1.0"), Track::Stable).unwrap();
        assert_eq!(available.latest.version, v("0.3.0"));
        assert_eq!(
            available.changes.iter().map(|c| c.0).collect::<Vec<_>>(),
            [v("0.3.0"), v("0.2.0")],
            "newest first, without the running version, drafts or pre-releases"
        );
        assert_eq!(
            available.changelog(),
            "## [0.3.0](link) (2026-11-27)\n\n### Bug Fixes\n\n* b\n\n\
             ## [0.2.0](link) (2026-11-20)\n\n### Features\n\n* a"
        );
    }

    #[test]
    fn nightlies_are_offered_only_on_the_nightly_track() {
        let nightly = |tag| Listed {
            prerelease: true,
            ..listed(tag, "")
        };
        let releases = || {
            vec![
                listed("v0.2.0", ""),
                nightly("v0.2.1-nightly.202611180300"),
                nightly("v0.3.0-rc.1"),
                // Published as a release by mistake: not one.
                listed("v0.2.1-nightly.202611190300", ""),
            ]
        };
        let latest =
            |current, track| newer(releases(), v(current), track).map(|a| a.latest.version);
        assert_eq!(latest("0.1.0", Track::Stable), Some(v("0.2.0")));
        assert_eq!(latest("0.2.0", Track::Stable), None);
        assert_eq!(
            latest("0.2.0", Track::Nightly),
            Some(v("0.2.1-nightly.202611180300"))
        );
        assert_eq!(
            latest("0.2.1-nightly.202611170300", Track::Nightly),
            Some(v("0.2.1-nightly.202611180300"))
        );
        assert_eq!(
            latest("0.2.1-nightly.202611180300", Track::Nightly),
            None,
            "the newest nightly is running"
        );
        assert_eq!(
            latest("0.2.1-nightly.202611180300", Track::Stable),
            None,
            "back on stable, a nightly waits for the next release rather than going back"
        );
    }

    #[test]
    fn tracks_round_trip_through_their_keys() {
        for track in [Track::Stable, Track::Nightly] {
            assert_eq!(Track::from_key(track.key()), Some(track));
        }
        assert_eq!(Track::from_key("beta"), None);
    }

    #[test]
    fn changelogs_without_a_heading_get_one() {
        let available = newer(
            vec![listed("v0.3.0", "  \n"), listed("v0.2.0", "* a fix")],
            v("0.1.0"),
            Track::Stable,
        )
        .unwrap();
        assert_eq!(
            available.changelog(),
            "## 0.3.0\n\nNo changes were noted.\n\n## 0.2.0\n\n* a fix"
        );
    }

    #[test]
    fn github_listings_parse() {
        let json = r#"[{
            "tag_name": "v0.2.0",
            "name": "Postden 0.2.0",
            "body": "notes",
            "html_url": "https://github.com/mattsverse/postden/releases/tag/v0.2.0",
            "draft": false,
            "prerelease": false,
            "assets": [{"name": "SHA256SUMS", "browser_download_url": "https://example.com/SHA256SUMS", "size": 1}]
        }]"#;
        let listed: Vec<Listed> = serde_json::from_str(json).unwrap();
        let available = newer(listed, v("0.1.0"), Track::Stable).unwrap();
        assert_eq!(
            available.latest.asset("SHA256SUMS").map(|a| a.url.as_str()),
            Some("https://example.com/SHA256SUMS")
        );
    }
}
