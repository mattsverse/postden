//! Apple Mail, the mail app that comes with macOS.
//!
//! Mail keeps its accounts with the Mac's other Internet Accounts, in the SQLite database
//! `Library/Accounts/Accounts4.sqlite` in the home directory. Each account is a `ZACCOUNT` row
//! whose kind is its `ZACCOUNTTYPE`'s identifier, such as `com.apple.account.IMAP`. Its settings
//! are `ZACCOUNTPROPERTY` rows, each value archived with `NSKeyedArchiver`. An account's SMTP
//! server is an account of its own, named by its `SendingAccountIdentifier`. A Gmail or iCloud
//! mailbox is an IMAP account under the Google or iCloud account it belongs to.
//!
//! None of this is documented, so it's read loosely: an account whose servers aren't as
//! expected is listed as unreadable, not an error.
//!
//! macOS keeps the directory private: Postden can only read it with Full Disk Access, and
//! says so with [`Error::NeedsFullDiskAccess`]. The passwords are in the keychain, filed in a
//! way that doesn't tie them to an account, so they aren't read.

use std::collections::HashMap;
use std::io::ErrorKind;
use std::path::{Path, PathBuf};
use std::time::Duration;

use plist::Value;
use postden_core::account::{Security, Server};
use rusqlite::{Connection, OpenFlags};

use super::{Found, Setup, Source, Unsupported, is_loopback, non_empty, once_each};
use crate::{Error, Result};

/// Where the accounts database is, in the home directory, on macOS.
const DATABASE: &str = "Library/Accounts/Accounts4.sqlite";

/// How long to wait for macOS to be done writing the database.
const BUSY: Duration = Duration::from_secs(5);

/// The kinds of accounts, as their `ZACCOUNTTYPE` identifiers.
mod kind {
    pub(super) const IMAP: &str = "com.apple.account.IMAP";
    /// A mailbox under an iCloud account, on some versions of macOS.
    pub(super) const IMAP_MAIL: &str = "com.apple.account.IMAPMail";
    pub(super) const POP: &str = "com.apple.account.POP";
    /// Exchange, and Outlook.com or Microsoft 365 added as one.
    pub(super) const EXCHANGE: &str = "com.apple.account.Exchange";
    /// A Google account, which has its Gmail mailbox as an IMAP account under it.
    pub(super) const GOOGLE: &str = "com.apple.account.Google";
    /// An iCloud account, which has its iCloud Mail mailbox under it.
    pub(super) const ICLOUD: &str = "com.apple.account.AppleAccount";
}

/// The `ZACCOUNTPROPERTY` keys Postden reads.
mod key {
    pub(super) const HOST: &str = "Hostname";
    pub(super) const PORT: &str = "PortNumber";
    pub(super) const TLS: &str = "SSLEnabled";
    /// Whether TLS starts with the connection, rather than with `STARTTLS`.
    pub(super) const TLS_DIRECT: &str = "SSLIsDirect";
    pub(super) const SMTP: &str = "SendingAccountIdentifier";
    /// The address it sends from.
    pub(super) const ADDRESS: &str = "IdentityEmailAddress";
    pub(super) const ADDRESSES: &str = "EmailAddresses";
    /// The name it sends under.
    pub(super) const NAME: &str = "IdentityDisplayName";
    pub(super) const FULL_NAME: &str = "FullUserName";
    /// An iCloud account's owner's name.
    pub(super) const ACCOUNT_NAME: &str = "ACPropertyFullName";
    /// An iCloud account's other addresses, its iCloud Mail one among them. Its login is the
    /// Apple ID, which may be an address elsewhere.
    pub(super) const ALIASES: &str = "appleIDAliases";
    /// Set on iCloud mailboxes, which store no servers, but not only on them.
    pub(super) const MAIL_DROP: &str = "UseMailDrop";

    pub(super) const ALL: [&str; 12] = [
        HOST,
        PORT,
        TLS,
        TLS_DIRECT,
        SMTP,
        ADDRESS,
        ADDRESSES,
        NAME,
        FULL_NAME,
        ACCOUNT_NAME,
        ALIASES,
        MAIL_DROP,
    ];
}

/// iCloud Mail's servers, which Mail doesn't store. They log in with the address.
mod icloud {
    /// The domains of iCloud Mail addresses.
    pub(super) const DOMAINS: [&str; 3] = ["icloud.com", "me.com", "mac.com"];
    pub(super) const IMAP: (&str, u16) = ("imap.mail.me.com", 993);
    pub(super) const SMTP: (&str, u16) = ("smtp.mail.me.com", 587);
}

/// The Mac's Internet Accounts database, `Accounts4.sqlite`, which Apple Mail's accounts are in.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AppleMail {
    database: PathBuf,
}

impl AppleMail {
    /// The accounts database in the home directory `home`, if it's there. It's found even when
    /// macOS keeps Postden from reading it, so the list can say what to allow.
    #[must_use]
    pub fn find(home: &Path) -> Option<Self> {
        let database = home.join(DATABASE);
        database.is_file().then_some(Self { database })
    }

    /// The accounts database at `database`.
    #[must_use]
    pub const fn at(database: PathBuf) -> Self {
        Self { database }
    }

    fn read(&self) -> rusqlite::Result<Store> {
        // Read-only, so the Mac's own copy is never changed.
        let db = Connection::open_with_flags(
            &self.database,
            OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX,
        )?;
        db.busy_timeout(BUSY)?;
        let kinds = db
            .prepare("SELECT Z_PK, ZIDENTIFIER FROM ZACCOUNTTYPE")?
            .query_map([], |row| Ok((row.get(0)?, row.get(1)?)))?
            .collect::<rusqlite::Result<HashMap<i64, Option<String>>>>()?;
        // Accounts are only under others on some versions of macOS.
        let parent = if has_column(&db, "ZACCOUNT", "ZPARENTACCOUNT")? {
            "ZPARENTACCOUNT"
        } else {
            "NULL"
        };
        let accounts = db
            .prepare(&format!(
                "SELECT Z_PK, ZACCOUNTTYPE, {parent}, ZIDENTIFIER, ZACCOUNTDESCRIPTION, \
                 ZUSERNAME FROM ZACCOUNT ORDER BY Z_PK"
            ))?
            .query_map([], |row| {
                let kind: Option<i64> = row.get(1)?;
                Ok(Account {
                    pk: row.get(0)?,
                    kind: kind.and_then(|k| kinds.get(&k).cloned().flatten()),
                    parent: row.get(2)?,
                    identifier: row.get(3)?,
                    description: row.get(4)?,
                    username: row.get(5)?,
                })
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        let keys = key::ALL.map(|k| format!("'{k}'")).join(", ");
        let mut properties = HashMap::new();
        let mut rows = db.prepare(&format!(
            "SELECT ZOWNER, ZKEY, ZVALUE FROM ZACCOUNTPROPERTY WHERE ZKEY IN ({keys})"
        ))?;
        let mut rows = rows.query([])?;
        while let Some(row) = rows.next()? {
            let (owner, key): (Option<i64>, String) = (row.get(0)?, row.get(1)?);
            // A value of the wrong kind is as good as none.
            let value = row.get::<_, Option<Vec<u8>>>(2).ok().flatten();
            if let Some(owner) = owner {
                properties.insert((owner, key), value.as_deref().and_then(unarchive));
            }
        }
        Ok(Store {
            accounts,
            properties,
        })
    }
}

impl Source for AppleMail {
    fn app(&self) -> &'static str {
        "Apple Mail"
    }

    fn accounts(&self) -> Result<Vec<Found>> {
        // SQLite's own error for this doesn't say what to do about it.
        if let Err(e) = std::fs::File::open(&self.database)
            && e.kind() == ErrorKind::PermissionDenied
        {
            return Err(Error::NeedsFullDiskAccess);
        }
        let store = self
            .read()
            .map_err(|e| Error::Invalid(format!("couldn't read the Mac's accounts: {e}")))?;
        Ok(once_each(
            store
                .accounts
                .iter()
                .filter_map(|a| store.found(a))
                .collect(),
        ))
    }
}

/// Whether `table` has the column `column`.
fn has_column(db: &Connection, table: &str, column: &str) -> rusqlite::Result<bool> {
    db.prepare("SELECT 1 FROM pragma_table_info(?1) WHERE name = ?2")?
        .exists([table, column])
}

/// A row of `ZACCOUNT`, with its kind's identifier.
#[derive(Debug)]
struct Account {
    pk: i64,
    kind: Option<String>,
    parent: Option<i64>,
    identifier: Option<String>,
    description: Option<String>,
    username: Option<String>,
}

/// Everything Postden reads of the database.
#[derive(Debug)]
struct Store {
    accounts: Vec<Account>,
    /// The properties Postden reads, by owner and key, unarchived. `None` for one that's there
    /// but couldn't be unarchived.
    properties: HashMap<(i64, String), Option<Value>>,
}

/// Who a mailbox is with, as far as it changes how it's set up.
#[derive(Debug, PartialEq, Eq)]
enum Provider {
    Google,
    ICloud,
    Other,
}

impl Store {
    /// The account `account` is, or `None` when it's not a mailbox, or has no address to file
    /// it under.
    fn found(&self, account: &Account) -> Option<Found> {
        let parent = account
            .parent
            .and_then(|pk| self.accounts.iter().find(|a| a.pk == pk));
        let (provider, setup) = match account.kind.as_deref()? {
            kind::IMAP | kind::IMAP_MAIL => (self.provider(account, parent), None),
            kind::POP => (Provider::Other, Some(Setup::Unsupported(Unsupported::Pop))),
            kind::EXCHANGE => (
                Provider::Other,
                Some(Setup::Unsupported(Unsupported::Microsoft)),
            ),
            _ => return None,
        };
        let address = self.address(account, parent, &provider)?;
        let setup = setup.unwrap_or_else(|| self.imap_setup(account, &provider, &address));
        let display_name = std::iter::once(account)
            .chain(parent)
            .flat_map(|a| [key::NAME, key::FULL_NAME, key::ACCOUNT_NAME].map(|k| (a.pk, k)))
            .find_map(|(pk, k)| self.text(pk, k));
        let label = non_empty(account.description.clone())
            .or_else(|| parent.and_then(|p| non_empty(p.description.clone())))
            .filter(|l| !l.eq_ignore_ascii_case(&address));
        Some(Found {
            address,
            display_name,
            label,
            setup,
        })
    }

    /// The address `account` sends from: its identity's, else the first it lists, else what
    /// it, or the account it's under, logs in as, when that's an address. An iCloud mailbox's
    /// is its iCloud Mail address, wherever that's found first, as the iCloud account it's
    /// under logs in with the Apple ID, which may be an address elsewhere, such as Gmail.
    fn address(
        &self,
        account: &Account,
        parent: Option<&Account>,
        provider: &Provider,
    ) -> Option<String> {
        let mut addresses = self
            .text(account.pk, key::ADDRESS)
            .into_iter()
            .chain(self.texts(account.pk, key::ADDRESSES))
            .chain(non_empty(account.username.clone()))
            .chain(parent.into_iter().flat_map(|p| {
                let aliases = if *provider == Provider::ICloud {
                    self.texts(p.pk, key::ALIASES)
                } else {
                    Vec::new()
                };
                aliases.into_iter().chain(non_empty(p.username.clone()))
            }))
            .filter(|s| s.contains('@'));
        if *provider == Provider::ICloud {
            let addresses: Vec<_> = addresses.collect();
            let on_icloud = addresses.iter().find(|a| {
                a.rsplit_once('@')
                    .is_some_and(|(_, d)| icloud::DOMAINS.iter().any(|i| d.eq_ignore_ascii_case(i)))
            });
            return on_icloud.or_else(|| addresses.first()).cloned();
        }
        addresses.next()
    }

    /// Who the IMAP account `account`, under `parent`, is with.
    fn provider(&self, account: &Account, parent: Option<&Account>) -> Provider {
        let host = self.text(account.pk, key::HOST);
        let host = host.as_deref().map(str::to_ascii_lowercase);
        let ends_with = |suffix: &[&str]| {
            host.as_deref().is_some_and(|h| {
                suffix
                    .iter()
                    .any(|s| h == *s || h.ends_with(&format!(".{s}")))
            })
        };
        let parent_kind = parent.and_then(|p| p.kind.as_deref());
        if parent_kind == Some(kind::GOOGLE) || ends_with(&["gmail.com", "googlemail.com"]) {
            Provider::Google
        } else if parent_kind == Some(kind::ICLOUD)
            || ends_with(&["mail.me.com"])
            // Any account can use Mail Drop, but only iCloud's stores no servers.
            || (host.is_none()
                && self
                    .properties
                    .contains_key(&(account.pk, key::MAIL_DROP.into())))
        {
            Provider::ICloud
        } else {
            Provider::Other
        }
    }

    /// How the IMAP account `account`, with `provider` and `address`, is set up.
    fn imap_setup(&self, account: &Account, provider: &Provider, address: &str) -> Setup {
        match provider {
            Provider::Google => Setup::Google,
            Provider::ICloud => {
                let server = |(host, port): (&str, u16), security| Server {
                    host: host.to_owned(),
                    port,
                    security,
                    username: address.to_owned(),
                };
                Setup::Password {
                    imap: server(icloud::IMAP, Security::Tls),
                    smtp: server(icloud::SMTP, Security::StartTls),
                }
            }
            Provider::Other => self.servers(account, address).map_or_else(
                |why| Setup::Unsupported(Unsupported::Unreadable(why)),
                |(imap, smtp)| Setup::Password { imap, smtp },
            ),
        }
    }

    /// The IMAP account `account`'s servers, and its SMTP account's: each logs in as its
    /// account does, else with `address`.
    fn servers(&self, account: &Account, address: &str) -> Result<(Server, Server), String> {
        let login = non_empty(account.username.clone()).unwrap_or_else(|| address.to_owned());
        let imap = self
            .server(account.pk, &login, Security::default_imap_port)
            .map_err(|why| format!("IMAP server: {why}"))?;
        let smtp = self
            .text(account.pk, key::SMTP)
            .and_then(|id| {
                self.accounts
                    .iter()
                    .find(|a| a.identifier.as_deref() == Some(id.as_str()))
            })
            .ok_or_else(|| "SMTP server: missing".to_owned())?;
        let smtp_login = non_empty(smtp.username.clone()).unwrap_or(login);
        let smtp = self
            .server(smtp.pk, &smtp_login, Security::default_smtp_port)
            .map_err(|why| format!("SMTP server: {why}"))?;
        Ok((imap, smtp))
    }

    /// The server account `owner` connects to, logging in as `login`. With TLS on, it starts
    /// with the connection or with `STARTTLS` as Mail says; when it doesn't say, the ports
    /// that start plain are upgraded with `STARTTLS`, and others use TLS from the start.
    fn server(
        &self,
        owner: i64,
        login: &str,
        default_port: fn(Security) -> u16,
    ) -> Result<Server, String> {
        let host = self.text(owner, key::HOST).ok_or("missing")?;
        let port = self
            .number(owner, key::PORT)
            .map(|p| u16::try_from(p).map_err(|_range| format!("{p} isn't a port")))
            .transpose()?
            // Mail stores 0 when it picks the port itself.
            .filter(|p| *p != 0);
        // Mail's default is to use TLS.
        let tls = self.flag(owner, key::TLS).unwrap_or(true);
        let security = match (tls, self.flag(owner, key::TLS_DIRECT), port) {
            (true, Some(false), _) | (true, None, Some(25 | 143 | 587)) => Security::StartTls,
            (true, ..) => Security::Tls,
            (false, ..) if is_loopback(&host) => Security::None,
            (false, ..) => return Err("Apple Mail connects to it without encryption".into()),
        };
        Ok(Server {
            port: port.unwrap_or_else(|| default_port(security)),
            host,
            security,
            username: login.to_owned(),
        })
    }

    fn property(&self, owner: i64, key: &str) -> Option<&Value> {
        self.properties.get(&(owner, key.to_owned()))?.as_ref()
    }

    fn text(&self, owner: i64, key: &str) -> Option<String> {
        non_empty(self.property(owner, key).and_then(text))
    }

    /// The strings in the property, as one or a list of them, which may be in lists of their
    /// own.
    fn texts(&self, owner: i64, key: &str) -> Vec<String> {
        fn collect(value: &Value, into: &mut Vec<String>) {
            match value {
                Value::Array(items) => items.iter().for_each(|item| collect(item, into)),
                other => into.extend(non_empty(text(other))),
            }
        }
        let mut texts = Vec::new();
        if let Some(value) = self.property(owner, key) {
            collect(value, &mut texts);
        }
        texts
    }

    fn number(&self, owner: i64, key: &str) -> Option<i64> {
        let value = self.property(owner, key)?;
        value.as_signed_integer().or_else(|| {
            value
                .as_unsigned_integer()
                .and_then(|n| i64::try_from(n).ok())
        })
    }

    fn flag(&self, owner: i64, key: &str) -> Option<bool> {
        match self.property(owner, key)? {
            Value::Boolean(b) => Some(*b),
            Value::Integer(n) => n.as_signed().map(|n| n != 0),
            _ => None,
        }
    }
}

fn text(value: &Value) -> Option<String> {
    value.as_string().map(str::to_owned)
}

/// The value `NSKeyedArchiver` archived in `bytes`, with its strings, numbers and arrays as
/// plain plist values. A plist that isn't an archive is taken as it is.
fn unarchive(bytes: &[u8]) -> Option<Value> {
    let plist = Value::from_reader(std::io::Cursor::new(bytes)).ok()?;
    let Some(archive) = plist.as_dictionary() else {
        return Some(plist);
    };
    let (Some(Value::Array(objects)), Some(Value::Dictionary(top))) =
        (archive.get("$objects"), archive.get("$top"))
    else {
        return Some(plist);
    };
    plain(objects, top.get("root")?, 0)
}

/// `value`, from the objects of an archive, as a plain plist value: references followed, and
/// `NSString`s, `NSMutableString`s and `NSArray`s unwrapped. `None` for `nil`, or what's
/// nested too deep to be a setting.
fn plain(objects: &[Value], value: &Value, depth: u8) -> Option<Value> {
    if depth > 8 {
        return None;
    }
    let value = match value {
        Value::Uid(uid) => objects.get(usize::try_from(uid.get()).ok()?)?,
        other => other,
    };
    match value {
        Value::String(s) if s == "$null" => None,
        Value::Dictionary(d) => match (d.get("NS.string"), d.get("NS.objects")) {
            (Some(s), _) => plain(objects, s, depth + 1),
            (None, Some(Value::Array(items))) => Some(Value::Array(
                items
                    .iter()
                    .filter_map(|item| plain(objects, item, depth + 1))
                    .collect(),
            )),
            // Some other kind of object, which no setting Postden reads is.
            _ => None,
        },
        other => Some(other.clone()),
    }
}

#[cfg(test)]
mod tests {
    use plist::{Dictionary, Uid};

    use super::*;

    /// A value to archive, as Mail does each property.
    enum Archived<'a> {
        /// An `NSString`, archived as a plain string.
        Str(&'a str),
        /// An `NSMutableString`, archived as an object holding the string.
        MutableStr(&'a str),
        Int(i64),
        Bool(bool),
        /// An `NSArray` of strings.
        Strs(&'a [&'a str]),
    }

    /// `value` archived with the layout `NSKeyedArchiver` gives it.
    fn archive(value: &Archived<'_>) -> Vec<u8> {
        let uid = |n| Value::Uid(Uid::new(n));
        let class = |name: &str, supers: &[&str]| {
            let mut c = Dictionary::new();
            c.insert("$classname".into(), name.into());
            let names = std::iter::once(name).chain(supers.iter().copied());
            c.insert(
                "$classes".into(),
                Value::Array(names.map(Value::from).collect()),
            );
            Value::Dictionary(c)
        };
        let mut objects = vec![Value::from("$null")];
        match value {
            Archived::Str(s) => objects.push((*s).into()),
            Archived::Int(n) => objects.push((*n).into()),
            Archived::Bool(b) => objects.push((*b).into()),
            Archived::MutableStr(s) => {
                let mut o = Dictionary::new();
                o.insert("$class".into(), uid(2));
                o.insert("NS.string".into(), (*s).into());
                objects.push(Value::Dictionary(o));
                objects.push(class("NSMutableString", &["NSString", "NSObject"]));
            }
            Archived::Strs(items) => {
                let refs = (2..).take(items.len()).map(uid).collect();
                let mut o = Dictionary::new();
                o.insert(
                    "$class".into(),
                    uid(2 + u64::try_from(items.len()).unwrap()),
                );
                o.insert("NS.objects".into(), Value::Array(refs));
                objects.push(Value::Dictionary(o));
                objects.extend(items.iter().map(|s| Value::from(*s)));
                objects.push(class("NSArray", &["NSObject"]));
            }
        }
        let mut top = Dictionary::new();
        top.insert("root".into(), uid(1));
        let mut archive = Dictionary::new();
        archive.insert("$version".into(), 100_000.into());
        archive.insert("$archiver".into(), "NSKeyedArchiver".into());
        archive.insert("$top".into(), Value::Dictionary(top));
        archive.insert("$objects".into(), Value::Array(objects));
        let mut bytes = Vec::new();
        Value::Dictionary(archive)
            .to_writer_binary(&mut bytes)
            .unwrap();
        bytes
    }

    /// An account to put in the database: `(Z_PK, kind, parent, ZIDENTIFIER, description,
    /// username, properties)`.
    type Row<'a> = (
        i64,
        &'a str,
        Option<i64>,
        &'a str,
        Option<&'a str>,
        Option<&'a str>,
        Vec<(&'a str, Archived<'a>)>,
    );

    /// An accounts database at `dir` with the tables Mail's accounts are in, and these rows.
    fn database(dir: &Path, rows: &[Row<'_>]) -> PathBuf {
        std::fs::create_dir_all(dir).unwrap();
        let path = dir.join("Accounts4.sqlite");
        drop(std::fs::remove_file(&path));
        let db = Connection::open(&path).unwrap();
        // The columns Postden reads, and some it doesn't, as Core Data names them.
        db.execute_batch(
            "CREATE TABLE ZACCOUNTTYPE ( Z_PK INTEGER PRIMARY KEY, Z_ENT INTEGER, \
             ZACCOUNTTYPEDESCRIPTION VARCHAR, ZIDENTIFIER VARCHAR ); \
             CREATE TABLE ZACCOUNT ( Z_PK INTEGER PRIMARY KEY, Z_ENT INTEGER, ZACTIVE INTEGER, \
             ZACCOUNTTYPE INTEGER, ZPARENTACCOUNT INTEGER, ZACCOUNTDESCRIPTION VARCHAR, \
             ZIDENTIFIER VARCHAR, ZUSERNAME VARCHAR ); \
             CREATE TABLE ZACCOUNTPROPERTY ( Z_PK INTEGER PRIMARY KEY, Z_ENT INTEGER, \
             ZOWNER INTEGER, ZKEY VARCHAR, ZVALUE BLOB );",
        )
        .unwrap();
        for (pk, kind, parent, id, description, username, properties) in rows {
            db.execute(
                "INSERT INTO ZACCOUNTTYPE (ZIDENTIFIER) SELECT ?1 WHERE NOT EXISTS \
                 (SELECT 1 FROM ZACCOUNTTYPE WHERE ZIDENTIFIER = ?1)",
                [kind],
            )
            .unwrap();
            db.execute(
                "INSERT INTO ZACCOUNT (Z_PK, ZACCOUNTTYPE, ZPARENTACCOUNT, ZIDENTIFIER, \
                 ZACCOUNTDESCRIPTION, ZUSERNAME) VALUES (?1, (SELECT Z_PK FROM ZACCOUNTTYPE \
                 WHERE ZIDENTIFIER = ?2), ?3, ?4, ?5, ?6)",
                rusqlite::params![pk, kind, parent, id, description, username],
            )
            .unwrap();
            for (key, value) in properties {
                db.execute(
                    "INSERT INTO ZACCOUNTPROPERTY (ZOWNER, ZKEY, ZVALUE) VALUES (?1, ?2, ?3)",
                    rusqlite::params![pk, key, archive(value)],
                )
                .unwrap();
            }
        }
        path
    }

    fn temp(test: &str) -> PathBuf {
        std::env::temp_dir().join(format!("postden-apple-mail-{test}-{}", std::process::id()))
    }

    fn accounts(test: &str, rows: &[Row<'_>]) -> Vec<Found> {
        let dir = temp(test);
        let found = AppleMail::at(database(&dir, rows)).accounts().unwrap();
        std::fs::remove_dir_all(&dir).unwrap();
        found
    }

    /// A server's properties: its host, port and whether it uses TLS.
    fn at(host: &str, port: i64, tls: bool) -> Vec<(&str, Archived<'_>)> {
        vec![
            (key::HOST, Archived::MutableStr(host)),
            (key::PORT, Archived::Int(port)),
            (key::TLS, Archived::Bool(tls)),
        ]
    }

    /// An IMAP account `pk` with `props`, sending through the SMTP account `pk + 1` with
    /// `smtp`. Both log in as `login`.
    fn imap_and_smtp<'a>(
        pk: i64,
        description: &'a str,
        login: &'a str,
        mut props: Vec<(&'a str, Archived<'a>)>,
        smtp: Vec<(&'a str, Archived<'a>)>,
    ) -> [Row<'a>; 2] {
        let smtp_id: &'a str = format!("SMTP-{pk}").leak();
        props.push((key::SMTP, Archived::Str(smtp_id)));
        [
            (
                pk,
                kind::IMAP,
                None,
                "IMAP",
                Some(description),
                Some(login),
                props,
            ),
            (
                pk + 1,
                "com.apple.account.SMTP",
                None,
                smtp_id,
                None,
                Some(login),
                smtp,
            ),
        ]
    }

    #[test]
    fn an_imap_account_is_read_with_both_its_servers_and_names() {
        let mut props = at("mail.example.com", 993, true);
        props.push((
            key::ADDRESSES,
            Archived::Strs(&["ada@example.com", "a@example.com"]),
        ));
        props.push((key::NAME, Archived::Str("Ada Lovelace")));
        let found = accounts(
            "imap",
            &imap_and_smtp(1, "Work", "ada", props, at("smtp.example.com", 587, true)),
        );
        let server = |host: &str, port, security| Server {
            host: host.into(),
            port,
            security,
            username: "ada".into(),
        };
        assert_eq!(
            found,
            [Found {
                address: "ada@example.com".into(),
                display_name: Some("Ada Lovelace".into()),
                label: Some("Work".into()),
                setup: Setup::Password {
                    imap: server("mail.example.com", 993, Security::Tls),
                    smtp: server("smtp.example.com", 587, Security::StartTls),
                },
            }]
        );
    }

    #[test]
    fn the_identity_gives_the_address_and_name_and_says_how_tls_starts() {
        let mut props = at("imap.example.com", 1143, true);
        props.push((key::TLS_DIRECT, Archived::Bool(false)));
        props.push((key::ADDRESS, Archived::MutableStr("ada@example.com")));
        props.push((key::NAME, Archived::Str("Ada")));
        props.push((key::FULL_NAME, Archived::Str("Ada Lovelace")));
        // Mail Drop is on for this one, which isn't iCloud's.
        props.push((key::MAIL_DROP, Archived::Bool(false)));
        let mut smtp = at("smtp.example.com", 0, true);
        smtp.push((key::TLS_DIRECT, Archived::Bool(false)));
        let found = accounts("identity", &imap_and_smtp(1, "Work", "ada", props, smtp));
        assert_eq!(
            found[0].address, "ada@example.com",
            "the login isn't an address"
        );
        assert_eq!(found[0].display_name.as_deref(), Some("Ada"));
        let Setup::Password { imap, smtp } = &found[0].setup else {
            panic!("read as {:?}", found[0].setup);
        };
        assert_eq!(
            (imap.host.as_str(), imap.port, imap.security),
            ("imap.example.com", 1143, Security::StartTls)
        );
        assert_eq!((smtp.port, smtp.security), (587, Security::StartTls));
    }

    #[test]
    fn the_address_is_the_login_when_none_is_listed() {
        let found = accounts(
            "login",
            &imap_and_smtp(
                1,
                "ada@example.com",
                "ada@example.com",
                at("imap.example.com", 143, true),
                at("smtp.example.com", 465, true),
            ),
        );
        assert_eq!(found[0].address, "ada@example.com");
        assert_eq!(found[0].label, None, "the description is just the address");
        let Setup::Password { imap, smtp } = &found[0].setup else {
            panic!("read as {:?}", found[0].setup);
        };
        assert_eq!((imap.port, imap.security), (143, Security::StartTls));
        assert_eq!((smtp.port, smtp.security), (465, Security::Tls));
    }

    #[test]
    fn a_gmail_mailbox_signs_in_with_google() {
        let found = accounts(
            "gmail",
            &[
                (
                    1,
                    kind::GOOGLE,
                    None,
                    "G",
                    Some("Google"),
                    Some("ada@gmail.com"),
                    vec![],
                ),
                (
                    2,
                    kind::IMAP,
                    Some(1),
                    "GI",
                    None,
                    None,
                    at("imap.gmail.com", 993, true),
                ),
                // Added by hand, with no Google account over it.
                (
                    3,
                    kind::IMAP,
                    None,
                    "HI",
                    None,
                    Some("bob@googlemail.com"),
                    at("IMAP.GMAIL.COM", 993, true),
                ),
            ],
        );
        let read: Vec<_> = found
            .iter()
            .map(|f| (f.address.as_str(), f.label.as_deref(), &f.setup))
            .collect();
        assert_eq!(
            read,
            [
                ("ada@gmail.com", Some("Google"), &Setup::Google),
                ("bob@googlemail.com", None, &Setup::Google),
            ],
            "the Google account itself isn't a mailbox"
        );
    }

    #[test]
    fn an_icloud_mailbox_gets_its_icloud_address_and_icloud_servers() {
        // As a Mac had it: the Apple ID is a Gmail address, and the iCloud Mail address is
        // only among its aliases. The mailbox has no address or login of its own.
        let found = accounts(
            "icloud",
            &[
                (
                    1,
                    kind::ICLOUD,
                    None,
                    "A",
                    Some("iCloud"),
                    Some("ada@gmail.com"),
                    vec![
                        (key::ALIASES, Archived::Strs(&["ada@icloud.com"])),
                        (key::ACCOUNT_NAME, Archived::Str("Ada Lovelace")),
                    ],
                ),
                (
                    2,
                    kind::IMAP,
                    Some(1),
                    "AI",
                    None,
                    None,
                    vec![
                        (key::MAIL_DROP, Archived::Bool(true)),
                        (key::PORT, Archived::Int(993)),
                        (key::TLS_DIRECT, Archived::Bool(true)),
                    ],
                ),
            ],
        );
        let Setup::Password { imap, smtp } = &found[0].setup else {
            panic!("read as {:?}", found[0].setup);
        };
        assert_eq!(found[0].address, "ada@icloud.com", "not the Apple ID");
        assert_eq!(found[0].display_name.as_deref(), Some("Ada Lovelace"));
        assert_eq!(
            (
                imap.host.as_str(),
                imap.port,
                imap.security,
                imap.username.as_str()
            ),
            ("imap.mail.me.com", 993, Security::Tls, "ada@icloud.com")
        );
        assert_eq!(
            (smtp.host.as_str(), smtp.port, smtp.security),
            ("smtp.mail.me.com", 587, Security::StartTls)
        );
    }

    #[test]
    fn pop_and_exchange_accounts_are_listed_as_unsupported_and_others_arent_listed() {
        let found = accounts(
            "kinds",
            &[
                (1, kind::POP, None, "P", None, Some("a@example.com"), vec![]),
                (
                    2,
                    kind::EXCHANGE,
                    None,
                    "E",
                    None,
                    Some("b@outlook.com"),
                    vec![],
                ),
                (
                    3,
                    "com.apple.account.CalDAV",
                    None,
                    "C",
                    None,
                    Some("c@example.com"),
                    vec![],
                ),
                (
                    4,
                    "com.apple.account.SMTP",
                    None,
                    "S",
                    None,
                    Some("d@example.com"),
                    vec![],
                ),
                (
                    5,
                    kind::IMAP,
                    None,
                    "N",
                    Some("No address"),
                    Some("ada"),
                    vec![],
                ),
            ],
        );
        let read: Vec<_> = found
            .iter()
            .map(|f| (f.address.as_str(), &f.setup))
            .collect();
        assert_eq!(
            read,
            [
                ("a@example.com", &Setup::Unsupported(Unsupported::Pop)),
                ("b@outlook.com", &Setup::Unsupported(Unsupported::Microsoft)),
            ]
        );
    }

    #[test]
    fn servers_that_cant_be_read_say_why() {
        let mut no_smtp = at("imap.example.com", 993, true);
        no_smtp.push((key::SMTP, Archived::Str("GONE")));
        let rows: Vec<Row<'_>> = [
            imap_and_smtp(
                1,
                "Plain",
                "a@example.com",
                at("imap.example.com", 143, false),
                vec![],
            ),
            imap_and_smtp(3, "No host", "b@example.com", vec![], vec![]),
            imap_and_smtp(
                5,
                "Port",
                "c@example.com",
                at("imap.example.com", 70_000, true),
                vec![],
            ),
        ]
        .into_iter()
        .flatten()
        .chain([(
            7,
            kind::IMAP,
            None,
            "I",
            None,
            Some("d@example.com"),
            no_smtp,
        )])
        .collect();
        let found = accounts("unreadable", &rows);
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
                "IMAP server: Apple Mail connects to it without encryption",
                "IMAP server: missing",
                "IMAP server: 70000 isn't a port",
                "SMTP server: missing",
            ]
        );
    }

    #[test]
    fn a_local_bridge_connects_without_encryption_and_a_missing_port_is_the_usual_one() {
        let mut bridge = at("127.0.0.1", 1143, false);
        bridge.retain(|(k, _)| *k != key::PORT);
        let found = accounts(
            "bridge",
            &imap_and_smtp(
                1,
                "Proton",
                "ada@proton.me",
                bridge,
                at("localhost", 0, false),
            ),
        );
        let Setup::Password { imap, smtp } = &found[0].setup else {
            panic!("read as {:?}", found[0].setup);
        };
        assert_eq!((imap.port, imap.security), (143, Security::None));
        assert_eq!((smtp.port, smtp.security), (587, Security::None));
    }

    #[test]
    fn a_database_without_parent_accounts_is_read() {
        let dir = temp("no-parents");
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("Accounts4.sqlite");
        let db = Connection::open(&path).unwrap();
        db.execute_batch(
            "CREATE TABLE ZACCOUNTTYPE (Z_PK INTEGER PRIMARY KEY, ZIDENTIFIER VARCHAR); \
             CREATE TABLE ZACCOUNT (Z_PK INTEGER PRIMARY KEY, ZACCOUNTTYPE INTEGER, \
             ZIDENTIFIER VARCHAR, ZACCOUNTDESCRIPTION VARCHAR, ZUSERNAME VARCHAR); \
             CREATE TABLE ZACCOUNTPROPERTY (Z_PK INTEGER PRIMARY KEY, ZOWNER INTEGER, \
             ZKEY VARCHAR, ZVALUE BLOB); \
             INSERT INTO ZACCOUNTTYPE VALUES (1, 'com.apple.account.POP'); \
             INSERT INTO ZACCOUNT VALUES (1, 1, 'P', NULL, 'ada@example.com');",
        )
        .unwrap();
        drop(db);
        let found = AppleMail::at(path).accounts().unwrap();
        std::fs::remove_dir_all(&dir).unwrap();
        assert_eq!(found.len(), 1, "{found:?}");
    }

    /// Hex of what `NSKeyedArchiver` itself wrote on macOS for an `NSMutableString`, an
    /// `NSArray` of two strings and `true`.
    const REAL: [&str; 3] = [
        "62706c6973743030d4010203040506070a582476657273696f6e592461726368697665725424746f7058246f626a6563747312000186a05f100f4e534b657965644172636869766572d1080954726f6f748001a30b0c1155246e756c6cd20d0e0f105624636c617373594e532e737472696e6780025f1010696d61702e6578616d706c652e636f6dd2121314155a24636c6173736e616d655824636c61737365735f100f4e534d757461626c65537472696e67a3141617584e53537472696e67584e534f626a65637408111a24293237494c5153575d62697375888d98a1b3b7c000000000000001010000000000000018000000000000000000000000000000c9",
        "62706c6973743030d4010203040506070a582476657273696f6e592461726368697665725424746f7058246f626a6563747312000186a05f100f4e534b657965644172636869766572d1080954726f6f748001a50b0c13141555246e756c6cd20d0e0f125a4e532e6f626a656374735624636c617373a210118002800380045f100f616461406578616d706c652e636f6d5f101061646140776f726b2e6578616d706c65d2161718195a24636c6173736e616d655824636c6173736573574e534172726179a2181a584e534f626a65637408111a24293237494c5153595f646f76797b7d7f91a4a9b4bdc5c80000000000000101000000000000001b000000000000000000000000000000d1",
        "62706c6973743030d4010203040506070a582476657273696f6e592461726368697665725424746f7058246f626a6563747312000186a05f100f4e534b657965644172636869766572d1080954726f6f748001a20b0c55246e756c6c0908111a24293237494c5153565c0000000000000101000000000000000d0000000000000000000000000000005d",
    ];

    #[test]
    fn what_nskeyedarchiver_writes_is_unarchived() {
        let bytes = |hex: &str| -> Vec<u8> {
            (0..hex.len())
                .step_by(2)
                .map(|i| u8::from_str_radix(&hex[i..i + 2], 16).unwrap())
                .collect()
        };
        let [string, array, flag] = REAL.map(|hex| unarchive(&bytes(hex)));
        assert_eq!(string, Some("imap.example.com".into()));
        assert_eq!(
            array,
            Some(Value::Array(vec![
                "ada@example.com".into(),
                "ada@work.example".into()
            ]))
        );
        assert_eq!(flag, Some(true.into()));
        assert_eq!(unarchive(b"not a plist"), None);
    }

    #[test]
    fn apple_mail_is_found_in_the_macos_accounts_directory() {
        let home = temp("home");
        assert_eq!(AppleMail::find(&home), None, "not before it's there");
        let dir = home.join(DATABASE);
        let path = database(dir.parent().unwrap(), &[]);
        assert_eq!(AppleMail::find(&home), Some(AppleMail::at(path)));
        let sources = super::super::installed(&home);
        let apps: Vec<_> = sources.iter().map(|s| s.app()).collect();
        assert_eq!(apps, ["Apple Mail"]);
        std::fs::remove_dir_all(&home).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn a_database_macos_keeps_private_says_to_allow_full_disk_access() {
        use std::os::unix::fs::PermissionsExt;

        let dir = temp("private");
        let path = database(&dir, &[]);
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o000)).unwrap();
        let read = AppleMail::at(path).accounts();
        std::fs::remove_dir_all(&dir).unwrap();
        assert!(matches!(read, Err(Error::NeedsFullDiskAccess)), "{read:?}");
    }

    #[test]
    fn a_file_that_isnt_the_accounts_database_is_an_error() {
        let dir = temp("garbage");
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("Accounts4.sqlite");
        std::fs::write(&path, "not a database").unwrap();
        let read = AppleMail::at(path).accounts();
        std::fs::remove_dir_all(&dir).unwrap();
        assert!(
            matches!(read, Err(Error::Invalid(ref why)) if why.starts_with("couldn't read")),
            "{read:?}"
        );
    }
}
