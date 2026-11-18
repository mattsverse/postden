//! Setting up the accounts another mail app on this computer has, so their servers aren't typed
//! in again.
//!
//! Each app Postden reads is a [`Source`], and [`installed`] lists those on this computer. What
//! a source finds is a [`Found`] account: its address, names and servers. Other apps don't share
//! what they log in with, so a found account is set up with its password typed in, or by signing
//! in with Google again; one Postden can't log in to says why, in [`Unsupported`].
//!
//! Adding an app is adding a [`Source`] for it, and listing it in [`installed`].

mod spark;

use std::collections::HashSet;
use std::fmt;
use std::path::Path;

use postden_core::account::Server;

pub use spark::Spark;

use crate::Result;

/// A mail app whose accounts Postden can read.
pub trait Source: fmt::Debug + Send + Sync {
    /// The app's name, as its users know it, such as "Spark".
    fn app(&self) -> &'static str;

    /// The app's accounts, in the order it shows them, each address once. Its other kinds of
    /// accounts, such as calendars or a subscription, aren't listed.
    ///
    /// # Errors
    ///
    /// Returns an error when the app's accounts can't be read at all. One account that can't
    /// be read is listed as [`Unsupported::Unreadable`] instead.
    fn accounts(&self) -> Result<Vec<Found>>;
}

/// The mail apps Postden can read that are set up for the user whose home directory is `home`.
#[must_use]
pub fn installed(home: &Path) -> Vec<Box<dyn Source>> {
    let mut sources: Vec<Box<dyn Source>> = Vec::new();
    if let Some(spark) = Spark::find(home) {
        sources.push(Box::new(spark));
    }
    sources
}

/// An account another mail app has.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Found {
    /// Its email address, which becomes the account's
    /// [`Account::id`](postden_core::account::Account::id).
    pub address: String,
    /// The name it sends mail under, if set.
    pub display_name: Option<String>,
    /// What the app calls it, if that's not just its address.
    pub label: Option<String>,
    pub setup: Setup,
}

/// How a [`Found`] account is set up in Postden.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Setup {
    /// With a password, typed in, for its IMAP and SMTP servers.
    Password { imap: Server, smtp: Server },
    /// By signing in with Google, as it's a Gmail account.
    Google,
    /// It can't be: Postden can't log in to it.
    Unsupported(Unsupported),
}

/// Why Postden can't set up a [`Found`] account.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[non_exhaustive]
pub enum Unsupported {
    #[error("Microsoft accounts need signing in with Microsoft, which Postden can't do yet")]
    Microsoft,
    #[error("its servers couldn't be read: {0}")]
    Unreadable(String),
}

/// `found` with each address once, the first time it's listed. Addresses are compared ignoring
/// case.
fn once_each(found: Vec<Found>) -> Vec<Found> {
    let mut seen = HashSet::new();
    found
        .into_iter()
        .filter(|f| seen.insert(f.address.to_lowercase()))
        .collect()
}

/// `s` trimmed, or `None` when that leaves nothing.
fn non_empty(s: Option<String>) -> Option<String> {
    s.map(|s| s.trim().to_owned()).filter(|s| !s.is_empty())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn found(address: &str, setup: Setup) -> Found {
        Found {
            address: address.into(),
            display_name: Some("Ada".into()),
            label: Some("Work".into()),
            setup,
        }
    }

    #[test]
    fn an_address_is_listed_once_whatever_its_case() {
        let listed = once_each(vec![
            found("ada@example.com", Setup::Google),
            found(
                "Ada@Example.com",
                Setup::Unsupported(Unsupported::Microsoft),
            ),
            found("bob@example.com", Setup::Google),
        ]);
        let addresses: Vec<_> = listed.iter().map(|f| f.address.as_str()).collect();
        assert_eq!(addresses, ["ada@example.com", "bob@example.com"]);
        assert_eq!(listed[0].setup, Setup::Google, "the first one stays");
    }

    #[test]
    fn nothing_is_installed_in_an_empty_home() {
        let home = std::env::temp_dir().join(format!("postden-import-{}", std::process::id()));
        std::fs::create_dir_all(&home).unwrap();
        assert!(installed(&home).is_empty(), "no app's files are there");
        std::fs::remove_dir_all(&home).unwrap();
    }
}
