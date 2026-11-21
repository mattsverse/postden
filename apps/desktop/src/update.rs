//! Where checking for and installing an update has got to, and what each step's result changes.

use jiff::ZonedDateTime;
use postden_update::{Available, Install, Version};

/// The running version: the one CI built it as, such as a nightly's, else the crate's.
pub(crate) fn current() -> Version {
    option_env!("POSTDEN_VERSION")
        .unwrap_or(env!("CARGO_PKG_VERSION"))
        .parse()
        .unwrap_or(Version {
            major: 0,
            minor: 0,
            patch: 0,
            nightly: None,
        })
}

/// What the updater is doing.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Phase {
    /// Nothing yet this session.
    Idle,
    /// Asking GitHub; `manual` when the user pressed Check for updates.
    Checking { manual: bool },
    /// The newest release is the one running.
    UpToDate,
    /// The last check failed.
    CheckFailed(String),
    /// [`Updater::available`] is newer than the running version.
    Available,
    /// Downloading it: the share done, when the size is known.
    Downloading(Option<u8>),
    /// Installed; it runs once the app restarts.
    Ready,
    /// Installing failed; the release is still available.
    InstallFailed(String),
}

#[derive(Debug)]
pub(crate) struct Updater {
    /// How this copy was installed, which decides whether it updates itself.
    pub(crate) install: Install,
    pub(crate) phase: Phase,
    /// The newest release, once a check found one newer than the running version.
    pub(crate) available: Option<Available>,
    /// The announcement, with the changelog, is shown over the window.
    pub(crate) dialog_open: bool,
    /// The settings' update channel list is open.
    pub(crate) channel_open: bool,
    /// When the last check finished, successfully or not.
    pub(crate) checked_at: Option<ZonedDateTime>,
    /// Counts scheduled checks, so only the latest timer starts one.
    pub(crate) timer: u64,
}

impl Default for Updater {
    fn default() -> Self {
        Self {
            install: Install::Unsupported,
            phase: Phase::Idle,
            available: None,
            dialog_open: false,
            channel_open: false,
            checked_at: None,
            timer: 0,
        }
    }
}

impl Updater {
    pub(crate) fn new() -> Self {
        Self {
            install: Install::detect(),
            ..Self::default()
        }
    }

    /// Whether a check can start: not while one runs, nor once an update is downloading or in.
    pub(crate) const fn can_check(&self) -> bool {
        !matches!(
            self.phase,
            Phase::Checking { .. } | Phase::Downloading(_) | Phase::Ready
        )
    }

    /// Whether the update can be downloaded and installed now.
    pub(crate) const fn can_install(&self) -> bool {
        self.install.can_install()
            && self.available.is_some()
            && matches!(self.phase, Phase::Available | Phase::InstallFailed(_))
    }

    /// Takes in a check's result. Returns whether to announce the update: always after a manual
    /// check, else only for a release that isn't `skipped` and wasn't announced already.
    pub(crate) fn checked(
        &mut self,
        result: Result<Option<Available>, String>,
        skipped: Option<Version>,
        now: ZonedDateTime,
    ) -> bool {
        let manual = matches!(self.phase, Phase::Checking { manual: true });
        self.checked_at = Some(now);
        match result {
            Ok(Some(available)) => {
                let version = available.latest.version;
                let known = self
                    .available
                    .as_ref()
                    .is_some_and(|a| a.latest.version == version);
                self.available = Some(available);
                self.phase = Phase::Available;
                manual || (!known && skipped != Some(version))
            }
            Ok(None) => {
                self.available = None;
                self.phase = Phase::UpToDate;
                false
            }
            Err(error) => {
                // A release found earlier can still be installed.
                self.phase = if self.available.is_some() {
                    Phase::Available
                } else {
                    Phase::CheckFailed(error)
                };
                false
            }
        }
    }

    /// What the settings say about updates.
    pub(crate) fn status(&self) -> String {
        let checked = self
            .checked_at
            .as_ref()
            .map(|at| format!(" (checked at {})", at.strftime("%H:%M")))
            .unwrap_or_default();
        let latest = self.available.as_ref().map(|a| a.latest.version);
        match (&self.phase, latest) {
            (Phase::Idle, _) => String::new(),
            (Phase::Checking { .. }, _) => "Checking for updates…".into(),
            (Phase::UpToDate, _) => format!("Postden is up to date{checked}."),
            (Phase::CheckFailed(error), _) => format!("Couldn't check for updates: {error}"),
            (Phase::Downloading(Some(percent)), Some(v)) => {
                format!("Downloading Postden {v}… {percent}%")
            }
            (Phase::Downloading(_), Some(v)) => format!("Downloading Postden {v}…"),
            (Phase::Ready, Some(v)) => {
                format!("Postden {v} is installed. Restart to start using it.")
            }
            (Phase::InstallFailed(error), _) => format!("Couldn't install the update: {error}"),
            (_, Some(v)) => format!("Postden {v} is available{checked}."),
            (_, None) => String::new(),
        }
    }
}

#[cfg(test)]
mod tests {
    use postden_update::Release;

    use super::*;

    fn available(version: &str) -> Available {
        Available {
            current: "0.1.0".parse().unwrap(),
            latest: Release {
                version: version.parse().unwrap(),
                notes: String::new(),
                page: String::new(),
                assets: Vec::new(),
            },
            changes: Vec::new(),
        }
    }

    fn checking(manual: bool) -> Updater {
        Updater {
            phase: Phase::Checking { manual },
            ..Updater::default()
        }
    }

    #[test]
    fn automatic_checks_announce_each_release_once_unless_skipped() {
        let mut u = checking(false);
        assert!(
            u.checked(Ok(Some(available("0.2.0"))), None, ZonedDateTime::now()),
            "a new release"
        );
        u.phase = Phase::Checking { manual: false };
        assert!(
            !u.checked(Ok(Some(available("0.2.0"))), None, ZonedDateTime::now()),
            "already announced"
        );
        u.phase = Phase::Checking { manual: false };
        assert!(
            u.checked(Ok(Some(available("0.3.0"))), None, ZonedDateTime::now()),
            "a newer one"
        );

        let skipped = Some("0.2.0".parse().unwrap());
        let mut u = checking(false);
        assert!(
            !u.checked(Ok(Some(available("0.2.0"))), skipped, ZonedDateTime::now()),
            "skipped"
        );
        assert_eq!(
            u.phase,
            Phase::Available,
            "but still offered in the settings"
        );
        let mut u = checking(true);
        assert!(
            u.checked(Ok(Some(available("0.2.0"))), skipped, ZonedDateTime::now()),
            "asked for"
        );
    }

    #[test]
    fn a_failed_check_keeps_an_update_found_earlier() {
        let mut u = checking(false);
        assert!(
            !u.checked(Err("offline".into()), None, ZonedDateTime::now()),
            "nothing to announce"
        );
        assert_eq!(u.phase, Phase::CheckFailed("offline".into()));

        let mut u = checking(false);
        u.available = Some(available("0.2.0"));
        u.checked(Err("offline".into()), None, ZonedDateTime::now());
        assert_eq!(u.phase, Phase::Available);
    }

    #[test]
    fn checks_wait_for_an_install() {
        let mut u = Updater::default();
        assert!(u.can_check(), "idle");
        for phase in [
            Phase::Checking { manual: true },
            Phase::Downloading(None),
            Phase::Ready,
        ] {
            u.phase = phase;
            assert!(!u.can_check(), "{:?}", u.phase);
        }
    }

    #[test]
    fn only_copies_that_update_themselves_install() {
        let mut u = Updater {
            phase: Phase::Available,
            available: Some(available("0.2.0")),
            ..Updater::default()
        };
        assert!(!u.can_install(), "unsupported install");
        u.install = Install::App {
            bundle: "/Applications/Postden.app".into(),
        };
        assert!(u.can_install(), "app bundle");
        u.phase = Phase::Downloading(None);
        assert!(!u.can_install(), "already downloading");
    }
}
