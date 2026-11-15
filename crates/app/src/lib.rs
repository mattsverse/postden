//! What every Postden frontend does the same way, so the frontends only draw.
//!
//! - [`data_dir`] and [`secrets_backend`]: where mail and passwords live.
//! - [`NewAccount`], [`save_account`] and [`remove_account`]: managing accounts.
//! - [`start_google_sign_in`] and [`finish_google_sign_in`]: adding a Gmail account by signing in
//!   with Google.
//! - [`check_logins`]: checking a new account's IMAP and SMTP logins.
//! - [`Mailroom`]: watching every account's inbox and saving what arrives, and sending mail.
//! - [`Draft`]: a message being written, blank or as a reply.
//! - [`signature`]: what each account adds below the mail it sends, images included.
//! - [`Settings`]: the app's preferences, such as its [`Theme`] and whether to check for updates.
//! - [`format_date`] and [`recipients`]: dates and recipients as a mail list shows them.
//! - [`mail_notice`] and [`new_mail_alert`]: what to tell the user when mail arrives.

mod accounts;
mod compose;
mod google;
mod mailroom;
mod settings;
pub mod signature;

use std::path::PathBuf;

pub use accounts::{
    Login, NewAccount, Passwords, check_logins, credentials, oauth_key, password_key,
    remove_account, save_account, smtp_password, smtp_password_key,
};
pub use compose::{Draft, Quote};
pub use google::{finish_google_sign_in, google_account, start_google_sign_in};
use jiff::{Timestamp, Zoned};
pub use mailroom::{Mailroom, Update};
use postden_core::message::Envelope;
use postden_secrets::Backend;
pub use settings::{POLL_INTERVALS, Settings, Theme};

/// Why a Postden operation failed, phrased for the user.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum Error {
    #[error(transparent)]
    Store(#[from] postden_store::Error),
    #[error(transparent)]
    Secret(#[from] postden_core::secrets::SecretError),
    #[error(transparent)]
    Mail(#[from] postden_mail::Error),
    #[error(transparent)]
    Io(#[from] std::io::Error),
    #[error("couldn't find a home directory; set POSTDEN_DATA_DIR")]
    NoHome,
    #[error("no password is stored for {0}")]
    NoPassword(String),
    #[error("{0} isn't signed in to Google; sign in with Google again")]
    NotSignedIn(String),
    #[error("{0}")]
    Invalid(String),
}

pub type Result<T, E = Error> = std::result::Result<T, E>;

/// The directory holding the local mail cache: `dir` if given, else `POSTDEN_DATA_DIR`, else the
/// platform's data directory. It's created if missing.
///
/// # Errors
///
/// Returns an error when there's no home directory or the directory can't be created.
pub fn data_dir(dir: Option<PathBuf>) -> Result<PathBuf> {
    let dir = match dir.or_else(|| std::env::var_os("POSTDEN_DATA_DIR").map(PathBuf::from)) {
        Some(dir) => dir,
        None => directories::ProjectDirs::from("", "", "postden")
            .ok_or(Error::NoHome)?
            .data_dir()
            .to_owned(),
    };
    std::fs::create_dir_all(&dir)?;
    Ok(dir)
}

/// The SQLite database inside a [`data_dir`].
#[must_use]
pub fn database_path(data_dir: &std::path::Path) -> PathBuf {
    data_dir.join("postden.db")
}

/// Where passwords live: the command if given, else `POSTDEN_PASSWORD_COMMAND`, else the OS keyring.
#[must_use]
pub fn secrets_backend(password_command: Option<String>) -> Backend {
    password_command
        .or_else(|| std::env::var("POSTDEN_PASSWORD_COMMAND").ok())
        .filter(|c| !c.trim().is_empty())
        .map_or(Backend::Keyring, |command| Backend::Command { command })
}

/// What to tell the user when mail was saved for the account named `label`.
#[must_use]
pub fn mail_notice(label: &str, count: usize, first_sync: bool, now: &Zoned) -> String {
    let s = if count == 1 { "" } else { "s" };
    match (first_sync, count) {
        (true, 0) => format!("{label}: the inbox is empty"),
        (true, _) => format!("{label}: synced the newest {count} message{s}"),
        (false, _) => format!(
            "{label}: {count} new message{s} at {}",
            now.strftime("%H:%M")
        ),
    }
}

/// A system notification's title and body for mail that arrived unread in the account `label`.
///
/// One message shows its sender and subject; several, how many came and from whom. `None` when
/// nothing did.
#[must_use]
pub fn new_mail_alert(label: &str, unread: &[Envelope]) -> Option<(String, String)> {
    match unread {
        [] => None,
        [only] => {
            let from = only.from.label();
            let title = if from.is_empty() {
                "Unknown sender"
            } else {
                from
            };
            let subject = only.subject.as_deref().map(str::trim).unwrap_or_default();
            let body = if subject.is_empty() {
                "(no subject)"
            } else {
                subject
            };
            Some((title.to_owned(), body.to_owned()))
        }
        several => {
            let mut senders: Vec<&str> = Vec::new();
            for m in several {
                let from = m.from.label();
                if !from.is_empty() && !senders.contains(&from) {
                    senders.push(from);
                }
            }
            let title = format!("{} new messages in {label}", several.len());
            let body = match senders.as_slice() {
                [] => String::new(),
                [one] => format!("From {one}"),
                [one, two] => format!("From {one} and {two}"),
                [one, two, rest @ ..] => format!("From {one}, {two} and {} more", rest.len()),
            };
            Some((title, body))
        }
    }
}

/// Who a message went to, short enough for a mail list: the first recipient, and how many more.
#[must_use]
pub fn recipients(to: &[postden_core::message::Address]) -> String {
    match to {
        [] => "(no recipients)".to_owned(),
        [only] => only.label().to_owned(),
        [first, rest @ ..] => format!("{} +{}", first.label(), rest.len()),
    }
}

/// A short date for a mail list: the time for today, the day this year, the full date otherwise.
/// Empty when there's no date.
#[must_use]
pub fn format_date(unix: Option<i64>, now: &Zoned) -> String {
    let Some(at) = unix.and_then(|s| Timestamp::from_second(s).ok()) else {
        return String::new();
    };
    let at = at.to_zoned(now.time_zone().clone());
    let format = if at.date() == now.date() {
        "%H:%M"
    } else if at.year() == now.year() {
        "%b %d"
    } else {
        "%Y-%m-%d"
    };
    at.strftime(format).to_string()
}

#[cfg(test)]
mod tests {
    use jiff::tz::TimeZone;

    use super::*;

    #[test]
    fn notices_say_what_arrived() {
        let now = Timestamp::from_second(1_794_475_800)
            .unwrap()
            .to_zoned(TimeZone::UTC);
        assert_eq!(mail_notice("Me", 0, true, &now), "Me: the inbox is empty");
        assert_eq!(
            mail_notice("Me", 1, true, &now),
            "Me: synced the newest 1 message"
        );
        assert_eq!(
            mail_notice("Me", 2, false, &now),
            "Me: 2 new messages at 09:30"
        );
    }

    #[test]
    fn alerts_name_one_message_or_sum_up_several() {
        use postden_core::message::Address;
        let from = |name: Option<&str>, subject: Option<&str>| Envelope {
            uid: 1,
            message_id: None,
            from: Address {
                name: name.map(str::to_owned),
                email: None,
            },
            to: Vec::new(),
            subject: subject.map(str::to_owned),
            date: None,
            seen: false,
        };
        let alert = |unread: &[Envelope]| new_mail_alert("Work", unread);
        let pair = |title: &str, body: &str| Some((title.to_owned(), body.to_owned()));
        assert_eq!(alert(&[]), None);
        assert_eq!(
            alert(&[from(Some("Ada"), Some(" Lunch? "))]),
            pair("Ada", "Lunch?")
        );
        assert_eq!(
            alert(&[from(None, None)]),
            pair("Unknown sender", "(no subject)")
        );
        assert_eq!(
            alert(&[from(Some("Ada"), None), from(Some("Ada"), None)]),
            pair("2 new messages in Work", "From Ada")
        );
        assert_eq!(
            alert(&[
                from(Some("Ada"), None),
                from(None, None),
                from(Some("Bob"), None)
            ]),
            pair("3 new messages in Work", "From Ada and Bob")
        );
        let many: Vec<_> = ["Ada", "Bob", "Cy", "Di"]
            .into_iter()
            .map(|n| from(Some(n), None))
            .collect();
        assert_eq!(
            alert(&many),
            pair("4 new messages in Work", "From Ada, Bob and 2 more")
        );
    }

    #[test]
    fn recipients_are_summed_up() {
        use postden_core::message::Address;
        let a = |name: &str| Address {
            name: Some(name.to_owned()),
            email: None,
        };
        assert_eq!(recipients(&[]), "(no recipients)");
        assert_eq!(recipients(&[a("Ada")]), "Ada");
        assert_eq!(recipients(&[a("Ada"), a("Bob"), a("Cy")]), "Ada +2");
    }

    #[test]
    fn dates_get_shorter_the_closer_they_are() {
        let now = Timestamp::from_second(1_794_475_800)
            .unwrap()
            .to_zoned(TimeZone::UTC);
        assert_eq!(format_date(Some(1_794_475_800 - 3600), &now), "08:30");
        assert_eq!(
            format_date(Some(1_794_475_800 - 30 * 86_400), &now),
            "Oct 13"
        );
        assert_eq!(format_date(Some(1_700_000_000), &now), "2023-11-14");
        assert_eq!(format_date(None, &now), "");
    }
}
