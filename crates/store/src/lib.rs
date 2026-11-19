//! The local, on-device cache of accounts, mailboxes and messages, in SQLite.
//!
//! Account passwords never land here; they live in a [`postden_core::secrets::SecretStore`].

use std::cell::RefCell;
use std::collections::{HashMap, HashSet};
use std::path::Path;
use std::time::{Duration, Instant};

use postden_core::account::{Account, Auth, Security, Server, Signature};
pub use postden_core::message::MailboxState;
use postden_core::message::{Address, Body, Envelope, InlineImage, Mailbox, Role};
use rusqlite::{Connection, OptionalExtension, Row, Transaction, TransactionBehavior, params};

mod sorting;

pub use sorting::{
    AutosortState, Autosorted, Sorting, SortingChanges, SortingClass, SortingTaught, SortingWeight,
};

/// Why a [`Store`] call failed.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum Error {
    #[error("database error: {0}")]
    Sqlite(#[from] rusqlite::Error),
    /// A row holds a value this version can't read, for example from a newer Postden.
    #[error("corrupt row: {0}")]
    Corrupt(String),
}

pub type Result<T, E = Error> = std::result::Result<T, E>;

/// A message as listed across accounts.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StoredMessage {
    pub account: String,
    pub mailbox: String,
    pub envelope: Envelope,
}

/// A cached message as [`Store::learning_messages`] gives it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Learnable {
    pub message: StoredMessage,
    /// The UIDVALIDITY its mailbox is cached under: its UID names it only while it holds.
    pub uid_validity: u32,
    /// What its mailbox is for; but where an archive, such as Gmail's All Mail, lists it while
    /// it's snoozed, or once it was moved elsewhere ([`Store::put_away`]): the Trash or Junk if
    /// that's where, else [`Role::Snoozed`], as it says nothing of where it went.
    pub role: Role,
    /// The beginning of its body's text, if it was downloaded.
    pub text: Option<String>,
}

/// A snoozed message, waiting in an account's Snoozed mailbox until it's due back in the inbox.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Snooze {
    pub account: String,
    /// Its `Message-ID`, without angle brackets: unlike its UID, it stays the same as it moves.
    pub message_id: String,
    /// The mailbox it waits in.
    pub mailbox: String,
    /// When it's due back, as Unix seconds.
    pub until: i64,
}

/// Which cached mail to look through.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Scope<'a> {
    /// Every account's mailbox with this role, such as every inbox.
    Role(Role),
    /// One of an account's mailboxes.
    Mailbox { account: &'a str, name: &'a str },
}

const SCHEMA: &str = "
PRAGMA foreign_keys = ON;
PRAGMA journal_mode = WAL;

CREATE TABLE IF NOT EXISTS accounts (
    id            TEXT PRIMARY KEY,
    display_name  TEXT,
    imap_host     TEXT NOT NULL,
    imap_port     INTEGER NOT NULL,
    imap_security TEXT NOT NULL,
    imap_username TEXT NOT NULL,
    smtp_host     TEXT NOT NULL,
    smtp_port     INTEGER NOT NULL,
    smtp_security TEXT NOT NULL,
    smtp_username TEXT NOT NULL,
    -- How the account logs in: 'password' or 'google'.
    auth          TEXT NOT NULL DEFAULT 'password',
    -- What the UI calls the account, if not its display name or address.
    label         TEXT
) STRICT;

CREATE TABLE IF NOT EXISTS mailboxes (
    account      TEXT NOT NULL REFERENCES accounts(id) ON DELETE CASCADE,
    name         TEXT NOT NULL,
    uid_validity INTEGER NOT NULL,
    role         TEXT NOT NULL DEFAULT 'inbox',
    -- Every message from this UID up is cached, and the unread ones below it were looked for:
    -- older mail is fetched below it. NULL until unread mail was first looked for, as every
    -- message from the lowest UID cached up is then.
    floor        INTEGER,
    -- 1 once its unread mail was searched for on the server under this UIDVALIDITY: its unread
    -- count is the server's, even when it's none.
    counted      INTEGER NOT NULL DEFAULT 0,
    PRIMARY KEY (account, name)
) STRICT;

CREATE TABLE IF NOT EXISTS messages (
    account    TEXT NOT NULL,
    mailbox    TEXT NOT NULL,
    uid        INTEGER NOT NULL,
    message_id TEXT,
    from_name  TEXT,
    from_email TEXT,
    subject    TEXT,
    date       INTEGER,
    seen       INTEGER NOT NULL,
    -- The To header: each address as name, U+001F, email; addresses separated by U+001E.
    to_list    TEXT,
    -- 1 when starred: flagged as such on the server.
    flagged    INTEGER NOT NULL DEFAULT 0,
    PRIMARY KEY (account, mailbox, uid),
    FOREIGN KEY (account, mailbox) REFERENCES mailboxes(account, name) ON DELETE CASCADE
) STRICT;

CREATE INDEX IF NOT EXISTS messages_by_date ON messages(date DESC);

-- Messages the server last had unread that aren't cached, for the mailbox's unread count: the
-- cache holds only the newest, and the newest of the older unread mail.
CREATE TABLE IF NOT EXISTS unseen (
    account TEXT NOT NULL,
    mailbox TEXT NOT NULL,
    uid     INTEGER NOT NULL,
    PRIMARY KEY (account, mailbox, uid),
    FOREIGN KEY (account, mailbox) REFERENCES mailboxes(account, name) ON DELETE CASCADE
) STRICT;

-- Messages downloaded ahead of their opening, or kept once opened. Address lists are encoded as
-- in messages.to_list; references and attachment names are separated by U+001E.
CREATE TABLE IF NOT EXISTS bodies (
    account      TEXT NOT NULL,
    mailbox      TEXT NOT NULL,
    uid          INTEGER NOT NULL,
    to_list      TEXT NOT NULL,
    cc_list      TEXT NOT NULL,
    reply_to     TEXT NOT NULL,
    refs         TEXT NOT NULL,
    text         TEXT NOT NULL,
    html         TEXT,
    attachments  TEXT NOT NULL,
    -- 1 when kept because it was opened: only downloading ahead's own are dropped as they age.
    kept         INTEGER NOT NULL DEFAULT 0,
    PRIMARY KEY (account, mailbox, uid),
    FOREIGN KEY (account, mailbox, uid) REFERENCES messages(account, mailbox, uid)
        ON DELETE CASCADE
) STRICT;

-- Every mailbox the server last listed for an account, synced or not.
CREATE TABLE IF NOT EXISTS listed_mailboxes (
    account   TEXT NOT NULL REFERENCES accounts(id) ON DELETE CASCADE,
    name      TEXT NOT NULL,
    delimiter TEXT,
    role      TEXT NOT NULL,
    PRIMARY KEY (account, name)
) STRICT;

-- The app's preferences, such as its theme, by name.
CREATE TABLE IF NOT EXISTS settings (
    name  TEXT PRIMARY KEY,
    value TEXT NOT NULL
) STRICT;

-- What's added below the mail each account sends.
CREATE TABLE IF NOT EXISTS signatures (
    account TEXT PRIMARY KEY REFERENCES accounts(id) ON DELETE CASCADE,
    html    TEXT NOT NULL,
    -- 1 when written as HTML source, 0 in the visual editor.
    source  INTEGER NOT NULL
) STRICT;

-- The images a signature shows, in order.
CREATE TABLE IF NOT EXISTS signature_images (
    account  TEXT NOT NULL REFERENCES signatures(account) ON DELETE CASCADE,
    position INTEGER NOT NULL,
    cid      TEXT NOT NULL,
    name     TEXT NOT NULL,
    mime     TEXT NOT NULL,
    data     BLOB NOT NULL,
    PRIMARY KEY (account, cid)
) STRICT;

-- Messages moved elsewhere than an archive, by Message-ID, with where they went and its role:
-- where an archive still lists one, as Gmail's All Mail does, it wasn't archived but put away
-- there, as the Trash, Junk or a folder, while that's still listed: for a week after it was
-- noted, as by then where it went is cached, and it may have moved since, unseen.
CREATE TABLE IF NOT EXISTS put_away (
    account    TEXT NOT NULL REFERENCES accounts(id) ON DELETE CASCADE,
    message_id TEXT NOT NULL,
    mailbox    TEXT NOT NULL,
    role       TEXT NOT NULL,
    -- When it was noted, as Unix seconds.
    noted      INTEGER NOT NULL,
    PRIMARY KEY (account, message_id)
) STRICT;

-- Snoozed messages, by Message-ID, and when each is due back in the inbox, as Unix seconds.
CREATE TABLE IF NOT EXISTS snoozes (
    account    TEXT NOT NULL REFERENCES accounts(id) ON DELETE CASCADE,
    message_id TEXT NOT NULL,
    mailbox    TEXT NOT NULL,
    until      INTEGER NOT NULL,
    PRIMARY KEY (account, message_id)
) STRICT;

-- People mail was sent to, suggested as recipients, kept only while the setting says to.
CREATE TABLE IF NOT EXISTS contacts (
    email     TEXT PRIMARY KEY COLLATE NOCASE,
    name      TEXT,
    -- How many messages went to them, and when the last did, as Unix seconds.
    sent      INTEGER NOT NULL,
    last_sent INTEGER NOT NULL
) STRICT;

-- What sorting mail learned: each class of each of an account's models, as postden_ai keeps it.
CREATE TABLE IF NOT EXISTS sorting_classes (
    id       INTEGER PRIMARY KEY,
    account  TEXT NOT NULL REFERENCES accounts(id) ON DELETE CASCADE,
    model    TEXT NOT NULL,
    name     TEXT NOT NULL,
    messages INTEGER NOT NULL,
    guessed  INTEGER NOT NULL,
    correct  INTEGER NOT NULL,
    UNIQUE (account, model, name)
) STRICT;

-- What each class learned of each feature it has a weight for: FTRL-Proximal's z and n.
CREATE TABLE IF NOT EXISTS sorting_weights (
    class   INTEGER NOT NULL REFERENCES sorting_classes(id) ON DELETE CASCADE,
    feature INTEGER NOT NULL,
    z       REAL NOT NULL,
    n       REAL NOT NULL,
    PRIMARY KEY (class, feature)
) STRICT, WITHOUT ROWID;

-- What each message, by Message-ID, was last taught as, for each of an account's models.
CREATE TABLE IF NOT EXISTS sorting_taught (
    account    TEXT NOT NULL REFERENCES accounts(id) ON DELETE CASCADE,
    model      TEXT NOT NULL,
    message_id TEXT NOT NULL,
    class      TEXT NOT NULL,
    PRIMARY KEY (account, model, message_id)
) STRICT;

-- The mailboxes inbox mail is moved to by itself, once sorting is sure it belongs there.
CREATE TABLE IF NOT EXISTS autosort_mailboxes (
    account TEXT NOT NULL REFERENCES accounts(id) ON DELETE CASCADE,
    name    TEXT NOT NULL,
    PRIMARY KEY (account, name)
) STRICT;

-- The messages autosort moves or moved, by Message-ID, and where to: each is moved once, and
-- isn't learned from as filed there by hand.
CREATE TABLE IF NOT EXISTS autosorted (
    account    TEXT NOT NULL REFERENCES accounts(id) ON DELETE CASCADE,
    message_id TEXT NOT NULL,
    mailbox    TEXT NOT NULL,
    -- 'moving' while it's on its way, 'moved' once the server moved it, 'refused' if it didn't,
    -- 'unknown' if the app quit before it answered;
    -- 'kept' for a message the user keeps in the inbox, as one snoozed there, never moved.
    state      TEXT NOT NULL DEFAULT 'moving',
    PRIMARY KEY (account, message_id)
) STRICT;
";

/// The schema version [`SCHEMA`] creates, kept in `PRAGMA user_version`.
const VERSION: i64 = 7;

/// How long, in seconds, what [`Store::put_away_message`] noted of a message holds: a week. Its
/// copy where it went is cached by then, and says so itself; and moved since by another mail
/// app, it isn't there anymore.
const PUT_AWAY_HOLDS: i64 = 7 * 24 * 60 * 60;

/// Columns added since version 0, for databases created before them. Tables added since are
/// created by [`SCHEMA`] itself.
const MIGRATIONS: [(&str, &str, &str); 8] = [
    ("mailboxes", "role", "TEXT NOT NULL DEFAULT 'inbox'"),
    ("messages", "to_list", "TEXT"),
    // Version 2.
    ("accounts", "auth", "TEXT NOT NULL DEFAULT 'password'"),
    // Version 3.
    ("accounts", "label", "TEXT"),
    // Version 4.
    ("messages", "flagged", "INTEGER NOT NULL DEFAULT 0"),
    // Version 5.
    ("bodies", "kept", "INTEGER NOT NULL DEFAULT 0"),
    // Version 6.
    ("mailboxes", "floor", "INTEGER"),
    // Version 7.
    ("mailboxes", "counted", "INTEGER NOT NULL DEFAULT 0"),
];

const MESSAGE_COLUMNS: &str = "m.account, m.mailbox, m.uid, m.message_id, m.from_name, m.from_email, m.subject, m.date, m.seen, m.to_list, m.flagged";

/// A transaction that takes the write lock at once, for one that reads what it then writes on:
/// begun deferred, another connection's write in between (the sorter's, the mail watchers')
/// would fail it.
fn immediate(conn: &mut Connection) -> Result<Transaction<'_>> {
    Ok(conn.transaction_with_behavior(TransactionBehavior::Immediate)?)
}

/// Whether `account` reads the mailboxes the account with its id saved reads, if there's one:
/// on the same IMAP server, port and username.
fn same_mailboxes(conn: &Connection, account: &Account) -> Result<bool> {
    let mailbox_owner: Option<(String, u16, String)> = conn
        .query_row(
            "SELECT imap_host, imap_port, imap_username FROM accounts WHERE id = ?1",
            [&account.id],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )
        .optional()?;
    Ok(mailbox_owner.is_none_or(|(host, port, username)| {
        host.eq_ignore_ascii_case(&account.imap.host)
            && port == account.imap.port
            && username == account.imap.username
    }))
}

/// Forgets what's known of account `id`'s mailboxes, as it reads another server's or user's
/// now: what's cached of the old ones, down to their UIDs and UIDVALIDITY, says nothing about
/// the new ones. Messages and bodies go with their mailbox; snoozes too, as they're of messages
/// on the old server; and what sorting learned of where its mail goes, and the mailboxes it
/// autosorts to.
fn forget_mailboxes_of(tx: &Transaction<'_>, id: &str) -> Result<()> {
    tx.execute("DELETE FROM mailboxes WHERE account = ?1", [id])?;
    tx.execute("DELETE FROM listed_mailboxes WHERE account = ?1", [id])?;
    tx.execute("DELETE FROM snoozes WHERE account = ?1", [id])?;
    tx.execute("DELETE FROM put_away WHERE account = ?1", [id])?;
    sorting::forget_sorting_of(tx, id)?;
    sorting::forget_autosort_of(tx, id)?;
    Ok(())
}

/// A message, by account, mailbox and UID, or a whole mailbox with `None`.
type Touched = (String, String, Option<u32>);

/// What was last written for a message, or a whole mailbox: each with when, as
/// [`Written::at`] counts.
#[derive(Debug, Clone, Copy, Default)]
struct Marks {
    /// The mailbox's UIDVALIDITY they were written under: under another, the UID names another
    /// message.
    uid_validity: u32,
    seen: Option<(bool, u64)>,
    flagged: Option<(bool, u64)>,
    /// For a whole mailbox: every message below this UID was marked read.
    read_below: Option<(u32, u64)>,
    /// When anything was last written here.
    last: Option<Instant>,
}

/// How long what was written is kept: longer than any fetch can take (each IMAP command gives
/// up after two minutes, and a fetch's logout after seconds), so one that started before it and
/// is saved after it finds it. Kept
/// longer, it's left alone by what was read after it anyway.
const KEEP_WRITTEN: Duration = Duration::from_mins(10);

/// Forgets what was written longer ago than [`KEEP_WRITTEN`].
fn forget_old(all: &mut HashMap<Touched, Marks>) {
    all.retain(|_, marks| marks.last.is_some_and(|t| t.elapsed() < KEEP_WRITTEN));
}

/// `marks`, as kept under `uid_validity`: started afresh if they were kept under another.
fn under(marks: &mut Marks, uid_validity: u32) -> &mut Marks {
    if marks.uid_validity != uid_validity {
        *marks = Marks {
            uid_validity,
            ..Marks::default()
        };
    }
    marks.last = Some(Instant::now());
    marks
}

/// The SQLite-backed cache.
#[derive(Debug)]
pub struct Store {
    conn: Connection,
    /// Messages [`Store::remove_message`] dropped, by account, mailbox and UID, which mail
    /// fetched before they were dropped mustn't bring back. A UID isn't reused while its
    /// mailbox's UIDVALIDITY holds, so they're kept until it changes. Fetches don't outlive the
    /// app, so this isn't saved.
    removed: HashSet<(String, String, u32)>,
    /// The last read mark and star written for each message, by account, mailbox and UID, and
    /// the last time a whole mailbox was marked read, with `None`: what a sync read before then
    /// is older. Kept for [`KEEP_WRITTEN`].
    written: RefCell<HashMap<Touched, Marks>>,
}

/// A read mark or star the server answered, and when, as counted by the mail watchers' shared
/// count of changes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Written {
    pub at: u64,
    /// Read or unread, if that's what was written.
    pub seen: Option<bool>,
    /// Starred or not, if that's what was written.
    pub flagged: Option<bool>,
}

impl Store {
    /// Opens or creates the database at `path`, creating its tables if needed.
    ///
    /// # Errors
    ///
    /// Returns an error when the file can't be opened or the schema can't be applied.
    pub fn open(path: &Path) -> Result<Self> {
        Self::init(Connection::open(path)?)
    }

    /// An in-memory database, for tests.
    ///
    /// # Errors
    ///
    /// Returns an error when the schema can't be applied.
    pub fn open_in_memory() -> Result<Self> {
        Self::init(Connection::open_in_memory()?)
    }

    fn init(conn: Connection) -> Result<Self> {
        conn.execute_batch(SCHEMA)?;
        let version: i64 = conn.query_row("PRAGMA user_version", [], |row| row.get(0))?;
        if version < VERSION {
            // Tables made by an older version lack the newer columns; fresh ones already have them.
            for (table, column, definition) in MIGRATIONS {
                let present: bool = conn.query_row(
                    "SELECT EXISTS (SELECT 1 FROM pragma_table_info(?1) WHERE name = ?2)",
                    [table, column],
                    |row| row.get(0),
                )?;
                if !present {
                    conn.execute_batch(&format!(
                        "ALTER TABLE {table} ADD COLUMN {column} {definition}"
                    ))?;
                }
            }
            conn.execute_batch(&format!("PRAGMA user_version = {VERSION}"))?;
        }
        Ok(Self {
            conn,
            removed: HashSet::new(),
            written: RefCell::new(HashMap::new()),
        })
    }

    /// Adds `account`, or replaces the settings of the account with the same id.
    /// Replacing keeps its signature and its label, which only [`Store::set_names`] changes, so
    /// adding an account again doesn't forget what it was called; and its cached mail, while its
    /// mailboxes are the same ones: on the same IMAP server, port and username.
    ///
    /// # Errors
    ///
    /// Returns an error when the write fails.
    pub fn save_account(&self, account: &Account) -> Result<()> {
        // Immediate: what's read is written on, and the sorter may write meanwhile.
        let tx = Transaction::new_unchecked(&self.conn, TransactionBehavior::Immediate)?;
        if !same_mailboxes(&tx, account)? {
            forget_mailboxes_of(&tx, &account.id)?;
        }
        upsert_account(&tx, account)?;
        tx.commit()?;
        Ok(())
    }

    /// Adds `account`, or replaces the account with the same id entirely: its settings, its
    /// names, which [`Store::save_account`] keeps, and its signature, or none with `None`. Its
    /// cached mail is kept only while its mailboxes are the same ones: on the same IMAP server,
    /// port and username. It's all or nothing: when a write fails, nothing is written.
    ///
    /// # Errors
    ///
    /// Returns an error when a write fails.
    pub fn replace_account(
        &mut self,
        account: &Account,
        signature: Option<&Signature>,
    ) -> Result<()> {
        let tx = immediate(&mut self.conn)?;
        if !same_mailboxes(&tx, account)? {
            forget_mailboxes_of(&tx, &account.id)?;
        }
        upsert_account(&tx, account)?;
        tx.execute(
            "UPDATE accounts SET label = ?2, display_name = ?3 WHERE id = ?1",
            params![account.id, account.label, account.display_name],
        )?;
        write_signature(&tx, &account.id, signature)?;
        tx.commit()?;
        Ok(())
    }

    /// Sets what the account is called in the UI and the name its mail is sent under. Returns
    /// whether the account exists.
    ///
    /// # Errors
    ///
    /// Returns an error when the write fails.
    pub fn set_names(
        &self,
        id: &str,
        label: Option<&str>,
        display_name: Option<&str>,
    ) -> Result<bool> {
        Ok(self.conn.execute(
            "UPDATE accounts SET label = ?2, display_name = ?3 WHERE id = ?1",
            params![id, label, display_name],
        )? > 0)
    }

    /// The account's signature, or `None` when it has none.
    ///
    /// # Errors
    ///
    /// Returns an error when the read fails.
    pub fn signature(&self, account: &str) -> Result<Option<Signature>> {
        let found: Option<(String, bool)> = self
            .conn
            .query_row(
                "SELECT html, source FROM signatures WHERE account = ?1",
                [account],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .optional()?;
        let Some((html, source)) = found else {
            return Ok(None);
        };
        let mut stmt = self.conn.prepare(
            "SELECT cid, name, mime, data FROM signature_images WHERE account = ?1
             ORDER BY position",
        )?;
        let images = stmt
            .query_map([account], |row| {
                Ok(InlineImage {
                    cid: row.get(0)?,
                    name: row.get(1)?,
                    mime: row.get(2)?,
                    data: row.get::<_, Vec<u8>>(3)?.into(),
                })
            })?
            .collect::<rusqlite::Result<_>>()?;
        Ok(Some(Signature {
            html,
            source,
            images,
        }))
    }

    /// Replaces the account's signature, or removes it with `None`.
    ///
    /// # Errors
    ///
    /// Returns an error when the write fails, such as for an account that doesn't exist. Nothing
    /// is written in that case.
    pub fn save_signature(&mut self, account: &str, signature: Option<&Signature>) -> Result<()> {
        let tx = self.conn.transaction()?;
        write_signature(&tx, account, signature)?;
        tx.commit()?;
        Ok(())
    }

    /// Every account, ordered by id.
    ///
    /// # Errors
    ///
    /// Returns an error when the read fails or a row is corrupt.
    pub fn accounts(&self) -> Result<Vec<Account>> {
        let mut stmt = self.conn.prepare("SELECT * FROM accounts ORDER BY id")?;
        let rows = stmt.query_map([], |row| Ok(account_from_row(row)))?;
        rows.map(|r| r?).collect()
    }

    /// The account with this id, if there is one.
    ///
    /// # Errors
    ///
    /// Returns an error when the read fails or the row is corrupt.
    pub fn account(&self, id: &str) -> Result<Option<Account>> {
        self.conn
            .query_row("SELECT * FROM accounts WHERE id = ?1", [id], |row| {
                Ok(account_from_row(row))
            })
            .optional()?
            .transpose()
    }

    /// Removes the account and all its cached mail. Returns whether it existed.
    ///
    /// # Errors
    ///
    /// Returns an error when the write fails.
    pub fn remove_account(&self, id: &str) -> Result<bool> {
        let tx = self.conn.unchecked_transaction()?;
        let removed = tx.execute("DELETE FROM accounts WHERE id = ?1", [id])? > 0;
        // What sorting learned of it goes along, from the sorter's memory too.
        if removed {
            sorting::forget_sorting_of(&tx, id)?;
        }
        tx.commit()?;
        Ok(removed)
    }

    /// What's cached of `mailbox`, or `None` if it was never synced.
    ///
    /// # Errors
    ///
    /// Returns an error when the read fails.
    pub fn mailbox_state(&self, account: &str, mailbox: &str) -> Result<Option<MailboxState>> {
        Ok(self
            .conn
            .query_row(
                "SELECT b.uid_validity, MAX(m.uid) FROM mailboxes b
                 LEFT JOIN messages m ON m.account = b.account AND m.mailbox = b.name
                 WHERE b.account = ?1 AND b.name = ?2
                 GROUP BY b.account, b.name",
                [account, mailbox],
                |row| {
                    Ok(MailboxState {
                        uid_validity: row.get(0)?,
                        last_uid: row.get(1)?,
                    })
                },
            )
            .optional()?)
    }

    /// The account's mailbox with this role, and what's cached of it, or `None` if none was synced.
    ///
    /// # Errors
    ///
    /// Returns an error when the read fails.
    pub fn mailbox_for_role(
        &self,
        account: &str,
        role: Role,
    ) -> Result<Option<(String, MailboxState)>> {
        let name: Option<String> = self
            .conn
            .query_row(
                "SELECT name FROM mailboxes WHERE account = ?1 AND role = ?2 LIMIT 1",
                [account, role.as_str()],
                |row| row.get(0),
            )
            .optional()?;
        let Some(name) = name else { return Ok(None) };
        Ok(self
            .mailbox_state(account, &name)?
            .map(|state| (name, state)))
    }

    /// What's cached of each of the account's synced mailboxes, by name.
    ///
    /// # Errors
    ///
    /// Returns an error when the read fails.
    pub fn mailbox_states(&self, account: &str) -> Result<HashMap<String, MailboxState>> {
        let mut stmt = self.conn.prepare(
            "SELECT b.name, b.uid_validity, MAX(m.uid) FROM mailboxes b
             LEFT JOIN messages m ON m.account = b.account AND m.mailbox = b.name
             WHERE b.account = ?1
             GROUP BY b.account, b.name",
        )?;
        let rows = stmt.query_map([account], |row| {
            Ok((
                row.get::<_, String>(0)?,
                MailboxState {
                    uid_validity: row.get(1)?,
                    last_uid: row.get(2)?,
                },
            ))
        })?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    /// Replaces the account's list of mailboxes with what the server listed. Cached mail from a
    /// mailbox that's no longer listed (deleted or renamed on the server) is dropped, and a
    /// cached mailbox still listed takes the role it's listed with, as when it's now the Drafts.
    ///
    /// # Errors
    ///
    /// Returns an error when the write fails. Nothing is written in that case.
    pub fn save_mailbox_list(&mut self, account: &str, mailboxes: &[Mailbox]) -> Result<()> {
        let tx = self.conn.transaction()?;
        tx.execute("DELETE FROM listed_mailboxes WHERE account = ?1", [account])?;
        {
            let mut insert = tx.prepare("INSERT INTO listed_mailboxes VALUES (?1, ?2, ?3, ?4)")?;
            for m in mailboxes {
                insert.execute(params![account, m.name, m.delimiter, m.role.as_str()])?;
            }
        }
        tx.execute(
            "DELETE FROM mailboxes WHERE account = ?1
             AND name NOT IN (SELECT name FROM listed_mailboxes WHERE account = ?1)",
            [account],
        )?;
        tx.execute(
            "UPDATE mailboxes SET role = (SELECT l.role FROM listed_mailboxes l
                WHERE l.account = mailboxes.account AND l.name = mailboxes.name)
             WHERE account = ?1",
            [account],
        )?;
        // Mail put away in a mailbox gone says nothing of one made again under its name.
        tx.execute(
            "DELETE FROM put_away WHERE account = ?1
             AND mailbox NOT IN (SELECT name FROM listed_mailboxes WHERE account = ?1)",
            [account],
        )?;
        tx.commit()?;
        Ok(())
    }

    /// The account's mailboxes as last listed: the inbox, then Sent, then the rest by name.
    ///
    /// # Errors
    ///
    /// Returns an error when the read fails or a row is corrupt.
    pub fn mailboxes(&self, account: &str) -> Result<Vec<Mailbox>> {
        let mut stmt = self.conn.prepare(
            "SELECT name, delimiter, role FROM listed_mailboxes WHERE account = ?1
             ORDER BY CASE role WHEN 'inbox' THEN 0 WHEN 'sent' THEN 1 ELSE 2 END, name",
        )?;
        let rows = stmt.query_map([account], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, Option<String>>(1)?,
                row.get::<_, String>(2)?,
            ))
        })?;
        rows.map(|row| {
            let (name, delimiter, role) = row?;
            let role = Role::from_name(&role)
                .ok_or_else(|| Error::Corrupt(format!("unknown mailbox role {role:?}")))?;
            Ok(Mailbox {
                name,
                delimiter,
                role,
            })
        })
        .collect()
    }

    /// Caches `messages` fetched from `mailbox` under `uid_validity`, and files the mailbox under
    /// `role`. For the inbox, Sent and the other roles but Drafts and [`Role::Other`], another
    /// mailbox the account had with that role, such as a renamed Sent folder, is dropped along
    /// with its mail. A server may mark several mailboxes Drafts.
    ///
    /// If `uid_validity` differs from what was cached, the old messages are dropped first: their UIDs
    /// no longer mean anything. Messages already cached are updated in place.
    ///
    /// # Errors
    ///
    /// Returns an error when the write fails. Nothing is written in that case.
    pub fn save_messages(
        &mut self,
        account: &str,
        mailbox: &str,
        role: Role,
        uid_validity: u32,
        messages: &[Envelope],
    ) -> Result<()> {
        let tx = immediate(&mut self.conn)?;
        if !matches!(role, Role::Other | Role::Drafts) {
            tx.execute(
                "DELETE FROM mailboxes WHERE account = ?1 AND role = ?2 AND name != ?3",
                [account, role.as_str(), mailbox],
            )?;
        }
        let cached: Option<u32> = tx
            .query_row(
                "SELECT uid_validity FROM mailboxes WHERE account = ?1 AND name = ?2",
                [account, mailbox],
                |row| row.get(0),
            )
            .optional()?;
        if cached != Some(uid_validity) {
            tx.execute(
                "DELETE FROM messages WHERE account = ?1 AND mailbox = ?2",
                [account, mailbox],
            )?;
            tx.execute(
                "DELETE FROM unseen WHERE account = ?1 AND mailbox = ?2",
                [account, mailbox],
            )?;
            // Its UIDs now name other messages.
            self.removed
                .retain(|(a, m, _)| !(a == account && m == mailbox));
        }
        tx.execute(
            "INSERT INTO mailboxes (account, name, uid_validity, role) VALUES (?1, ?2, ?3, ?4)
             ON CONFLICT(account, name) DO UPDATE SET
                floor = CASE WHEN uid_validity = excluded.uid_validity THEN floor END,
                counted = CASE WHEN uid_validity = excluded.uid_validity THEN counted ELSE 0 END,
                uid_validity = excluded.uid_validity, role = excluded.role",
            params![account, mailbox, uid_validity, role.as_str()],
        )?;
        let kept = kept(&self.removed, account, mailbox, messages);
        insert_messages(&tx, account, mailbox, &kept)?;
        tx.commit()?;
        Ok(())
    }

    /// Adds `messages`, fetched from further back than what's cached, to `mailbox`. Unlike
    /// [`Store::save_messages`] this never starts the mailbox over: if it's no longer cached under
    /// `uid_validity`, the messages are stale and nothing is saved. Returns whether they were saved.
    ///
    /// # Errors
    ///
    /// Returns an error when the write fails.
    pub fn save_older_messages(
        &mut self,
        account: &str,
        mailbox: &str,
        uid_validity: u32,
        messages: &[Envelope],
    ) -> Result<bool> {
        let tx = immediate(&mut self.conn)?;
        let cached: Option<u32> = tx
            .query_row(
                "SELECT uid_validity FROM mailboxes WHERE account = ?1 AND name = ?2",
                [account, mailbox],
                |row| row.get(0),
            )
            .optional()?;
        if cached != Some(uid_validity) {
            return Ok(false);
        }
        let kept = kept(&self.removed, account, mailbox, messages);
        insert_messages(&tx, account, mailbox, &kept)?;
        // They reach down from the floor without gaps, so it's lower now.
        if let Some(lowest) = kept.iter().map(|m| m.uid).min() {
            tx.execute(
                "UPDATE mailboxes SET floor = MIN(floor, ?3)
                 WHERE account = ?1 AND name = ?2 AND floor IS NOT NULL",
                params![account, mailbox, lowest],
            )?;
        }
        tx.commit()?;
        Ok(true)
    }

    /// The UID older mail is fetched below in `mailbox`: every message from it up is cached. `None`
    /// when nothing is.
    ///
    /// Unread messages below it can be cached too, as [`Store::save_unread`] found them.
    ///
    /// # Errors
    ///
    /// Returns an error when the read fails.
    pub fn oldest_uid(&self, account: &str, mailbox: &str) -> Result<Option<u32>> {
        Ok(self.conn.query_row(
            "SELECT COALESCE(
                (SELECT floor FROM mailboxes WHERE account = ?1 AND name = ?2),
                (SELECT MIN(uid) FROM messages WHERE account = ?1 AND mailbox = ?2))",
            [account, mailbox],
            |row| row.get(0),
        )?)
    }

    /// The UIDs cached in `mailbox` below `below`, ascending.
    ///
    /// # Errors
    ///
    /// Returns an error when the read fails.
    pub fn uids_below(&self, account: &str, mailbox: &str, below: u32) -> Result<Vec<u32>> {
        let mut stmt = self.conn.prepare(
            "SELECT uid FROM messages WHERE account = ?1 AND mailbox = ?2 AND uid < ?3
             ORDER BY uid",
        )?;
        let rows = stmt.query_map(params![account, mailbox, below], |row| row.get(0))?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    /// Saves what a search for unread mail below `upto` in `mailbox` found, under the mailbox's
    /// `uid_validity`, in a search that started at `asked`: the messages cached below `upto` are
    /// read unless in `unseen`, sorted ascending, and `messages`, unread ones from below `below`
    /// not cached, are added. The rest of `unseen` not cached is kept, for the mailbox's
    /// [unread count](Store::unread_counts). From then on, older mail is fetched below where it
    /// was fetched below before, and the mailbox's unread mail
    /// [was looked for](Store::unread_searched). What was [written](Store::written) since is
    /// newer, and wins.
    ///
    /// If the mailbox is no longer cached under `uid_validity`, nothing is saved. Returns how many
    /// messages changed or were added, and by how much the unread mail not cached changed, or
    /// `None` then.
    ///
    /// # Errors
    ///
    /// Returns an error when the write fails. Nothing is written in that case.
    pub fn save_unread(
        &mut self,
        account: &str,
        mailbox: &str,
        (uid_validity, asked): (u32, u64),
        (below, upto): (u32, u32),
        unseen: &[u32],
        messages: &[Envelope],
    ) -> Result<Option<usize>> {
        let cached = self.uids_below(account, mailbox, upto)?;
        let seen: Vec<(u32, bool)> = cached
            .iter()
            .map(|&uid| {
                let at = (uid_validity, uid);
                let (since, _) = self.written_since(account, mailbox, at, asked);
                (
                    uid,
                    since.unwrap_or_else(|| unseen.binary_search(&uid).is_err()),
                )
            })
            .collect();
        let mut messages = kept(&self.removed, account, mailbox, messages);
        self.with_written(account, mailbox, (uid_validity, asked), &mut messages);
        // Unread on the server and not cached, unless put away or marked read since. Only what
        // the server found counts: it says OK to marking unread a message that's gone.
        let mut uncached: Vec<u32> = unseen
            .iter()
            .copied()
            .filter(|&uid| {
                uid < upto
                    && cached.binary_search(&uid).is_err()
                    && !messages.iter().any(|m| m.uid == uid)
                    && !self
                        .removed
                        .contains(&(account.to_owned(), mailbox.to_owned(), uid))
                    && self
                        .written_since(account, mailbox, (uid_validity, uid), asked)
                        .0
                        != Some(true)
            })
            .collect();
        uncached.sort_unstable();
        uncached.dedup();
        let tx = immediate(&mut self.conn)?;
        let current: Option<u32> = tx
            .query_row(
                "SELECT uid_validity FROM mailboxes WHERE account = ?1 AND name = ?2",
                [account, mailbox],
                |row| row.get(0),
            )
            .optional()?;
        if current != Some(uid_validity) {
            return Ok(None);
        }
        tx.execute(
            "UPDATE mailboxes SET counted = 1 WHERE account = ?1 AND name = ?2",
            [account, mailbox],
        )?;
        // Before what's below is added: every message from the lowest UID cached up was. With
        // nothing cached (`below` at 0), there's no such UID yet.
        if below > 0 {
            tx.execute(
                "UPDATE mailboxes SET floor = COALESCE(floor,
                    (SELECT MIN(uid) FROM messages WHERE account = ?1 AND mailbox = ?2), ?3)
                 WHERE account = ?1 AND name = ?2",
                params![account, mailbox, below],
            )?;
        }
        let mut changed = 0;
        {
            let mut update = tx.prepare(
                "UPDATE messages SET seen = ?4
                 WHERE account = ?1 AND mailbox = ?2 AND uid = ?3 AND seen != ?4",
            )?;
            for (uid, seen) in seen {
                changed += update.execute(params![account, mailbox, uid, seen])?;
            }
        }
        let before = uncached_unread(&tx, account, mailbox)?;
        insert_messages(&tx, account, mailbox, &messages)?;
        // Only what was searched: a search that stopped short says nothing of what's above.
        tx.execute(
            "DELETE FROM unseen WHERE account = ?1 AND mailbox = ?2 AND uid < ?3",
            params![account, mailbox, upto],
        )?;
        {
            let mut insert = tx.prepare("INSERT OR IGNORE INTO unseen VALUES (?1, ?2, ?3)")?;
            for uid in &uncached {
                insert.execute(params![account, mailbox, uid])?;
            }
        }
        let after = uncached_unread(&tx, account, mailbox)?;
        tx.commit()?;
        Ok(Some(changed + messages.len() + before.abs_diff(after)))
    }

    /// How many of `mailbox`'s unread messages aren't cached, as a search on the server last
    /// found them, or as marked since.
    ///
    /// # Errors
    ///
    /// Returns an error when the read fails.
    pub fn uncached_unread(&self, account: &str, mailbox: &str) -> Result<usize> {
        uncached_unread(&self.conn, account, mailbox)
    }

    /// Whether unread mail older than what's cached of `mailbox` was looked for, or there's
    /// nothing cached to be older than, nor unread mail known of that waits to be cached.
    ///
    /// # Errors
    ///
    /// Returns an error when the read fails.
    pub fn unread_searched(&self, account: &str, mailbox: &str) -> Result<bool> {
        Ok(self.conn.query_row(
            "SELECT EXISTS (SELECT 1 FROM mailboxes WHERE account = ?1 AND name = ?2
                AND floor IS NOT NULL)
             OR NOT EXISTS (SELECT 1 FROM messages WHERE account = ?1 AND mailbox = ?2)
                AND NOT EXISTS (SELECT 1 FROM unseen WHERE account = ?1 AND mailbox = ?2)",
            [account, mailbox],
            |row| row.get(0),
        )?)
    }

    /// Marks a cached message read or unread. Returns whether it's cached.
    ///
    /// # Errors
    ///
    /// Returns an error when the write fails.
    pub fn set_seen(&self, account: &str, mailbox: &str, uid: u32, seen: bool) -> Result<bool> {
        // Unchecked, as in `set_settings`.
        let tx = self.conn.unchecked_transaction()?;
        let cached = tx.execute(
            "UPDATE messages SET seen = ?4 WHERE account = ?1 AND mailbox = ?2 AND uid = ?3",
            params![account, mailbox, uid, seen],
        )? > 0;
        // One not cached, as one a search on the server found, no longer counts once read.
        // Marked unread, it counts once a search finds it there: the server says OK to marking a
        // message that's gone.
        if seen {
            tx.execute(
                "DELETE FROM unseen WHERE account = ?1 AND mailbox = ?2 AND uid = ?3",
                params![account, mailbox, uid],
            )?;
        }
        tx.commit()?;
        Ok(cached)
    }

    /// Notes a read mark or star the server answered for message `uid`, under the mailbox's
    /// `uid_validity`: what a sync read before it is older, and doesn't undo it. Each keeps its
    /// own time.
    pub fn written(
        &self,
        account: &str,
        mailbox: &str,
        (uid_validity, uid): (u32, u32),
        written: Written,
    ) {
        let key = (account.to_owned(), mailbox.to_owned(), Some(uid));
        let mut all = self.written.borrow_mut();
        forget_old(&mut all);
        let marks = under(all.entry(key).or_default(), uid_validity);
        let newer = |old: Option<(bool, u64)>, new: Option<bool>| match (old, new) {
            (Some((_, at)), Some(_)) if at > written.at => old,
            (_, Some(value)) => Some((value, written.at)),
            (old, None) => old,
        };
        marks.seen = newer(marks.seen, written.seen);
        marks.flagged = newer(marks.flagged, written.flagged);
    }

    /// Notes that every message of a mailbox with a UID below `up_to` was marked read under its
    /// `uid_validity`, answered at `at`, as [`Store::written`].
    pub fn written_all_read(
        &self,
        account: &str,
        mailbox: &str,
        (uid_validity, up_to): (u32, u32),
        at: u64,
    ) {
        let key = (account.to_owned(), mailbox.to_owned(), None);
        let mut all = self.written.borrow_mut();
        forget_old(&mut all);
        let marks = under(all.entry(key).or_default(), uid_validity);
        if marks.read_below.is_none_or(|(_, old)| old < at) {
            marks.read_below = Some((up_to, at));
        }
    }

    /// Whether message `uid`, under the mailbox's `uid_validity`, is read, and starred, as
    /// written after a sync that started at `asked`: the newest of its own read mark and its
    /// mailbox's marking all read.
    fn written_since(
        &self,
        account: &str,
        mailbox: &str,
        (uid_validity, uid): (u32, u32),
        asked: u64,
    ) -> (Option<bool>, Option<bool>) {
        let all = self.written.borrow();
        let get = |uid| {
            all.get(&(account.to_owned(), mailbox.to_owned(), uid))
                .copied()
                .filter(|marks| marks.uid_validity == uid_validity)
                .unwrap_or_default()
        };
        let (own, mailbox_wide) = (get(Some(uid)), get(None));
        let all_read = mailbox_wide
            .read_below
            .filter(|&(up_to, _)| uid < up_to)
            .map(|(_, at)| (true, at));
        let seen = [own.seen, all_read]
            .into_iter()
            .flatten()
            .filter(|&(_, at)| at > asked)
            .max_by_key(|&(_, at)| at)
            .map(|(value, _)| value);
        let flagged = own
            .flagged
            .filter(|&(_, at)| at > asked)
            .map(|(value, _)| value);
        (seen, flagged)
    }

    /// Shows `messages`, fetched under the mailbox's `uid_validity` by a sync that started at
    /// `asked`, read and starred as written since under it, which is newer.
    pub fn with_written(
        &self,
        account: &str,
        mailbox: &str,
        (uid_validity, asked): (u32, u64),
        messages: &mut [Envelope],
    ) {
        for m in messages {
            let at = (uid_validity, m.uid);
            let (seen, flagged) = self.written_since(account, mailbox, at, asked);
            m.seen = seen.unwrap_or(m.seen);
            m.flagged = flagged.unwrap_or(m.flagged);
        }
    }

    /// Stars a cached message, or takes its star away. Returns whether it's cached.
    ///
    /// # Errors
    ///
    /// Returns an error when the write fails.
    pub fn set_flagged(
        &self,
        account: &str,
        mailbox: &str,
        uid: u32,
        flagged: bool,
    ) -> Result<bool> {
        Ok(self.conn.execute(
            "UPDATE messages SET flagged = ?4 WHERE account = ?1 AND mailbox = ?2 AND uid = ?3",
            params![account, mailbox, uid, flagged],
        )? > 0)
    }

    /// Marks cached messages read or unread and starred or not, each as `(uid, seen, flagged)`
    /// says, while the mailbox is cached under `uid_validity`: as the server said they were in a
    /// sync that started at `asked`. What was [written](Store::written) since is newer, and
    /// wins. Returns how many changed.
    ///
    /// # Errors
    ///
    /// Returns an error when the write fails. Nothing is written in that case.
    pub fn set_flags(
        &mut self,
        account: &str,
        mailbox: &str,
        uid_validity: u32,
        asked: u64,
        flags: &[(u32, bool, bool)],
    ) -> Result<usize> {
        // What was written since is newer than what it read.
        let since: HashMap<u32, (Option<bool>, Option<bool>)> = flags
            .iter()
            .map(|&(uid, ..)| {
                let at = (uid_validity, uid);
                (uid, self.written_since(account, mailbox, at, asked))
            })
            .collect();
        let tx = immediate(&mut self.conn)?;
        let cached: Option<u32> = tx
            .query_row(
                "SELECT uid_validity FROM mailboxes WHERE account = ?1 AND name = ?2",
                [account, mailbox],
                |row| row.get(0),
            )
            .optional()?;
        if cached != Some(uid_validity) {
            return Ok(0);
        }
        let mut changed = 0;
        {
            let mut update = tx.prepare(
                "UPDATE messages SET seen = ?4, flagged = ?5
                 WHERE account = ?1 AND mailbox = ?2 AND uid = ?3
                    AND (seen != ?4 OR flagged != ?5)",
            )?;
            for &(uid, seen, flagged) in flags {
                let (seen_since, flagged_since) = since.get(&uid).copied().unwrap_or_default();
                let seen = seen_since.unwrap_or(seen);
                let flagged = flagged_since.unwrap_or(flagged);
                changed += update.execute(params![account, mailbox, uid, seen, flagged])?;
            }
        }
        tx.commit()?;
        Ok(changed)
    }

    /// Marks every cached message in a mailbox with a UID below `up_to` read. Returns how many
    /// were unread.
    ///
    /// # Errors
    ///
    /// Returns an error when the write fails.
    pub fn set_all_seen(&self, account: &str, mailbox: &str, up_to: u32) -> Result<usize> {
        // Unchecked, as in `set_settings`.
        let tx = self.conn.unchecked_transaction()?;
        // Unread mail not cached is read too.
        tx.execute(
            "DELETE FROM unseen WHERE account = ?1 AND mailbox = ?2 AND uid < ?3",
            params![account, mailbox, up_to],
        )?;
        let changed = tx.execute(
            "UPDATE messages SET seen = 1
             WHERE account = ?1 AND mailbox = ?2 AND uid < ?3 AND NOT seen",
            params![account, mailbox, up_to],
        )?;
        tx.commit()?;
        Ok(changed)
    }

    /// Drops a cached message, such as one moved to another mailbox. Returns whether it was cached.
    ///
    /// # Errors
    ///
    /// Returns an error when the write fails.
    /// Mail fetched before then and saved after doesn't bring it back.
    pub fn remove_message(&mut self, account: &str, mailbox: &str, uid: u32) -> Result<bool> {
        let tx = self.conn.transaction()?;
        let removed = tx.execute(
            "DELETE FROM messages WHERE account = ?1 AND mailbox = ?2 AND uid = ?3",
            params![account, mailbox, uid],
        )? > 0;
        tx.execute(
            "DELETE FROM unseen WHERE account = ?1 AND mailbox = ?2 AND uid = ?3",
            params![account, mailbox, uid],
        )?;
        tx.commit()?;
        self.removed
            .insert((account.to_owned(), mailbox.to_owned(), uid));
        Ok(removed)
    }

    /// Notes that the message `uid` in `mailbox` was moved to the mailbox `to`, as
    /// [`Store::put_away_message`] does by its Message-ID, if it has one.
    ///
    /// # Errors
    ///
    /// Returns an error when the read or a write fails.
    pub fn put_away(&mut self, account: &str, mailbox: &str, uid: u32, to: &str) -> Result<()> {
        let id: Option<String> = self
            .conn
            .query_row(
                "SELECT message_id FROM messages WHERE account = ?1 AND mailbox = ?2 AND uid = ?3",
                params![account, mailbox, uid],
                |row| row.get(0),
            )
            .optional()?
            .flatten();
        id.map_or(Ok(()), |id| self.put_away_message(account, &id, to))
    }

    /// Notes that the message with the Message-ID `message_id` was moved to the mailbox `to`: if
    /// that isn't an archive, as listed, a copy an archive still lists, as Gmail's All Mail does,
    /// was put away there rather than archived, while `to` is listed. Moved to an archive, that's
    /// forgotten. What's noted holds for [`PUT_AWAY_HOLDS`], then goes.
    ///
    /// # Errors
    ///
    /// Returns an error when a write fails.
    pub fn put_away_message(&mut self, account: &str, message_id: &str, to: &str) -> Result<()> {
        let tx = immediate(&mut self.conn)?;
        tx.execute(
            "DELETE FROM put_away WHERE account = ?1 AND message_id = ?2",
            [account, message_id],
        )?;
        // As listed, so even before it's synced; as synced, if it isn't listed (yet).
        tx.execute(
            "INSERT INTO put_away (account, message_id, mailbox, role, noted)
             SELECT ?1, ?2, ?3, role, unixepoch() FROM (
                SELECT role FROM listed_mailboxes WHERE account = ?1 AND name = ?3
                UNION ALL
                SELECT role FROM mailboxes WHERE account = ?1 AND name = ?3
                LIMIT 1)
             WHERE role != 'archive'",
            [account, message_id, to],
        )?;
        tx.execute(
            "DELETE FROM put_away WHERE account = ?1 AND noted < unixepoch() - ?2",
            params![account, PUT_AWAY_HOLDS],
        )?;
        tx.commit()?;
        Ok(())
    }

    /// Caches a message's downloaded body, `kept` when that's because it was opened: then
    /// [`Store::trim_bodies`] leaves it, and it stays kept once downloaded ahead again. Returns
    /// whether it was cached: not when the message itself isn't, such as one moved away
    /// meanwhile.
    ///
    /// # Errors
    ///
    /// Returns an error when the write fails.
    pub fn save_body(
        &self,
        account: &str,
        mailbox: &str,
        uid: u32,
        body: &Body,
        kept: bool,
    ) -> Result<bool> {
        Ok(self.conn.execute(
            "INSERT INTO bodies
                (account, mailbox, uid, to_list, cc_list, reply_to, refs, text, html, attachments,
                 kept)
             SELECT ?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11
             WHERE EXISTS (SELECT 1 FROM messages WHERE account = ?1 AND mailbox = ?2 AND uid = ?3)
             ON CONFLICT(account, mailbox, uid) DO UPDATE SET
                to_list = excluded.to_list, cc_list = excluded.cc_list,
                reply_to = excluded.reply_to, refs = excluded.refs, text = excluded.text,
                html = excluded.html, attachments = excluded.attachments,
                kept = max(kept, excluded.kept)",
            params![
                account,
                mailbox,
                uid,
                encode_addresses(&body.to),
                encode_addresses(&body.cc),
                encode_addresses(&body.reply_to),
                body.references.join("\u{1e}"),
                body.text,
                body.html,
                body.attachments.join("\u{1e}"),
                kept,
            ],
        )? > 0)
    }

    /// Keeps a message's cached body as one opened, so [`Store::trim_bodies`] leaves it. Returns
    /// whether its body is cached.
    ///
    /// # Errors
    ///
    /// Returns an error when the write fails.
    pub fn keep_body(&self, account: &str, mailbox: &str, uid: u32) -> Result<bool> {
        Ok(self.conn.execute(
            "UPDATE bodies SET kept = 1 WHERE account = ?1 AND mailbox = ?2 AND uid = ?3",
            params![account, mailbox, uid],
        )? > 0)
    }

    /// Stops keeping the bodies of opened messages, of every account: all are dropped but those
    /// among the `newest` most recent of each account's mailbox `ahead`, whose newest are
    /// downloaded ahead, which stay as if downloaded ahead. Every body outside `ahead` goes,
    /// kept or not: only keeping opened messages caches those. Returns how many were dropped.
    ///
    /// # Errors
    ///
    /// Returns an error, having changed nothing, when the write fails.
    pub fn release_kept_bodies(&mut self, ahead: &str, newest: usize) -> Result<usize> {
        let newest = i64::try_from(newest).unwrap_or(i64::MAX);
        let tx = self.conn.transaction()?;
        let dropped = tx.execute(
            "DELETE FROM bodies WHERE mailbox <> ?1 OR (kept = 1 AND (account, uid) NOT IN (
                SELECT account, uid FROM (
                    SELECT account, uid, row_number() OVER (
                        PARTITION BY account ORDER BY date IS NULL, date DESC, uid DESC
                    ) AS position
                    FROM messages WHERE mailbox = ?1
                ) WHERE position <= ?2
             ))",
            params![ahead, newest],
        )?;
        tx.execute("UPDATE bodies SET kept = 0 WHERE kept = 1", [])?;
        tx.commit()?;
        Ok(dropped)
    }

    /// Drops every cached body, downloaded ahead or kept once opened, of every account. Returns
    /// how many were dropped.
    ///
    /// # Errors
    ///
    /// Returns an error when the write fails.
    pub fn clear_bodies(&self) -> Result<usize> {
        Ok(self.conn.execute("DELETE FROM bodies", [])?)
    }

    /// A message's cached body, or `None` when it wasn't downloaded ahead or kept.
    ///
    /// # Errors
    ///
    /// Returns an error when the read fails.
    pub fn body(&self, account: &str, mailbox: &str, uid: u32) -> Result<Option<Body>> {
        let list = |column: String| -> Vec<String> {
            column
                .split('\u{1e}')
                .filter(|s| !s.is_empty())
                .map(str::to_owned)
                .collect()
        };
        Ok(self
            .conn
            .query_row(
                "SELECT to_list, cc_list, reply_to, refs, text, html, attachments FROM bodies
                 WHERE account = ?1 AND mailbox = ?2 AND uid = ?3",
                params![account, mailbox, uid],
                |row| {
                    Ok(Body {
                        to: decode_addresses(&row.get::<_, String>(0)?),
                        cc: decode_addresses(&row.get::<_, String>(1)?),
                        reply_to: decode_addresses(&row.get::<_, String>(2)?),
                        references: list(row.get(3)?),
                        text: row.get(4)?,
                        html: row.get(5)?,
                        attachments: list(row.get(6)?),
                    })
                },
            )
            .optional()?)
    }

    /// The UIDs among the `newest` most recent messages in `mailbox` whose bodies aren't cached,
    /// newest first.
    ///
    /// # Errors
    ///
    /// Returns an error when the read fails.
    pub fn missing_bodies(&self, account: &str, mailbox: &str, newest: usize) -> Result<Vec<u32>> {
        let mut stmt = self.conn.prepare(
            "SELECT m.uid FROM (
                SELECT uid, date FROM messages WHERE account = ?1 AND mailbox = ?2
                ORDER BY date IS NULL, date DESC, uid DESC LIMIT ?3
             ) m
             WHERE NOT EXISTS (
                SELECT 1 FROM bodies b WHERE b.account = ?1 AND b.mailbox = ?2 AND b.uid = m.uid
             )
             ORDER BY m.date IS NULL, m.date DESC, m.uid DESC",
        )?;
        let newest = i64::try_from(newest).unwrap_or(i64::MAX);
        let rows = stmt.query_map(params![account, mailbox, newest], |row| row.get(0))?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    /// Drops the cached bodies in `mailbox` but those of its `newest` most recent messages and
    /// those kept once opened. Returns how many were dropped.
    ///
    /// # Errors
    ///
    /// Returns an error when the write fails.
    pub fn trim_bodies(&self, account: &str, mailbox: &str, newest: usize) -> Result<usize> {
        let newest = i64::try_from(newest).unwrap_or(i64::MAX);
        Ok(self.conn.execute(
            "DELETE FROM bodies WHERE account = ?1 AND mailbox = ?2 AND kept = 0 AND uid NOT IN (
                SELECT uid FROM messages WHERE account = ?1 AND mailbox = ?2
                ORDER BY date IS NULL, date DESC, uid DESC LIMIT ?3
             )",
            params![account, mailbox, newest],
        )?)
    }

    /// The `limit` most recent messages in every account's mailbox with this role, newest first.
    ///
    /// # Errors
    ///
    /// Returns an error when the read fails.
    pub fn recent_messages(&self, role: Role, limit: usize) -> Result<Vec<StoredMessage>> {
        self.newest(Scope::Role(role), false, limit)
    }

    /// Every cached message, with its mailbox's UIDVALIDITY and role (as [`Learnable::role`]
    /// says), and the first `text_chars` characters of its body's text, if it
    /// was downloaded: what sorting mail learns from.
    ///
    /// # Errors
    ///
    /// Returns an error when the read fails or a row is corrupt.
    pub fn learning_messages(&self, text_chars: usize) -> Result<Vec<Learnable>> {
        let mut stmt = self.conn.prepare(&format!(
            "SELECT {MESSAGE_COLUMNS},
                CASE WHEN b.role != 'archive' THEN b.role
                    WHEN EXISTS (
                        SELECT 1 FROM snoozes s
                        WHERE s.account = m.account AND s.message_id = m.message_id
                    ) THEN 'snoozed'
                    -- Put away elsewhere: the Trash or Junk says so; elsewhere, its copy there
                    -- does once it's cached, and this one says nothing meanwhile.
                    ELSE COALESCE((
                        SELECT CASE WHEN p.role IN ('trash', 'junk') THEN p.role
                            ELSE 'snoozed' END
                        FROM put_away p
                        WHERE p.account = m.account AND p.message_id = m.message_id
                            AND p.noted >= unixepoch() - {PUT_AWAY_HOLDS}
                            AND (EXISTS (
                                SELECT 1 FROM listed_mailboxes l
                                WHERE l.account = p.account AND l.name = p.mailbox
                            ) OR EXISTS (
                                SELECT 1 FROM mailboxes d
                                WHERE d.account = p.account AND d.name = p.mailbox
                            ))
                    ), b.role) END,
                substr(t.text, 1, ?1), b.uid_validity
             FROM messages m
             JOIN mailboxes b ON b.account = m.account AND b.name = m.mailbox
             LEFT JOIN bodies t
                ON t.account = m.account AND t.mailbox = m.mailbox AND t.uid = m.uid"
        ))?;
        let text_chars = i64::try_from(text_chars).unwrap_or(i64::MAX);
        let rows = stmt.query_map([text_chars], |row| {
            Ok((
                message_from_row(row)?,
                row.get::<_, String>(11)?,
                row.get::<_, Option<String>>(12)?,
                row.get::<_, u32>(13)?,
            ))
        })?;
        rows.map(|row| {
            let (message, role, text, uid_validity) = row?;
            let role = Role::from_name(&role)
                .ok_or_else(|| Error::Corrupt(format!("unknown mailbox role {role:?}")))?;
            Ok(Learnable {
                message,
                uid_validity,
                role,
                text,
            })
        })
        .collect()
    }

    /// The `limit` most recent messages in one of the account's mailboxes, newest first.
    ///
    /// # Errors
    ///
    /// Returns an error when the read fails.
    pub fn mailbox_messages(
        &self,
        account: &str,
        mailbox: &str,
        limit: usize,
    ) -> Result<Vec<StoredMessage>> {
        let scope = Scope::Mailbox {
            account,
            name: mailbox,
        };
        self.newest(scope, false, limit)
    }

    /// One cached message, or `None` when it isn't cached.
    ///
    /// # Errors
    ///
    /// Returns an error when the read fails.
    pub fn message(&self, account: &str, mailbox: &str, uid: u32) -> Result<Option<StoredMessage>> {
        Ok(self
            .conn
            .query_row(
                &format!(
                    "SELECT {MESSAGE_COLUMNS} FROM messages m
                     WHERE m.account = ?1 AND m.mailbox = ?2 AND m.uid = ?3"
                ),
                params![account, mailbox, uid],
                message_from_row,
            )
            .optional()?)
    }

    /// Whether a cached message is read, or `None` when it isn't cached.
    ///
    /// # Errors
    ///
    /// Returns an error when the read fails.
    pub fn seen(&self, account: &str, mailbox: &str, uid: u32) -> Result<Option<bool>> {
        Ok(self
            .conn
            .query_row(
                "SELECT seen FROM messages WHERE account = ?1 AND mailbox = ?2 AND uid = ?3",
                params![account, mailbox, uid],
                |row| row.get(0),
            )
            .optional()?)
    }

    /// The `limit` most recent messages in `scope` for which `matches` holds, newest first.
    ///
    /// # Errors
    ///
    /// Returns an error when the read fails.
    pub fn matching_messages(
        &self,
        scope: Scope<'_>,
        mut matches: impl FnMut(&StoredMessage) -> bool,
        limit: usize,
    ) -> Result<Vec<StoredMessage>> {
        const ORDER: &str = "ORDER BY m.date IS NULL, m.date DESC, m.uid DESC";
        let (mut stmt, params) = match scope {
            Scope::Role(role) => (
                self.conn.prepare(&format!(
                    "SELECT {MESSAGE_COLUMNS} FROM messages m
                     JOIN mailboxes b ON b.account = m.account AND b.name = m.mailbox
                     WHERE b.role = ?1 {ORDER}"
                ))?,
                vec![role.as_str()],
            ),
            Scope::Mailbox { account, name } => (
                self.conn.prepare(&format!(
                    "SELECT {MESSAGE_COLUMNS} FROM messages m
                     WHERE m.account = ?1 AND m.mailbox = ?2 {ORDER}"
                ))?,
                vec![account, name],
            ),
        };
        let mut found = Vec::new();
        // Rows are read one by one, so a search stops as soon as it has enough.
        for message in stmt.query_map(rusqlite::params_from_iter(params), message_from_row)? {
            if found.len() >= limit {
                break;
            }
            let message = message?;
            if matches(&message) {
                found.push(message);
            }
        }
        Ok(found)
    }

    /// The `limit` most recent unread messages in `scope`, newest first.
    ///
    /// # Errors
    ///
    /// Returns an error when the read fails.
    pub fn unread_messages(&self, scope: Scope<'_>, limit: usize) -> Result<Vec<StoredMessage>> {
        self.newest(scope, true, limit)
    }

    /// The `limit` most recent messages in `scope`, or only its unread ones, newest first.
    fn newest(&self, scope: Scope<'_>, unread: bool, limit: usize) -> Result<Vec<StoredMessage>> {
        let unread = if unread { "AND NOT m.seen" } else { "" };
        let order = "ORDER BY m.date IS NULL, m.date DESC, m.uid DESC";
        let limit = i64::try_from(limit).unwrap_or(i64::MAX);
        let rows = match scope {
            Scope::Role(role) => self
                .conn
                .prepare(&format!(
                    "SELECT {MESSAGE_COLUMNS} FROM messages m
                     JOIN mailboxes b ON b.account = m.account AND b.name = m.mailbox
                     WHERE b.role = ?1 {unread} {order} LIMIT ?2"
                ))?
                .query_map(params![role.as_str(), limit], message_from_row)?
                .collect::<rusqlite::Result<_>>()?,
            Scope::Mailbox { account, name } => self
                .conn
                .prepare(&format!(
                    "SELECT {MESSAGE_COLUMNS} FROM messages m
                     WHERE m.account = ?1 AND m.mailbox = ?2 {unread} {order} LIMIT ?3"
                ))?
                .query_map(params![account, name, limit], message_from_row)?
                .collect::<rusqlite::Result<_>>()?,
        };
        Ok(rows)
    }

    /// The mailboxes whose unread mail was searched for on the server since they were last
    /// started over, by account id and mailbox name: their [unread count](Store::unread_counts)
    /// is known, even when it's none. The others' may only not be counted yet.
    ///
    /// # Errors
    ///
    /// Returns an error when the read fails.
    pub fn counted_mailboxes(&self) -> Result<HashSet<(String, String)>> {
        let mut stmt = self
            .conn
            .prepare("SELECT account, name FROM mailboxes WHERE counted")?;
        let rows = stmt.query_map([], |row| Ok((row.get(0)?, row.get(1)?)))?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    /// How many messages each mailbox has unread, by account id and mailbox name: those cached,
    /// and those the server last had unread that aren't. Mailboxes with none are left out.
    ///
    /// # Errors
    ///
    /// Returns an error when the read fails.
    pub fn unread_counts(&self) -> Result<HashMap<(String, String), usize>> {
        let mut stmt = self.conn.prepare(
            "SELECT account, mailbox, COUNT(*) FROM (
                SELECT account, mailbox FROM messages WHERE NOT seen
                UNION ALL
                SELECT u.account, u.mailbox FROM unseen u
                WHERE NOT EXISTS (SELECT 1 FROM messages m
                    WHERE m.account = u.account AND m.mailbox = u.mailbox AND m.uid = u.uid)
             )
             GROUP BY account, mailbox",
        )?;
        let rows = stmt.query_map([], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, i64>(2)?,
            ))
        })?;
        rows.map(|row| {
            let (account, mailbox, count) = row?;
            let count = usize::try_from(count).map_err(|e| Error::Corrupt(e.to_string()))?;
            Ok(((account, mailbox), count))
        })
        .collect()
    }

    /// The setting called `name`, if it was ever set.
    ///
    /// # Errors
    ///
    /// Returns an error when the read fails.
    pub fn setting(&self, name: &str) -> Result<Option<String>> {
        Ok(self
            .conn
            .query_row(
                "SELECT value FROM settings WHERE name = ?1",
                [name],
                |row| row.get(0),
            )
            .optional()?)
    }

    /// Sets the setting called `name` to `value`, replacing what it was.
    ///
    /// # Errors
    ///
    /// Returns an error when the write fails.
    pub fn set_setting(&self, name: &str, value: &str) -> Result<()> {
        self.set_settings(&[(name, value)])
    }

    /// Sets each setting, by name, to its value, all or nothing: when a write fails, none is
    /// changed.
    ///
    /// # Errors
    ///
    /// Returns an error when a write fails.
    pub fn set_settings(&self, settings: &[(&str, &str)]) -> Result<()> {
        // Unchecked: `&self` can't rule out a transaction already open, but none ever is, as
        // every other one is opened and committed within a single call.
        let tx = self.conn.unchecked_transaction()?;
        for (name, value) in settings {
            tx.execute(
                "INSERT INTO settings (name, value) VALUES (?1, ?2)
                 ON CONFLICT (name) DO UPDATE SET value = excluded.value",
                [name, value],
            )?;
        }
        tx.commit()?;
        Ok(())
    }

    /// Snoozes a message until `snooze.until`, replacing when it was due if it was snoozed already.
    ///
    /// # Errors
    ///
    /// Returns an error when the write fails, as when the account isn't saved.
    pub fn snooze(&self, snooze: &Snooze) -> Result<()> {
        self.conn.execute(
            "INSERT INTO snoozes (account, message_id, mailbox, until) VALUES (?1, ?2, ?3, ?4)
             ON CONFLICT (account, message_id) DO UPDATE SET
                mailbox = excluded.mailbox, until = excluded.until",
            params![
                snooze.account,
                snooze.message_id,
                snooze.mailbox,
                snooze.until
            ],
        )?;
        Ok(())
    }

    /// Forgets a snooze, but only if it's still due at `until`: one snoozed again meanwhile
    /// stays. Returns whether it was forgotten.
    ///
    /// # Errors
    ///
    /// Returns an error when the write fails.
    pub fn unsnooze(&self, account: &str, message_id: &str, until: i64) -> Result<bool> {
        Ok(self.conn.execute(
            "DELETE FROM snoozes WHERE account = ?1 AND message_id = ?2 AND until = ?3",
            params![account, message_id, until],
        )? > 0)
    }

    /// Every snooze due by `now`, in Unix seconds, soonest first.
    ///
    /// # Errors
    ///
    /// Returns an error when the read fails.
    pub fn due_snoozes(&self, now: i64) -> Result<Vec<Snooze>> {
        let mut stmt = self.conn.prepare(
            "SELECT account, message_id, mailbox, until FROM snoozes
             WHERE until <= ?1 ORDER BY until",
        )?;
        let rows = stmt.query_map([now], |row| {
            Ok(Snooze {
                account: row.get(0)?,
                message_id: row.get(1)?,
                mailbox: row.get(2)?,
                until: row.get(3)?,
            })
        })?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    /// Whether a message snoozed in `account`'s mailbox `mailbox` is still to be woken from there,
    /// due or not: by it or by any account on the same IMAP login, which reads the same mailbox.
    ///
    /// # Errors
    ///
    /// Returns an error when the read fails.
    pub fn has_snoozed_in(&self, account: &str, mailbox: &str) -> Result<bool> {
        Ok(self.conn.query_row(
            "SELECT EXISTS (
                SELECT 1 FROM snoozes s
                JOIN accounts other ON other.id = s.account
                JOIN accounts this ON this.id = ?1
                    AND lower(this.imap_host) = lower(other.imap_host)
                    AND this.imap_port = other.imap_port
                    AND this.imap_username = other.imap_username
                WHERE s.mailbox = ?2
            )",
            [account, mailbox],
            |row| row.get(0),
        )?)
    }

    /// Remembers that mail went to `addresses` at `at` (Unix seconds), as contacts to suggest.
    /// One already known is counted again, and keeps its name unless this one has one. An
    /// address without an email is skipped.
    ///
    /// # Errors
    ///
    /// Returns an error when the write fails. Nothing is written in that case.
    pub fn remember_contacts(&mut self, addresses: &[Address], at: i64) -> Result<()> {
        let tx = self.conn.transaction()?;
        {
            let mut insert = tx.prepare(
                "INSERT INTO contacts (email, name, sent, last_sent) VALUES (?1, ?2, 1, ?3)
                 ON CONFLICT(email) DO UPDATE SET
                    name = COALESCE(excluded.name, contacts.name),
                    sent = contacts.sent + 1,
                    last_sent = MAX(contacts.last_sent, excluded.last_sent)",
            )?;
            for address in addresses {
                let Some(email) = address
                    .email
                    .as_deref()
                    .map(str::trim)
                    .filter(|e| !e.is_empty())
                else {
                    continue;
                };
                let name = address
                    .name
                    .as_deref()
                    .map(str::trim)
                    .filter(|n| !n.is_empty());
                insert.execute(params![email, name, at])?;
            }
        }
        tx.commit()?;
        Ok(())
    }

    /// Up to `limit` of the contacts `matches` accepts, those written to most first, then most
    /// recently.
    ///
    /// # Errors
    ///
    /// Returns an error when the read fails.
    pub fn matching_contacts(
        &self,
        mut matches: impl FnMut(&Address) -> bool,
        limit: usize,
    ) -> Result<Vec<Address>> {
        let mut stmt = self.conn.prepare(
            "SELECT name, email FROM contacts ORDER BY sent DESC, last_sent DESC, email",
        )?;
        let mut found = Vec::new();
        for contact in stmt.query_map([], |row| {
            Ok(Address {
                name: row.get(0)?,
                email: Some(row.get(1)?),
            })
        })? {
            if found.len() >= limit {
                break;
            }
            let contact = contact?;
            if matches(&contact) {
                found.push(contact);
            }
        }
        Ok(found)
    }

    /// Forgets every contact, returning how many there were.
    ///
    /// # Errors
    ///
    /// Returns an error when the write fails.
    pub fn forget_contacts(&self) -> Result<usize> {
        Ok(self.conn.execute("DELETE FROM contacts", [])?)
    }
}

/// Adds `account` or replaces its settings, keeping its label. See [`Store::save_account`].
fn upsert_account(conn: &Connection, account: &Account) -> Result<()> {
    conn.execute(
        "INSERT INTO accounts (id, display_name, imap_host, imap_port, imap_security,
            imap_username, smtp_host, smtp_port, smtp_security, smtp_username, auth, label)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12)
         ON CONFLICT(id) DO UPDATE SET
            display_name = excluded.display_name,
            imap_host = excluded.imap_host, imap_port = excluded.imap_port,
            imap_security = excluded.imap_security, imap_username = excluded.imap_username,
            smtp_host = excluded.smtp_host, smtp_port = excluded.smtp_port,
            smtp_security = excluded.smtp_security, smtp_username = excluded.smtp_username,
            auth = excluded.auth",
        params![
            account.id,
            account.display_name,
            account.imap.host,
            account.imap.port,
            account.imap.security.as_str(),
            account.imap.username,
            account.smtp.host,
            account.smtp.port,
            account.smtp.security.as_str(),
            account.smtp.username,
            account.auth.as_str(),
            account.label,
        ],
    )?;
    Ok(())
}

/// Replaces the account's signature, or removes it with `None`. See [`Store::save_signature`].
fn write_signature(tx: &Connection, account: &str, signature: Option<&Signature>) -> Result<()> {
    // Its images go with it.
    tx.execute("DELETE FROM signatures WHERE account = ?1", [account])?;
    if let Some(signature) = signature {
        tx.execute(
            "INSERT INTO signatures (account, html, source) VALUES (?1, ?2, ?3)",
            params![account, signature.html, signature.source],
        )?;
        let mut insert = tx.prepare(
            "INSERT INTO signature_images (account, position, cid, name, mime, data)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
        )?;
        for (position, image) in signature.images.iter().enumerate() {
            let position = i64::try_from(position).unwrap_or(i64::MAX);
            insert.execute(params![
                account,
                position,
                image.cid,
                image.name,
                image.mime,
                &*image.data
            ])?;
        }
    }
    Ok(())
}

fn message_from_row(row: &Row<'_>) -> rusqlite::Result<StoredMessage> {
    Ok(StoredMessage {
        account: row.get(0)?,
        mailbox: row.get(1)?,
        envelope: Envelope {
            uid: row.get(2)?,
            message_id: row.get(3)?,
            from: Address {
                name: row.get(4)?,
                email: row.get(5)?,
            },
            subject: row.get(6)?,
            date: row.get(7)?,
            seen: row.get(8)?,
            to: decode_addresses(
                row.get::<_, Option<String>>(9)?
                    .as_deref()
                    .unwrap_or_default(),
            ),
            flagged: row.get(10)?,
        },
    })
}

/// Addresses as one column: each as name, U+001F, email; separated by U+001E. Missing parts are
/// empty.
fn encode_addresses(addresses: &[Address]) -> String {
    addresses
        .iter()
        .map(|a| {
            format!(
                "{}\u{1f}{}",
                a.name.as_deref().unwrap_or_default(),
                a.email.as_deref().unwrap_or_default()
            )
        })
        .collect::<Vec<_>>()
        .join("\u{1e}")
}

fn decode_addresses(column: &str) -> Vec<Address> {
    let part = |s: &str| (!s.is_empty()).then(|| s.to_owned());
    column
        .split('\u{1e}')
        .filter(|a| !a.is_empty())
        .map(|a| {
            let (name, email) = a.split_once('\u{1f}').unwrap_or(("", a));
            Address {
                name: part(name),
                email: part(email),
            }
        })
        .collect()
}

fn account_from_row(row: &Row<'_>) -> Result<Account> {
    let server = |prefix: &str| -> Result<Server> {
        let security: String = row.get(format!("{prefix}_security").as_str())?;
        Ok(Server {
            host: row.get(format!("{prefix}_host").as_str())?,
            port: row.get(format!("{prefix}_port").as_str())?,
            security: security
                .parse::<Security>()
                .map_err(|e| Error::Corrupt(e.to_string()))?,
            username: row.get(format!("{prefix}_username").as_str())?,
        })
    };
    let auth: String = row.get("auth")?;
    Ok(Account {
        id: row.get("id")?,
        display_name: row.get("display_name")?,
        label: row.get("label")?,
        imap: server("imap")?,
        smtp: server("smtp")?,
        auth: auth
            .parse::<Auth>()
            .map_err(|e| Error::Corrupt(e.to_string()))?,
    })
}

/// Inserts `messages` into `mailbox`, updating any already there.
/// `messages` from `mailbox`, without those [`Store::remove_message`] dropped.
fn kept(
    removed: &HashSet<(String, String, u32)>,
    account: &str,
    mailbox: &str,
    messages: &[Envelope],
) -> Vec<Envelope> {
    messages
        .iter()
        .filter(|e| !removed.contains(&(account.to_owned(), mailbox.to_owned(), e.uid)))
        .cloned()
        .collect()
}

/// How many of `mailbox`'s unread messages aren't cached, as [`Store::unread_counts`] counts them.
fn uncached_unread(conn: &Connection, account: &str, mailbox: &str) -> Result<usize> {
    let count: i64 = conn.query_row(
        "SELECT COUNT(*) FROM unseen u
         WHERE u.account = ?1 AND u.mailbox = ?2 AND NOT EXISTS (SELECT 1 FROM messages m
            WHERE m.account = u.account AND m.mailbox = u.mailbox AND m.uid = u.uid)",
        [account, mailbox],
        |row| row.get(0),
    )?;
    usize::try_from(count).map_err(|e| Error::Corrupt(e.to_string()))
}

fn insert_messages(
    tx: &Transaction<'_>,
    account: &str,
    mailbox: &str,
    messages: &[Envelope],
) -> Result<()> {
    let mut insert = tx.prepare(
        "INSERT INTO messages VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11)
         ON CONFLICT(account, mailbox, uid) DO UPDATE SET
            message_id = excluded.message_id, from_name = excluded.from_name,
            from_email = excluded.from_email, subject = excluded.subject,
            date = excluded.date, seen = excluded.seen, to_list = excluded.to_list,
            flagged = excluded.flagged",
    )?;
    for m in messages {
        insert.execute(params![
            account,
            mailbox,
            m.uid,
            m.message_id,
            m.from.name,
            m.from.email,
            m.subject,
            m.date,
            m.seen,
            encode_addresses(&m.to),
            m.flagged,
        ])?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    pub(crate) fn account(id: &str) -> Account {
        let server = |host: &str, port, security| Server {
            host: host.into(),
            port,
            security,
            username: id.into(),
        };
        Account {
            id: id.into(),
            display_name: Some("Me".into()),
            label: None,
            imap: server("imap.example.com", 993, Security::Tls),
            smtp: server("smtp.example.com", 587, Security::StartTls),
            auth: Auth::Password,
        }
    }

    /// An [`Store::unread_counts`] key.
    fn key(account: &str, mailbox: &str) -> (String, String) {
        (account.to_owned(), mailbox.to_owned())
    }

    fn envelope(uid: u32, date: i64) -> Envelope {
        Envelope {
            uid,
            message_id: Some(format!("{uid}@example.com")),
            from: Address {
                name: Some("Ada".into()),
                email: Some("ada@example.com".into()),
            },
            subject: Some(format!("message {uid}")),
            date: Some(date),
            seen: false,
            to: vec![Address {
                name: None,
                email: Some("me@example.com".into()),
            }],
            flagged: false,
        }
    }

    #[test]
    fn snoozes_come_due_in_order_and_go_with_their_account() {
        let store = Store::open_in_memory().unwrap();
        store.save_account(&account("me@example.com")).unwrap();
        let snooze = |id: &str, until| Snooze {
            account: "me@example.com".into(),
            message_id: id.into(),
            mailbox: "Snoozed".into(),
            until,
        };
        store.snooze(&snooze("b@x", 200)).unwrap();
        store.snooze(&snooze("a@x", 100)).unwrap();
        store.snooze(&snooze("c@x", 300)).unwrap();
        assert_eq!(store.due_snoozes(50).unwrap(), vec![]);
        assert_eq!(
            store.due_snoozes(200).unwrap(),
            vec![snooze("a@x", 100), snooze("b@x", 200)]
        );
        // Snoozed again, it's due later, and the earlier wake-up no longer forgets it.
        store.snooze(&snooze("a@x", 400)).unwrap();
        assert!(!store.unsnooze("me@example.com", "a@x", 100).unwrap());
        assert!(store.unsnooze("me@example.com", "b@x", 200).unwrap());
        assert_eq!(
            store.due_snoozes(1000).unwrap(),
            vec![snooze("c@x", 300), snooze("a@x", 400)]
        );
        assert!(
            store.snooze(&snooze_for("nobody@example.com")).is_err(),
            "a snooze needs its account"
        );
        store.remove_account("me@example.com").unwrap();
        assert_eq!(store.due_snoozes(1000).unwrap(), vec![]);
    }

    fn snooze_for(account: &str) -> Snooze {
        Snooze {
            account: account.into(),
            message_id: "z@x".into(),
            mailbox: "Snoozed".into(),
            until: 1,
        }
    }

    #[test]
    fn accounts_round_trip_and_update_in_place() {
        let store = Store::open_in_memory().unwrap();
        let mut a = account("me@example.com");
        store.save_account(&a).unwrap();
        assert_eq!(store.accounts().unwrap(), vec![a.clone()]);

        a.smtp.port = 465;
        a.smtp.security = Security::Tls;
        a.auth = Auth::Google;
        store.save_account(&a).unwrap();
        assert_eq!(store.account(&a.id).unwrap(), Some(a));
    }

    #[test]
    fn names_are_set_and_adding_again_keeps_the_label() {
        let store = Store::open_in_memory().unwrap();
        let mut a = account("me@example.com");
        store.save_account(&a).unwrap();
        assert!(
            store
                .set_names(&a.id, Some("Work"), Some("Ada Lovelace"))
                .unwrap()
        );
        let saved = store.account(&a.id).unwrap().unwrap();
        assert_eq!(saved.label.as_deref(), Some("Work"));
        assert_eq!(saved.display_name.as_deref(), Some("Ada Lovelace"));

        a.display_name = None;
        store.save_account(&a).unwrap();
        let saved = store.account(&a.id).unwrap().unwrap();
        assert_eq!(saved.label.as_deref(), Some("Work"), "the label stays");
        assert_eq!(saved.display_name, None, "the rest is replaced");
        assert!(
            !store.set_names("nobody@example.com", None, None).unwrap(),
            "no such account"
        );
    }

    #[test]
    fn one_message_is_read_by_its_uid() {
        let mut store = Store::open_in_memory().unwrap();
        let a = account("me@example.com");
        store.save_account(&a).unwrap();
        store
            .save_messages(
                &a.id,
                "INBOX",
                Role::Inbox,
                1,
                &[envelope(1, 10), envelope(2, 20)],
            )
            .unwrap();
        let found = store.message(&a.id, "INBOX", 1).unwrap().unwrap();
        assert_eq!(found.envelope, envelope(1, 10));
        assert_eq!(
            store.message(&a.id, "INBOX", 3).unwrap(),
            None,
            "not cached"
        );
    }

    fn image(cid: &str, data: &[u8]) -> InlineImage {
        InlineImage {
            cid: cid.into(),
            name: format!("{cid}.png"),
            mime: "image/png".into(),
            data: data.into(),
        }
    }

    #[test]
    fn saving_an_account_on_another_login_forgets_its_mailboxes() {
        let mut store = Store::open_in_memory().unwrap();
        let mine = account("me@example.com");
        store.save_account(&mine).unwrap();
        let work = Mailbox {
            name: "Work".into(),
            delimiter: None,
            role: Role::Other,
        };
        store.save_mailbox_list(&mine.id, &[work]).unwrap();
        store
            .save_messages(&mine.id, "Work", Role::Other, 7, &[envelope(1, 10)])
            .unwrap();
        // Saved again as it was, as when a password changes: it's all kept.
        store.save_account(&mine).unwrap();
        assert!(store.mailbox_state(&mine.id, "Work").unwrap().is_some());
        assert_eq!(store.mailboxes(&mine.id).unwrap().len(), 1);

        let mut moved = mine.clone();
        moved.imap.username = "someone-else".into();
        store.save_account(&moved).unwrap();
        assert!(store.mailbox_state(&mine.id, "Work").unwrap().is_none());
        assert_eq!(store.mailboxes(&mine.id).unwrap(), []);
        assert!(store.message(&mine.id, "Work", 1).unwrap().is_none());
    }

    #[test]
    fn replacing_an_account_keeps_its_mail_only_on_the_same_mailboxes() {
        let mut store = Store::open_in_memory().unwrap();
        let mine = account("me@example.com");
        store.save_account(&mine).unwrap();
        let cache = |store: &mut Store| {
            store
                .save_messages(&mine.id, "INBOX", Role::Inbox, 1, &[envelope(1, 10)])
                .unwrap();
            store
                .save_body(&mine.id, "INBOX", 1, &Body::default(), false)
                .unwrap();
            store
                .snooze(&Snooze {
                    account: mine.id.clone(),
                    message_id: "1@example.com".into(),
                    mailbox: "Snoozed".into(),
                    until: 5,
                })
                .unwrap();
        };
        cache(&mut store);

        let mut renamed = mine.clone();
        renamed.label = Some("Renamed".into());
        renamed.smtp.host = "smtp.elsewhere.example".into();
        renamed.imap.host = renamed.imap.host.to_uppercase();
        store.replace_account(&renamed, None).unwrap();
        assert!(
            store.message(&mine.id, "INBOX", 1).unwrap().is_some(),
            "same mailboxes"
        );
        assert!(store.body(&mine.id, "INBOX", 1).unwrap().is_some());
        assert_eq!(store.due_snoozes(5).unwrap().len(), 1, "and its snoozes");

        for change in [
            |a: &mut Account| a.imap.host = "imap.elsewhere.example".into(),
            |a: &mut Account| a.imap.port += 1,
            |a: &mut Account| a.imap.username = "someone-else".into(),
        ] {
            cache(&mut store);
            let mut moved = mine.clone();
            change(&mut moved);
            store.replace_account(&moved, None).unwrap();
            assert_eq!(
                store.message(&mine.id, "INBOX", 1).unwrap(),
                None,
                "{moved:?}"
            );
            assert_eq!(store.body(&mine.id, "INBOX", 1).unwrap(), None);
            assert_eq!(store.mailbox_state(&mine.id, "INBOX").unwrap(), None);
            assert_eq!(
                store.due_snoozes(5).unwrap(),
                vec![],
                "snoozed on the old server"
            );
            store.replace_account(&mine, None).unwrap();
        }
    }

    #[test]
    fn replacing_an_account_is_all_or_nothing() {
        let mut store = Store::open_in_memory().unwrap();
        let mut mine = account("me@example.com");
        mine.label = Some("Mine".into());
        store.save_account(&mine).unwrap();
        let old = Signature {
            html: "<p>old</p>".into(),
            source: false,
            images: Vec::new(),
        };
        store.save_signature(&mine.id, Some(&old)).unwrap();

        let mut theirs = account("me@example.com");
        theirs.label = Some("Theirs".into());
        theirs.display_name = None;
        theirs.imap.host = "imap.elsewhere.example".into();
        // Two images with one Content-ID: the second can't be written.
        let broken = Signature {
            html: "<p>new</p>".into(),
            source: false,
            images: vec![image("logo", &[1]), image("logo", &[2])],
        };
        assert!(store.replace_account(&theirs, Some(&broken)).is_err());
        assert_eq!(store.account(&mine.id).unwrap(), Some(mine.clone()));
        assert_eq!(store.signature(&mine.id).unwrap(), Some(old));

        store.replace_account(&theirs, None).unwrap();
        assert_eq!(store.account(&mine.id).unwrap(), Some(theirs), "names too");
        assert_eq!(store.signature(&mine.id).unwrap(), None);
    }

    #[test]
    fn signatures_keep_their_images_in_order_and_go_with_the_account() {
        let mut store = Store::open_in_memory().unwrap();
        store.save_account(&account("me@example.com")).unwrap();
        assert_eq!(store.signature("me@example.com").unwrap(), None);

        let signature = Signature {
            html: "<p><b>Ada</b></p>".into(),
            source: false,
            images: vec![image("logo", &[1, 2, 3]), image("badge", &[4])],
        };
        store
            .save_signature("me@example.com", Some(&signature))
            .unwrap();
        assert_eq!(
            store.signature("me@example.com").unwrap().as_ref(),
            Some(&signature)
        );

        let replaced = Signature {
            html: "<table><tr><td><img src=\"cid:badge\"></td></tr></table>".into(),
            source: true,
            images: vec![image("badge", &[4])],
        };
        store
            .save_signature("me@example.com", Some(&replaced))
            .unwrap();
        assert_eq!(store.signature("me@example.com").unwrap(), Some(replaced));

        assert!(
            store
                .save_signature("nobody@example.com", Some(&signature))
                .is_err(),
            "no such account"
        );
        store.remove_account("me@example.com").unwrap();
        store.save_account(&account("me@example.com")).unwrap();
        assert_eq!(
            store.signature("me@example.com").unwrap(),
            None,
            "removing the account removed it"
        );
        store
            .save_signature("me@example.com", Some(&signature))
            .unwrap();
        store.save_signature("me@example.com", None).unwrap();
        assert_eq!(store.signature("me@example.com").unwrap(), None);
    }

    #[test]
    fn mail_to_learn_from_comes_with_its_role_and_the_start_of_its_text() {
        let mut store = Store::open_in_memory().unwrap();
        let me = "me@example.com";
        store.save_account(&account(me)).unwrap();
        store
            .save_messages(me, "INBOX", Role::Inbox, 1, &[envelope(1, 10)])
            .unwrap();
        store
            .save_messages(me, "Receipts", Role::Other, 7, &[envelope(2, 20)])
            .unwrap();
        store
            .save_messages(me, "[Gmail]/All Mail", Role::Archive, 3, &[envelope(3, 30)])
            .unwrap();
        for snoozed in ["3@example.com", "1@example.com"] {
            store
                .snooze(&Snooze {
                    account: me.into(),
                    message_id: snoozed.into(),
                    mailbox: "Snoozed".into(),
                    until: 100,
                })
                .unwrap();
        }
        let body = Body {
            text: "Thanks for your order".into(),
            ..Body::default()
        };
        assert!(store.save_body(me, "INBOX", 1, &body, false).unwrap());
        let mut learnable = store.learning_messages(6).unwrap();
        learnable.sort_by_key(|l| l.message.envelope.uid);
        let got: Vec<_> = learnable
            .iter()
            .map(|l| {
                let m = l.message.mailbox.as_str();
                (m, l.uid_validity, l.role, l.text.as_deref())
            })
            .collect();
        assert_eq!(
            got,
            [
                ("INBOX", 1, Role::Inbox, Some("Thanks")),
                ("Receipts", 7, Role::Other, None),
                ("[Gmail]/All Mail", 3, Role::Snoozed, None),
            ],
            "each with its mailbox's UIDVALIDITY and role, snoozed in an archive, not the inbox it's \
             still in, and the text downloaded, cut"
        );
    }

    const ME: &str = "me@example.com";

    /// A Gmail account's store, its Trash and a Receipts label listed but not synced, as `list`
    /// says, with the inbox's message 1 also in All Mail under UID 9.
    fn gmail(list: &[Mailbox]) -> Store {
        let mut store = Store::open_in_memory().unwrap();
        store.save_account(&account(ME)).unwrap();
        store
            .save_messages(ME, "INBOX", Role::Inbox, 1, &[envelope(1, 10)])
            .unwrap();
        store.save_mailbox_list(ME, list).unwrap();
        let mut copy = envelope(9, 10);
        copy.message_id = Some("1@example.com".into());
        store
            .save_messages(ME, "[Gmail]/All Mail", Role::Archive, 1, &[copy])
            .unwrap();
        store
    }

    fn gmail_list() -> [Mailbox; 4] {
        let listed = |name: &str, role| Mailbox {
            name: name.into(),
            delimiter: Some("/".into()),
            role,
        };
        [
            listed("INBOX", Role::Inbox),
            listed("[Gmail]/All Mail", Role::Archive),
            listed("[Gmail]/Trash", Role::Trash),
            listed("Receipts", Role::Other),
        ]
    }

    /// How All Mail's copy with `uid` is learned from.
    fn all_mail(store: &Store, uid: u32) -> Option<Role> {
        store
            .learning_messages(0)
            .unwrap()
            .into_iter()
            .find(|l| l.message.mailbox == "[Gmail]/All Mail" && l.message.envelope.uid == uid)
            .map(|l| l.role)
    }

    #[test]
    fn mail_put_away_isnt_archived_where_all_mail_still_lists_it() {
        let mut store = gmail(&gmail_list());
        assert_eq!(all_mail(&store, 9), Some(Role::Archive));
        store.put_away(ME, "INBOX", 1, "[Gmail]/Trash").unwrap();
        assert!(store.remove_message(ME, "INBOX", 1).unwrap());
        assert_eq!(
            all_mail(&store, 9),
            Some(Role::Trash),
            "deleted, not archived"
        );
        store
            .put_away_message(ME, "1@example.com", "INBOX")
            .unwrap();
        assert_eq!(
            all_mail(&store, 9),
            Some(Role::Snoozed),
            "moved back to the inbox by undoing, which says so once its copy is cached"
        );
        store
            .put_away_message(ME, "1@example.com", "[Gmail]/Trash")
            .unwrap();
        assert_eq!(all_mail(&store, 9), Some(Role::Trash), "and deleted again");
        store
            .put_away_message(ME, "1@example.com", "Receipts")
            .unwrap();
        assert_eq!(
            all_mail(&store, 9),
            Some(Role::Snoozed),
            "filed in a folder, not archived: that copy says where once it's cached"
        );
        store
            .put_away_message(ME, "1@example.com", "[Gmail]/All Mail")
            .unwrap();
        assert_eq!(all_mail(&store, 9), Some(Role::Archive), "archived since");
    }

    #[test]
    fn mail_filed_in_a_folder_isnt_archived_while_the_folder_is_there() {
        let list = gmail_list();
        let mut store = gmail(&list);
        store
            .put_away_message(ME, "1@example.com", "Receipts")
            .unwrap();
        // A message just in, filed before its other copies are cached, then another: what was
        // noted of the first is kept all the same, for when they are.
        store
            .save_messages(
                ME,
                "INBOX",
                Role::Inbox,
                1,
                &[envelope(3, 10), envelope(5, 10)],
            )
            .unwrap();
        for uid in [3, 5] {
            store.put_away(ME, "INBOX", uid, "Receipts").unwrap();
            assert!(store.remove_message(ME, "INBOX", uid).unwrap());
        }
        let mut later = envelope(4, 10);
        later.message_id = Some("3@example.com".into());
        store
            .save_messages(ME, "[Gmail]/All Mail", Role::Archive, 1, &[later])
            .unwrap();
        assert_eq!(all_mail(&store, 4), Some(Role::Snoozed), "both filed");
        assert_eq!(all_mail(&store, 9), Some(Role::Snoozed));
        // The folder gone from the server, its copies with it: All Mail's is archived now, and
        // stays so once a folder of the same name is made again.
        store.save_mailbox_list(ME, &list[..3]).unwrap();
        assert_eq!(
            all_mail(&store, 9),
            Some(Role::Archive),
            "the folder's gone"
        );
        store.save_mailbox_list(ME, &list).unwrap();
        assert_eq!(
            all_mail(&store, 9),
            Some(Role::Archive),
            "another folder now"
        );
    }

    #[test]
    fn what_was_noted_of_mail_put_away_holds_a_week() {
        let mut store = gmail(&gmail_list());
        store
            .put_away_message(ME, "1@example.com", "Receipts")
            .unwrap();
        assert_eq!(all_mail(&store, 9), Some(Role::Snoozed));
        // Moved since by another mail app, it may be archived: a week on, it's learned as such.
        store
            .conn
            .execute(
                "UPDATE put_away SET noted = noted - ?1 - 1",
                [PUT_AWAY_HOLDS],
            )
            .unwrap();
        assert_eq!(all_mail(&store, 9), Some(Role::Archive));
    }

    #[test]
    fn bodies_downloaded_ahead_round_trip_and_go_with_their_message() {
        let mut store = Store::open_in_memory().unwrap();
        let me = "me@example.com";
        store.save_account(&account(me)).unwrap();
        store
            .save_messages(me, "INBOX", Role::Inbox, 1, &[envelope(1, 10)])
            .unwrap();
        let body = Body {
            to: vec![Address {
                name: Some("Me".into()),
                email: Some("me@example.com".into()),
            }],
            cc: Vec::new(),
            reply_to: vec![Address {
                name: None,
                email: Some("list@example.com".into()),
            }],
            references: vec!["a@example.com".into(), "b@example.com".into()],
            text: "Hello".into(),
            html: Some("<p>Hello</p>".into()),
            attachments: vec!["report.pdf".into()],
        };
        assert_eq!(store.body(me, "INBOX", 1).unwrap(), None);
        assert!(store.save_body(me, "INBOX", 1, &body, false).unwrap());
        assert_eq!(store.body(me, "INBOX", 1).unwrap(), Some(body.clone()));
        assert!(
            !store.save_body(me, "INBOX", 2, &body, false).unwrap(),
            "not cached, so its body isn't either"
        );
        // Updated in place, the message keeps its body.
        store
            .save_messages(me, "INBOX", Role::Inbox, 1, &[envelope(1, 10)])
            .unwrap();
        assert!(store.body(me, "INBOX", 1).unwrap().is_some());
        store.remove_message(me, "INBOX", 1).unwrap();
        assert_eq!(store.body(me, "INBOX", 1).unwrap(), None);
    }

    #[test]
    fn only_the_newest_bodies_are_missing_or_kept() {
        let mut store = Store::open_in_memory().unwrap();
        let me = "me@example.com";
        store.save_account(&account(me)).unwrap();
        let messages: Vec<_> = (1..=4).map(|uid| envelope(uid, i64::from(uid))).collect();
        store
            .save_messages(me, "INBOX", Role::Inbox, 1, &messages)
            .unwrap();
        assert_eq!(store.missing_bodies(me, "INBOX", 2).unwrap(), [4, 3]);
        for uid in [1, 3] {
            store
                .save_body(me, "INBOX", uid, &Body::default(), false)
                .unwrap();
        }
        assert_eq!(store.missing_bodies(me, "INBOX", 3).unwrap(), [4, 2]);
        assert_eq!(store.trim_bodies(me, "INBOX", 3).unwrap(), 1, "UID 1");
        assert!(store.body(me, "INBOX", 3).unwrap().is_some());
        assert_eq!(store.trim_bodies(me, "INBOX", 0).unwrap(), 1);
        assert_eq!(
            store.missing_bodies(me, "INBOX", 0).unwrap(),
            Vec::<u32>::new()
        );
    }

    #[test]
    fn opened_bodies_are_kept_until_released_or_cleared() {
        let mut store = Store::open_in_memory().unwrap();
        let me = "me@example.com";
        store.save_account(&account(me)).unwrap();
        let messages: Vec<_> = (1..=3).map(|uid| envelope(uid, i64::from(uid))).collect();
        store
            .save_messages(me, "INBOX", Role::Inbox, 1, &messages)
            .unwrap();
        store
            .save_messages(me, "Archive", Role::Archive, 1, &messages[..1])
            .unwrap();
        let cached = |store: &Store, mailbox, uid| store.body(me, mailbox, uid).unwrap().is_some();
        // Another account's inbox, whose newest are counted apart.
        let you = "you@example.com";
        store.save_account(&account(you)).unwrap();
        store
            .save_messages(you, "INBOX", Role::Inbox, 1, &messages[..1])
            .unwrap();
        store
            .save_body(you, "INBOX", 1, &Body::default(), true)
            .unwrap();

        store
            .save_body(me, "INBOX", 1, &Body::default(), true)
            .unwrap();
        store
            .save_body(me, "INBOX", 2, &Body::default(), false)
            .unwrap();
        assert!(
            store.keep_body(me, "INBOX", 2).unwrap(),
            "opened once downloaded ahead"
        );
        assert!(!store.keep_body(me, "INBOX", 3).unwrap(), "not cached");
        store
            .save_body(me, "INBOX", 3, &Body::default(), false)
            .unwrap();
        // Saved as not kept, outside the inbox: released all the same.
        store
            .save_body(me, "Archive", 1, &Body::default(), false)
            .unwrap();
        // Downloaded ahead again, it stays kept.
        store
            .save_body(me, "INBOX", 1, &Body::default(), false)
            .unwrap();
        assert_eq!(store.trim_bodies(me, "INBOX", 0).unwrap(), 1, "UID 3");
        assert!(cached(&store, "INBOX", 1) && cached(&store, "INBOX", 2));

        assert_eq!(
            store.release_kept_bodies("INBOX", 2).unwrap(),
            2,
            "the archived one, and UID 1, past the newest two"
        );
        assert!(!cached(&store, "Archive", 1) && !cached(&store, "INBOX", 1));
        assert!(cached(&store, "INBOX", 2), "among the newest");
        assert!(
            store.body(you, "INBOX", 1).unwrap().is_some(),
            "its own newest"
        );
        assert_eq!(
            store.trim_bodies(me, "INBOX", 1).unwrap(),
            1,
            "no longer kept, UID 2 goes as one downloaded ahead"
        );

        store
            .save_body(me, "Archive", 1, &Body::default(), true)
            .unwrap();
        assert_eq!(store.clear_bodies().unwrap(), 2, "and the other account's");
        assert!(!cached(&store, "Archive", 1));
    }

    #[test]
    fn a_version_4_database_keeps_its_bodies_as_downloaded_ahead() {
        let dir = std::env::temp_dir().join(format!("postden-store-v4-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("v4.db");
        let _ignored = std::fs::remove_file(&path);
        let me = "me@example.com";
        {
            let mut store = Store::open(&path).unwrap();
            store.save_account(&account(me)).unwrap();
            store
                .save_messages(me, "INBOX", Role::Inbox, 1, &[envelope(1, 1)])
                .unwrap();
            store
                .save_body(me, "INBOX", 1, &Body::default(), false)
                .unwrap();
            // As version 4 left it: bodies without `kept`.
            store
                .conn
                .execute_batch("ALTER TABLE bodies DROP COLUMN kept; PRAGMA user_version = 4;")
                .unwrap();
        }
        let store = Store::open(&path).unwrap();
        assert!(
            store.body(me, "INBOX", 1).unwrap().is_some(),
            "kept through"
        );
        assert_eq!(
            store.trim_bodies(me, "INBOX", 0).unwrap(),
            1,
            "as one downloaded ahead"
        );
        drop(store);
        Store::open(&path).unwrap();
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn a_moved_message_leaves_the_cache_and_the_unread_count() {
        let mut store = Store::open_in_memory().unwrap();
        store.save_account(&account("me@example.com")).unwrap();
        store
            .save_messages(
                "me@example.com",
                "INBOX",
                Role::Inbox,
                1,
                &[envelope(1, 10), envelope(2, 20)],
            )
            .unwrap();
        assert!(store.remove_message("me@example.com", "INBOX", 2).unwrap());
        assert!(
            !store.remove_message("me@example.com", "INBOX", 2).unwrap(),
            "already gone"
        );
        let left: Vec<u32> = store
            .recent_messages(Role::Inbox, 10)
            .unwrap()
            .iter()
            .map(|m| m.envelope.uid)
            .collect();
        assert_eq!(left, [1]);
        assert_eq!(
            store
                .unread_counts()
                .unwrap()
                .get(&key("me@example.com", "INBOX")),
            Some(&1)
        );

        // Fetched before it was moved, saved after: it stays gone.
        store
            .save_messages(
                "me@example.com",
                "INBOX",
                Role::Inbox,
                1,
                &[envelope(2, 20)],
            )
            .unwrap();
        store
            .save_older_messages("me@example.com", "INBOX", 1, &[envelope(2, 20)])
            .unwrap();
        let cached = |store: &Store| store.recent_messages(Role::Inbox, 10).unwrap().len();
        assert_eq!(cached(&store), 1);
        // Started over, its UIDs name other messages, so UID 2 is a new one.
        store
            .save_messages(
                "me@example.com",
                "INBOX",
                Role::Inbox,
                2,
                &[envelope(2, 20)],
            )
            .unwrap();
        assert_eq!(cached(&store), 1);
        assert_eq!(
            store.recent_messages(Role::Inbox, 10).unwrap()[0]
                .envelope
                .uid,
            2
        );
    }

    #[test]
    fn removing_an_account_drops_its_mail() {
        let mut store = Store::open_in_memory().unwrap();
        store.save_account(&account("me@example.com")).unwrap();
        store
            .save_messages(
                "me@example.com",
                "INBOX",
                Role::Inbox,
                1,
                &[envelope(1, 10)],
            )
            .unwrap();

        assert!(
            store.remove_account("me@example.com").unwrap(),
            "it existed"
        );
        assert!(
            store.recent_messages(Role::Inbox, 10).unwrap().is_empty(),
            "mail is gone"
        );
        assert!(
            !store.remove_account("me@example.com").unwrap(),
            "already gone"
        );
    }

    #[test]
    fn mailbox_state_tracks_the_highest_uid() {
        let mut store = Store::open_in_memory().unwrap();
        store.save_account(&account("me@example.com")).unwrap();
        assert_eq!(
            store.mailbox_state("me@example.com", "INBOX").unwrap(),
            None
        );

        store
            .save_messages("me@example.com", "INBOX", Role::Inbox, 7, &[])
            .unwrap();
        assert_eq!(
            store.mailbox_state("me@example.com", "INBOX").unwrap(),
            Some(MailboxState {
                uid_validity: 7,
                last_uid: None
            })
        );

        store
            .save_messages(
                "me@example.com",
                "INBOX",
                Role::Inbox,
                7,
                &[envelope(3, 10), envelope(9, 20)],
            )
            .unwrap();
        assert_eq!(
            store
                .mailbox_state("me@example.com", "INBOX")
                .unwrap()
                .unwrap()
                .last_uid,
            Some(9)
        );
    }

    #[test]
    fn a_new_uid_validity_discards_stale_messages() {
        let mut store = Store::open_in_memory().unwrap();
        store.save_account(&account("me@example.com")).unwrap();
        store
            .save_messages(
                "me@example.com",
                "INBOX",
                Role::Inbox,
                1,
                &[envelope(1, 10), envelope(2, 20)],
            )
            .unwrap();
        store
            .save_messages(
                "me@example.com",
                "INBOX",
                Role::Inbox,
                2,
                &[envelope(1, 30)],
            )
            .unwrap();

        let messages = store.recent_messages(Role::Inbox, 10).unwrap();
        assert_eq!(messages.len(), 1, "only the refetched message is left");
        assert_eq!(messages[0].envelope.date, Some(30));
    }

    #[test]
    fn messages_can_be_marked_read_and_unread() {
        let mut store = Store::open_in_memory().unwrap();
        store.save_account(&account("me@example.com")).unwrap();
        store
            .save_messages(
                "me@example.com",
                "INBOX",
                Role::Inbox,
                1,
                &[envelope(1, 10)],
            )
            .unwrap();
        let seen = |store: &Store| {
            store.recent_messages(Role::Inbox, 1).unwrap()[0]
                .envelope
                .seen
        };

        assert!(store.set_seen("me@example.com", "INBOX", 1, true).unwrap());
        assert!(seen(&store));
        assert!(store.set_seen("me@example.com", "INBOX", 1, false).unwrap());
        assert!(!seen(&store));
        assert!(
            !store.set_seen("me@example.com", "INBOX", 2, true).unwrap(),
            "not cached"
        );
        assert_eq!(
            store.seen("me@example.com", "INBOX", 1).unwrap(),
            Some(false)
        );
        assert_eq!(store.seen("me@example.com", "INBOX", 2).unwrap(), None);
    }

    #[test]
    fn messages_can_be_starred_and_stars_follow_the_server() {
        let mut store = Store::open_in_memory().unwrap();
        store.save_account(&account("me@example.com")).unwrap();
        let save = |store: &mut Store, flagged| {
            let message = Envelope {
                flagged,
                ..envelope(1, 10)
            };
            store
                .save_messages("me@example.com", "INBOX", Role::Inbox, 1, &[message])
                .unwrap();
        };
        let flagged = |store: &Store| {
            store.recent_messages(Role::Inbox, 1).unwrap()[0]
                .envelope
                .flagged
        };
        save(&mut store, false);
        assert!(!flagged(&store));
        assert!(
            store
                .set_flagged("me@example.com", "INBOX", 1, true)
                .unwrap()
        );
        assert!(flagged(&store));
        save(&mut store, false);
        assert!(!flagged(&store), "unstarred elsewhere");
        save(&mut store, true);
        assert!(flagged(&store), "starred elsewhere");
        assert!(
            !store
                .set_flagged("me@example.com", "INBOX", 2, true)
                .unwrap(),
            "not cached"
        );
        // As the server says when synced again: read and unstarred elsewhere.
        assert_eq!(
            store
                .set_flags(
                    "me@example.com",
                    "INBOX",
                    1,
                    0,
                    &[(1, true, false), (9, true, true)]
                )
                .unwrap(),
            1
        );
        assert!(!flagged(&store));
        assert_eq!(
            store.seen("me@example.com", "INBOX", 1).unwrap(),
            Some(true)
        );
        assert_eq!(
            store
                .set_flags("me@example.com", "INBOX", 1, 0, &[(1, true, false)])
                .unwrap(),
            0,
            "nothing changed"
        );
        assert_eq!(
            store
                .set_flags("me@example.com", "INBOX", 2, 0, &[(1, false, true)])
                .unwrap(),
            0,
            "another UIDVALIDITY's UIDs"
        );
        assert!(!flagged(&store));
    }

    #[test]
    fn a_whole_mailbox_is_marked_read_up_to_a_uid() {
        let mut store = Store::open_in_memory().unwrap();
        store.save_account(&account("me@example.com")).unwrap();
        let envelopes = [envelope(1, 10), envelope(2, 20), envelope(3, 30)];
        store
            .save_messages("me@example.com", "INBOX", Role::Inbox, 1, &envelopes)
            .unwrap();
        store.set_seen("me@example.com", "INBOX", 1, true).unwrap();
        let seen = |uid| store.seen("me@example.com", "INBOX", uid).unwrap();

        assert_eq!(
            store.set_all_seen("me@example.com", "INBOX", 3).unwrap(),
            1,
            "only the unread ones below the UID count"
        );
        assert_eq!(seen(1), Some(true));
        assert_eq!(seen(2), Some(true));
        assert_eq!(seen(3), Some(false), "arrived after it was marked");
        assert_eq!(
            store
                .unread_counts()
                .unwrap()
                .get(&key("me@example.com", "INBOX")),
            Some(&1)
        );
    }

    #[test]
    fn older_messages_join_the_cache_only_under_its_uid_validity() {
        let mut store = Store::open_in_memory().unwrap();
        store.save_account(&account("me@example.com")).unwrap();
        store
            .save_messages(
                "me@example.com",
                "INBOX",
                Role::Inbox,
                1,
                &[envelope(10, 100)],
            )
            .unwrap();
        assert_eq!(
            store.oldest_uid("me@example.com", "INBOX").unwrap(),
            Some(10)
        );

        assert!(
            !store
                .save_older_messages("me@example.com", "INBOX", 2, &[envelope(3, 30)])
                .unwrap(),
            "stale"
        );
        assert!(
            store
                .save_older_messages("me@example.com", "INBOX", 1, &[envelope(4, 40)])
                .unwrap()
        );
        assert_eq!(
            store.oldest_uid("me@example.com", "INBOX").unwrap(),
            Some(4)
        );
        assert_eq!(
            store
                .mailbox_state("me@example.com", "INBOX")
                .unwrap()
                .unwrap()
                .last_uid,
            Some(10),
            "new mail is still fetched from the newest"
        );
        assert_eq!(store.oldest_uid("me@example.com", "Archive").unwrap(), None);
    }

    #[test]
    fn unread_mail_below_the_cache_leaves_older_mail_fetched_from_where_it_was() {
        let (me, inbox) = ("me@example.com", "INBOX");
        let mut store = Store::open_in_memory().unwrap();
        store.save_account(&account(me)).unwrap();
        let read = |uid, date| Envelope {
            seen: true,
            ..envelope(uid, date)
        };
        store
            .save_messages(me, inbox, Role::Inbox, 1, &[read(10, 100), read(11, 110)])
            .unwrap();
        assert!(!store.unread_searched(me, inbox).unwrap());
        assert_eq!(store.unread_counts().unwrap().get(&key(me, inbox)), None);

        assert_eq!(
            store
                .save_unread(me, inbox, (2, 0), (10, 10), &[3], &[envelope(3, 30)])
                .unwrap(),
            None,
            "stale"
        );
        assert_eq!(
            store
                .save_unread(
                    me,
                    inbox,
                    (1, 0),
                    (10, 10),
                    &[3, 7],
                    &[envelope(3, 30), envelope(7, 70)]
                )
                .unwrap(),
            Some(2)
        );
        assert!(store.unread_searched(me, inbox).unwrap());
        assert_eq!(
            store.unread_counts().unwrap().get(&key(me, inbox)),
            Some(&2)
        );
        assert_eq!(
            store.oldest_uid(me, inbox).unwrap(),
            Some(10),
            "what's between is still fetched as older mail"
        );
        assert_eq!(store.uids_below(me, inbox, 10).unwrap(), vec![3, 7]);

        // 7 was read elsewhere, and 3 here since the search started.
        store.written(
            me,
            inbox,
            (1, 3),
            Written {
                at: 5,
                seen: Some(true),
                flagged: None,
            },
        );
        assert_eq!(
            store
                .save_unread(me, inbox, (1, 4), (10, 10), &[3], &[])
                .unwrap(),
            Some(2)
        );
        assert_eq!(store.seen(me, inbox, 7).unwrap(), Some(true));
        assert_eq!(store.seen(me, inbox, 3).unwrap(), Some(true), "read since");

        assert!(
            store
                .save_older_messages(me, inbox, 1, &[read(8, 80), read(9, 90)])
                .unwrap()
        );
        assert_eq!(
            store.oldest_uid(me, inbox).unwrap(),
            Some(8),
            "older mail lowers it"
        );

        store
            .save_messages(me, inbox, Role::Inbox, 2, &[read(20, 200)])
            .unwrap();
        assert!(!store.unread_searched(me, inbox).unwrap(), "started over");
        assert_eq!(store.oldest_uid(me, inbox).unwrap(), Some(20));
    }

    #[test]
    fn recent_messages_are_newest_first_across_accounts() {
        let mut store = Store::open_in_memory().unwrap();
        store.save_account(&account("a@example.com")).unwrap();
        store.save_account(&account("b@example.com")).unwrap();
        store
            .save_messages(
                "a@example.com",
                "INBOX",
                Role::Inbox,
                1,
                &[envelope(1, 10), envelope(2, 30)],
            )
            .unwrap();
        store
            .save_messages("b@example.com", "INBOX", Role::Inbox, 1, &[envelope(1, 20)])
            .unwrap();

        let order: Vec<_> = store
            .recent_messages(Role::Inbox, 2)
            .unwrap()
            .into_iter()
            .map(|m| (m.account, m.envelope.date))
            .collect();
        assert_eq!(
            order,
            vec![
                ("a@example.com".to_owned(), Some(30)),
                ("b@example.com".to_owned(), Some(20))
            ]
        );
    }

    #[test]
    fn mailboxes_are_listed_by_role_and_keep_their_recipients() {
        let mut store = Store::open_in_memory().unwrap();
        store.save_account(&account("me@example.com")).unwrap();
        store
            .save_messages(
                "me@example.com",
                "INBOX",
                Role::Inbox,
                1,
                &[envelope(1, 10)],
            )
            .unwrap();
        let mut sent = envelope(1, 20);
        sent.to = vec![
            Address {
                name: Some("Ada".into()),
                email: Some("ada@example.com".into()),
            },
            Address {
                name: None,
                email: Some("bob@example.com".into()),
            },
        ];
        sent.seen = true;
        store
            .save_messages("me@example.com", "Sent", Role::Sent, 4, &[sent.clone()])
            .unwrap();

        let inbox = store.recent_messages(Role::Inbox, 10).unwrap();
        assert_eq!(inbox.len(), 1);
        assert_eq!(inbox[0].mailbox, "INBOX");
        let listed = store.recent_messages(Role::Sent, 10).unwrap();
        assert_eq!(listed.len(), 1);
        assert_eq!(listed[0].envelope, sent, "recipients survive");
        assert_eq!(
            store
                .mailbox_for_role("me@example.com", Role::Sent)
                .unwrap(),
            Some((
                "Sent".to_owned(),
                MailboxState {
                    uid_validity: 4,
                    last_uid: Some(1)
                }
            ))
        );
        assert_eq!(
            store.unread_counts().unwrap(),
            HashMap::from([(key("me@example.com", "INBOX"), 1)]),
            "read mail isn't counted"
        );
    }

    #[test]
    fn unread_mail_not_cached_counts_until_cached_read_or_gone() {
        let mut store = Store::open_in_memory().unwrap();
        let (me, lists) = ("me@example.com", "Lists");
        store.save_account(&account(me)).unwrap();
        let mut read = envelope(20, 200);
        read.seen = true;
        store
            .save_messages(me, lists, Role::Other, 1, &[read, envelope(21, 210)])
            .unwrap();
        let count = |store: &Store| store.unread_counts().unwrap().get(&key(me, lists)).copied();
        // 1 to 5 are unread and older than what's cached, and only 5 is fetched; elsewhere, 20
        // was marked unread and 21 read: everything below 22 was searched.
        assert_eq!(
            store
                .save_unread(
                    me,
                    lists,
                    (1, 0),
                    (20, 22),
                    &[1, 2, 3, 4, 5, 20],
                    &[envelope(5, 50)]
                )
                .unwrap(),
            Some(2 + 1 + 4),
            "20 and 21 changed, 5 added, 1 to 4 counted"
        );
        assert_eq!(
            store.seen(me, lists, 20).unwrap(),
            Some(false),
            "above the floor too"
        );
        assert_eq!(store.seen(me, lists, 21).unwrap(), Some(true));
        assert_eq!(count(&store), Some(6), "1 to 4 not cached, 5 and 20 cached");

        // Fetched as older mail later, they count once.
        store
            .save_older_messages(me, lists, 1, &[envelope(4, 40)])
            .unwrap();
        assert_eq!(count(&store), Some(6));
        store.set_seen(me, lists, 4, true).unwrap();
        assert_eq!(count(&store), Some(5), "read here");

        assert!(
            !store.remove_message(me, lists, 3).unwrap(),
            "moved, though not cached"
        );
        assert_eq!(count(&store), Some(4));
        store.set_all_seen(me, lists, 22).unwrap();
        assert_eq!(count(&store), None, "all read");

        store
            .save_unread(me, lists, (1, 0), (5, 22), &[1, 2], &[])
            .unwrap();
        assert_eq!(count(&store), Some(2));
        store.save_messages(me, lists, Role::Other, 2, &[]).unwrap();
        assert_eq!(count(&store), None, "the mailbox started over");
    }

    #[test]
    fn unread_mail_not_cached_counts_what_searches_find_even_short() {
        let mut store = Store::open_in_memory().unwrap();
        let (me, lists) = ("me@example.com", "Lists");
        store.save_account(&account(me)).unwrap();
        store
            .save_messages(me, lists, Role::Other, 1, &[envelope(20, 200)])
            .unwrap();
        let count = |store: &Store| store.unread_counts().unwrap().get(&key(me, lists)).copied();
        store
            .save_unread(me, lists, (1, 0), (20, 21), &[1, 2, 20], &[])
            .unwrap();
        assert_eq!(count(&store), Some(3));

        // A server search's message, not cached, read and then unread again: it counts again
        // once a search finds it.
        assert!(!store.set_seen(me, lists, 1, true).unwrap());
        assert_eq!(count(&store), Some(2));
        assert!(!store.set_seen(me, lists, 1, false).unwrap());
        assert_eq!(count(&store), Some(2));
        store
            .save_unread(me, lists, (1, 0), (20, 21), &[1, 2, 20], &[])
            .unwrap();
        assert_eq!(count(&store), Some(3));

        // Marked unread after a search started that didn't find it: only what the server finds
        // counts, as marking a message that's gone is OK too; the next search finds it.
        store.written(
            me,
            lists,
            (1, 3),
            Written {
                at: 5,
                seen: Some(false),
                flagged: None,
            },
        );
        assert_eq!(
            store
                .save_unread(me, lists, (1, 4), (20, 21), &[1, 2, 20], &[])
                .unwrap(),
            Some(0)
        );
        assert_eq!(count(&store), Some(3), "1, 2 and 20");

        // Without UIDNEXT, a search stops at the floor: what's above it stays counted.
        store
            .save_unread(me, lists, (1, 4), (20, 20), &[2], &[])
            .unwrap();
        assert_eq!(count(&store), Some(2), "1 read elsewhere; 20 cached");
    }

    #[test]
    fn a_mailbox_with_nothing_cached_still_counts_without_a_floor() {
        let mut store = Store::open_in_memory().unwrap();
        let (me, lists) = ("me@example.com", "Lists");
        store.save_account(&account(me)).unwrap();
        store.save_messages(me, lists, Role::Other, 1, &[]).unwrap();
        assert_eq!(
            store
                .save_unread(me, lists, (1, 0), (0, 4), &[2, 3], &[])
                .unwrap(),
            Some(2)
        );
        assert_eq!(store.uncached_unread(me, lists).unwrap(), 2);
        assert_eq!(store.oldest_uid(me, lists).unwrap(), None, "no floor yet");
        assert!(
            !store.unread_searched(me, lists).unwrap(),
            "unread mail waits to be cached: a list of unread mail isn't empty yet"
        );
    }

    #[test]
    fn a_mailbox_is_counted_once_searched_until_it_starts_over() {
        let mut store = Store::open_in_memory().unwrap();
        let (me, lists) = ("me@example.com", "Lists");
        let counted = |store: &Store| store.counted_mailboxes().unwrap().contains(&key(me, lists));
        store.save_account(&account(me)).unwrap();
        store.save_messages(me, lists, Role::Other, 1, &[]).unwrap();
        assert!(!counted(&store), "cached, but never searched");
        // Under a UIDVALIDITY it no longer has: nothing's known of it.
        store
            .save_unread(me, lists, (2, 0), (0, 4), &[], &[])
            .unwrap();
        assert!(!counted(&store), "a stale search");
        store
            .save_unread(me, lists, (1, 0), (0, 4), &[], &[])
            .unwrap();
        assert!(counted(&store), "none unread, and that's known");
        assert_eq!(store.unread_counts().unwrap().get(&key(me, lists)), None);
        store.save_messages(me, lists, Role::Other, 1, &[]).unwrap();
        assert!(counted(&store), "synced again as it was");
        store.save_messages(me, lists, Role::Other, 2, &[]).unwrap();
        assert!(!counted(&store), "started over");
    }

    #[test]
    fn a_badge_that_changes_while_rows_dont_still_reports_a_change() {
        let mut store = Store::open_in_memory().unwrap();
        let (me, lists) = ("me@example.com", "Lists");
        store.save_account(&account(me)).unwrap();
        store
            .save_messages(me, lists, Role::Other, 1, &[envelope(20, 200)])
            .unwrap();
        store
            .save_unread(me, lists, (1, 0), (20, 21), &[5, 20], &[])
            .unwrap();
        // Cached since, as older mail, and read: its row no longer counts.
        let mut read = envelope(5, 50);
        read.seen = true;
        store.save_older_messages(me, lists, 1, &[read]).unwrap();
        let count = |store: &Store| {
            store
                .unread_counts()
                .unwrap()
                .get(&key(me, lists))
                .copied()
                .unwrap_or_default()
        };
        assert_eq!(count(&store), 1, "5 is cached read: only 20");
        // Another arrived unread below the floor, uncached: one row for another.
        assert_eq!(
            store
                .save_unread(me, lists, (1, 0), (20, 21), &[6, 20], &[])
                .unwrap(),
            Some(1),
            "the same row count, but 6 counts where 5 didn't"
        );
        assert_eq!(count(&store), 2);
    }

    #[test]
    fn every_mailbox_counts_its_unread_mail() {
        let mut store = Store::open_in_memory().unwrap();
        let me = "me@example.com";
        store.save_account(&account(me)).unwrap();
        let mut read = envelope(3, 30);
        read.seen = true;
        store
            .save_messages(me, "INBOX", Role::Inbox, 1, &[envelope(1, 10), read])
            .unwrap();
        store
            .save_messages(
                me,
                "Lists",
                Role::Other,
                2,
                &[envelope(1, 10), envelope(2, 20)],
            )
            .unwrap();
        store
            .save_messages(me, "Junk", Role::Junk, 3, &[envelope(1, 10)])
            .unwrap();
        assert_eq!(
            store.unread_counts().unwrap(),
            HashMap::from([
                (key(me, "INBOX"), 1),
                (key(me, "Lists"), 2),
                (key(me, "Junk"), 1),
            ])
        );
    }

    #[test]
    fn a_renamed_sent_mailbox_replaces_the_old_one() {
        let mut store = Store::open_in_memory().unwrap();
        store.save_account(&account("me@example.com")).unwrap();
        store
            .save_messages("me@example.com", "Sent", Role::Sent, 1, &[envelope(1, 10)])
            .unwrap();
        store
            .save_messages("me@example.com", "Sent Items", Role::Sent, 1, &[])
            .unwrap();
        assert_eq!(
            store.recent_messages(Role::Sent, 10).unwrap(),
            vec![],
            "old mail is gone"
        );
        assert_eq!(
            store
                .mailbox_for_role("me@example.com", Role::Sent)
                .unwrap()
                .map(|(name, _)| name),
            Some("Sent Items".to_owned())
        );
    }

    #[test]
    fn matching_messages_are_newest_first_within_their_scope() {
        let mut store = Store::open_in_memory().unwrap();
        store.save_account(&account("a@example.com")).unwrap();
        store.save_account(&account("b@example.com")).unwrap();
        let inbox = [envelope(1, 10), envelope(2, 30), envelope(3, 20)];
        store
            .save_messages("a@example.com", "INBOX", Role::Inbox, 1, &inbox)
            .unwrap();
        store
            .save_messages("b@example.com", "INBOX", Role::Inbox, 1, &[envelope(4, 40)])
            .unwrap();
        store
            .save_messages("a@example.com", "Sent", Role::Sent, 1, &[envelope(5, 50)])
            .unwrap();
        let not_2 = |m: &StoredMessage| m.envelope.uid != 2;
        let uids = |found: Vec<StoredMessage>| -> Vec<u32> {
            found.into_iter().map(|m| m.envelope.uid).collect()
        };

        let every_inbox = store
            .matching_messages(Scope::Role(Role::Inbox), not_2, 10)
            .unwrap();
        assert_eq!(uids(every_inbox), [4, 3, 1], "Sent is left out");
        let one = Scope::Mailbox {
            account: "a@example.com",
            name: "INBOX",
        };
        assert_eq!(
            uids(store.matching_messages(one, not_2, 10).unwrap()),
            [3, 1]
        );
        assert_eq!(
            uids(store.matching_messages(one, not_2, 1).unwrap()),
            [3],
            "only as many as asked for"
        );
        assert_eq!(
            uids(store.matching_messages(one, |_| false, 10).unwrap()),
            Vec::<u32>::new()
        );
    }

    #[test]
    fn unread_messages_are_the_newest_unread_within_their_scope() {
        let mut store = Store::open_in_memory().unwrap();
        store.save_account(&account("a@example.com")).unwrap();
        let read = Envelope {
            seen: true,
            ..envelope(2, 30)
        };
        let inbox = [envelope(1, 10), read, envelope(3, 20)];
        store
            .save_messages("a@example.com", "INBOX", Role::Inbox, 1, &inbox)
            .unwrap();
        store
            .save_messages("a@example.com", "Sent", Role::Sent, 1, &[envelope(4, 40)])
            .unwrap();
        let uids = |found: Vec<StoredMessage>| -> Vec<u32> {
            found.into_iter().map(|m| m.envelope.uid).collect()
        };
        let inboxes = Scope::Role(Role::Inbox);
        assert_eq!(uids(store.unread_messages(inboxes, 10).unwrap()), [3, 1]);
        assert_eq!(uids(store.unread_messages(inboxes, 1).unwrap()), [3]);
        let sent = Scope::Mailbox {
            account: "a@example.com",
            name: "Sent",
        };
        assert_eq!(uids(store.unread_messages(sent, 10).unwrap()), [4]);
    }

    #[test]
    fn settings_are_kept_and_replaced() {
        let store = Store::open_in_memory().unwrap();
        assert_eq!(store.setting("theme").unwrap(), None);
        store.set_setting("theme", "dark").unwrap();
        store.set_setting("theme", "light").unwrap();
        assert_eq!(store.setting("theme").unwrap().as_deref(), Some("light"));
    }

    #[test]
    fn settings_are_saved_all_or_nothing() {
        let store = Store::open_in_memory().unwrap();
        store
            .set_settings(&[("theme", "dark"), ("poll", "60")])
            .unwrap();
        // A setting the database refuses, as a full disk would.
        store
            .conn
            .execute_batch(
                "CREATE TRIGGER refuse BEFORE UPDATE ON settings WHEN NEW.name = 'poll'
                 BEGIN SELECT RAISE(ABORT, 'refused'); END;",
            )
            .unwrap();
        let refused = store.set_settings(&[("theme", "light"), ("poll", "300")]);
        assert!(refused.is_err());
        assert_eq!(
            store.setting("theme").unwrap().as_deref(),
            Some("dark"),
            "the first one isn't changed either"
        );
        store.set_setting("theme", "light").unwrap();
        assert_eq!(store.setting("theme").unwrap().as_deref(), Some("light"));
    }

    #[test]
    fn a_version_0_database_gains_the_new_columns() {
        let dir = std::env::temp_dir().join(format!("postden-store-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("old.db");
        let _ignored = std::fs::remove_file(&path);
        {
            let old = Connection::open(&path).unwrap();
            old.execute_batch(
                "CREATE TABLE accounts (
                    id TEXT PRIMARY KEY, display_name TEXT,
                    imap_host TEXT NOT NULL, imap_port INTEGER NOT NULL,
                    imap_security TEXT NOT NULL, imap_username TEXT NOT NULL,
                    smtp_host TEXT NOT NULL, smtp_port INTEGER NOT NULL,
                    smtp_security TEXT NOT NULL, smtp_username TEXT NOT NULL) STRICT;
                 CREATE TABLE mailboxes (
                    account TEXT NOT NULL REFERENCES accounts(id) ON DELETE CASCADE,
                    name TEXT NOT NULL, uid_validity INTEGER NOT NULL,
                    PRIMARY KEY (account, name)) STRICT;
                 CREATE TABLE messages (
                    account TEXT NOT NULL, mailbox TEXT NOT NULL, uid INTEGER NOT NULL,
                    message_id TEXT, from_name TEXT, from_email TEXT, subject TEXT,
                    date INTEGER, seen INTEGER NOT NULL,
                    PRIMARY KEY (account, mailbox, uid),
                    FOREIGN KEY (account, mailbox) REFERENCES mailboxes(account, name)
                        ON DELETE CASCADE) STRICT;
                 INSERT INTO accounts VALUES ('me@example.com', NULL, 'imap.example.com', 993,
                    'tls', 'me', 'smtp.example.com', 465, 'tls', 'me');
                 INSERT INTO mailboxes VALUES ('me@example.com', 'INBOX', 1);
                 INSERT INTO messages VALUES ('me@example.com', 'INBOX', 1, NULL, NULL,
                    'ada@example.com', 'hi', 10, 0);",
            )
            .unwrap();
        }
        let store = Store::open(&path).unwrap();
        let inbox = store.recent_messages(Role::Inbox, 10).unwrap();
        assert_eq!(inbox.len(), 1, "old mail is filed as the inbox");
        assert_eq!(inbox[0].envelope.to, vec![]);
        assert!(!inbox[0].envelope.flagged, "old mail isn't starred");
        let old = store.account("me@example.com").unwrap().unwrap();
        assert_eq!(
            old.auth,
            Auth::Password,
            "old accounts log in with a password"
        );
        assert_eq!(old.label, None, "and have no label");
        assert_eq!(store.signature("me@example.com").unwrap(), None);
        drop(store);
        Store::open(&path).unwrap();
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn a_mailbox_holding_snoozed_mail_says_so_until_it_wakes() {
        let me = "me@example.com";
        let store = Store::open_in_memory().unwrap();
        store.save_account(&account(me)).unwrap();
        assert!(!store.has_snoozed_in(me, "Projects/Snoozed").unwrap());
        let snooze = Snooze {
            account: me.into(),
            message_id: "a@example.com".into(),
            mailbox: "Projects/Snoozed".into(),
            until: 4_000_000_000,
        };
        store.snooze(&snooze).unwrap();
        // Not due for long, it's still there.
        assert!(store.has_snoozed_in(me, "Projects/Snoozed").unwrap());
        assert!(!store.has_snoozed_in(me, "Snoozed").unwrap());
        // Another account on the same login reads the same mailbox.
        let mut twin = account("twin@example.com");
        twin.imap = account(me).imap;
        twin.imap.host = twin.imap.host.to_uppercase();
        store.save_account(&twin).unwrap();
        assert!(
            store
                .has_snoozed_in("twin@example.com", "Projects/Snoozed")
                .unwrap()
        );
        store.unsnooze(me, "a@example.com", snooze.until).unwrap();
        assert!(!store.has_snoozed_in(me, "Projects/Snoozed").unwrap());
        assert!(
            !store
                .has_snoozed_in("twin@example.com", "Projects/Snoozed")
                .unwrap()
        );
    }

    #[test]
    fn drafts_moving_to_another_mailbox_keeps_the_first_and_its_mail() {
        let me = "me@example.com";
        let mut store = Store::open_in_memory().unwrap();
        store.save_account(&account(me)).unwrap();
        let mailbox = |name: &str, role| Mailbox {
            name: name.to_owned(),
            delimiter: Some("/".to_owned()),
            role,
        };
        // Cached before Drafts had a role of its own.
        store
            .save_messages(me, "Brouillons", Role::Other, 1, &[envelope(1, 10)])
            .unwrap();
        let listed = [
            mailbox("INBOX", Role::Inbox),
            mailbox("Brouillons", Role::Drafts),
        ];
        store.save_mailbox_list(me, &listed).unwrap();
        let drafts = |store: &Store| store.mailbox_for_role(me, Role::Drafts).unwrap();
        assert_eq!(
            drafts(&store).map(|(n, _)| n).as_deref(),
            Some("Brouillons")
        );

        // The server marks another: the first is a folder again, mail and all.
        let listed = [
            mailbox("INBOX", Role::Inbox),
            mailbox("Brouillons", Role::Other),
            mailbox("Drafts", Role::Drafts),
        ];
        store.save_mailbox_list(me, &listed).unwrap();
        store
            .save_messages(me, "Drafts", Role::Drafts, 1, &[envelope(1, 10)])
            .unwrap();
        assert_eq!(drafts(&store).map(|(n, _)| n).as_deref(), Some("Drafts"));
        assert_eq!(
            store.mailbox_messages(me, "Brouillons", 10).unwrap().len(),
            1
        );
    }

    #[test]
    fn listed_mailboxes_are_kept_and_unlisted_ones_dropped() {
        let mut store = Store::open_in_memory().unwrap();
        store.save_account(&account("me@example.com")).unwrap();
        let mailbox = |name: &str, role| Mailbox {
            name: name.to_owned(),
            delimiter: Some("/".to_owned()),
            role,
        };
        let listed = [
            mailbox("Receipts", Role::Other),
            mailbox("INBOX", Role::Inbox),
            mailbox("Archive", Role::Other),
            mailbox("Sent", Role::Sent),
        ];
        store.save_mailbox_list("me@example.com", &listed).unwrap();
        let names: Vec<String> = store
            .mailboxes("me@example.com")
            .unwrap()
            .into_iter()
            .map(|m| m.name)
            .collect();
        assert_eq!(names, ["INBOX", "Sent", "Archive", "Receipts"]);

        for name in ["Archive", "Receipts"] {
            store
                .save_messages("me@example.com", name, Role::Other, 1, &[envelope(1, 10)])
                .unwrap();
        }
        assert_eq!(
            store.mailbox_states("me@example.com").unwrap().len(),
            2,
            "two Other mailboxes coexist"
        );
        assert_eq!(
            store
                .mailbox_messages("me@example.com", "Archive", 10)
                .unwrap()
                .len(),
            1
        );

        store
            .save_mailbox_list("me@example.com", &listed[1..3])
            .unwrap();
        assert_eq!(
            store
                .mailbox_messages("me@example.com", "Receipts", 10)
                .unwrap(),
            vec![],
            "a mailbox gone from the server takes its mail with it"
        );
        assert_eq!(
            store
                .mailbox_messages("me@example.com", "Archive", 10)
                .unwrap()
                .len(),
            1
        );
    }

    #[test]
    fn a_sync_older_than_a_star_keeps_a_read_mark_it_saw_after() {
        let mut store = Store::open_in_memory().unwrap();
        store.save_account(&account("me@example.com")).unwrap();
        store
            .save_messages(
                "me@example.com",
                "INBOX",
                Role::Inbox,
                1,
                &[envelope(1, 10)],
            )
            .unwrap();
        let state = |store: &Store| {
            let m = &store.recent_messages(Role::Inbox, 1).unwrap()[0].envelope;
            (m.seen, m.flagged)
        };
        // Read here at 1; a sync starts at 1, after it was marked unread elsewhere; starred at 2.
        let written = |at, seen, flagged| Written { at, seen, flagged };
        store.written(
            "me@example.com",
            "INBOX",
            (1, 1),
            written(1, Some(true), None),
        );
        store.written(
            "me@example.com",
            "INBOX",
            (1, 1),
            written(2, None, Some(true)),
        );
        store
            .set_flags("me@example.com", "INBOX", 1, 1, &[(1, false, false)])
            .unwrap();
        assert_eq!(
            state(&store),
            (false, true),
            "unread as the sync saw, after the read mark; starred, after the sync"
        );
    }

    #[test]
    fn a_message_marked_unread_after_its_mailbox_was_all_read_stays_unread() {
        let mut store = Store::open_in_memory().unwrap();
        store.save_account(&account("me@example.com")).unwrap();
        let messages = [envelope(1, 10), envelope(2, 20), envelope(30, 30)];
        store
            .save_messages("me@example.com", "INBOX", Role::Inbox, 1, &messages)
            .unwrap();
        // A sync starts at 1; INBOX is marked all read below UID 10 at 2; UID 1 unread at 3.
        store.written_all_read("me@example.com", "INBOX", (1, 10), 2);
        let unread = Written {
            at: 3,
            seen: Some(false),
            flagged: None,
        };
        store.written("me@example.com", "INBOX", (1, 1), unread);
        store
            .set_flags(
                "me@example.com",
                "INBOX",
                1,
                1,
                &[(1, false, false), (2, false, false), (30, false, false)],
            )
            .unwrap();
        let seen = |uid| store.seen("me@example.com", "INBOX", uid).unwrap();
        assert_eq!(seen(1), Some(false), "marked unread last");
        assert_eq!(seen(2), Some(true), "marked all read since the sync");
        assert_eq!(seen(30), Some(false), "above where all was marked read");
    }

    #[test]
    fn contacts_are_counted_kept_named_and_forgotten() {
        let mut store = Store::open_in_memory().unwrap();
        let to = |name: Option<&str>, email: &str| Address {
            name: name.map(str::to_owned),
            email: Some(email.to_owned()),
        };
        store
            .remember_contacts(
                &[
                    to(Some("Ada"), "ada@example.com"),
                    to(None, "bob@example.com"),
                ],
                10,
            )
            .unwrap();
        store
            .remember_contacts(&[to(None, "BOB@example.com"), to(None, " ")], 20)
            .unwrap();
        store
            .remember_contacts(&[to(None, "ADA@example.com")], 5)
            .unwrap();
        store
            .remember_contacts(&[to(Some("Bob B"), "bob@example.com")], 30)
            .unwrap();
        let all = store.matching_contacts(|_| true, 10).unwrap();
        assert_eq!(
            all,
            vec![
                to(Some("Bob B"), "bob@example.com"),
                to(Some("Ada"), "ada@example.com")
            ],
            "one per address whatever its case, written to most first, the name kept unless a \
             new one is given"
        );
        let ada = store
            .matching_contacts(|c| c.name.as_deref() == Some("Ada"), 10)
            .unwrap();
        assert_eq!(ada.len(), 1);
        assert_eq!(store.matching_contacts(|_| true, 1).unwrap().len(), 1);
        assert_eq!(store.forget_contacts().unwrap(), 2);
        assert_eq!(store.matching_contacts(|_| true, 10).unwrap(), vec![]);
    }
}
