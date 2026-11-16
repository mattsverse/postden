//! Opening one message, setting whether it's been read, and moving it to another mailbox.
//!
//! Each call opens a connection of its own, so it never waits behind a watcher's IDLE.

use futures::TryStreamExt as _;
use mail_parser::MessageParser;
use postden_core::account::Server;
use postden_core::message::{Address, Body};

use super::connection::{Session, connect};
use super::watch::with_timeout;
use crate::{Credentials, Error, Result};

/// Downloads message `uid` from `mailbox` and marks it read.
///
/// `uid_validity` is the mailbox's UIDVALIDITY when the message was listed; if the server's differs,
/// the UID may now name another message, so nothing is fetched.
///
/// # Errors
///
/// Returns an error when the connection fails, the mailbox changed, or the message is gone.
pub async fn open_message(
    server: &Server,
    credentials: &Credentials,
    mailbox: &str,
    uid_validity: u32,
    uid: u32,
) -> Result<Body> {
    let mut session = connect(server, credentials).await?;
    let result = async {
        select(&mut session, mailbox, uid_validity).await?;
        // PEEK, then an explicit STORE: a plain BODY[] marks the message read only on some servers.
        let fetches: Vec<_> = with_timeout(async {
            session
                .uid_fetch(uid.to_string(), "(UID BODY.PEEK[])")
                .await?
                .try_collect()
                .await
        })
        .await?;
        let raw = fetches
            .iter()
            .find(|f| f.uid == Some(uid))
            .and_then(|f| f.body())
            .ok_or(Error::MessageGone)?;
        let body = parse_body(raw);
        store_seen(&mut session, uid, true).await?;
        Ok(body)
    }
    .await;
    // The work is done or failed already; a failed goodbye doesn't change that.
    let _ignored = session.logout().await;
    result
}

/// What [`prefetch_messages`] downloaded.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Prefetched {
    /// The messages downloaded, by UID.
    pub bodies: Vec<(u32, Body)>,
    /// The messages left out for being larger than asked, which only opening them downloads.
    pub too_big: Vec<u32>,
}

/// Downloads messages `uids` from `mailbox` without marking them read, ahead of their opening.
///
/// Those larger than `max_size` bytes, such as ones carrying big attachments, are left out, as
/// are those no longer on the server. See [`open_message`] for `uid_validity`.
///
/// # Errors
///
/// Returns an error when the connection fails or the mailbox changed.
pub async fn prefetch_messages(
    server: &Server,
    credentials: &Credentials,
    mailbox: &str,
    uid_validity: u32,
    uids: &[u32],
    max_size: u32,
) -> Result<Prefetched> {
    if uids.is_empty() {
        return Ok(Prefetched::default());
    }
    let mut session = connect(server, credentials).await?;
    let result = async {
        select(&mut session, mailbox, uid_validity).await?;
        let set = uid_set(uids);
        let sizes: Vec<_> = with_timeout(async {
            session
                .uid_fetch(&set, "(UID RFC822.SIZE)")
                .await?
                .try_collect()
                .await
        })
        .await?;
        let mut small = Vec::new();
        let mut too_big = Vec::new();
        for fetch in &sizes {
            match (fetch.uid, fetch.size) {
                (Some(uid), Some(size)) if size > max_size => too_big.push(uid),
                (Some(uid), _) if uids.contains(&uid) => small.push(uid),
                _ => {}
            }
        }
        if small.is_empty() {
            return Ok(Prefetched {
                bodies: Vec::new(),
                too_big,
            });
        }
        let fetches: Vec<_> = with_timeout(async {
            session
                .uid_fetch(uid_set(&small), "(UID BODY.PEEK[])")
                .await?
                .try_collect()
                .await
        })
        .await?;
        let bodies = fetches
            .iter()
            .filter_map(|f| Some((f.uid?, parse_body(f.body()?))))
            .filter(|(uid, _)| small.contains(uid))
            .collect();
        Ok(Prefetched { bodies, too_big })
    }
    .await;
    let _ignored = session.logout().await;
    result
}

/// UIDs as an IMAP sequence set, such as `3,7,12`.
fn uid_set(uids: &[u32]) -> String {
    uids.iter()
        .map(u32::to_string)
        .collect::<Vec<_>>()
        .join(",")
}

/// Marks message `uid` in `mailbox` read or unread. See [`open_message`] for `uid_validity`.
///
/// # Errors
///
/// Returns an error when the connection fails or the mailbox changed.
pub async fn set_seen(
    server: &Server,
    credentials: &Credentials,
    mailbox: &str,
    uid_validity: u32,
    uid: u32,
    seen: bool,
) -> Result<()> {
    let mut session = connect(server, credentials).await?;
    let result = async {
        select(&mut session, mailbox, uid_validity).await?;
        store_seen(&mut session, uid, seen).await
    }
    .await;
    let _ignored = session.logout().await;
    result
}

/// Marks every message in `mailbox` read. See [`open_message`] for `uid_validity`.
///
/// Returns the mailbox's UIDNEXT when it was selected: what was marked is every message with a
/// UID below it, so mail arriving meanwhile stays unread.
///
/// # Errors
///
/// Returns an error when the connection fails or the mailbox changed.
pub async fn mark_all_seen(
    server: &Server,
    credentials: &Credentials,
    mailbox: &str,
    uid_validity: u32,
) -> Result<u32> {
    let mut session = connect(server, credentials).await?;
    let result = async {
        let selected = with_timeout(session.select(mailbox)).await?;
        if selected.uid_validity != Some(uid_validity) {
            return Err(Error::UidValidityChanged(mailbox.to_owned()));
        }
        // Without UIDNEXT, which every IMAP4rev1 server sends, mark everything there is.
        let up_to = selected.uid_next.unwrap_or(u32::MAX);
        if selected.exists == 0 || up_to <= 1 {
            return Ok(up_to);
        }
        let range = if up_to == u32::MAX {
            "1:*".to_owned()
        } else {
            format!("1:{}", up_to - 1)
        };
        with_timeout(async {
            session
                .uid_store(range, "+FLAGS.SILENT (\\Seen)")
                .await?
                .try_for_each(|_fetch| async { Ok(()) })
                .await
        })
        .await?;
        Ok(up_to)
    }
    .await;
    let _ignored = session.logout().await;
    result
}

/// Moves message `uid` from `mailbox` to the account's mailbox `to`. See [`open_message`] for
/// `uid_validity`. A message no longer in `mailbox` is left alone, and that isn't an error.
///
/// Uses MOVE (RFC 6851), or else COPY, flags the original `\Deleted` and expunges just it
/// (UIDPLUS, RFC 4315). A server with neither is refused: a plain EXPUNGE would also remove
/// every other message flagged `\Deleted` in the mailbox.
///
/// # Errors
///
/// Returns an error when the connection fails, the mailbox changed, the server can't move
/// messages, or it refused to.
pub async fn move_message(
    server: &Server,
    credentials: &Credentials,
    mailbox: &str,
    uid_validity: u32,
    uid: u32,
    to: &str,
) -> Result<()> {
    let mut session = connect(server, credentials).await?;
    let result = async {
        let capabilities = with_timeout(session.capabilities()).await?;
        select(&mut session, mailbox, uid_validity).await?;
        let uid = uid.to_string();
        if capabilities.has_str("MOVE") {
            with_timeout(session.uid_mv(&uid, to)).await
        } else if capabilities.has_str("UIDPLUS") {
            with_timeout(session.uid_copy(&uid, to)).await?;
            with_timeout(async {
                session
                    .uid_store(&uid, "+FLAGS.SILENT (\\Deleted)")
                    .await?
                    .try_for_each(|_fetch| async { Ok(()) })
                    .await
            })
            .await?;
            with_timeout(async {
                session
                    .uid_expunge(&uid)
                    .await?
                    .try_for_each(|_uid| async { Ok(()) })
                    .await
            })
            .await
        } else {
            Err(Error::CantMove)
        }
    }
    .await;
    let _ignored = session.logout().await;
    result
}

async fn select(session: &mut Session, mailbox: &str, uid_validity: u32) -> Result<()> {
    let selected = with_timeout(session.select(mailbox)).await?;
    if selected.uid_validity == Some(uid_validity) {
        Ok(())
    } else {
        Err(Error::UidValidityChanged(mailbox.to_owned()))
    }
}

async fn store_seen(session: &mut Session, uid: u32, seen: bool) -> Result<()> {
    let change = if seen { '+' } else { '-' };
    let query = format!("{change}FLAGS.SILENT (\\Seen)");
    with_timeout(async {
        session
            .uid_store(uid.to_string(), query)
            .await?
            .try_for_each(|_fetch| async { Ok(()) })
            .await
    })
    .await
}

/// Reads a whole RFC 5322 message. What can't be parsed is left empty.
fn parse_body(raw: &[u8]) -> Body {
    let Some(message) = MessageParser::default().parse(raw) else {
        return Body::default();
    };
    let addresses = |list: Option<mail_parser::AddressList<'_>>| -> Vec<Address> {
        list.map(|l| {
            l.mailboxes()
                .map(|m| Address {
                    name: m.name().map(str::to_owned),
                    email: m.address().map(str::to_owned),
                })
                .collect()
        })
        .unwrap_or_default()
    };
    Body {
        to: addresses(message.to()),
        cc: addresses(message.cc()),
        reply_to: addresses(message.reply_to()),
        references: message
            .references()
            .map(|ids| ids.iter().map(str::to_owned).collect())
            .unwrap_or_default(),
        html: html_body(&message),
        text: message
            .text_bodies()
            .collect::<Vec<_>>()
            .join("\n\n")
            .trim_end()
            .to_owned(),
        attachments: message
            .attachments()
            .map(|a| a.attachment_name().unwrap_or("(unnamed)").to_owned())
            .collect(),
    }
}

/// The HTML parts of the body, joined, if any are HTML: a message with only text has none, as
/// converting its text to HTML adds nothing.
fn html_body(message: &mail_parser::Message<'_>) -> Option<String> {
    let has_html = message
        .html_body()
        .any(|part| matches!(part.kind(), mail_parser::PartKind::Html));
    has_html.then(|| message.html_bodies().collect::<Vec<_>>().join("\n"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn plain_text_and_recipients_are_read() {
        let raw = b"From: Ada <ada@example.com>\r\n\
            To: Me <me@example.com>, bob@example.com\r\n\
            Cc: =?UTF-8?Q?Ren=C3=A9?= <rene@example.com>\r\n\
            Subject: hi\r\n\r\n\
            Hello there.\r\n";
        let body = parse_body(raw);
        assert_eq!(
            body.to,
            vec![
                Address {
                    name: Some("Me".into()),
                    email: Some("me@example.com".into())
                },
                Address {
                    name: None,
                    email: Some("bob@example.com".into())
                },
            ]
        );
        assert_eq!(body.cc[0].name.as_deref(), Some("René"));
        assert_eq!(body.text, "Hello there.");
        assert_eq!(body.html, None, "plain text has no HTML");
        assert_eq!(body.attachments, Vec::<String>::new());
        assert_eq!(body.reply_to, Vec::<Address>::new());
        assert_eq!(body.references, Vec::<String>::new());
    }

    #[test]
    fn reply_to_and_references_are_read() {
        let raw = b"From: ada@example.com\r\n\
            Reply-To: List <list@example.com>\r\n\
            References: <a@example.com> <b@example.com>\r\n\r\n\
            Hi\r\n";
        let body = parse_body(raw);
        assert_eq!(body.reply_to[0].email.as_deref(), Some("list@example.com"));
        assert_eq!(body.references, vec!["a@example.com", "b@example.com"]);
    }

    #[test]
    fn html_only_mail_is_shown_as_text_and_attachments_are_listed() {
        let raw = b"From: ada@example.com\r\n\
            Content-Type: multipart/mixed; boundary=b\r\n\r\n\
            --b\r\n\
            Content-Type: text/html\r\n\r\n\
            <p>Hello <b>there</b></p>\r\n\
            --b\r\n\
            Content-Type: application/pdf\r\n\
            Content-Disposition: attachment; filename=report.pdf\r\n\r\n\
            %PDF\r\n\
            --b--\r\n";
        let body = parse_body(raw);
        assert!(body.text.contains("Hello there"), "got {:?}", body.text);
        assert!(!body.text.contains('<'), "got {:?}", body.text);
        assert_eq!(
            body.html.as_deref().map(str::trim),
            Some("<p>Hello <b>there</b></p>")
        );
        assert_eq!(body.attachments, vec!["report.pdf".to_owned()]);
    }
}
