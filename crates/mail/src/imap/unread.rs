//! Finding unread mail older than what's cached, which a sync, fetching only the newest, never
//! reaches, and every message the server has unread, for the mailbox's unread count.

use futures::TryStreamExt as _;
use postden_core::account::Server;
use postden_core::message::Envelope;

use super::connection::{Session, connect};
use super::envelope;
use super::watch::{Changes, with_timeout};
use crate::{Credentials, Error, Result};

/// How long to wait for the server to answer a logout before dropping the connection.
const LOGOUT_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(5);

/// What [`fetch_unread`] found.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Unread {
    /// Every UID below this one was searched: the mailbox's UIDNEXT as it was selected, so mail
    /// arriving meanwhile isn't taken for read. The UID asked below, if the server didn't say.
    pub upto: u32,
    /// Every UID below `upto` that the server has unread, ascending.
    pub unseen: Vec<u32>,
    /// The newest of those below the UID asked below that aren't cached, oldest first.
    pub messages: Vec<Envelope>,
}

/// Searches `mailbox` for every message it has unread, and fetches the newest `count` of those
/// with a UID below `below`, but none of `cached`, sorted ascending: those are cached already.
///
/// `uid_validity` is the mailbox's UIDVALIDITY as cached; if the server's differs, the UIDs mean
/// something else now, so nothing is fetched. A search the server doesn't answer OK fails,
/// rather than finding nothing unread. With `below` at 0, nothing is cached, and nothing older
/// is fetched.
///
/// # Errors
///
/// Returns an error when the connection fails or the mailbox changed.
pub async fn fetch_unread(
    server: &Server,
    credentials: &Credentials,
    mailbox: &str,
    uid_validity: u32,
    below: u32,
    cached: &[u32],
    count: usize,
) -> Result<Unread> {
    let mut session = connect(server, credentials).await?;
    let result = unread_in(&mut session, mailbox, uid_validity, below, cached, count).await;
    // Bounded, so the result arrives soon after it's read: what was changed meanwhile is kept
    // only so long for it (the store's `KEEP_WRITTEN`).
    let _ignored = tokio::time::timeout(LOGOUT_TIMEOUT, session.logout()).await;
    result
}

/// A mailbox for [`fetch_unread_each`] to search, with what [`fetch_unread`] takes for it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UnreadIn {
    pub mailbox: String,
    /// The mailbox's UIDVALIDITY as cached.
    pub uid_validity: u32,
    /// The lowest UID cached: older unread mail is fetched below it, not including it. 0 when
    /// nothing is cached.
    pub below: u32,
    /// The UIDs cached below `below`, sorted ascending.
    pub cached: Vec<u32>,
}

/// Looks for unread mail in each of `mailboxes`, as [`fetch_unread`], one after the other over
/// one connection.
///
/// An account with many mailboxes so doesn't open as many connections at once. Calls `found`
/// after each search, in order, with the count `changes` had before that mailbox was read: what
/// was changed after is newer.
///
/// A mailbox the server refuses fails alone. Once the connection fails, so does every mailbox
/// left, with the same error, as text; and once `going` says to stop, asked before connecting and
/// before each, every one left fails unsearched.
pub async fn fetch_unread_each(
    server: &Server,
    credentials: &Credentials,
    mailboxes: &[UnreadIn],
    (count, changes): (usize, &Changes),
    going: impl Fn() -> bool,
    mut found: impl FnMut(&UnreadIn, u64, std::result::Result<Unread, String>),
) {
    let connected = if going() {
        connect(server, credentials).await
    } else {
        Err(Error::Closed)
    };
    let mut session = match connected {
        Ok(session) => session,
        Err(e) => {
            let e = e.to_string();
            for m in mailboxes {
                found(m, changes.now(), Err(e.clone()));
            }
            return;
        }
    };
    let mut broken: Option<String> = None;
    for m in mailboxes {
        let asked = changes.now();
        if broken.is_none() && !going() {
            broken = Some("stopped".to_owned());
        }
        if let Some(e) = &broken {
            found(m, asked, Err(e.clone()));
            continue;
        }
        let result = unread_in(
            &mut session,
            &m.mailbox,
            m.uid_validity,
            m.below,
            &m.cached,
            count,
        )
        .await;
        if let Err(e) = &result
            && !is_refusal(e)
        {
            broken = Some(e.to_string());
        }
        found(m, asked, result.map_err(|e| e.to_string()));
    }
    let _ignored = tokio::time::timeout(LOGOUT_TIMEOUT, session.logout()).await;
}

/// As [`fetch_unread`], on a connection already made.
async fn unread_in(
    session: &mut Session,
    mailbox: &str,
    uid_validity: u32,
    below: u32,
    cached: &[u32],
    count: usize,
) -> Result<Unread> {
    let selected = with_timeout(session.select(mailbox)).await?;
    if selected.uid_validity != Some(uid_validity) {
        return Err(Error::UidValidityChanged(mailbox.to_owned()));
    }
    let upto = selected.uid_next.unwrap_or(below);
    if upto <= 1 {
        return Ok(Unread {
            upto,
            ..Unread::default()
        });
    }
    let found = with_timeout(search(session, &format!("UNSEEN UID 1:{}", upto - 1))).await?;
    let unseen = below_sorted(found, upto);
    let older = unseen
        .get(..unseen.partition_point(|&uid| uid < below))
        .unwrap_or_default();
    let wanted = to_fetch(older, cached, count);
    if wanted.is_empty() {
        return Ok(Unread {
            upto,
            unseen,
            messages: Vec::new(),
        });
    }
    let set = wanted
        .iter()
        .map(u32::to_string)
        .collect::<Vec<_>>()
        .join(",");
    let fetches: Vec<_> = with_timeout(async {
        session
            .uid_fetch(set, envelope::QUERY)
            .await?
            .try_collect()
            .await
    })
    .await?;
    // The server may also send FETCH responses of its own, such as flags another client
    // changed: only the headers asked for are the messages.
    let mut messages: Vec<Envelope> = fetches
        .iter()
        .filter(|f| f.header().is_some())
        .filter_map(envelope::from_fetch)
        .filter(|m| wanted.binary_search(&m.uid).is_ok())
        .collect();
    messages.sort_by_key(|m| m.uid);
    messages.dedup_by_key(|m| m.uid);
    Ok(Unread {
        upto,
        unseen,
        messages,
    })
}

/// The UIDs a `UID SEARCH` for `query` finds, once the server answers it OK. The crate's own
/// search drops that answer, so a refused search would find nothing, as if all were read.
async fn search(session: &mut Session, query: &str) -> Result<Vec<u32>> {
    use async_imap::error::Error as Imap;
    use async_imap::imap_proto::{MailboxDatum, Response, Status};
    let tag = session.run_command(format!("UID SEARCH {query}")).await?;
    let mut uids = Vec::new();
    loop {
        let response = session
            .read_response()
            .await
            .map_err(Imap::Io)?
            .ok_or(Error::Closed)?;
        match response.parsed() {
            Response::MailboxData(MailboxDatum::Search(found)) => uids.extend(found),
            Response::Done {
                tag: done,
                status,
                outcome,
            } if *done == tag => {
                let why = || format!("{:?}", outcome.information);
                return match status {
                    Status::Ok => Ok(uids),
                    Status::No => Err(Imap::No(why()).into()),
                    _ => Err(Imap::Bad(why()).into()),
                };
            }
            // Anything else the server says meanwhile, such as new mail arriving.
            _ => {}
        }
    }
}

/// Whether the server turned one mailbox down, leaving the connection usable for the next.
const fn is_refusal(e: &Error) -> bool {
    matches!(
        e,
        Error::UidValidityChanged(_)
            | Error::Imap(async_imap::error::Error::No(_) | async_imap::error::Error::Bad(_))
    )
}

/// The UIDs of `found` below `below`, ascending: `1:n` also matches the last message, whatever
/// its UID.
fn below_sorted(found: impl IntoIterator<Item = u32>, below: u32) -> Vec<u32> {
    let mut uids: Vec<u32> = found.into_iter().filter(|&uid| uid < below).collect();
    uids.sort_unstable();
    uids
}

/// The newest `count` of `unseen` not in `cached`, ascending, both sorted ascending.
fn to_fetch(unseen: &[u32], cached: &[u32], count: usize) -> Vec<u32> {
    let mut newest: Vec<u32> = unseen
        .iter()
        .rev()
        .take(count)
        .filter(|uid| cached.binary_search(uid).is_err())
        .copied()
        .collect();
    newest.reverse();
    newest
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_uids_below_count() {
        assert_eq!(below_sorted([9, 3, 12, 1], 10), vec![1, 3, 9]);
    }

    #[test]
    fn fetches_the_newest_unread_not_cached() {
        assert_eq!(
            to_fetch(&[1, 3, 5, 7, 9], &[7], 3),
            vec![5, 9],
            "7 is cached"
        );
        assert_eq!(to_fetch(&[1, 3], &[], 5), vec![1, 3]);
        assert_eq!(
            to_fetch(&[1, 3, 5], &[3, 5], 2),
            Vec::<u32>::new(),
            "the newest are all cached: older ones aren't reached for"
        );
    }
}
