//! Thunderbird, by Mozilla.
//!
//! Thunderbird keeps each profile's settings in its `prefs.js`, as `user_pref("key", value);`
//! lines, and lists its profiles in `profiles.ini` in its data directory. The profile it opens
//! is the one its install section names, else the one marked as the default, else the first.
//!
//! Its accounts are listed, in order, in `mail.accountmanager.accounts`. Each account has an
//! incoming server, `mail.account.<account>.server`, whose settings are under
//! `mail.server.<server>.`, and identities, `mail.account.<account>.identities`, the first of
//! which gives its address and name, and its SMTP server, whose settings are under
//! `mail.smtpserver.<smtp>.`, else the default SMTP server. A setting a server doesn't have is
//! the one under `mail.server.default.` or `mail.smtpserver.default.`, if set, and so for
//! identities under `mail.identity.default.`. Accounts that aren't mailboxes, such as Local
//! Folders, feeds and newsgroups, aren't listed.
//!
//! Its passwords are in its own store, encrypted with its primary password or a key of the
//! system's, so they aren't read.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use postden_core::account::{Security, Server};

use super::{Found, Setup, Source, Unsupported, is_loopback, non_empty, once_each};
use crate::{Error, Result};

/// Where Thunderbird's data directory may be, in the home directory: on macOS, on Linux as it's
/// usually installed, as a Flatpak and as a Snap, and on Windows.
const DATA_DIRECTORIES: [&str; 5] = [
    "Library/Thunderbird",
    ".thunderbird",
    ".var/app/org.mozilla.Thunderbird/.thunderbird",
    "snap/thunderbird/common/.thunderbird",
    "AppData/Roaming/Thunderbird",
];

/// The kinds of incoming servers, in `mail.server.<server>.type`.
mod kind {
    pub(super) const IMAP: &str = "imap";
    pub(super) const POP: &str = "pop3";
    /// Exchange, over Exchange Web Services.
    pub(super) const EXCHANGE: &str = "ews";
    /// Exchange, through the Owl add-on.
    pub(super) const OWL: &str = "owl";
    pub(super) const OWL_EWS: &str = "owl-ews";
    pub(super) const OWL_EAS: &str = "owl-eas";

    pub(super) const MICROSOFT: [&str; 4] = [EXCHANGE, OWL, OWL_EWS, OWL_EAS];
}

/// How a server's connection is encrypted, in its `socketType` or `try_ssl`.
mod socket {
    pub(super) const PLAIN: i64 = 0;
    /// `STARTTLS` if the server offers it: older Thunderbirds' default.
    pub(super) const TRY_STARTTLS: i64 = 1;
    pub(super) const STARTTLS: i64 = 2;
    pub(super) const TLS: i64 = 3;
}

/// A Thunderbird profile's settings, its `prefs.js`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Thunderbird {
    prefs: PathBuf,
}

impl Thunderbird {
    /// The settings of the profile Thunderbird opens, in the home directory `home`, if it's
    /// there.
    #[must_use]
    pub fn find(home: &Path) -> Option<Self> {
        DATA_DIRECTORIES.iter().find_map(|dir| {
            let dir = home.join(dir);
            let ini = std::fs::read_to_string(dir.join("profiles.ini")).ok()?;
            let prefs = default_profile(&dir, &ini)?.join("prefs.js");
            prefs.is_file().then_some(Self { prefs })
        })
    }

    /// The profile settings at `prefs`.
    #[must_use]
    pub const fn at(prefs: PathBuf) -> Self {
        Self { prefs }
    }
}

impl Source for Thunderbird {
    fn app(&self) -> &'static str {
        "Thunderbird"
    }

    fn accounts(&self) -> Result<Vec<Found>> {
        let text = std::fs::read_to_string(&self.prefs)
            .map_err(|e| Error::Invalid(format!("couldn't read Thunderbird's settings: {e}")))?;
        let prefs = Prefs::parse(&text);
        Ok(once_each(
            prefs
                .list("mail.accountmanager.accounts")
                .into_iter()
                .filter_map(|account| prefs.found(account))
                .collect(),
        ))
    }
}

/// The directory of the profile Thunderbird opens, from its data directory `dir` and its
/// `profiles.ini`, `ini`.
fn default_profile(dir: &Path, ini: &str) -> Option<PathBuf> {
    let sections = ini_sections(ini);
    let path_of = |section: &HashMap<&str, &str>| {
        let path = section.get("Path")?;
        Some(if section.get("IsRelative") == Some(&"0") {
            PathBuf::from(path)
        } else {
            dir.join(path)
        })
    };
    let profiles = || {
        sections
            .iter()
            .filter(|(name, _)| name.starts_with("Profile"))
            .map(|(_, section)| section)
    };
    // An install's default is a profile's path, as it's written in that profile's section. An
    // install whose profile is gone may still be listed, without one or with the old one.
    let installed = sections
        .iter()
        .filter(|(name, _)| name.starts_with("Install"))
        .filter_map(|(_, section)| section.get("Default"))
        .find_map(|default| profiles().find(|p| p.get("Path") == Some(default)));
    installed
        .or_else(|| profiles().find(|p| p.get("Default") == Some(&"1")))
        .or_else(|| profiles().next())
        .and_then(path_of)
}

/// The sections of an INI file, in order, each with its `key=value` lines.
fn ini_sections(ini: &str) -> Vec<(&str, HashMap<&str, &str>)> {
    let mut sections: Vec<(&str, HashMap<&str, &str>)> = Vec::new();
    for line in ini.lines().map(str::trim) {
        if let Some(name) = line.strip_prefix('[').and_then(|l| l.strip_suffix(']')) {
            sections.push((name.trim(), HashMap::new()));
        } else if let Some((key, value)) = line.split_once('=')
            && let Some((_, section)) = sections.last_mut()
        {
            section.insert(key.trim(), value.trim());
        }
    }
    sections
}

/// A profile's preferences, each as written: a string unquoted, a number or a boolean as it is.
#[derive(Debug, Default)]
struct Prefs(HashMap<String, String>);

impl Prefs {
    /// The `user_pref("key", value);` lines of `prefs.js`. Lines that aren't are left out.
    fn parse(text: &str) -> Self {
        Self(text.lines().filter_map(user_pref).collect())
    }

    fn text(&self, key: &str) -> Option<String> {
        non_empty(self.0.get(key).cloned())
    }

    /// The server setting `key`, from the server's settings under `prefix`, else from the
    /// defaults under `defaults`.
    fn setting(&self, prefix: &str, defaults: &str, key: &str) -> Option<String> {
        self.text(&format!("{prefix}{key}"))
            .or_else(|| self.text(&format!("{defaults}{key}")))
    }

    /// A comma-separated list, such as an account's identities.
    fn list(&self, key: &str) -> Vec<&str> {
        self.0.get(key).map_or_else(Vec::new, |value| {
            value
                .split(',')
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .collect()
        })
    }

    /// The account `account`, or `None` when it's not a mailbox, or has no address to file it
    /// under.
    fn found(&self, account: &str) -> Option<Found> {
        let server = self.text(&format!("mail.account.{account}.server"))?;
        let incoming = |key: &str| format!("mail.server.{server}.{key}");
        let setting = |key| self.setting(&incoming(""), "mail.server.default.", key);
        let kind = self.text(&incoming("type"))?;
        let microsoft = kind::MICROSOFT.contains(&kind.as_str());
        if !microsoft && kind != kind::IMAP && kind != kind::POP {
            return None;
        }
        let identity = self
            .list(&format!("mail.account.{account}.identities"))
            .first()
            .map(|id| format!("mail.identity.{id}."));
        let of_identity = |key| {
            identity
                .as_ref()
                .and_then(|id| self.setting(id, "mail.identity.default.", key))
        };
        let login = setting("userName");
        let address =
            of_identity("useremail").or_else(|| login.clone().filter(|l| l.contains('@')))?;

        let host = setting("hostname").map(|h| h.to_ascii_lowercase());
        let on = |domains: &[&str]| {
            host.as_deref().is_some_and(|h| {
                domains
                    .iter()
                    .any(|d| h == *d || h.ends_with(&format!(".{d}")))
            })
        };
        let setup = if kind == kind::POP {
            Setup::Unsupported(Unsupported::Pop)
        } else if on(&["gmail.com", "googlemail.com"]) {
            Setup::Google
        } else if microsoft || on(&["office365.com", "outlook.com", "hotmail.com", "live.com"]) {
            Setup::Unsupported(Unsupported::Microsoft)
        } else {
            let login = login.unwrap_or_else(|| address.clone());
            self.servers(&server, of_identity("smtpServer"), &login)
                .map_or_else(
                    |why| Setup::Unsupported(Unsupported::Unreadable(why)),
                    |(imap, smtp)| Setup::Password { imap, smtp },
                )
        };
        let label = self
            .text(&incoming("name"))
            .filter(|name| !name.eq_ignore_ascii_case(&address));
        Some(Found {
            display_name: of_identity("fullName"),
            label,
            setup,
            address,
        })
    }

    /// The IMAP server `server` and the SMTP server `smtp`: the IMAP server logs in as `login`,
    /// and the SMTP server as it says, else as `login` too. When `smtp` isn't set up, it's the
    /// default SMTP server, else the first one, as in Thunderbird.
    fn servers(
        &self,
        server: &str,
        smtp: Option<String>,
        login: &str,
    ) -> Result<(Server, Server), String> {
        let imap = self
            .server(
                &format!("mail.server.{server}."),
                "mail.server.default.",
                "socketType",
                login,
                Security::default_imap_port,
            )
            .map_err(|why| format!("IMAP server: {why}"))?;
        // One that was removed may still be named.
        let set_up = |key: &String| {
            self.text(&format!("mail.smtpserver.{key}.hostname"))
                .is_some()
        };
        let smtp = smtp
            .filter(set_up)
            .or_else(|| self.text("mail.smtp.defaultserver").filter(set_up))
            .or_else(|| {
                self.list("mail.smtpservers")
                    .into_iter()
                    .map(str::to_owned)
                    .find(set_up)
            })
            .ok_or_else(|| "SMTP server: missing".to_owned())?;
        let prefix = format!("mail.smtpserver.{smtp}.");
        let smtp_login = self
            .setting(&prefix, "mail.smtpserver.default.", "username")
            .unwrap_or_else(|| login.to_owned());
        let smtp = self
            .server(
                &prefix,
                "mail.smtpserver.default.",
                "try_ssl",
                &smtp_login,
                Security::default_smtp_port,
            )
            .map_err(|why| format!("SMTP server: {why}"))?;
        Ok((imap, smtp))
    }

    /// The server whose settings start with `prefix`, else are the defaults under `defaults`,
    /// its encryption in `encryption`, logging in as `login`.
    fn server(
        &self,
        prefix: &str,
        defaults: &str,
        encryption: &str,
        login: &str,
        default_port: fn(Security) -> u16,
    ) -> Result<Server, String> {
        let setting = |key| self.setting(prefix, defaults, key);
        let host = setting("hostname").ok_or("missing")?;
        // As in Thunderbird: the server's own setting, else whether TLS is on, as older ones
        // said with `isSecure`, else the default, else a plain connection, which isn't written.
        let own = |key: &str| self.text(&format!("{prefix}{key}"));
        let legacy = (own("isSecure").as_deref() == Some("true")).then_some(socket::TLS);
        let default = self.text(&format!("{defaults}{encryption}"));
        let [own, default] = [own(encryption), default].map(|n| n.as_deref().map(number));
        let security = match own
            .transpose()?
            .or(legacy)
            .or(default.transpose()?)
            .unwrap_or(socket::PLAIN)
        {
            socket::TLS => Security::Tls,
            socket::STARTTLS | socket::TRY_STARTTLS => Security::StartTls,
            socket::PLAIN if is_loopback(&host) => Security::None,
            socket::PLAIN => return Err("Thunderbird connects to it without encryption".into()),
            other => return Err(format!("unknown connection type {other}")),
        };
        let port = setting("port")
            .as_deref()
            .map(number)
            .transpose()?
            // Thunderbird stores 0 or -1 when it picks the port itself.
            .filter(|p| *p > 0)
            .map(|p| u16::try_from(p).map_err(|_range| format!("{p} isn't a port")))
            .transpose()?;
        Ok(Server {
            port: port.unwrap_or_else(|| default_port(security)),
            host,
            security,
            username: login.to_owned(),
        })
    }
}

/// `value` as a number.
fn number(value: &str) -> Result<i64, String> {
    value
        .parse()
        .map_err(|_parse| format!("{value:?} isn't a number"))
}

/// The key and value of a `user_pref("key", value);` line.
fn user_pref(line: &str) -> Option<(String, String)> {
    let args = line
        .trim()
        .strip_prefix("user_pref(")?
        .trim_end()
        .strip_suffix(';')?
        .trim_end()
        .strip_suffix(')')?;
    let (key, rest) = string(args.trim_start())?;
    let value = rest.trim_start().strip_prefix(',')?.trim();
    let value = if value.starts_with(['"', '\'']) {
        let (value, rest) = string(value)?;
        rest.trim().is_empty().then_some(value)?
    } else {
        value.to_owned()
    };
    Some((key, value))
}

/// The JavaScript string literal `s` starts with, unescaped, and what follows it. As in
/// Thunderbird, a `\xNN` escape is a byte, so several of them may spell one UTF-8 character.
fn string(s: &str) -> Option<(String, &str)> {
    let quote = s.chars().next().filter(|c| matches!(c, '"' | '\''))?;
    let mut out = Vec::new();
    let push = |out: &mut Vec<u8>, c: char| {
        out.extend_from_slice(c.encode_utf8(&mut [0; 4]).as_bytes());
    };
    let mut chars = s.char_indices().skip(1);
    let hex = |chars: &mut dyn Iterator<Item = (usize, char)>, n: usize| {
        let digits: String = chars.take(n).map(|(_, c)| c).collect();
        (digits.len() == n)
            .then(|| u32::from_str_radix(&digits, 16).ok())
            .flatten()
    };
    while let Some((i, c)) = chars.next() {
        match c {
            c if c == quote => {
                return Some((String::from_utf8(out).ok()?, &s[i + c.len_utf8()..]));
            }
            '\\' => match chars.next()?.1 {
                'n' => out.push(b'\n'),
                't' => out.push(b'\t'),
                'r' => out.push(b'\r'),
                'x' => out.push(u8::try_from(hex(&mut chars, 2)?).ok()?),
                'u' => {
                    let unit = hex(&mut chars, 4)?;
                    let c = if (0xD800..0xDC00).contains(&unit) {
                        // A surrogate pair: the second half follows as `\uXXXX`.
                        let (_, '\\') = chars.next()? else {
                            return None;
                        };
                        let (_, 'u') = chars.next()? else { return None };
                        let low = hex(&mut chars, 4)?;
                        char::from_u32(
                            0x10000 + ((unit - 0xD800) << 10) + (low.checked_sub(0xDC00)?),
                        )
                    } else {
                        char::from_u32(unit)
                    };
                    push(&mut out, c?);
                }
                other => push(&mut out, other),
            },
            c => push(&mut out, c),
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp(test: &str) -> PathBuf {
        std::env::temp_dir().join(format!("postden-thunderbird-{test}-{}", std::process::id()))
    }

    /// `prefs.js` with these preferences, as Thunderbird writes it.
    fn prefs_js(prefs: &[(&str, &str)]) -> String {
        use std::fmt::Write as _;

        let mut js = String::from(
            "// Mozilla User Preferences\n\n// DO NOT EDIT THIS FILE.\n\
             user_pref(\"mail.rights.version\", 1);\n",
        );
        for (key, value) in prefs {
            writeln!(js, "user_pref(\"{key}\", {value});").unwrap();
        }
        js
    }

    fn accounts(test: &str, prefs: &[(&str, &str)]) -> Vec<Found> {
        let dir = temp(test);
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("prefs.js");
        std::fs::write(&path, prefs_js(prefs)).unwrap();
        let found = Thunderbird::at(path).accounts().unwrap();
        std::fs::remove_dir_all(&dir).unwrap();
        found
    }

    /// An IMAP account, `account<n>`, at `host`, with its identity and its own SMTP server.
    fn imap(n: u8, address: &str, host: &str) -> Vec<(String, String)> {
        let q = |s: &str| format!("\"{s}\"");
        vec![
            (
                format!("mail.account.account{n}.server"),
                q(&format!("server{n}")),
            ),
            (
                format!("mail.account.account{n}.identities"),
                q(&format!("id{n}")),
            ),
            (format!("mail.server.server{n}.type"), q("imap")),
            (format!("mail.server.server{n}.hostname"), q(host)),
            (format!("mail.server.server{n}.port"), "993".into()),
            (format!("mail.server.server{n}.socketType"), "3".into()),
            (format!("mail.server.server{n}.userName"), q(address)),
            (format!("mail.server.server{n}.name"), q(address)),
            (format!("mail.identity.id{n}.useremail"), q(address)),
            (format!("mail.identity.id{n}.fullName"), q("Ada Lovelace")),
            (
                format!("mail.identity.id{n}.smtpServer"),
                q(&format!("smtp{n}")),
            ),
            (
                format!("mail.smtpserver.smtp{n}.hostname"),
                q(&format!("smtp.{host}")),
            ),
            (format!("mail.smtpserver.smtp{n}.port"), "587".into()),
            (format!("mail.smtpserver.smtp{n}.try_ssl"), "2".into()),
            (format!("mail.smtpserver.smtp{n}.username"), q(address)),
        ]
    }

    fn with(
        accounts: &str,
        sets: Vec<Vec<(String, String)>>,
        changes: &[(&str, &str)],
    ) -> Vec<(String, String)> {
        let mut prefs = vec![(
            "mail.accountmanager.accounts".to_owned(),
            format!("\"{accounts}\""),
        )];
        prefs.extend(sets.into_iter().flatten());
        for (key, value) in changes {
            prefs.retain(|(k, _)| k != key);
            if !value.is_empty() {
                prefs.push(((*key).to_owned(), (*value).to_owned()));
            }
        }
        prefs
    }

    fn read(test: &str, prefs: &[(String, String)]) -> Vec<Found> {
        let prefs: Vec<_> = prefs
            .iter()
            .map(|(k, v)| (k.as_str(), v.as_str()))
            .collect();
        accounts(test, &prefs)
    }

    #[test]
    fn an_imap_account_is_read_with_its_servers_and_names() {
        let found = read(
            "imap",
            &with(
                "account1",
                vec![imap(1, "ada@example.com", "example.com")],
                &[("mail.server.server1.name", "\"Work\"")],
            ),
        );
        let server = |host: &str, port, security| Server {
            host: host.into(),
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
                    imap: server("example.com", 993, Security::Tls),
                    smtp: server("smtp.example.com", 587, Security::StartTls),
                },
            }]
        );
    }

    #[test]
    fn a_name_that_is_the_address_isnt_a_label() {
        let found = read(
            "label",
            &with(
                "account1",
                vec![imap(1, "ada@example.com", "example.com")],
                &[],
            ),
        );
        assert_eq!(found[0].label, None);
    }

    #[test]
    fn the_default_smtp_server_is_used_when_the_identity_names_none() {
        let found = read(
            "default-smtp",
            &with(
                "account1",
                vec![imap(1, "ada@example.com", "example.com")],
                &[
                    ("mail.identity.id1.smtpServer", ""),
                    ("mail.smtp.defaultserver", "\"smtp1\""),
                    ("mail.smtpserver.smtp1.username", ""),
                    ("mail.server.server1.userName", "\"ada\""),
                ],
            ),
        );
        let Setup::Password { imap, smtp } = &found[0].setup else {
            panic!("read as {:?}", found[0].setup);
        };
        assert_eq!(imap.username, "ada");
        assert_eq!(
            (smtp.host.as_str(), smtp.username.as_str()),
            ("smtp.example.com", "ada"),
            "it logs in as the IMAP server does"
        );
    }

    #[test]
    fn gmail_microsoft_and_pop_accounts_are_set_up_as_they_can_be() {
        let found = read(
            "kinds",
            &with(
                "account1,account2,account3,account4,account5,account6",
                vec![
                    imap(1, "ada@gmail.com", "imap.gmail.com"),
                    imap(2, "ada@outlook.com", "outlook.office365.com"),
                    imap(3, "ada@example.com", "exchange.example.com"),
                    imap(4, "ada@example.org", "pop.example.org"),
                    imap(5, "ada@googlemail.com", "pop.gmail.com"),
                    imap(6, "ada@example.net", "outlook.office365.com"),
                ],
                &[
                    ("mail.server.server3.type", "\"ews\""),
                    ("mail.server.server4.type", "\"pop3\""),
                    ("mail.server.server5.type", "\"pop3\""),
                    ("mail.server.server6.type", "\"owl-ews\""),
                ],
            ),
        );
        let setups: Vec<_> = found
            .iter()
            .map(|f| (f.address.as_str(), &f.setup))
            .collect();
        assert_eq!(
            setups,
            [
                ("ada@gmail.com", &Setup::Google),
                (
                    "ada@outlook.com",
                    &Setup::Unsupported(Unsupported::Microsoft)
                ),
                (
                    "ada@example.com",
                    &Setup::Unsupported(Unsupported::Microsoft)
                ),
                ("ada@example.org", &Setup::Unsupported(Unsupported::Pop)),
                ("ada@googlemail.com", &Setup::Unsupported(Unsupported::Pop)),
                (
                    "ada@example.net",
                    &Setup::Unsupported(Unsupported::Microsoft)
                ),
            ]
        );
    }

    #[test]
    fn local_folders_feeds_and_news_arent_accounts() {
        let found = read(
            "skipped",
            &with(
                "account1,account2,account3,account4",
                vec![
                    imap(1, "a@example.com", "example.com"),
                    imap(2, "b@example.com", "example.com"),
                    imap(3, "c@example.com", "example.com"),
                    imap(4, "d@example.com", "example.com"),
                ],
                &[
                    ("mail.server.server1.type", "\"none\""),
                    ("mail.server.server2.type", "\"rss\""),
                    ("mail.server.server3.type", "\"nntp\""),
                    // No identity, and its login isn't an address.
                    ("mail.account.account4.identities", ""),
                    ("mail.server.server4.userName", "\"d\""),
                ],
            ),
        );
        assert_eq!(found, []);
    }

    #[test]
    fn servers_that_cant_be_read_say_why() {
        let found = read(
            "unreadable",
            &with(
                "account1,account2,account3,account4",
                vec![
                    imap(1, "a@example.com", "example.com"),
                    imap(2, "b@example.com", "example.com"),
                    imap(3, "c@example.com", "example.com"),
                    imap(4, "d@example.com", "example.com"),
                ],
                &[
                    ("mail.server.server1.socketType", ""),
                    ("mail.identity.id2.smtpServer", ""),
                    ("mail.server.server3.port", "70000"),
                    ("mail.smtpserver.smtp4.try_ssl", "9"),
                ],
            ),
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
                "IMAP server: Thunderbird connects to it without encryption",
                "SMTP server: missing",
                "IMAP server: 70000 isn't a port",
                "SMTP server: unknown connection type 9",
            ]
        );
    }

    #[test]
    fn a_local_bridge_connects_without_encryption_and_a_missing_port_is_the_usual_one() {
        let found = read(
            "bridge",
            &with(
                "account1",
                vec![imap(1, "ada@proton.me", "127.0.0.1")],
                &[
                    ("mail.server.server1.socketType", "0"),
                    ("mail.server.server1.port", "-1"),
                    ("mail.smtpserver.smtp1.hostname", "\"localhost\""),
                    ("mail.smtpserver.smtp1.try_ssl", ""),
                    ("mail.smtpserver.smtp1.port", "0"),
                ],
            ),
        );
        let Setup::Password { imap, smtp } = &found[0].setup else {
            panic!("read as {:?}", found[0].setup);
        };
        assert_eq!((imap.port, imap.security), (143, Security::None));
        assert_eq!((smtp.port, smtp.security), (587, Security::None));
    }

    #[test]
    fn accounts_come_in_thunderbird_order_each_address_once() {
        let found = read(
            "order",
            &with(
                "account2, account1,account3",
                vec![
                    imap(1, "a@example.com", "example.com"),
                    imap(2, "b@example.com", "example.com"),
                    imap(3, "B@example.com", "example.com"),
                ],
                &[],
            ),
        );
        let listed: Vec<_> = found.iter().map(|f| f.address.as_str()).collect();
        assert_eq!(listed, ["b@example.com", "a@example.com"]);
    }

    #[test]
    fn a_removed_smtp_server_falls_back_to_the_default_then_the_first() {
        let smtp_host = |test, changes: &[(&str, &str)]| {
            let found = read(
                test,
                &with(
                    "account1",
                    vec![imap(1, "ada@example.com", "example.com")],
                    changes,
                ),
            );
            match &found[0].setup {
                Setup::Password { smtp, .. } => smtp.host.clone(),
                other => panic!("read as {other:?}"),
            }
        };
        let gone = [
            ("mail.identity.id1.smtpServer", "\"smtp9\""),
            ("mail.smtpserver.smtp2.hostname", "\"smtp.other.com\""),
            ("mail.smtpserver.smtp2.try_ssl", "3"),
            ("mail.smtpservers", "\"smtp9,smtp2,smtp1\""),
        ];
        assert_eq!(smtp_host("first-smtp", &gone), "smtp.other.com");
        let mut with_default = gone.to_vec();
        with_default.push(("mail.smtp.defaultserver", "\"smtp1\""));
        assert_eq!(
            smtp_host("default-smtp-2", &with_default),
            "smtp.example.com"
        );
        let inherited = [
            ("mail.identity.id1.smtpServer", ""),
            ("mail.identity.default.smtpServer", "\"smtp2\""),
            ("mail.smtpserver.smtp2.hostname", "\"smtp.other.com\""),
            ("mail.smtpserver.smtp2.try_ssl", "3"),
            ("mail.smtp.defaultserver", "\"smtp1\""),
        ];
        assert_eq!(
            smtp_host("identity-default-smtp", &inherited),
            "smtp.other.com",
            "the identities' default comes before the default server"
        );
    }

    #[test]
    fn encryption_a_server_doesnt_set_is_the_default_or_the_old_setting() {
        let found = read(
            "inherited",
            &with(
                "account1,account2",
                vec![
                    imap(1, "a@example.com", "example.com"),
                    imap(2, "b@example.com", "example.com"),
                ],
                &[
                    ("mail.server.server1.socketType", ""),
                    ("mail.server.server1.port", ""),
                    ("mail.server.default.socketType", "2"),
                    ("mail.smtpserver.smtp1.try_ssl", ""),
                    ("mail.smtpserver.default.try_ssl", "3"),
                    ("mail.server.server2.socketType", ""),
                    ("mail.server.server2.isSecure", "true"),
                    ("mail.server.server2.hostname", ""),
                    ("mail.server.server2.userName", ""),
                    ("mail.server.default.hostname", "\"imap.example.org\""),
                    ("mail.server.default.userName", "\"ada\""),
                ],
            ),
        );
        let Setup::Password { imap, smtp } = &found[0].setup else {
            panic!("read as {:?}", found[0].setup);
        };
        assert_eq!((imap.port, imap.security), (143, Security::StartTls));
        assert_eq!(smtp.security, Security::Tls);
        let Setup::Password { imap, .. } = &found[1].setup else {
            panic!("read as {:?}", found[1].setup);
        };
        assert_eq!(
            (imap.host.as_str(), imap.username.as_str()),
            ("imap.example.org", "ada"),
            "the default's"
        );
        assert_eq!(
            imap.security,
            Security::Tls,
            "the old setting wins over the default"
        );
    }

    #[test]
    fn strings_are_unescaped() {
        assert_eq!(
            user_pref(r#"user_pref("mail.identity.id1.fullName", "Adé \"A\" \\ 📨");"#),
            Some((
                "mail.identity.id1.fullName".into(),
                "Adé \"A\" \\ 📨".into()
            ))
        );
        assert_eq!(
            user_pref("user_pref(\"mail.server.server1.port\", 993);"),
            Some(("mail.server.server1.port".into(), "993".into()))
        );
        assert_eq!(
            user_pref(r#"user_pref("a", "Ren\xC3\xA9");"#),
            Some(("a".into(), "René".into())),
            "\\x escapes are UTF-8 bytes"
        );
        assert_eq!(user_pref(r#"user_pref("a", "\xFF");"#), None, "not UTF-8");
        assert_eq!(user_pref("user_pref(\"unterminated, 1);"), None);
        assert_eq!(user_pref("// user_pref(\"a\", 1);"), None);
    }

    #[test]
    fn the_profile_thunderbird_opens_is_found() {
        let ini = "[Profile1]\nName=old\nIsRelative=1\nPath=Profiles/old\nDefault=1\n\n\
                   [Profile0]\nName=new\nIsRelative=1\nPath=Profiles/new\n\n\
                   [General]\nStartWithLastProfile=1\n";
        let dir = Path::new("/tb");
        assert_eq!(
            default_profile(dir, ini),
            Some(dir.join("Profiles/old")),
            "the one marked as the default"
        );
        let installed = format!("[Install1234]\nDefault=Profiles/new\nLocked=1\n\n{ini}");
        assert_eq!(
            default_profile(dir, &installed),
            Some(dir.join("Profiles/new")),
            "the install's default wins"
        );
        let absolute = "[Profile0]\nIsRelative=0\nPath=/elsewhere/p\n";
        assert_eq!(
            default_profile(dir, absolute),
            Some(PathBuf::from("/elsewhere/p"))
        );
        let stale = format!(
            "[Install1]\nDefault=\n\n[Install2]\nDefault=Profiles/gone\n\n\
                             [Install3]\nDefault=Profiles/new\n\n{ini}"
        );
        assert_eq!(
            default_profile(dir, &stale),
            Some(dir.join("Profiles/new")),
            "installs whose profile is gone are passed over"
        );
        assert_eq!(default_profile(dir, "[General]\n"), None);
    }

    #[test]
    fn thunderbird_is_found_in_its_data_directory() {
        let home = temp("home");
        assert_eq!(Thunderbird::find(&home), None, "not before it's there");
        let dir = home.join("Library/Thunderbird");
        std::fs::create_dir_all(dir.join("Profiles/abc.default")).unwrap();
        std::fs::write(
            dir.join("profiles.ini"),
            "[Profile0]\nName=default\nIsRelative=1\nPath=Profiles/abc.default\nDefault=1\n",
        )
        .unwrap();
        assert_eq!(Thunderbird::find(&home), None, "not without its settings");
        let prefs = dir.join("Profiles/abc.default/prefs.js");
        std::fs::write(&prefs, prefs_js(&[])).unwrap();
        assert_eq!(Thunderbird::find(&home), Some(Thunderbird::at(prefs)));
        let sources = super::super::installed(&home);
        let apps: Vec<_> = sources.iter().map(|s| s.app()).collect();
        assert_eq!(apps, ["Thunderbird"]);
        assert_eq!(sources[0].accounts().unwrap(), [], "it has no accounts yet");
        std::fs::remove_dir_all(&home).unwrap();
    }

    #[test]
    fn settings_that_cant_be_read_are_an_error() {
        let read = Thunderbird::at(temp("missing").join("prefs.js")).accounts();
        assert!(
            matches!(read, Err(Error::Invalid(ref why)) if why.starts_with("couldn't read Thunderbird")),
            "{read:?}"
        );
    }
}
