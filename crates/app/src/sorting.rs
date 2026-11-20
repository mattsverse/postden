//! Sorting mail in the background: [`Sorter`] learns from the mail cached, on a thread of its own
//! with a database connection of its own, each time it's asked to, and hands back what
//! [`postden_ai::sort`] says of each inbox message.
//!
//! What it learned is kept in the database, so it outlives the cache and the app. It's kept in
//! memory too, as the sorter is the only one learning: it's read again only once something else
//! forgot or replaced it, which [`Store::sorting_generation`] tells.

use std::collections::{HashMap, HashSet};
use std::path::Path;
use std::sync::mpsc::{Receiver, Sender, channel};
use std::time::Duration;

use jiff::Timestamp;
use postden_ai::{Class, Example, FEATURES, Kind, Learning, SETTLED_AFTER, Weight, sort};
pub use postden_ai::{Tags, is_filed_in};
use postden_store::{
    AutosortState, Sorting, SortingChanges, SortingClass, SortingTaught, SortingWeight, Store,
};

use crate::Result;
use crate::settings::Settings;

/// How much of a body is learned from, in characters.
const TEXT_CHARS: usize = 4000;
/// How long the sorter waits once asked, so mail saved in a burst, as when every mailbox syncs at
/// start, is learned from once.
const SETTLE: Duration = Duration::from_millis(300);
/// What [`Sorter`] says of the inbox messages it tagged.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Sorted {
    /// Each tagged message's tags, by account, mailbox and UID; those without a tag aren't in it.
    pub tags: HashMap<(String, String, u32), Tags>,
    /// The UIDVALIDITY each of their mailboxes was cached under, by account and mailbox: their
    /// UIDs name the messages tagged only while it holds.
    pub uid_validity: HashMap<(String, String), u32>,
    /// The tagged messages to move by themselves to where they're suggested to go, as that's a
    /// mailbox autosorted to, by account, mailbox and UID, each with its Message-ID.
    pub autosort: Vec<Autosort>,
}

/// An inbox message to move by itself to where sorting says it goes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Autosort {
    /// The message, by account, mailbox and UID.
    pub key: (String, String, u32),
    /// Its Message-ID, which autosort notes it moved by, so it's moved once.
    pub message_id: String,
    /// Where it goes: the mailbox it's suggested to go to.
    pub to: String,
}

/// Learns where mail goes and what matters from the mail cached, in the background.
#[derive(Debug)]
pub struct Sorter {
    asks: Sender<()>,
}

impl Sorter {
    /// Starts sorting the mail cached in the database at `database`, which [`Sorter::sort`]
    /// asks for, while the settings say to; each time, `sorted` gets every inbox message's tags,
    /// and the generation of what was learned that says so, as [`Store::sorting_generation`]
    /// counts: of another generation since, they're stale. It stops once dropped.
    ///
    /// # Errors
    ///
    /// Returns an error when the database can't be opened or the thread can't be started.
    pub fn start(database: &Path, sorted: impl Fn(u64, Sorted) + Send + 'static) -> Result<Self> {
        // A connection of its own: SQLite connections can't be shared across threads.
        let store = Store::open(database)?;
        let (asks, asked) = channel();
        std::thread::Builder::new()
            .name("postden-sorter".into())
            .spawn(move || run(store, &asked, &sorted))?;
        Ok(Self { asks })
    }

    /// Learns from the mail cached again, and tags the inbox by what it says, soon. Asked again
    /// meanwhile, it does so once.
    pub fn sort(&self) {
        let _stopped = self.asks.send(());
    }
}

fn run(mut store: Store, asked: &Receiver<()>, sorted: &impl Fn(u64, Sorted)) {
    let mut learner = Learner::default();
    while asked.recv().is_ok() {
        std::thread::sleep(SETTLE);
        while asked.try_recv().is_ok() {}
        // Unreadable, the cache is tried again when next asked; the tags shown stay as they were.
        if let Ok(Some(tags)) = learner.sort(&mut store, Timestamp::now().as_second())
            && let Some((generation, _)) = &learner.kept
        {
            sorted(*generation, tags);
        }
    }
}

/// What was learned, as kept in a store, and from which generation of it.
#[derive(Debug, Default)]
pub(crate) struct Learner {
    kept: Option<(u64, Learning)>,
}

impl Learner {
    /// Learns from the mail cached in `store` what wasn't learned yet, saves it there, and says
    /// what it learned of each inbox message, at `now` (Unix seconds). `None`, having saved
    /// nothing, when sorting is off, or what it learned from was forgotten or replaced meanwhile,
    /// so what it says is stale.
    ///
    /// # Errors
    ///
    /// Returns an error when the store can't be read or written. What was learned is read again
    /// next time.
    pub(crate) fn sort(&mut self, store: &mut Store, now: i64) -> Result<Option<Sorted>> {
        let sorted = self.sort_unsaved(store, now);
        if sorted.is_err() {
            self.kept = None;
        }
        sorted
    }

    fn sort_unsaved(&mut self, store: &mut Store, now: i64) -> Result<Option<Sorted>> {
        // Learned by features no message has anymore, it's learned again from scratch.
        store.keep_sorting_learned_by(FEATURES)?;
        let generation = store.sorting_generation()?;
        // Turned off meanwhile, nothing is learned of the mail cached since. Read after the
        // generation: turning it off renews that once it's saved, so a pass that still read it
        // on has its learning refused.
        if !Settings::load(store)?.sort_mail {
            return Ok(None);
        }
        let mut learning = match self.kept.take() {
            Some((kept, learning)) if kept == generation => learning,
            _ => learning_from(&store.sorting()?),
        };
        let mut mailboxes = HashMap::new();
        for account in store.accounts()? {
            let names: HashSet<String> = store
                .mailboxes(&account.id)?
                .into_iter()
                .map(|m| m.name)
                .collect();
            mailboxes.insert(account.id, names);
        }
        // Read at once: a move landing in between would leave the message moved while still in
        // the inbox, which would teach that it was moved back, or moved here by hand, which
        // would teach where autosort put it.
        let (autosorted, cached) = store.autosorted_and_learning_messages(TEXT_CHARS)?;
        let cached_in: HashSet<(&str, &str, &str)> = cached
            .iter()
            .filter_map(|l| {
                let m = &l.message;
                Some((
                    m.account.as_str(),
                    m.envelope.message_id.as_deref()?,
                    m.mailbox.as_str(),
                ))
            })
            .collect();
        let autosorted_to = |account: &str, id: Option<&str>| {
            let id = id?;
            let a = autosorted.get(&(account.to_owned(), id.to_owned()))?;
            // Refused, or kept, it stayed where it was: what's done with it since was done by
            // hand. Whether it moved unknown, it did if it's where it was going.
            let moved = match a.state {
                AutosortState::Refused | AutosortState::Kept => return None,
                AutosortState::Unknown if !cached_in.contains(&(account, id, &a.mailbox)) => {
                    return None;
                }
                AutosortState::Moving => false,
                AutosortState::Moved | AutosortState::Unknown => true,
            };
            Some(postden_ai::Autosorted {
                to: &a.mailbox,
                moved,
            })
        };
        let examples: Vec<Example<'_>> = cached
            .iter()
            .map(|l| Example {
                account: &l.message.account,
                mailbox: &l.message.mailbox,
                role: l.role,
                envelope: &l.message.envelope,
                text: l.text.as_deref(),
                autosorted: autosorted_to(
                    &l.message.account,
                    l.message.envelope.message_id.as_deref(),
                ),
                moved_here: l.moved_here,
            })
            .collect();
        let sorting = sort(&mut learning, &examples, &mailboxes, now);
        let changes = changes(&learning, &sorting.changes);
        if changes != SortingChanges::default() && !store.save_sorting(generation, &changes)? {
            // What was learned from was forgotten or replaced meanwhile: it's read again next
            // time.
            return Ok(None);
        }
        let autosort_to = store.autosort_mailboxes()?;
        let mut sorted = Sorted::default();
        for (l, tags) in cached.into_iter().zip(sorting.tags) {
            if tags.is_empty() {
                continue;
            }
            let m = l.message;
            let autosort = autosort(&learning, &autosort_to, &autosorted, &m, &tags, now);
            if let Some(autosort) = autosort {
                sorted.autosort.push(autosort);
            }
            sorted
                .uid_validity
                .insert((m.account.clone(), m.mailbox.clone()), l.uid_validity);
            sorted
                .tags
                .insert((m.account, m.mailbox, m.envelope.uid), tags);
        }
        self.kept = Some((generation, learning));
        Ok(Some(sorted))
    }
}

/// Whether to move the inbox message `m`, tagged with `tags`, by itself, and where: to where it's
/// suggested to go, as that's one of `autosort_to`'s mailboxes. Only mail still waiting to be
/// filed is: not mail that already stayed in the inbox long enough to be learned as staying, or
/// that was moved back there, or that's older than that at `now` (Unix seconds) anyway; and
/// not mail autosort moves, moved or tried to before, as in `autosorted`.
fn autosort(
    learning: &Learning,
    autosort_to: &HashSet<(String, String)>,
    autosorted: &HashMap<(String, String), postden_store::Autosorted>,
    m: &postden_store::StoredMessage,
    tags: &Tags,
    now: i64,
) -> Option<Autosort> {
    let to = tags.suggested.as_ref()?;
    let id = m.envelope.message_id.as_ref()?;
    let account = &m.account;
    let recent = m.envelope.date.is_some_and(|d| now - d < SETTLED_AFTER);
    let waiting = recent
        && learning
            .accounts
            .get(account)
            .is_none_or(|l| !l.folders.taught.contains_key(id));
    let wanted = autosort_to.contains(&(account.clone(), to.clone()));
    let again = autosorted.contains_key(&(account.clone(), id.clone()));
    (waiting && wanted && !again).then(|| Autosort {
        key: (account.clone(), m.mailbox.clone(), m.envelope.uid),
        message_id: id.clone(),
        to: to.clone(),
    })
}

/// What was learned, as `sorting` keeps it. What a newer Postden learned, of models this one
/// doesn't have, is left out.
#[must_use]
fn learning_from(sorting: &Sorting) -> Learning {
    let mut learning = Learning::default();
    for c in &sorting.classes {
        let Some(kind) = Kind::from_name(&c.model) else {
            continue;
        };
        let model = learning
            .accounts
            .entry(c.account.clone())
            .or_default()
            .model_mut(kind);
        model.add_class(Class {
            name: c.name.clone(),
            messages: c.messages,
            guessed: c.guessed,
            right: c.correct,
        });
        for w in &c.weights {
            model.set_weight(&c.name, w.feature, Weight { z: w.z, n: w.n });
        }
    }
    for t in &sorting.taught {
        if let Some(kind) = Kind::from_name(&t.model)
            && let Some(learned) = learning.accounts.get_mut(&t.account)
        {
            let model = learned.model_mut(kind);
            model.taught.insert(t.message_id.clone(), t.class.clone());
        }
    }
    learning
}

/// The class `class` of `account`'s `kind` of model, as kept, with `weights` of its own.
fn kept_class(
    account: &str,
    kind: Kind,
    class: &Class,
    weights: Vec<SortingWeight>,
) -> SortingClass {
    SortingClass {
        account: account.to_owned(),
        model: kind.as_str().to_owned(),
        name: class.name.clone(),
        messages: class.messages,
        guessed: class.guessed,
        correct: class.right,
        weights,
    }
}

/// What changed in `learning`, as `touched` says, to save.
fn changes(
    learning: &Learning,
    touched: &HashMap<(String, Kind), postden_ai::Touched>,
) -> SortingChanges {
    let mut changes = SortingChanges::default();
    for ((account, kind), touched) in touched {
        let model_name = kind.as_str();
        changes.removed.extend(
            touched
                .removed
                .iter()
                .map(|c| (account.clone(), model_name.to_owned(), c.clone())),
        );
        let Some(model) = learning.accounts.get(account).map(|l| l.model(*kind)) else {
            continue;
        };
        let mut weights: HashMap<&str, Vec<SortingWeight>> = HashMap::new();
        for (class, features) in &touched.weights {
            let kept = features.iter().filter_map(|&feature| {
                let w = model.weight(class, feature)?;
                Some(SortingWeight {
                    feature,
                    z: w.z,
                    n: w.n,
                })
            });
            weights.insert(class, kept.collect());
        }
        let names: HashSet<&str> = touched
            .classes
            .iter()
            .map(String::as_str)
            .chain(weights.keys().copied())
            .collect();
        for name in names {
            if let Some(class) = model.class(name) {
                let weights = weights.remove(name).unwrap_or_default();
                changes
                    .classes
                    .push(kept_class(account, *kind, class, weights));
            }
        }
        for id in &touched.taught {
            if let Some(class) = model.taught.get(id) {
                changes.taught.push(SortingTaught {
                    account: account.clone(),
                    model: model_name.to_owned(),
                    message_id: id.clone(),
                    class: class.clone(),
                });
            }
        }
    }
    changes
}

#[cfg(test)]
mod tests {
    use postden_core::message::{Address, Envelope, Role};

    use super::*;
    use crate::google_account;

    const DAY: i64 = 24 * 60 * 60;
    const NOW: i64 = 1_800_000_000;

    /// Everything `learning` has, as kept.
    fn kept_learning(learning: &Learning) -> Sorting {
        let mut sorting = Sorting::default();
        for (account, learned) in &learning.accounts {
            for kind in [Kind::Folders, Kind::Importance] {
                let model = learned.model(kind);
                for (class, weights) in model.classes.iter().zip(model.weights_by_class()) {
                    let weights = weights
                        .into_iter()
                        .map(|(feature, w)| SortingWeight {
                            feature,
                            z: w.z,
                            n: w.n,
                        })
                        .collect();
                    sorting
                        .classes
                        .push(kept_class(account, kind, class, weights));
                }
                sorting
                    .taught
                    .extend(model.taught.iter().map(|(id, class)| SortingTaught {
                        account: account.clone(),
                        model: kind.as_str().to_owned(),
                        message_id: id.clone(),
                        class: class.clone(),
                    }));
            }
        }
        sorting
    }

    fn envelope(uid: u32, from: &str, subject: &str, date: i64) -> Envelope {
        Envelope {
            uid,
            message_id: Some(format!("{uid}@example.com")),
            from: Address {
                name: None,
                email: Some(from.into()),
            },
            to: Vec::new(),
            subject: Some(subject.into()),
            date: Some(date),
            seen: true,
            flagged: false,
        }
    }

    /// Receipts filed, and ordinary mail kept in the inbox, with a receipt just in among it.
    fn cache_receipts(store: &mut Store, me: &str) {
        store.save_account(&google_account(me, None)).unwrap();
        let old = NOW - 10 * DAY;
        let receipts: Vec<_> = (1..=8)
            .map(|uid| envelope(uid, "orders@shop.example", "Your receipt", old))
            .collect();
        store
            .save_messages(me, "Receipts", Role::Other, 1, &receipts)
            .unwrap();
        let mut inbox: Vec<_> = (10..20)
            .map(|uid| {
                let from = format!("pal{uid}@p{uid}.example");
                envelope(uid, &from, &format!("hi {uid}"), old)
            })
            .collect();
        inbox.push(envelope(
            30,
            "orders@shop.example",
            "Your receipt",
            NOW - DAY,
        ));
        store
            .save_messages(me, "INBOX", Role::Inbox, 1, &inbox)
            .unwrap();
    }

    fn receipt(me: &str) -> (String, String, u32) {
        (me.to_owned(), "INBOX".to_owned(), 30)
    }

    #[test]
    fn inbox_mail_is_tagged_by_the_mail_cached() {
        let mut store = Store::open_in_memory().unwrap();
        let me = "me@example.com";
        cache_receipts(&mut store, me);
        let sorted = Learner::default()
            .sort(&mut store, NOW)
            .unwrap()
            .unwrap()
            .tags;
        assert_eq!(
            sorted
                .get(&receipt(me))
                .and_then(|t| t.suggested.as_deref()),
            Some("Receipts"),
            "{sorted:?}"
        );
        assert_eq!(sorted.len(), 1, "only what has a tag: {sorted:?}");
    }

    #[test]
    fn mail_is_autosorted_only_to_the_mailboxes_chosen() {
        let mut store = Store::open_in_memory().unwrap();
        let me = "me@example.com";
        cache_receipts(&mut store, me);
        let mut learner = Learner::default();
        let sorted = learner.sort(&mut store, NOW).unwrap().unwrap();
        assert!(sorted.autosort.is_empty(), "none chosen: {sorted:?}");
        store.set_autosort(me, "Receipts", true).unwrap();
        let sorted = learner.sort(&mut store, NOW).unwrap().unwrap();
        assert_eq!(
            sorted.autosort,
            vec![Autosort {
                key: receipt(me),
                message_id: "30@example.com".into(),
                to: "Receipts".into(),
            }],
            "only the receipt just in, not the mail kept in the inbox"
        );
    }

    #[test]
    fn mail_is_autosorted_once() {
        let mut store = Store::open_in_memory().unwrap();
        let me = "me@example.com";
        cache_receipts(&mut store, me);
        store.set_autosort(me, "Receipts", true).unwrap();
        // Moved back to the inbox since: it stays there.
        assert!(
            store
                .note_autosorted(me, "30@example.com", "Receipts")
                .unwrap()
        );
        let sorted = Learner::default().sort(&mut store, NOW).unwrap().unwrap();
        assert!(sorted.autosort.is_empty(), "{sorted:?}");
    }

    #[test]
    fn mail_autosort_may_have_moved_before_a_restart_isnt_learned_from_where_it_went() {
        let mut store = Store::open_in_memory().unwrap();
        let me = "me@example.com";
        cache_receipts(&mut store, me);
        // Moved before the app quit, unanswered: it's in Receipts.
        store
            .note_autosorted(me, "30@example.com", "Receipts")
            .unwrap();
        store.settle_autosorts().unwrap();
        store.remove_message(me, "INBOX", 30).unwrap();
        let moved = envelope(31, "orders@shop.example", "Your receipt", NOW - DAY);
        let moved = Envelope {
            message_id: Some("30@example.com".into()),
            ..moved
        };
        store
            .save_messages(me, "Receipts", Role::Other, 1, &[moved])
            .unwrap();
        let mut learner = Learner::default();
        let _sorted = learner.sort(&mut store, NOW).unwrap().unwrap();
        let taught = store.sorting().unwrap().taught;
        assert!(
            !taught.iter().any(|t| t.message_id == "30@example.com"),
            "{taught:?}"
        );
    }

    #[test]
    fn mail_kept_in_the_inbox_isnt_autosorted() {
        let mut store = Store::open_in_memory().unwrap();
        let me = "me@example.com";
        cache_receipts(&mut store, me);
        store.set_autosort(me, "Receipts", true).unwrap();
        // The receipt just in, left in the inbox for days, is learned as staying.
        let later = NOW + 5 * DAY;
        let sorted = Learner::default().sort(&mut store, later).unwrap().unwrap();
        assert!(sorted.autosort.is_empty(), "{sorted:?}");
    }

    #[test]
    fn what_was_learned_is_kept_as_it_was_learned() {
        let mut store = Store::open_in_memory().unwrap();
        let me = "me@example.com";
        cache_receipts(&mut store, me);
        let mut learner = Learner::default();
        let _sorted = learner.sort(&mut store, NOW).unwrap().unwrap().tags;
        let (_, kept) = learner.kept.clone().unwrap();
        assert!(!kept.accounts.is_empty());
        // In any order: which class or weight comes first changes nothing.
        let in_order = |learning: &Learning| {
            let mut kept = kept_learning(learning);
            kept.classes.sort_by(|a, b| a.name.cmp(&b.name));
            for class in &mut kept.classes {
                class.weights.sort_by_key(|w| w.feature);
            }
            kept.taught.sort_by(|a, b| a.message_id.cmp(&b.message_id));
            kept
        };
        let stored = store.sorting().unwrap();
        assert_eq!(in_order(&learning_from(&stored)), in_order(&kept));
        assert!(stored.classes.iter().all(|c| !c.weights.is_empty()));
        // A learner of its own, as after a restart, reads it: and the cache gone, it still knows.
        for uid in 1..=8 {
            store.remove_message(me, "Receipts", uid).unwrap();
        }
        let sorted = Learner::default()
            .sort(&mut store, NOW)
            .unwrap()
            .unwrap()
            .tags;
        assert_eq!(
            sorted
                .get(&receipt(me))
                .and_then(|t| t.suggested.as_deref()),
            Some("Receipts")
        );
    }

    #[test]
    fn turned_off_it_learns_nothing() {
        let mut store = Store::open_in_memory().unwrap();
        cache_receipts(&mut store, "me@example.com");
        let off = Settings {
            sort_mail: false,
            ..Settings::default()
        };
        off.save(&store).unwrap();
        assert_eq!(Learner::default().sort(&mut store, NOW).unwrap(), None);
        assert_eq!(store.sorting().unwrap(), Sorting::default());
    }

    #[test]
    fn forgotten_it_learns_again_from_the_cache() {
        let mut store = Store::open_in_memory().unwrap();
        let me = "me@example.com";
        cache_receipts(&mut store, me);
        let mut learner = Learner::default();
        let _sorted = learner.sort(&mut store, NOW).unwrap().unwrap().tags;
        store.forget_sorting().unwrap();
        for uid in 1..=8 {
            store.remove_message(me, "Receipts", uid).unwrap();
        }
        let sorted = learner.sort(&mut store, NOW).unwrap().unwrap().tags;
        assert!(
            sorted.is_empty(),
            "not from what it learned before: {sorted:?}"
        );
    }

    #[test]
    fn what_was_learned_by_other_features_is_forgotten() {
        let mut store = Store::open_in_memory().unwrap();
        let me = "me@example.com";
        cache_receipts(&mut store, me);
        let _sorted = Learner::default()
            .sort(&mut store, NOW)
            .unwrap()
            .unwrap()
            .tags;
        store.set_setting("sorting_features", "0").unwrap();
        for uid in 1..=8 {
            store.remove_message(me, "Receipts", uid).unwrap();
        }
        let sorted = Learner::default()
            .sort(&mut store, NOW)
            .unwrap()
            .unwrap()
            .tags;
        assert!(sorted.is_empty(), "{sorted:?}");
    }

    #[test]
    fn the_sorter_sorts_in_the_background_once_asked() {
        let dir = std::env::temp_dir().join(format!("postden-sorter-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let database = dir.join("postden.sqlite3");
        let me = "me@example.com";
        let mut store = Store::open(&database).unwrap();
        store.save_account(&google_account(me, None)).unwrap();
        // Mail of now, as the sorter sorts at the time it is.
        let now = Timestamp::now().as_second();
        let old = now - 10 * DAY;
        let receipts: Vec<_> = (1..=8)
            .map(|uid| envelope(uid, "orders@shop.example", "Your receipt", old))
            .collect();
        store
            .save_messages(me, "Receipts", Role::Other, 1, &receipts)
            .unwrap();
        let mut inbox: Vec<_> = (10..20)
            .map(|uid| envelope(uid, &format!("pal{uid}@p{uid}.example"), "hi", old))
            .collect();
        inbox.push(envelope(30, "orders@shop.example", "Your receipt", now));
        store
            .save_messages(me, "INBOX", Role::Inbox, 1, &inbox)
            .unwrap();
        let (tx, rx) = channel();
        let sorter =
            Sorter::start(&database, move |_, sorted| tx.send(sorted.tags).unwrap()).unwrap();
        // Asked twice at once, it sorts once.
        sorter.sort();
        sorter.sort();
        let tags = rx.recv_timeout(Duration::from_secs(10)).unwrap();
        assert!(tags.contains_key(&receipt(me)), "{tags:?}");
        assert!(
            rx.recv_timeout(SETTLE * 3).is_err(),
            "the second ask came while settling"
        );
        drop(sorter);
        drop(store);
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
