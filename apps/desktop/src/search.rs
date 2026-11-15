//! Searching mail: the cache as the query is typed, and the server once it's submitted.

use std::cmp::Reverse;
use std::collections::{HashMap, HashSet};

use postden_core::message::Envelope;
use postden_store::StoredMessage;
use rune::TextEdit;
use rune_kit::search::Fold;

/// A mailbox, by account id and name.
pub(crate) type MailboxKey = (String, String);

/// A message, by account id, mailbox name and UID.
type MessageKey = (String, String, u32);

/// How the search on the server is going, for the query as it is now.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ServerSearch {
    /// The server wasn't asked about this query.
    NotAsked,
    Running,
    /// Some mailboxes couldn't be searched: this many.
    Failed(usize),
    /// Every mailbox was searched.
    Done,
}

/// A server match shown read or unread before the server agreed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Unconfirmed {
    /// What the server last agreed to, or what it showed before the first change.
    confirmed: bool,
    /// Changes the server hasn't answered yet.
    pending: usize,
    /// What the latest change showed.
    shown: bool,
}

/// How many matches the server sends back from each mailbox searched.
pub(crate) const SERVER_PAGE: usize = 100;

/// The search over the message list.
pub(crate) struct Search {
    pub(crate) query: TextEdit,
    /// Searches every inbox rather than what the list shows.
    pub(crate) everywhere: bool,
    /// Counts searches on the server, so results from one that was replaced are dropped.
    pub(crate) id: u64,
    /// The query the server was asked, while its results still stand.
    pub(crate) asked: Option<String>,
    /// Mailboxes the server is still searching.
    pub(crate) pending: HashSet<MailboxKey>,
    /// Mailboxes the server searched, whether it found anything or not.
    searched: HashSet<MailboxKey>,
    /// Mailboxes the server couldn't search.
    pub(crate) failed: HashSet<MailboxKey>,
    /// Messages among what the server found that were shown read or unread before the server
    /// agreed. Kept when the server is searched again, as the answers are still to come and the
    /// new search may bring the message back as it was before the change; and once answered,
    /// kept while a search that may have seen it before then is still out.
    unconfirmed: HashMap<MessageKey, Unconfirmed>,
    /// What the server found, cached or not.
    pub(crate) found: Vec<StoredMessage>,
    /// A mailbox had more matches on the server than it sent.
    pub(crate) more: bool,
}

impl Default for Search {
    fn default() -> Self {
        Self {
            query: TextEdit::new(""),
            everywhere: false,
            id: 0,
            asked: None,
            pending: HashSet::new(),
            searched: HashSet::new(),
            failed: HashSet::new(),
            unconfirmed: HashMap::new(),
            found: Vec::new(),
            more: false,
        }
    }
}

impl Search {
    /// The query, trimmed, while there is one.
    pub(crate) fn query(&self) -> Option<&str> {
        Some(self.query.text.trim()).filter(|q| !q.is_empty())
    }

    /// Whether the list shows search results.
    pub(crate) fn is_active(&self) -> bool {
        self.query().is_some()
    }

    /// Whether the server was asked about the query as it is now.
    pub(crate) fn asked_server(&self) -> bool {
        self.asked.is_some() && self.asked.as_deref() == self.query()
    }

    /// How the search on the server is going, for the query as it is now.
    pub(crate) fn server(&self) -> ServerSearch {
        if !self.asked_server() {
            ServerSearch::NotAsked
        } else if !self.pending.is_empty() {
            ServerSearch::Running
        } else if self.failed.is_empty() {
            ServerSearch::Done
        } else {
            ServerSearch::Failed(self.failed.len())
        }
    }

    /// Drops what the server found, and ignores what it's still searching.
    pub(crate) fn forget_server(&mut self) {
        self.id += 1;
        self.asked = None;
        self.pending.clear();
        self.searched.clear();
        self.failed.clear();
        self.found.clear();
        self.more = false;
    }

    /// The server answered for a mailbox it was searching: what it found, or why it couldn't.
    /// Returns whether the answer was awaited; one for a mailbox that started over or went
    /// meanwhile isn't.
    pub(crate) fn answered(
        &mut self,
        account: &str,
        mailbox: &str,
        result: Result<(Vec<StoredMessage>, bool), ()>,
    ) -> bool {
        let key = (account.to_owned(), mailbox.to_owned());
        if !self.pending.remove(&key) {
            return false;
        }
        match result {
            Ok((found, more)) => {
                self.more |= more;
                // A message changed before this search came back shows the change still awaited.
                self.found.extend(found.into_iter().map(|mut m| {
                    let key = (m.account.clone(), m.mailbox.clone(), m.envelope.uid);
                    if let Some(change) = self.unconfirmed.get(&key) {
                        m.envelope.seen = change.shown;
                    }
                    m
                }));
                self.searched.insert(key);
            }
            Err(()) => {
                self.failed.insert(key);
            }
        }
        if self.pending.is_empty() {
            // Every search out came back, so none can still bring a message as it was before a
            // change the server answered.
            drop_answered(&mut self.unconfirmed);
        }
        true
    }

    /// Drops what the server found in a mailbox that started over, as its UIDs now name other
    /// messages. If it was searched, or being searched, that counts as failed, so it can be
    /// searched again.
    pub(crate) fn forget_mailbox(&mut self, account: &str, mailbox: &str) {
        let key = (account.to_owned(), mailbox.to_owned());
        self.drop_results(|a, m| a == account && m == mailbox);
        if self.searched.remove(&key) | self.pending.remove(&key) {
            self.failed.insert(key);
        }
    }

    /// Forgets mailboxes for which `gone` holds, such as ones deleted or renamed on the server:
    /// what was found there, and whether they were searched. Returns whether any were.
    pub(crate) fn forget_gone(&mut self, gone: impl Fn(&str, &str) -> bool) -> bool {
        let before = (
            self.found.len(),
            self.pending.len(),
            self.searched.len(),
            self.failed.len(),
        );
        self.drop_results(&gone);
        for set in [&mut self.pending, &mut self.searched, &mut self.failed] {
            set.retain(|(a, m)| !gone(a, m));
        }
        before
            != (
                self.found.len(),
                self.pending.len(),
                self.searched.len(),
                self.failed.len(),
            )
    }

    fn drop_results(&mut self, gone: impl Fn(&str, &str) -> bool) {
        self.found.retain(|m| !gone(&m.account, &m.mailbox));
        self.unconfirmed.retain(|(a, m, _), _| !gone(a, m));
    }

    /// Shows a message read or unread among what the server found, before the server agrees.
    /// [`Search::settle`] keeps it or takes it back, once the server answers.
    pub(crate) fn set_seen(&mut self, account: &str, mailbox: &str, uid: u32, seen: bool) {
        for m in &mut self.found {
            if m.account == account && m.mailbox == mailbox && m.envelope.uid == uid {
                let change = self
                    .unconfirmed
                    .entry((account.to_owned(), mailbox.to_owned(), uid))
                    .or_insert(Unconfirmed {
                        confirmed: m.envelope.seen,
                        pending: 0,
                        shown: seen,
                    });
                change.pending += 1;
                change.shown = seen;
                m.envelope.seen = seen;
            }
        }
    }

    /// The server answered a change [`Search::set_seen`] showed: `Some` with what it agreed to,
    /// or `None` if it refused. Once every change is answered the message shows what the server
    /// last agreed to.
    pub(crate) fn settle(&mut self, account: &str, mailbox: &str, uid: u32, agreed: Option<bool>) {
        let key = (account.to_owned(), mailbox.to_owned(), uid);
        let Some(change) = self.unconfirmed.get_mut(&key) else {
            return;
        };
        if let Some(seen) = agreed {
            change.confirmed = seen;
        }
        change.pending = change.pending.saturating_sub(1);
        if change.pending > 0 {
            return;
        }
        let confirmed = change.confirmed;
        change.shown = confirmed;
        // A search still out may have seen the message before the server changed it.
        if self.pending.is_empty() {
            self.unconfirmed.remove(&key);
        }
        for m in &mut self.found {
            if m.account == account && m.mailbox == mailbox && m.envelope.uid == uid {
                m.envelope.seen = confirmed;
            }
        }
    }

    /// `cached` matches with what the server found, each message once, newest first, and at most
    /// `limit` of them. The cache's copy of a message wins, as it follows what's read.
    pub(crate) fn merge(&self, cached: Vec<StoredMessage>, limit: usize) -> Vec<StoredMessage> {
        let known: HashSet<(&str, &str, u32)> = cached
            .iter()
            .map(|m| (m.account.as_str(), m.mailbox.as_str(), m.envelope.uid))
            .collect();
        let extra: Vec<StoredMessage> = self
            .found
            .iter()
            .filter(|m| !known.contains(&(m.account.as_str(), m.mailbox.as_str(), m.envelope.uid)))
            .cloned()
            .collect();
        let mut all = cached;
        all.extend(extra);
        // As the store orders them: undated last, then newest first.
        all.sort_by_key(|m| {
            (
                m.envelope.date.is_none(),
                Reverse(m.envelope.date),
                Reverse(m.envelope.uid),
            )
        });
        all.truncate(limit);
        all
    }
}

/// Drops the changes the server has answered.
fn drop_answered(unconfirmed: &mut HashMap<MessageKey, Unconfirmed>) {
    unconfirmed.retain(|_, change| change.pending > 0);
}

/// Whether every word of `query` is in the message's subject, sender or recipients, ignoring case
/// and, for words typed without them, accents.
pub(crate) fn matches(envelope: &Envelope, query: &str) -> bool {
    let fold = Fold::default();
    let fields: Vec<&str> = [
        envelope.subject.as_deref(),
        envelope.from.name.as_deref(),
        envelope.from.email.as_deref(),
    ]
    .into_iter()
    .chain(
        envelope
            .to
            .iter()
            .flat_map(|a| [a.name.as_deref(), a.email.as_deref()]),
    )
    .flatten()
    .collect();
    query.split_whitespace().all(|word| {
        fields
            .iter()
            .any(|field| !fold.find(field, word).is_empty())
    })
}

#[cfg(test)]
mod tests {
    use postden_core::message::Address;

    use super::*;

    fn envelope(uid: u32, date: Option<i64>) -> Envelope {
        Envelope {
            uid,
            message_id: None,
            from: Address {
                name: Some("Adèle Martin".into()),
                email: Some("adele@example.com".into()),
            },
            to: vec![Address {
                name: None,
                email: Some("bob@work.example".into()),
            }],
            subject: Some("Quarterly Invoice".into()),
            date,
            seen: false,
        }
    }

    fn stored(account: &str, uid: u32, date: Option<i64>) -> StoredMessage {
        StoredMessage {
            account: account.into(),
            mailbox: "INBOX".into(),
            envelope: envelope(uid, date),
        }
    }

    #[test]
    fn every_word_must_match_some_field() {
        let e = envelope(1, None);
        assert!(matches(&e, "invoice"), "subject, any case");
        assert!(matches(&e, "adele"), "sender, without its accent");
        assert!(matches(&e, "work.example invoice"), "recipient and subject");
        assert!(!matches(&e, "invoice receipt"), "every word is needed");
        assert!(!matches(&e, "adéla"), "an accent typed is kept");
    }

    #[test]
    fn server_results_join_the_cached_ones_once_in_date_order() {
        let search = Search {
            found: vec![
                stored("a", 1, Some(10)),
                stored("a", 7, Some(30)),
                stored("b", 1, None),
            ],
            ..Search::default()
        };
        let mut cached = stored("a", 1, Some(10));
        cached.envelope.seen = true;
        let merged = search.merge(vec![stored("a", 5, Some(20)), cached], 10);
        let order: Vec<_> = merged
            .iter()
            .map(|m| (m.account.as_str(), m.envelope.uid, m.envelope.seen))
            .collect();
        assert_eq!(
            order,
            [
                ("a", 7, false),
                ("a", 5, false),
                ("a", 1, true),
                ("b", 1, false)
            ]
        );
        assert_eq!(search.merge(Vec::new(), 2).len(), 2, "cut to the limit");
    }

    fn seen(s: &Search, uid: u32) -> bool {
        s.found
            .iter()
            .find(|m| m.envelope.uid == uid)
            .unwrap()
            .envelope
            .seen
    }

    #[test]
    fn a_change_the_server_refused_is_taken_back() {
        let mut search = Search {
            found: vec![stored("a", 1, Some(10)), stored("a", 2, Some(20))],
            ..Search::default()
        };
        search.set_seen("a", "INBOX", 1, true);
        search.set_seen("a", "INBOX", 2, true);
        search.settle("a", "INBOX", 1, Some(true));
        search.settle("a", "INBOX", 2, None);
        assert!(seen(&search, 1), "agreed");
        assert!(!seen(&search, 2), "refused");
        search.settle("a", "INBOX", 2, Some(true));
        assert!(!seen(&search, 2), "an answer to nothing changes nothing");
    }

    #[test]
    fn overlapping_changes_end_as_the_server_last_agreed() {
        // Read, then marked unread and read again before either answer.
        let start = || {
            let mut s = Search {
                found: vec![stored("a", 1, Some(10))],
                ..Search::default()
            };
            s.found[0].envelope.seen = true;
            s.set_seen("a", "INBOX", 1, false);
            s.set_seen("a", "INBOX", 1, true);
            s
        };
        let mut search = start();
        search.settle("a", "INBOX", 1, Some(false));
        assert!(
            seen(&search, 1),
            "still showing the change awaiting an answer"
        );
        search.settle("a", "INBOX", 1, None);
        assert!(!seen(&search, 1), "unread, as the server agreed");

        let mut search = start();
        search.settle("a", "INBOX", 1, None);
        search.settle("a", "INBOX", 1, Some(false));
        assert!(!seen(&search, 1), "the same in the other order");

        let mut search = start();
        search.settle("a", "INBOX", 1, None);
        search.settle("a", "INBOX", 1, None);
        assert!(seen(&search, 1), "both refused: read, as before");
    }

    #[test]
    fn a_change_awaited_survives_searching_again() {
        let mut search = asked(vec![stored("a", 1, Some(10))]);
        search.set_seen("a", "INBOX", 1, true);
        search.forget_server();
        search.asked = Some("invoice".into());
        search.pending.insert(("a".into(), "INBOX".into()));
        // The new search saw it before the server marked it read.
        assert!(search.answered("a", "INBOX", Ok((vec![stored("a", 1, Some(10))], false))));
        assert!(seen(&search, 1), "shows the change awaited");
        search.settle("a", "INBOX", 1, None);
        assert!(!seen(&search, 1), "refused: unread again");
    }

    #[test]
    fn a_change_answered_while_a_search_is_out_still_shows_in_its_results() {
        let mut search = asked(vec![stored("a", 1, Some(10))]);
        search.set_seen("a", "INBOX", 1, true);
        search.forget_server();
        search.asked = Some("invoice".into());
        search.pending.insert(("a".into(), "INBOX".into()));
        search.pending.insert(("b".into(), "INBOX".into()));
        search.settle("a", "INBOX", 1, Some(true));
        // That search saw it before the server marked it read.
        assert!(search.answered("a", "INBOX", Ok((vec![stored("a", 1, Some(10))], false))));
        assert!(seen(&search, 1), "read, as the server agreed");
        assert!(search.answered("b", "INBOX", Ok((Vec::new(), false))));
        assert_eq!(
            search.unconfirmed.len(),
            0,
            "forgotten once no search is out"
        );
    }

    #[test]
    fn the_last_search_back_still_shows_a_change_answered_while_it_was_out() {
        let mut search = asked(vec![stored("a", 1, Some(10))]);
        search.set_seen("a", "INBOX", 1, true);
        search.forget_server();
        search.asked = Some("invoice".into());
        search.pending.insert(("a".into(), "INBOX".into()));
        search.settle("a", "INBOX", 1, Some(true));
        assert!(search.answered("a", "INBOX", Ok((vec![stored("a", 1, Some(10))], false))));
        assert!(seen(&search, 1), "read, though the search saw it unread");
        assert_eq!(search.unconfirmed.len(), 0, "and forgotten after");
    }

    fn asked(found: Vec<StoredMessage>) -> Search {
        let mut search = Search {
            query: TextEdit::new("invoice"),
            asked: Some("invoice".into()),
            ..Search::default()
        };
        for key in [("a", "INBOX"), ("b", "INBOX"), ("c", "INBOX")] {
            search.pending.insert((key.0.into(), key.1.into()));
        }
        assert_eq!(search.server(), ServerSearch::Running);
        let (a, b): (Vec<_>, Vec<_>) = found.into_iter().partition(|m| m.account == "a");
        assert!(search.answered("a", "INBOX", Ok((a, false))));
        assert!(search.answered("b", "INBOX", Ok((b, false))));
        search
    }

    #[test]
    fn a_mailbox_that_started_over_counts_as_failed_found_or_not() {
        let mut search = asked(vec![stored("a", 1, Some(10))]);
        assert!(search.answered("c", "INBOX", Err(())));
        assert_eq!(search.server(), ServerSearch::Failed(1));
        assert!(!search.answered("c", "INBOX", Err(())), "answered already");

        let mut search = asked(vec![stored("a", 1, Some(10))]);
        search.forget_mailbox("c", "INBOX");
        assert!(
            !search.answered("c", "INBOX", Ok((vec![stored("c", 1, None)], false))),
            "an answer from before it started over is dropped"
        );
        search.forget_mailbox("a", "INBOX");
        search.forget_mailbox("b", "INBOX");
        assert_eq!(search.found.len(), 0, "nothing found is left");
        assert_eq!(
            search.server(),
            ServerSearch::Failed(3),
            "b found nothing, but what it searched is gone too"
        );
    }

    #[test]
    fn a_mailbox_gone_from_the_server_is_forgotten() {
        let mut search = asked(vec![stored("a", 1, Some(10)), stored("b", 1, Some(20))]);
        assert!(search.forget_gone(|account, _| account == "c"));
        assert_eq!(
            search.server(),
            ServerSearch::Done,
            "nothing left to wait for"
        );
        assert!(search.forget_gone(|account, _| account == "a"));
        assert_eq!(search.found.len(), 1);
        assert!(
            !search.forget_gone(|account, _| account == "a"),
            "gone already"
        );
    }

    #[test]
    fn the_server_is_asked_again_once_the_query_changes() {
        let mut search = Search {
            query: TextEdit::new(" invoice "),
            ..Search::default()
        };
        assert!(!search.asked_server());
        search.asked = Some("invoice".into());
        assert!(search.asked_server(), "the query is trimmed");
        search.query = TextEdit::new("invoices");
        assert!(!search.asked_server());
        search.forget_server();
        assert_eq!(search.asked, None);
        assert_eq!(search.id, 1, "replies to the old search are ignored");
    }
}
