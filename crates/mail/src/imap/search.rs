//! Searching a mailbox on the server, for mail older than what's cached and for text in bodies,
//! which the cache doesn't hold.

use std::time::Duration;

use futures::TryStreamExt as _;
use postden_core::account::Server;
use postden_core::message::Envelope;

use super::connection::connect;
use super::envelope;
use super::watch::with_timeout;
use crate::{Credentials, Error, Result};

/// The longest literal a server with `LITERAL-` takes without asking first (RFC 7888).
const LITERAL_MINUS_MAX: usize = 4096;

/// How long to wait for the server to answer a logout before dropping the connection.
const LOGOUT_TIMEOUT: Duration = Duration::from_secs(5);

/// Messages found by [`search`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Found {
    /// The newest matches, newest first.
    pub messages: Vec<Envelope>,
    /// Whether the server had more matches than were fetched.
    pub more: bool,
}

/// Fetches the newest `count` messages in `mailbox` whose subject, sender, recipients or text hold
/// every word of `query`, ignoring case.
///
/// `uid_validity` is the mailbox's UIDVALIDITY as cached; if the server's differs, nothing is
/// fetched, as the UIDs found wouldn't mean what the cache says they do.
///
/// # Errors
///
/// Returns an error when the connection fails, the mailbox changed, or the server refuses the
/// search, as one may for words outside ASCII if it takes no literals without asking.
pub async fn search(
    server: &Server,
    credentials: &Credentials,
    mailbox: &str,
    uid_validity: u32,
    query: &str,
    count: usize,
) -> Result<Found> {
    if words(query).next().is_none() {
        return Ok(Found {
            messages: Vec::new(),
            more: false,
        });
    }
    let mut session = connect(server, credentials).await?;
    let result = async {
        let capabilities = with_timeout(session.capabilities()).await?;
        let literals = if capabilities.has_str("LITERAL+") {
            Some(usize::MAX)
        } else if capabilities.has_str("LITERAL-") {
            Some(LITERAL_MINUS_MAX)
        } else {
            None
        };
        let criteria = criteria(query, literals).unwrap_or_default();
        let selected = with_timeout(session.select(mailbox)).await?;
        if selected.uid_validity != Some(uid_validity) {
            return Err(Error::UidValidityChanged(mailbox.to_owned()));
        }
        let found = with_timeout(session.uid_search(criteria)).await?;
        let (uids, more) = newest(found, count);
        if uids.is_empty() {
            return Ok(Found {
                messages: Vec::new(),
                more,
            });
        }
        let set = uids
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
        // changed: only the headers asked for, of the UIDs asked for, once each, are matches.
        let mut messages: Vec<Envelope> = fetches
            .iter()
            .filter(|f| asked_for(f.uid, f.header().is_some(), &uids))
            .filter_map(envelope::from_fetch)
            .collect();
        messages.sort_by_key(|m| std::cmp::Reverse(m.uid));
        messages.dedup_by_key(|m| m.uid);
        Ok(Found { messages, more })
    }
    .await;
    // After a failure, such as a server gone quiet, the connection is just dropped: a goodbye
    // could wait as long again, and the search's failure has to reach the UI. After success the
    // goodbye gets a short while.
    if result.is_ok() {
        let _ignored = tokio::time::timeout(LOGOUT_TIMEOUT, session.logout()).await;
    }
    result
}

/// The `UID SEARCH` criteria for `query`: each word in the subject, sender, recipients or body
/// (IMAP's `OR` takes two keys, so four nest three deep), and every word required. `None` when
/// there are no words.
///
/// A word outside ASCII can't go in a quoted string, so if the server takes literals without
/// asking first (`LITERAL+`, or `LITERAL-` up to 4096 bytes), `literals` is the longest it takes
/// and the word is sent as one. Otherwise it's quoted anyway, which lenient servers accept.
fn criteria(query: &str, literals: Option<usize>) -> Option<String> {
    let words: Vec<String> = words(query)
        .map(|w| {
            if !w.is_ascii() && literals.is_some_and(|max| w.len() <= max) {
                literal(&w)
            } else {
                quote(&w)
            }
        })
        .collect();
    if words.is_empty() {
        return None;
    }
    let keys = words
        .iter()
        .map(|w| format!("OR OR OR SUBJECT {w} FROM {w} TO {w} BODY {w}"))
        .collect::<Vec<_>>()
        .join(" ");
    // Servers assume US-ASCII unless told otherwise.
    Some(if query.is_ascii() {
        keys
    } else {
        format!("CHARSET UTF-8 {keys}")
    })
}

/// The query's words, without the control characters IMAP strings can't hold. Words left empty
/// are dropped.
fn words(query: &str) -> impl Iterator<Item = String> + '_ {
    query
        .split_whitespace()
        .map(|w| w.chars().filter(|c| !c.is_control()).collect::<String>())
        .filter(|w| !w.is_empty())
}

/// `word` as an IMAP quoted string.
pub(super) fn quote(word: &str) -> String {
    let mut quoted = String::with_capacity(word.len() + 2);
    quoted.push('"');
    for c in word.chars() {
        if matches!(c, '"' | '\\') {
            quoted.push('\\');
        }
        quoted.push(c);
    }
    quoted.push('"');
    quoted
}

/// `word` as a non-synchronizing literal (RFC 7888): its length in bytes, then the bytes, sent
/// without waiting for the server to ask for them.
fn literal(word: &str) -> String {
    format!("{{{}+}}\r\n{word}", word.len())
}

/// Whether a FETCH response with `uid` (and, if `has_header`, the headers) answers the fetch of
/// `asked`, sorted ascending.
fn asked_for(uid: Option<u32>, has_header: bool, asked: &[u32]) -> bool {
    has_header && uid.is_some_and(|uid| asked.binary_search(&uid).is_ok())
}

/// The `count` highest of `found`, ascending, and whether there were more.
fn newest(found: impl IntoIterator<Item = u32>, count: usize) -> (Vec<u32>, bool) {
    let mut uids: Vec<u32> = found.into_iter().collect();
    uids.sort_unstable();
    let more = uids.len() > count;
    let uids = uids.split_off(uids.len().saturating_sub(count));
    (uids, more)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_word_is_looked_for_in_each_field() {
        assert_eq!(
            criteria("  invoice  ada ", Some(usize::MAX)).as_deref(),
            Some(
                "OR OR OR SUBJECT \"invoice\" FROM \"invoice\" TO \"invoice\" BODY \"invoice\" \
                 OR OR OR SUBJECT \"ada\" FROM \"ada\" TO \"ada\" BODY \"ada\""
            )
        );
        assert_eq!(
            criteria("   ", Some(usize::MAX)),
            None,
            "no words, no search"
        );
        assert_eq!(
            criteria("\u{7}", Some(usize::MAX)),
            None,
            "nor with only control characters"
        );
    }

    #[test]
    fn quotes_are_escaped_and_control_characters_dropped() {
        assert_eq!(quote(r#"a"b\c"#), r#""a\"b\\c""#);
        assert_eq!(words("x\u{0}y \u{7}").collect::<Vec<_>>(), ["xy"]);
    }

    #[test]
    fn words_outside_ascii_go_as_literals_with_their_charset() {
        let l = "{5+}\r\ncafé";
        assert_eq!(
            criteria("café ok", Some(usize::MAX)),
            Some(format!(
                "CHARSET UTF-8 OR OR OR SUBJECT {l} FROM {l} TO {l} BODY {l} \
                 OR OR OR SUBJECT \"ok\" FROM \"ok\" TO \"ok\" BODY \"ok\""
            )),
            "five bytes: é takes two"
        );
        assert_eq!(
            criteria("café", None).as_deref(),
            Some("CHARSET UTF-8 OR OR OR SUBJECT \"café\" FROM \"café\" TO \"café\" BODY \"café\""),
            "quoted for a server that wants to be asked"
        );
        assert_eq!(
            criteria("café", Some(4)).as_deref(),
            Some("CHARSET UTF-8 OR OR OR SUBJECT \"café\" FROM \"café\" TO \"café\" BODY \"café\""),
            "quoted when longer than the server takes"
        );
        assert!(
            criteria("café", Some(5)).is_some_and(|c| c.contains("{5+}")),
            "a literal up to the limit"
        );
    }

    #[test]
    fn only_the_headers_asked_for_are_matches() {
        let asked = [3, 7, 9];
        assert!(asked_for(Some(7), true, &asked));
        assert!(
            !asked_for(Some(7), false, &asked),
            "flags another client changed"
        );
        assert!(!asked_for(Some(8), true, &asked), "a UID not asked for");
        assert!(!asked_for(None, true, &asked));
    }

    #[test]
    fn keeps_the_newest_and_says_when_some_were_left() {
        assert_eq!(newest([5, 1, 9, 3], 2), (vec![5, 9], true));
        assert_eq!(newest([2, 1], 5), (vec![1, 2], false));
        assert_eq!(newest([], 5), (vec![], false));
    }
}
