//! Moving Postden to another computer, from the settings: exporting every account and setting to
//! a file locked with a passphrase, and importing one.

use std::collections::HashSet;

use postden_app::{ImportOptions, MIN_PASSPHRASE, Profile};
use postden_core::secrets::SecretString;
use rune::TextEdit;

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
    /// The picked file's name and bytes, while unlocking.
    pub(crate) file: Option<(String, Vec<u8>)>,
    /// The unlocked profile, while reviewing it.
    pub(crate) profile: Option<Profile>,
    /// Which of the profile's accounts are set up already, by address.
    pub(crate) existing: HashSet<String>,
    /// What to do with accounts set up already, and whether to use the profile's settings.
    pub(crate) options: ImportOptions,
}

impl Transfer {
    /// Starts exporting: asking for a passphrase.
    pub(crate) fn export() -> Self {
        Self {
            step: Step::Export,
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
