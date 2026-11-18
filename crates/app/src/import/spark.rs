//! Spark Desktop, by Readdle.
//!
//! Spark keeps its accounts in a SQLite database, `core-data/messages.sqlite` in its data
//! directory: a row of the `accounts` table for each, its kind in `accountType` and its servers
//! as JSON in `additionalInfo`. The format isn't documented, so it's read loosely: an account
//! whose servers aren't as expected is listed as unreadable, not an error.
//!
//! Its passwords and sign-ins are in a store of Spark's own, encrypted for Spark alone, so they
//! aren't read.

use std::path::{Path, PathBuf};
use std::time::Duration;

use postden_core::account::{Security, Server};
use rusqlite::{Connection, OpenFlags};
use serde::Deserialize;

use super::{Found, Setup, Source, Unsupported, non_empty, once_each};
use crate::{Error, Result};

/// Where Spark's database is, in the home directory, on macOS.
const DATABASE: &str = "Library/Application Support/Spark Desktop/core-data/messages.sqlite";

/// How long to wait for Spark, if it's running, to be done writing its database.
const BUSY: Duration = Duration::from_secs(5);

/// The kinds of accounts in `accountType`, as far as they're known. Any other kind with an
/// IMAP and an SMTP server is read as one that logs in with a password.
mod kind {
    pub(super) const GMAIL: i64 = 0;
    /// Outlook.com, over IMAP with a Microsoft sign-in.
    pub(super) const OUTLOOK: i64 = 3;
    /// The Spark subscription itself: not a mailbox.
    pub(super) const SPARK: i64 = 30;
    /// Exchange, over Exchange Web Services.
    pub(super) const EXCHANGE: i64 = 33;
    /// Outlook.com or Microsoft 365, over Microsoft Graph.
    pub(super) const GRAPH: i64 = 35;
}

/// Spark's database, `messages.sqlite`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Spark {
    database: PathBuf,
}

impl Spark {
    /// Spark's database in the home directory `home`, if it's there.
    #[must_use]
    pub fn find(home: &Path) -> Option<Self> {
        let database = home.join(DATABASE);
        database.is_file().then_some(Self { database })
    }

    /// The Spark database at `database`.
    #[must_use]
    pub const fn at(database: PathBuf) -> Self {
        Self { database }
    }

    fn read(&self) -> rusqlite::Result<Vec<Row>> {
        // Read-only, so Spark's own copy is never changed, even while it's running.
        let db = Connection::open_with_flags(
            &self.database,
            OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX,
        )?;
        db.busy_timeout(BUSY)?;
        let mut rows = db.prepare(
            "SELECT accountType, accountTitle, ownerFullName, additionalInfo FROM accounts \
             ORDER BY orderNumber, pk",
        )?;
        rows.query_map([], |row| {
            Ok(Row {
                kind: row.get(0)?,
                title: row.get(1)?,
                owner: row.get(2)?,
                info: row.get(3)?,
            })
        })?
        .collect()
    }
}

impl Source for Spark {
    fn app(&self) -> &'static str {
        "Spark"
    }

    fn accounts(&self) -> Result<Vec<Found>> {
        let rows = self
            .read()
            .map_err(|e| Error::Invalid(format!("couldn't read Spark's accounts: {e}")))?;
        Ok(once_each(rows.into_iter().filter_map(Row::found).collect()))
    }
}

/// A row of Spark's `accounts` table.
#[derive(Debug)]
struct Row {
    kind: i64,
    title: String,
    owner: Option<String>,
    info: Option<String>,
}

/// What an account's `additionalInfo` says about its servers, as much as Postden reads of it.
#[derive(Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
struct Info {
    mail_server: Option<String>,
    mail_server_connection_type: Option<i64>,
    smtp_server: Option<String>,
    smtp_server_connection_type: Option<i64>,
}

impl Row {
    /// The account this row is, or `None` when it's not a mailbox, or has no address to file
    /// it under.
    fn found(self) -> Option<Found> {
        if self.kind == kind::SPARK {
            return None;
        }
        let info: serde_json::Value = serde_json::from_str(self.info.as_deref()?).ok()?;
        let address = info.get("accountAddress")?.as_str().map(str::to_owned);
        let address = non_empty(address)?;
        // Its servers are read apart from its address, so servers that aren't as expected
        // leave the account listed, as unreadable; a Gmail or Microsoft one doesn't need them.
        let setup = match self.kind {
            kind::GMAIL => Setup::Google,
            kind::OUTLOOK | kind::EXCHANGE | kind::GRAPH => {
                Setup::Unsupported(Unsupported::Microsoft)
            }
            _ => serde_json::from_value::<Info>(info)
                .map_err(|e| format!("its settings: {e}"))
                .and_then(|info| servers(&info, &address))
                .map_or_else(
                    |why| Setup::Unsupported(Unsupported::Unreadable(why)),
                    |(imap, smtp)| Setup::Password { imap, smtp },
                ),
        };
        let label = non_empty(Some(self.title)).filter(|t| !t.eq_ignore_ascii_case(&address));
        Some(Found {
            display_name: non_empty(self.owner),
            label,
            setup,
            address,
        })
    }
}

/// An account's IMAP and SMTP servers, logged in to with its address.
fn servers(info: &Info, address: &str) -> Result<(Server, Server), String> {
    let imap = server(
        info.mail_server.as_deref(),
        info.mail_server_connection_type,
        Security::default_imap_port,
        address,
    )
    .map_err(|why| format!("IMAP server: {why}"))?;
    let smtp = server(
        info.smtp_server.as_deref(),
        info.smtp_server_connection_type,
        Security::default_smtp_port,
        address,
    )
    .map_err(|why| format!("SMTP server: {why}"))?;
    Ok((imap, smtp))
}

/// A server from Spark's `host:port`, which may start with a scheme such as `https://` or leave
/// out the port, and its connection type.
fn server(
    at: Option<&str>,
    connection: Option<i64>,
    default_port: fn(Security) -> u16,
    address: &str,
) -> Result<Server, String> {
    let security = match connection {
        Some(4) => Security::Tls,
        Some(2) => Security::StartTls,
        Some(other) => return Err(format!("unknown connection type {other}")),
        None => return Err("no connection type".into()),
    };
    let at = at.map(str::trim).unwrap_or_default();
    let at = at.split_once("://").map_or(at, |(_, rest)| rest);
    let at = at.trim_end_matches('/');
    let (host, port) = match at.rsplit_once(':') {
        Some((host, port)) => (
            host,
            port.parse()
                .map_err(|_parse| format!("{port:?} isn't a port"))?,
        ),
        None => (at, default_port(security)),
    };
    if host.is_empty() {
        return Err("missing".into());
    }
    Ok(Server {
        host: host.to_owned(),
        port,
        security,
        username: address.to_owned(),
    })
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    /// A Spark database at `dir` with Spark's `accounts` table and these rows, as
    /// `(accountType, accountTitle, ownerFullName, additionalInfo)`.
    fn database(dir: &Path, rows: &[(i64, &str, Option<&str>, Option<String>)]) -> PathBuf {
        std::fs::create_dir_all(dir).unwrap();
        let path = dir.join("messages.sqlite");
        drop(std::fs::remove_file(&path));
        let db = Connection::open(&path).unwrap();
        // As Spark creates it.
        db.execute_batch(
            "CREATE TABLE accounts ( pk INTEGER PRIMARY KEY AUTOINCREMENT NOT NULL, \
             accountType INTEGER NOT NULL, accountTitle TEXT NOT NULL, ownerFullName TEXT, \
             ownerPictureURL TEXT, orderNumber INTEGER, additionalInfo TEXT );",
        )
        .unwrap();
        for (order, (kind, title, owner, info)) in (0_i64..).zip(rows) {
            db.execute(
                "INSERT INTO accounts (accountType, accountTitle, ownerFullName, orderNumber, \
                 additionalInfo) VALUES (?1, ?2, ?3, ?4, ?5)",
                rusqlite::params![kind, title, owner, order, info],
            )
            .unwrap();
        }
        path
    }

    fn temp(test: &str) -> PathBuf {
        std::env::temp_dir().join(format!("postden-spark-{test}-{}", std::process::id()))
    }

    /// An `additionalInfo` as Spark writes it, with some of the fields Postden doesn't read.
    #[expect(
        clippy::unnecessary_wraps,
        reason = "a row's additionalInfo may be missing"
    )]
    fn info(address: &str, imap: (&str, i64), smtp: (&str, i64)) -> Option<String> {
        Some(
            json!({
                "accountAddress": address,
                "mailServer": imap.0,
                "mailServerConnectionType": imap.1,
                "mailServerAuthenticationKCKey": "6BDB0EE3-05E0-490E-A631-F9820415F5A8",
                "smtpServer": smtp.0,
                "smtpServerConnectionType": smtp.1,
                "emailEnabled": true,
                "ignoreCertificateError": false,
            })
            .to_string(),
        )
    }

    fn accounts(test: &str, rows: &[(i64, &str, Option<&str>, Option<String>)]) -> Vec<Found> {
        let dir = temp(test);
        let found = Spark::at(database(&dir, rows)).accounts().unwrap();
        std::fs::remove_dir_all(&dir).unwrap();
        found
    }

    #[test]
    fn an_imap_account_is_read_with_its_servers_and_names() {
        let found = accounts(
            "imap",
            &[(
                5,
                "Work",
                Some("Ada Lovelace"),
                info(
                    "ada@example.com",
                    ("mail.example.com:993", 4),
                    ("mail.example.com:587", 2),
                ),
            )],
        );
        let server = |port, security| Server {
            host: "mail.example.com".into(),
            port,
            security,
            username: "ada@example.com".into(),
        };
        assert_eq!(
            found,
            [Found {
                address: "ada@example.com".into(),
                display_name: Some("Ada Lovelace".into()),
                label: Some("Work".into()),
                setup: Setup::Password {
                    imap: server(993, Security::Tls),
                    smtp: server(587, Security::StartTls),
                },
            }]
        );
    }

    #[test]
    fn a_title_that_is_the_address_isnt_a_label() {
        let found = accounts(
            "title",
            &[(
                5,
                "Ada@Example.com",
                None,
                info(
                    "ada@example.com",
                    ("imap.example.com:993", 4),
                    ("smtp.example.com:465", 4),
                ),
            )],
        );
        assert_eq!(found[0].label, None);
        assert_eq!(found[0].display_name, None);
    }

    #[test]
    fn a_gmail_account_signs_in_with_google_whatever_its_servers_say() {
        // Spark wrote some Gmail accounts' servers as URLs, without ports.
        let found = accounts(
            "gmail",
            &[(
                0,
                "Gmail",
                None,
                info(
                    "ada@gmail.com",
                    ("https://imap.gmail.com", 4),
                    ("https://smtp.gmail.com", 4),
                ),
            )],
        );
        assert_eq!(found[0].setup, Setup::Google);
    }

    #[test]
    fn microsoft_accounts_are_listed_as_unsupported() {
        let microsoft = |address| info(address, ("outlook.office365.com:443", 4), ("", 1));
        let found = accounts(
            "microsoft",
            &[
                (3, "Outlook", None, microsoft("a@outlook.com")),
                (33, "Exchange", None, microsoft("b@example.com")),
                (35, "Graph", None, microsoft("c@outlook.com")),
            ],
        );
        assert_eq!(found.len(), 3, "each is listed");
        for f in found {
            assert_eq!(
                f.setup,
                Setup::Unsupported(Unsupported::Microsoft),
                "{}",
                f.address
            );
        }
    }

    #[test]
    fn the_spark_subscription_and_rows_without_an_address_arent_accounts() {
        let subscription = json!({ "name": "Personal", "isPersonal": true, "teamId": 1 });
        let found = accounts(
            "skipped",
            &[
                (30, "Spark", None, Some(subscription.to_string())),
                (
                    5,
                    "No address",
                    None,
                    Some(json!({ "mailServer": "x:993" }).to_string()),
                ),
                (5, "No info", None, None),
                (5, "Broken", None, Some("{not json".into())),
            ],
        );
        assert_eq!(found, []);
    }

    #[test]
    fn servers_that_cant_be_read_say_why() {
        let found = accounts(
            "unreadable",
            &[
                (
                    5,
                    "Plain",
                    None,
                    info(
                        "a@example.com",
                        ("imap.example.com:143", 1),
                        ("smtp.example.com:25", 4),
                    ),
                ),
                (
                    5,
                    "No SMTP",
                    None,
                    info("b@example.com", ("imap.example.com:993", 4), ("", 4)),
                ),
                (
                    5,
                    "Bad port",
                    None,
                    info(
                        "c@example.com",
                        ("imap.example.com:imap", 4),
                        ("smtp.example.com", 4),
                    ),
                ),
            ],
        );
        let why: Vec<_> = found
            .iter()
            .map(|f| match &f.setup {
                Setup::Unsupported(Unsupported::Unreadable(why)) => why.as_str(),
                other => panic!("{} was read as {other:?}", f.address),
            })
            .collect();
        assert_eq!(
            why,
            [
                "IMAP server: unknown connection type 1",
                "SMTP server: missing",
                "IMAP server: \"imap\" isn't a port",
            ]
        );
    }

    #[test]
    fn servers_of_the_wrong_kind_leave_the_account_listed() {
        let wrong = |address: &str| {
            json!({
                "accountAddress": address,
                "mailServer": "imap.example.com:993",
                "mailServerConnectionType": "4",
            })
            .to_string()
        };
        let found = accounts(
            "wrong-kind",
            &[
                (0, "Gmail", None, Some(wrong("ada@gmail.com"))),
                (5, "Work", None, Some(wrong("ada@example.com"))),
            ],
        );
        assert_eq!(found[0].setup, Setup::Google, "Gmail doesn't need them");
        assert!(
            matches!(
                &found[1].setup,
                Setup::Unsupported(Unsupported::Unreadable(why)) if why.starts_with("its settings")
            ),
            "{:?}",
            found[1].setup
        );
    }

    #[test]
    fn a_server_without_a_port_gets_the_usual_one() {
        let found = accounts(
            "port",
            &[(
                5,
                "Work",
                None,
                info(
                    "ada@example.com",
                    ("imap.example.com", 2),
                    ("imaps://smtp.example.com/", 4),
                ),
            )],
        );
        let Setup::Password { imap, smtp } = &found[0].setup else {
            panic!("read as {:?}", found[0].setup);
        };
        assert_eq!((imap.host.as_str(), imap.port), ("imap.example.com", 143));
        assert_eq!((smtp.host.as_str(), smtp.port), ("smtp.example.com", 465));
    }

    #[test]
    fn accounts_come_in_spark_order_each_address_once() {
        let imap = |a| info(a, ("imap.example.com:993", 4), ("smtp.example.com:465", 4));
        let found = accounts(
            "order",
            &[
                (5, "First", None, imap("b@example.com")),
                (5, "Second", None, imap("a@example.com")),
                (5, "Again", None, imap("b@example.com")),
            ],
        );
        let listed: Vec<_> = found.iter().map(|f| f.label.as_deref()).collect();
        assert_eq!(listed, [Some("First"), Some("Second")]);
    }

    #[test]
    fn spark_is_found_in_its_macos_data_directory() {
        let home = temp("home");
        assert_eq!(Spark::find(&home), None, "not before it's there");
        let dir = home.join(DATABASE);
        let path = database(dir.parent().unwrap(), &[]);
        assert_eq!(Spark::find(&home), Some(Spark::at(path)));
        let sources = super::super::installed(&home);
        assert_eq!(sources.len(), 1, "Spark is installed");
        assert_eq!(sources[0].app(), "Spark");
        std::fs::remove_dir_all(&home).unwrap();
    }

    #[test]
    fn a_file_that_isnt_sparks_database_is_an_error() {
        let dir = temp("garbage");
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("messages.sqlite");
        std::fs::write(&path, "not a database").unwrap();
        let read = Spark::at(path).accounts();
        std::fs::remove_dir_all(&dir).unwrap();
        assert!(
            matches!(read, Err(Error::Invalid(ref why)) if why.starts_with("couldn't read Spark")),
            "{read:?}"
        );
    }
}
