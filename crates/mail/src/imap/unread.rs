//! Finding unread mail older than what's cached, which a sync, fetching only the newest, never
//! reaches.

use futures::TryStreamExt as _;
use postden_core::account::Server;
use postden_core::message::Envelope;

use super::connection::connect;
use super::envelope;
use super::watch::with_timeout;
use crate::{Credentials, Error, Result};

/// How long to wait for the server to answer a logout before dropping the connection.
const LOGOUT_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(5);

/// What [`fetch_unread`] found.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Unread {
    /// Every UID below the one asked for that the server has unread, ascending.
    pub unseen: Vec<u32>,
    /// The newest of those not cached, oldest first.
    pub messages: Vec<Envelope>,
}

/// Searches `mailbox` for its unread messages with a UID below `below`, and fetches the newest
/// `count` of them, but none of `cached`, sorted ascending: those are cached already.
///
/// `uid_validity` is the mailbox's UIDVALIDITY as cached; if the server's differs, the UIDs mean
/// something else now, so nothing is fetched.
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
    if below <= 1 {
        return Ok(Unread {
            unseen: Vec::new(),
            messages: Vec::new(),
        });
    }
    let mut session = connect(server, credentials).await?;
    let result = async {
        let selected = with_timeout(session.select(mailbox)).await?;
        if selected.uid_validity != Some(uid_validity) {
            return Err(Error::UidValidityChanged(mailbox.to_owned()));
        }
        let found = with_timeout(session.uid_search(format!("UNSEEN UID 1:{}", below - 1))).await?;
        let unseen = below_sorted(found, below);
        let wanted = to_fetch(&unseen, cached, count);
        if wanted.is_empty() {
            return Ok(Unread {
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
        Ok(Unread { unseen, messages })
    }
    .await;
    // Bounded, so the result arrives soon after it's read: what was changed meanwhile is kept
    // only so long for it (the store's `KEEP_WRITTEN`).
    let _ignored = tokio::time::timeout(LOGOUT_TIMEOUT, session.logout()).await;
    result
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
