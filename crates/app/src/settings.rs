//! The app's preferences: its theme, how often mail is checked, how many messages are downloaded
//! ahead of their opening, whether opened messages are kept, whether new mail is announced with a system notification, whether the
//! app keeps running with its window closed and whether updates are checked for, kept in the
//! [`Store`].

use std::time::Duration;

use postden_store::Store;
use postden_update::Version;

use crate::Result;

const THEME: &str = "theme";
const POLL_INTERVAL: &str = "poll_interval_secs";
const PREFETCH: &str = "prefetch";
const KEEP_OPENED: &str = "keep_opened";
const NOTIFY_NEW_MAIL: &str = "notify_new_mail";
const KEEP_RUNNING: &str = "keep_running";
const CHECK_UPDATES: &str = "check_updates";
const SKIPPED_UPDATE: &str = "skipped_update";

/// Whether the window is light, dark, or follows the system.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Theme {
    #[default]
    System,
    Light,
    Dark,
}

impl Theme {
    pub const ALL: [Self; 3] = [Self::System, Self::Light, Self::Dark];

    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            Self::System => "System",
            Self::Light => "Light",
            Self::Dark => "Dark",
        }
    }

    /// Whether to draw dark, given whether the system is.
    #[must_use]
    pub const fn is_dark(self, system_dark: bool) -> bool {
        match self {
            Self::System => system_dark,
            Self::Light => false,
            Self::Dark => true,
        }
    }

    const fn key(self) -> &'static str {
        match self {
            Self::System => "system",
            Self::Light => "light",
            Self::Dark => "dark",
        }
    }

    fn from_key(key: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|t| t.key() == key)
    }
}

/// The intervals offered for checking mail on servers that can't push it.
pub const POLL_INTERVALS: [Duration; 5] = [
    Duration::from_secs(30),
    Duration::from_mins(1),
    Duration::from_mins(2),
    Duration::from_mins(5),
    Duration::from_mins(15),
];

/// How many of each inbox's newest messages can be downloaded ahead of their opening. The first,
/// none, is the default: a message is downloaded only once it's opened.
pub const PREFETCH_COUNTS: [usize; 5] = [0, 25, 50, 100, 200];

/// The app's preferences.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[expect(
    clippy::struct_excessive_bools,
    reason = "each is a switch of its own in the settings"
)]
pub struct Settings {
    pub theme: Theme,
    /// How often to check for new mail when the server doesn't push it (IMAP IDLE).
    pub poll_interval: Duration,
    /// How many of each inbox's newest messages to download ahead, so they open without waiting
    /// for the server. None by default.
    pub prefetch: usize,
    /// Whether a message, once opened, stays downloaded, so it opens again without waiting for
    /// the server. Off by default: only those downloaded ahead are.
    pub keep_opened: bool,
    /// Whether mail arriving unread in an inbox shows a system notification.
    pub notify_new_mail: bool,
    /// Whether closing the window leaves the app running, to bring back from the tray or Dock.
    pub keep_running: bool,
    /// Whether to look for a new release on GitHub at start and once a day.
    pub check_updates: bool,
    /// A release the user chose to skip: automatic checks don't announce it, only newer ones.
    pub skipped_update: Option<Version>,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            theme: Theme::default(),
            poll_interval: Duration::from_mins(1),
            prefetch: 0,
            keep_opened: false,
            notify_new_mail: true,
            keep_running: true,
            check_updates: true,
            skipped_update: None,
        }
    }
}

impl Settings {
    /// The settings saved in `store`, with defaults for those never saved or unreadable, such as
    /// one written by a newer Postden.
    ///
    /// # Errors
    ///
    /// Returns an error when the store can't be read.
    pub fn load(store: &Store) -> Result<Self> {
        let default = Self::default();
        let theme = store
            .setting(THEME)?
            .and_then(|t| Theme::from_key(&t))
            .unwrap_or(default.theme);
        let poll_interval = store
            .setting(POLL_INTERVAL)?
            .and_then(|s| s.parse().ok())
            .filter(|&secs| secs > 0)
            .map_or(default.poll_interval, Duration::from_secs);
        let prefetch = store
            .setting(PREFETCH)?
            .and_then(|s| s.parse().ok())
            .unwrap_or(default.prefetch);
        let keep_opened = store
            .setting(KEEP_OPENED)?
            .and_then(|s| s.parse().ok())
            .unwrap_or(default.keep_opened);
        let notify_new_mail = store
            .setting(NOTIFY_NEW_MAIL)?
            .and_then(|s| s.parse().ok())
            .unwrap_or(default.notify_new_mail);
        let keep_running = store
            .setting(KEEP_RUNNING)?
            .and_then(|s| s.parse().ok())
            .unwrap_or(default.keep_running);
        let check_updates = store
            .setting(CHECK_UPDATES)?
            .and_then(|s| s.parse().ok())
            .unwrap_or(default.check_updates);
        let skipped_update = store.setting(SKIPPED_UPDATE)?.and_then(|s| s.parse().ok());
        Ok(Self {
            theme,
            poll_interval,
            prefetch,
            keep_opened,
            notify_new_mail,
            keep_running,
            check_updates,
            skipped_update,
        })
    }

    /// Saves every setting to `store`, all or nothing.
    ///
    /// # Errors
    ///
    /// Returns an error, having changed nothing, when the store can't be written.
    pub fn save(&self, store: &Store) -> Result<()> {
        let skipped = self
            .skipped_update
            .map(|v| v.to_string())
            .unwrap_or_default();
        store.set_settings(&[
            (THEME, self.theme.key()),
            (POLL_INTERVAL, &self.poll_interval.as_secs().to_string()),
            (PREFETCH, &self.prefetch.to_string()),
            (KEEP_OPENED, &self.keep_opened.to_string()),
            (NOTIFY_NEW_MAIL, &self.notify_new_mail.to_string()),
            (KEEP_RUNNING, &self.keep_running.to_string()),
            (CHECK_UPDATES, &self.check_updates.to_string()),
            (SKIPPED_UPDATE, &skipped),
        ])?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn settings_round_trip_and_default_when_missing() {
        let store = Store::open_in_memory().unwrap();
        assert_eq!(Settings::load(&store).unwrap(), Settings::default());
        let settings = Settings {
            theme: Theme::Dark,
            poll_interval: Duration::from_mins(5),
            prefetch: 50,
            keep_opened: true,
            notify_new_mail: false,
            keep_running: false,
            check_updates: false,
            skipped_update: Some("0.2.0".parse().unwrap()),
        };
        settings.save(&store).unwrap();
        assert_eq!(Settings::load(&store).unwrap(), settings);
    }

    #[test]
    fn unreadable_settings_fall_back_to_defaults() {
        let store = Store::open_in_memory().unwrap();
        store.set_setting(THEME, "sepia").unwrap();
        store.set_setting(POLL_INTERVAL, "0").unwrap();
        store.set_setting(PREFETCH, "lots").unwrap();
        store.set_setting(KEEP_OPENED, "forever").unwrap();
        store.set_setting(NOTIFY_NEW_MAIL, "loudly").unwrap();
        store.set_setting(KEEP_RUNNING, "sometimes").unwrap();
        store.set_setting(CHECK_UPDATES, "maybe").unwrap();
        store.set_setting(SKIPPED_UPDATE, "").unwrap();
        assert_eq!(Settings::load(&store).unwrap(), Settings::default());
    }

    #[test]
    fn system_theme_follows_the_system() {
        assert!(Theme::System.is_dark(true), "system dark");
        assert!(!Theme::System.is_dark(false), "system light");
        assert!(Theme::Dark.is_dark(false), "always dark");
        assert!(!Theme::Light.is_dark(true), "always light");
    }
}
