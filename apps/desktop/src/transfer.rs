//! Moving Postden to another computer, from the settings: exporting every account and setting to
//! a file locked with a passphrase, or sending that file straight to the other computer, and
//! importing one.

use std::collections::HashSet;

use postden_app::{ImportOptions, MIN_PASSPHRASE, Profile};
use postden_core::secrets::SecretString;
use rune::TextEdit;
use tokio::task::{AbortHandle, Id};

/// The extension of a profile file.
pub(crate) const EXTENSION: &str = "postden";

/// Where moving Postden is at.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub(crate) enum Step {
    /// Neither exporting nor importing: the section shows its two buttons.
    #[default]
    Idle,
    /// Typing the passphrase to lock an export with, twice.
    Export,
    /// Typing the passphrase to lock what's sent with, twice, then going online to send it.
    Send,
    /// Online: showing the code the other computer types, until it took the profile.
    Offering,
    /// Typing the code the sending computer shows, then downloading what it sends.
    Receive,
    /// A profile file was picked: typing the passphrase it was locked with.
    Unlock,
    /// The profile is unlocked: what it holds, and what to do with what's set up already.
    Review,
}

/// Exporting or importing a profile, in the settings.
#[derive(Default)]
pub(crate) struct Transfer {
    pub(crate) step: Step,
    pub(crate) passphrase: TextEdit,
    /// The passphrase again, when exporting, to catch a typo that would lock the file for good.
    pub(crate) confirm: TextEdit,
    /// Encrypting, decrypting or writing is under way.
    pub(crate) busy: bool,
    pub(crate) error: Option<String>,
    /// The code typed, while receiving.
    pub(crate) code: TextEdit,
    /// Sending to or receiving from another computer, under way.
    pub(crate) online: Option<Online>,
    /// The picked or received file's name and bytes, while unlocking.
    pub(crate) file: Option<(String, Vec<u8>)>,
    /// The unlocked profile, while reviewing it.
    pub(crate) profile: Option<Profile>,
    /// Which of the profile's accounts are set up already, by address.
    pub(crate) existing: HashSet<String>,
    /// What to do with accounts set up already, and whether to use the profile's settings.
    pub(crate) options: ImportOptions,
}

/// Sending a profile to another computer, or receiving one, in the background.
#[derive(Debug)]
pub(crate) struct Online {
    /// Locking the profile, going online and waiting for the other computer to take it; or
    /// dialing the sending computer and downloading. Stopped when this is dropped: when the
    /// transfer is cancelled, done, or replaced.
    task: AbortHandle,
    /// The code the other computer types, once the sender is online.
    pub(crate) code: Option<String>,
}

impl Online {
    pub(crate) const fn new(task: AbortHandle) -> Self {
        Self { task, code: None }
    }
}

impl Drop for Online {
    fn drop(&mut self) {
        self.task.abort();
    }
}

impl Transfer {
    /// Starts exporting: asking for a passphrase.
    pub(crate) fn export() -> Self {
        Self::at(Step::Export)
    }

    /// Starts sending to another computer: asking for a passphrase.
    pub(crate) fn send() -> Self {
        Self::at(Step::Send)
    }

    /// Starts receiving from another computer: asking for the code it shows.
    pub(crate) fn receive() -> Self {
        Self::at(Step::Receive)
    }

    fn at(step: Step) -> Self {
        Self {
            step,
            ..Self::default()
        }
    }

    /// Starts importing the file `name`: asking for its passphrase.
    pub(crate) fn unlock(name: String, file: Vec<u8>) -> Self {
        Self {
            step: Step::Unlock,
            file: Some((name, file)),
            ..Self::default()
        }
    }

    /// Shows what an unlocked profile holds. Its settings are used, and accounts set up already
    /// kept, unless changed.
    pub(crate) fn review(profile: Profile, existing: HashSet<String>) -> Self {
        Self {
            step: Step::Review,
            profile: Some(profile),
            existing,
            options: ImportOptions {
                replace_accounts: false,
                settings: true,
            },
            ..Self::default()
        }
    }

    /// The passphrase to lock an export with, once it's long enough and typed the same twice.
    pub(crate) fn new_passphrase(&self) -> Result<SecretString, String> {
        let typed = &self.passphrase.text;
        if typed.chars().count() < MIN_PASSPHRASE {
            return Err(format!(
                "the passphrase needs at least {MIN_PASSPHRASE} characters"
            ));
        }
        if *typed != self.confirm.text {
            return Err("the passphrases don't match".into());
        }
        Ok(SecretString::from(typed.clone()))
    }

    /// Whether `task` is the one sending or receiving now: an update from a transfer cancelled
    /// or replaced since, posted before it was stopped, is for no one.
    pub(crate) fn is_online(&self, task: Id) -> bool {
        self.online
            .as_ref()
            .is_some_and(|online| online.task.id() == task)
    }

    /// Whether the step under way can be cancelled: anything but writing or decrypting a file.
    pub(crate) const fn can_cancel(&self) -> bool {
        !self.busy || self.online.is_some()
    }

    /// The passphrase typed to unlock a profile.
    pub(crate) fn typed_passphrase(&self) -> Option<SecretString> {
        let typed = &self.passphrase.text;
        (!typed.is_empty()).then(|| SecretString::from(typed.clone()))
    }
}

#[cfg(test)]
mod tests {
    use rune::TextEdit;

    use super::*;

    fn typed(passphrase: &str, confirm: &str) -> Transfer {
        Transfer {
            passphrase: TextEdit::new(passphrase),
            confirm: TextEdit::new(confirm),
            ..Transfer::export()
        }
    }

    #[tokio::test]
    async fn only_the_transfer_under_way_takes_updates() {
        let now = tokio::spawn(std::future::pending::<()>());
        let cancelled = tokio::spawn(async {});
        let t = Transfer {
            online: Some(Online::new(now.abort_handle())),
            ..Transfer::send()
        };
        assert!(t.is_online(now.id()), "the task under way");
        assert!(!t.is_online(cancelled.id()), "a task cancelled before it");
        assert!(!Transfer::send().is_online(now.id()), "nothing under way");
        drop(t);
        assert!(
            now.await.unwrap_err().is_cancelled(),
            "stopped once dropped"
        );
    }

    #[test]
    fn an_export_passphrase_is_long_enough_and_typed_twice() {
        assert!(
            typed("correct horse", "correct horse")
                .new_passphrase()
                .is_ok()
        );
        let short = typed("short", "short").new_passphrase().unwrap_err();
        assert!(short.contains("at least"), "got {short}");
        let typo = typed("correct horse", "correct hose")
            .new_passphrase()
            .unwrap_err();
        assert!(typo.contains("don't match"), "got {typo}");
    }
}
