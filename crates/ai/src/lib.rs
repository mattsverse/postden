//! Sorting mail on this computer, by what was done with the mail already there: which mailbox
//! each inbox message seems to belong in, and which seem important.
//!
//! Nothing is sent anywhere, and nothing is labeled by hand. What the account does with its mail
//! is what's learned from, message by message, as it's seen in the cache: filed in a folder or
//! archived, kept in the inbox, starred, deleted, or replied to.
//!
//! - **Where mail goes.** Each account's mail in its folders and its Archive says what goes
//!   where, and mail kept in its inbox for a few days what stays; an inbox message is tagged
//!   with the mailbox it looks like it belongs in, to move it there.
//! - **What matters.** Starred mail, and mail from people the account has written to, is
//!   important; mail deleted, marked as spam, or left unread for a week isn't.
//!
//! Each is a model of its own for each account (see [`learner`]), kept in a [`Learning`] that
//! outlives the cache: what's learned from a message stays once it's gone from it. A message is
//! taught again only when what it says changes, as when it's moved; each time, the model is asked
//! first what it would have said, which is how often it's known to be right, and a tag is only
//! shown once that's often enough.

mod features;
mod learner;

use std::collections::{HashMap, HashSet};

use postden_core::message::{Envelope, Role};

pub use crate::learner::{Class, Model, Touched, Weight};

/// The version of what messages are judged by. A [`Learning`] learned by another knows features
/// no message has anymore, so it's learned again from scratch.
pub const FEATURES: u32 = 1;

/// How sure a model must be to tag a message.
const SURE: f64 = 0.8;
/// How many messages an inbox must have kept before mail in it is suggested to go elsewhere:
/// until then, there's no telling what stays.
const MIN_KEPT: u32 = 5;
/// How many important messages, and as many unimportant ones, must be learned before any mail
/// is called important.
const MIN_IMPORTANCE: u32 = 10;
/// How long mail can be left unread before that says it doesn't matter, in seconds.
const IGNORED_AFTER: i64 = 7 * 24 * 60 * 60;
/// How long mail stays in an inbox before that says it stays there, in seconds: it may just not
/// have been filed yet.
pub const SETTLED_AFTER: i64 = 3 * 24 * 60 * 60;

/// The classes of [`Kind::Importance`].
const IMPORTANT: &str = "important";
const UNIMPORTANT: &str = "unimportant";

/// A cached message to learn from, and, if it's in an inbox, to tag.
#[derive(Debug, Clone, Copy)]
pub struct Example<'a> {
    pub account: &'a str,
    pub mailbox: &'a str,
    pub role: Role,
    pub envelope: &'a Envelope,
    /// Its body's text, if it was downloaded.
    pub text: Option<&'a str>,
    /// Where autosort moves or moved it, if it does or did.
    pub autosorted: Option<Autosorted<'a>>,
    /// Whether it was moved to `mailbox` in this app lately, by hand or by autosort: put there,
    /// though just in.
    pub moved_here: bool,
}

/// Where autosort moves or moved a message.
///
/// It says nothing of where mail like it goes, as it wasn't filed by hand, until it's back in
/// the inbox and nowhere else autosort put it: moved back on purpose. Not while it's on its way,
/// nor while it's elsewhere: Gmail lists it under each of its labels, and where autosort put it
/// may not be cached yet.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Autosorted<'a> {
    pub to: &'a str,
    /// Whether the server moved it there.
    pub moved: bool,
}

/// What [`sort`] says of an inbox message.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Tags {
    /// It looks important.
    pub important: bool,
    /// The mailbox it looks like it belongs in, by name, to move it to.
    pub suggested: Option<String>,
}

impl Tags {
    #[must_use]
    pub const fn is_empty(&self) -> bool {
        !self.important && self.suggested.is_none()
    }
}

/// What an account's model tells apart.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum Kind {
    /// Which mailbox mail goes in: one class for each.
    Folders,
    /// Whether mail matters.
    Importance,
}

impl Kind {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Folders => "folders",
            Self::Importance => "importance",
        }
    }

    #[must_use]
    pub fn from_name(name: &str) -> Option<Self> {
        [Self::Folders, Self::Importance]
            .into_iter()
            .find(|k| k.as_str() == name)
    }
}

/// What an account's mail taught.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Learned {
    pub folders: Model,
    pub importance: Model,
}

impl Learned {
    #[must_use]
    pub const fn model(&self, kind: Kind) -> &Model {
        match kind {
            Kind::Folders => &self.folders,
            Kind::Importance => &self.importance,
        }
    }

    pub const fn model_mut(&mut self, kind: Kind) -> &mut Model {
        match kind {
            Kind::Folders => &mut self.folders,
            Kind::Importance => &mut self.importance,
        }
    }
}

/// What every account's mail taught, by account.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Learning {
    pub accounts: HashMap<String, Learned>,
}

/// What [`sort`] says, and what it learned.
#[derive(Debug, Clone, Default)]
pub struct Sorting {
    /// Each example's tags, in order.
    pub tags: Vec<Tags>,
    /// What changed in each account's models, to save.
    pub changes: HashMap<(String, Kind), Touched>,
}

/// What a message is to be taught as.
struct Lesson<'a> {
    account: &'a str,
    kind: Kind,
    id: &'a str,
    class: &'a str,
    /// Its example's index.
    example: usize,
}

/// Learns from `examples` what wasn't learned yet, then tags those in an inbox.
///
/// What's learned goes into `learning`; the examples not in an inbox get no tags. `mailboxes`
/// has every account's mailboxes, by name, as last listed: what's learned of an account or a
/// mailbox not among them is forgotten, unless the account's weren't listed yet. `now` is in Unix
/// seconds.
#[must_use]
#[expect(
    clippy::implicit_hasher,
    reason = "the mailboxes are only ever listed into std's maps"
)]
pub fn sort(
    learning: &mut Learning,
    examples: &[Example<'_>],
    mailboxes: &HashMap<String, HashSet<String>>,
    now: i64,
) -> Sorting {
    let mut changes: HashMap<(String, Kind), Touched> = HashMap::new();
    learning.accounts.retain(|a, _| mailboxes.contains_key(a));
    for (account, learned) in &mut learning.accounts {
        if let Some(listed) = mailboxes.get(account).filter(|l| !l.is_empty()) {
            let touched = changes.entry((account.clone(), Kind::Folders)).or_default();
            learned.folders.retain(|c| listed.contains(c), touched);
        }
    }

    let (senders, correspondents) = senders_and_correspondents(examples);
    let counted = counted(examples, learning);
    let labeled = gmail(mailboxes);
    let unfiled = put_by_autosort(examples, &labeled);
    let mut features = Vec::with_capacity(examples.len());
    let mut lessons = Vec::new();
    for (i, (e, &counts)) in examples.iter().zip(&counted).enumerate() {
        let from = lowercase(e.envelope.from.email.as_deref());
        let own = from
            .as_ref()
            .is_some_and(|f| senders.contains(&(e.account, f.clone())));
        let correspondent = from.is_some_and(|f| correspondents.contains(&(e.account, f)));
        let mut judged = features::features(e.envelope, e.text, correspondent);
        judged.push(learner::BIAS);
        features.push(judged);
        // What the account sent itself, such as a draft, says nothing about the mail it gets.
        let Some(id) = e.envelope.message_id.as_deref().filter(|_| counts && !own) else {
            continue;
        };
        let learned = learning.accounts.get(e.account);
        let taught = |kind| learned.and_then(|l| l.model(kind).taught.get(id));
        let by_hand = !unfiled.contains(&(e.account, id));
        if let Some(folder) = filed_in(e).filter(|_| by_hand) {
            let before = taught(Kind::Folders);
            let settled = is_settled(e, before.is_some(), labeled.contains(e.account), now);
            if settled && before.map(String::as_str) != Some(folder) {
                lessons.push(Lesson {
                    account: e.account,
                    kind: Kind::Folders,
                    id,
                    class: folder,
                    example: i,
                });
            }
        }
        if let Some(important) = importance(e, correspondent, now) {
            let class = if important { IMPORTANT } else { UNIMPORTANT };
            if taught(Kind::Importance).map(String::as_str) != Some(class) {
                lessons.push(Lesson {
                    account: e.account,
                    kind: Kind::Importance,
                    id,
                    class,
                    example: i,
                });
            }
        }
    }

    add_classes(learning, &mut lessons);

    // In no order the mail has, as learning from a folder's mail all at once, then another's,
    // would say each was the only one.
    lessons.sort_by_key(|l| features::hash(l.kind.as_str(), l.id));
    for lesson in lessons {
        let Some(features) = features.get(lesson.example) else {
            continue;
        };
        let model = learning
            .accounts
            .entry(lesson.account.to_owned())
            .or_default()
            .model_mut(lesson.kind);
        let touched = changes
            .entry((lesson.account.to_owned(), lesson.kind))
            .or_default();
        model.learn(lesson.class, features, SURE, touched);
        model
            .taught
            .insert(lesson.id.to_owned(), lesson.class.to_owned());
        touched.taught.insert(lesson.id.to_owned());
    }
    changes.retain(|_, t| {
        !(t.removed.is_empty()
            && t.classes.is_empty()
            && t.weights.is_empty()
            && t.taught.is_empty())
    });

    let tags = examples
        .iter()
        .zip(&features)
        .map(|(e, features)| {
            let learned = learning.accounts.get(e.account);
            if e.role != Role::Inbox {
                return Tags::default();
            }
            Tags {
                important: !e.envelope.flagged
                    && learned.is_some_and(|l| matters(&l.importance, features)),
                suggested: learned.and_then(|l| suggest(&l.folders, e.mailbox, features)),
            }
        })
        .collect();
    Sorting { tags, changes }
}

/// What `model` would answer for a message with `features`, if it's sure enough of anything.
fn guess<'a>(model: &'a Model, features: &[u64]) -> Option<&'a Class> {
    model
        .best(features)
        .filter(|(_, chance)| *chance >= SURE)
        .map(|(class, _)| class)
}

/// Adds the classes `lessons` teach to their models at once, and leaves out the lessons of a
/// model that would have only one: a class alone is sure of everything, so learns nothing.
fn add_classes(learning: &mut Learning, lessons: &mut Vec<Lesson<'_>>) {
    let mut taught: HashMap<(&str, Kind), HashSet<&str>> = HashMap::new();
    for l in lessons.iter() {
        taught
            .entry((l.account, l.kind))
            .or_default()
            .insert(l.class);
    }
    let mut alone = HashSet::new();
    for ((account, kind), names) in taught {
        let learned = learning.accounts.entry(account.to_owned()).or_default();
        let model = learned.model_mut(kind);
        let new: Vec<&str> = names
            .into_iter()
            .filter(|n| model.class(n).is_none())
            .collect();
        if model.classes.len() + new.len() < 2 {
            alone.insert((account, kind));
            continue;
        }
        for name in new {
            model.add_class(Class::new(name));
        }
    }
    lessons.retain(|l| !alone.contains(&(l.account, l.kind)));
}

/// The mailbox other than `inbox` that a message in it with `features` belongs in, if the
/// account's model of where mail goes is sure of it, and is right about it often enough.
fn suggest(folders: &Model, inbox: &str, features: &[u64]) -> Option<String> {
    let kept = folders.class(inbox).map_or(0, |c| c.messages);
    let best = guess(folders, features)?;
    (kept >= MIN_KEPT && best.name != inbox && best.is_trusted()).then(|| best.name.clone())
}

/// Whether a message with `features` looks important, if enough mail of both kinds was learned
/// and the model is right about it often enough.
fn matters(importance: &Model, features: &[u64]) -> bool {
    let enough = [IMPORTANT, UNIMPORTANT].iter().all(|c| {
        importance
            .class(c)
            .is_some_and(|c| c.messages >= MIN_IMPORTANCE)
    });
    enough && guess(importance, features).is_some_and(|c| c.name == IMPORTANT && c.is_trusted())
}

/// Whether a mailbox is one the server or a mail app keeps for itself, rather than a folder mail
/// is filed in: Gmail's own (Starred, Important, Drafts…), and drafts, templates and outboxes.
fn is_kept_by_the_app(mailbox: &str) -> bool {
    const GMAIL: [&str; 2] = ["[Gmail]/", "[Google Mail]/"];
    const KEPT: [&str; 4] = ["Drafts", "Draft", "Templates", "Outbox"];
    let last = mailbox.rsplit(['/', '.']).next().unwrap_or(mailbox);
    GMAIL.iter().any(|g| mailbox.starts_with(g))
        || KEPT.iter().any(|k| last.eq_ignore_ascii_case(k))
}

/// Which copy of a message counts, when the server lists it in several mailboxes, as Gmail lists
/// every message in All Mail too: lower ranks first; and of two alike, the one moved where it is
/// here, as Gmail keeps a label the message had beside the one it's filed in, else the one it was
/// taught as in `learning`, so it isn't taught otherwise once that move is forgotten.
fn rank(e: &Example<'_>, learning: &Learning) -> (u8, bool, bool) {
    let rank = match e.role {
        Role::Inbox => 0,
        Role::Other if !is_kept_by_the_app(e.mailbox) => 1,
        // Put away or snoozed, a message may still be listed in Gmail's All Mail.
        Role::Trash | Role::Junk => 2,
        Role::Snoozed => 3,
        Role::Archive => 4,
        Role::Sent | Role::Drafts | Role::Other => 5,
    };
    let taught_here = e.envelope.message_id.as_deref().is_some_and(|id| {
        learning
            .accounts
            .get(e.account)
            .and_then(|l| l.folders.taught.get(id))
            .is_some_and(|c| c == e.mailbox)
    });
    (rank, !e.moved_here, !taught_here)
}

/// Whether inbox mail can be suggested to go to a mailbox with `role`, named `mailbox`: the
/// Archive or a folder mail is filed in.
#[must_use]
pub fn is_filed_in(mailbox: &str, role: Role) -> bool {
    match role {
        Role::Archive => true,
        Role::Other => !is_kept_by_the_app(mailbox),
        _ => false,
    }
}

/// The mailbox a message says mail like it belongs in, if any: its own, if it's the inbox, the
/// Archive or a folder.
fn filed_in<'a>(e: &Example<'a>) -> Option<&'a str> {
    (e.role == Role::Inbox || is_filed_in(e.mailbox, e.role)).then_some(e.mailbox)
}

/// Whether a message says mail like it is important, isn't, or neither.
fn importance(e: &Example<'_>, correspondent: bool, now: i64) -> Option<bool> {
    let ignored = !e.envelope.seen && e.envelope.date.is_some_and(|d| now - d > IGNORED_AFTER);
    match e.role {
        Role::Sent | Role::Snoozed | Role::Drafts => None,
        // Such as a draft, starred or not: not mail that came.
        Role::Other if is_kept_by_the_app(e.mailbox) => None,
        _ if e.envelope.flagged && !matches!(e.role, Role::Trash | Role::Junk) => Some(true),
        Role::Trash | Role::Junk => Some(false),
        _ if correspondent => Some(true),
        _ if ignored => Some(false),
        _ => None,
    }
}

fn lowercase(email: Option<&str>) -> Option<String> {
    email.map(str::to_lowercase)
}

/// Addresses, lowercase, each with the account it's of.
type Addresses<'a> = HashSet<(&'a str, String)>;

/// Who each account sends mail as, and to, from its Sent mail.
fn senders_and_correspondents<'a>(examples: &[Example<'a>]) -> (Addresses<'a>, Addresses<'a>) {
    let mut senders = HashSet::new();
    let mut correspondents = HashSet::new();
    for e in examples.iter().filter(|e| e.role == Role::Sent) {
        senders.extend(lowercase(e.envelope.from.email.as_deref()).map(|s| (e.account, s)));
        let to = e
            .envelope
            .to
            .iter()
            .filter_map(|a| lowercase(a.email.as_deref()));
        correspondents.extend(to.map(|t| (e.account, t)));
    }
    (senders, correspondents)
}

/// Whether where `e` is says where it stays, at `now`, if it was `taught` before, in an account
/// whose mail is in each of its labels if `labeled`, as in Gmail.
///
/// Mail moved back to the inbox stays there, as far as anyone can tell, as does mail taken back
/// out of where autosort put it, or moved anywhere in this app; other mail just in may yet be
/// filed. So may mail just in that an archive lists, or, in Gmail, any label: All Mail and a
/// filter's label list the inbox's mail too, and may be cached before it.
fn is_settled(e: &Example<'_>, taught: bool, labeled: bool, now: i64) -> bool {
    let may_be_in_the_inbox = matches!(e.role, Role::Inbox | Role::Archive) || labeled;
    !may_be_in_the_inbox
        || taught
        || e.moved_here
        || e.autosorted.is_some()
        || e.envelope.date.is_some_and(|d| now - d >= SETTLED_AFTER)
}

/// The accounts that are Gmail's, as their mailboxes say: a message there is in each of its
/// labels, the inbox among them.
fn gmail(mailboxes: &HashMap<String, HashSet<String>>) -> HashSet<&str> {
    const GMAIL: [&str; 2] = ["[Gmail]/", "[Google Mail]/"];
    mailboxes
        .iter()
        .filter(|(_, names)| names.iter().any(|n| GMAIL.iter().any(|g| n.starts_with(g))))
        .map(|(account, _)| account.as_str())
        .collect()
}

/// The messages, by account and Message-ID, that autosort moves or moved, and that aren't back
/// in the inbox alone (see [`Autosorted`]), in accounts whose mail is in each of its labels if
/// `labeled`: where they are says nothing of where mail like them goes.
fn put_by_autosort<'a>(
    examples: &[Example<'a>],
    labeled: &HashSet<&str>,
) -> HashSet<(&'a str, &'a str)> {
    let mut autosorted = HashSet::new();
    let mut back = HashSet::new();
    let mut still_put = HashSet::new();
    for e in examples {
        let (Some(a), Some(id)) = (e.autosorted, e.envelope.message_id.as_deref()) else {
            continue;
        };
        let key = (e.account, id);
        autosorted.insert(key);
        // Gmail's All Mail lists every message, put back in the inbox or not.
        let all_mail = e.role == Role::Archive && labeled.contains(e.account);
        if !a.moved || (a.to == e.mailbox && !all_mail) {
            still_put.insert(key);
        } else if e.role == Role::Inbox {
            back.insert(key);
        }
    }
    autosorted.retain(|k| !back.contains(k) || still_put.contains(k));
    autosorted
}

/// The copies of each message that count, by index into `examples`: one per Message-ID in each
/// account, and every message without one.
fn counted(examples: &[Example<'_>], learning: &Learning) -> Vec<bool> {
    let mut best: HashMap<(&str, &str), usize> = HashMap::new();
    for (i, e) in examples.iter().enumerate() {
        let Some(id) = e.envelope.message_id.as_deref() else {
            continue;
        };
        best.entry((e.account, id))
            .and_modify(|b| {
                if examples
                    .get(*b)
                    .is_none_or(|kept| rank(e, learning) < rank(kept, learning))
                {
                    *b = i;
                }
            })
            .or_insert(i);
    }
    examples
        .iter()
        .enumerate()
        .map(|(i, e)| {
            e.envelope
                .message_id
                .as_deref()
                .is_none_or(|id| best.get(&(e.account, id)) == Some(&i))
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use postden_core::message::Address;

    use super::*;

    const NOW: i64 = 1_800_000_000;
    const DAY: i64 = 24 * 60 * 60;

    /// Mail as cached: owned, so examples can borrow it.
    #[derive(Clone)]
    struct Cached {
        mailbox: &'static str,
        role: Role,
        envelope: Envelope,
        /// Where autosort moves or moved it, if it does or did.
        autosorted: Option<Autosorted<'static>>,
        /// Moved to `mailbox` in this app lately.
        moved_here: bool,
    }

    /// Mail that came a week and a half ago.
    fn mail(mailbox: &'static str, role: Role, uid: u32, from: &str, subject: &str) -> Cached {
        Cached {
            mailbox,
            role,
            autosorted: None,
            moved_here: false,
            envelope: Envelope {
                uid,
                message_id: Some(format!("{mailbox}-{uid}@example")),
                from: Address {
                    name: None,
                    email: Some(from.into()),
                },
                to: vec![Address {
                    name: None,
                    email: Some("me@home.example".into()),
                }],
                subject: Some(subject.into()),
                date: Some(NOW - 10 * DAY),
                seen: true,
                flagged: false,
            },
        }
    }

    fn examples(cached: &[Cached]) -> Vec<Example<'_>> {
        cached
            .iter()
            .map(|c| Example {
                account: "me",
                mailbox: c.mailbox,
                role: c.role,
                envelope: &c.envelope,
                text: None,
                autosorted: c.autosorted,
                moved_here: c.moved_here,
            })
            .collect()
    }

    /// Every mailbox the mail is in, as listed.
    fn listed(cached: &[Cached]) -> HashMap<String, HashSet<String>> {
        let names = cached.iter().map(|c| c.mailbox.to_owned()).collect();
        HashMap::from([("me".to_owned(), names)])
    }

    fn sort_into(learning: &mut Learning, cached: &[Cached]) -> Sorting {
        sort(learning, &examples(cached), &listed(cached), NOW)
    }

    fn sort_once(cached: &[Cached]) -> Vec<Tags> {
        sort_into(&mut Learning::default(), cached).tags
    }

    /// Ordinary inbox mail, kept there, from people with nothing in common.
    fn inbox_chatter(first_uid: u32, count: u32) -> Vec<Cached> {
        (0..count)
            .map(|i| {
                let from = format!("friend{i}@people{i}.example");
                let subject = format!("plans topic{i}");
                mail("INBOX", Role::Inbox, first_uid + i, &from, &subject)
            })
            .collect()
    }

    fn receipts(count: u32) -> Vec<Cached> {
        (0..count)
            .map(|i| {
                mail(
                    "Receipts",
                    Role::Other,
                    i + 1,
                    "orders@shop.example",
                    "Your order receipt",
                )
            })
            .collect()
    }

    /// An order that just came in.
    fn new_order(uid: u32) -> Cached {
        let mut order = mail(
            "INBOX",
            Role::Inbox,
            uid,
            "orders@shop.example",
            "Your order receipt",
        );
        order.envelope.date = Some(NOW - DAY);
        order
    }

    #[test]
    fn inbox_mail_like_a_folders_is_suggested_to_go_there() {
        let mut cached = receipts(8);
        cached.extend(inbox_chatter(100, 10));
        cached.push(new_order(200));
        let tags = sort_once(&cached);
        let last = tags.last().unwrap();
        assert_eq!(last.suggested.as_deref(), Some("Receipts"), "{last:?}");
        let chatter = &tags[8..18];
        assert!(
            chatter.iter().all(|t| t.suggested.is_none()),
            "the rest stays: {chatter:?}"
        );
        let filed = &tags[..8];
        assert!(
            filed.iter().all(Tags::is_empty),
            "only inbox mail is tagged"
        );
    }

    #[test]
    fn a_folder_with_few_messages_isnt_suggested_yet() {
        let mut cached = receipts(2);
        cached.extend(inbox_chatter(100, 10));
        cached.push(new_order(200));
        assert_eq!(
            sort_once(&cached).last().unwrap().suggested,
            None,
            "too little to go by"
        );
    }

    #[test]
    fn nothing_is_suggested_before_the_inbox_kept_enough() {
        // All there is says everything goes to Receipts: nothing says what stays.
        let mut cached = receipts(8);
        cached.extend(inbox_chatter(100, MIN_KEPT - 1));
        cached.push(new_order(200));
        assert_eq!(sort_once(&cached).last().unwrap().suggested, None);
    }

    #[test]
    fn mail_autosorted_isnt_learned_from_where_it_was_put() {
        let mut cached = receipts(8);
        cached.extend(inbox_chatter(100, 10));
        let mut learning = Learning::default();
        let _sorted = sort_into(&mut learning, &cached);
        let before = learning.accounts["me"].folders.class("Receipts").cloned();
        let mut order = new_order(200);
        order.mailbox = "Receipts";
        order.role = Role::Other;
        order.autosorted = Some(Autosorted {
            to: "Receipts",
            moved: true,
        });
        cached.push(order);
        let _sorted = sort_into(&mut learning, &cached);
        let folders = &learning.accounts["me"].folders;
        assert!(
            !folders.taught.contains_key("Receipts-200@example"),
            "autosort put it there, not the user"
        );
        assert_eq!(
            folders.class("Receipts").cloned(),
            before,
            "nor is it counted as a right guess"
        );
    }

    #[test]
    fn mail_taken_back_out_of_where_autosort_put_it_is_learned_at_once() {
        let mut cached = receipts(8);
        cached.extend(inbox_chatter(100, 10));
        let mut order = new_order(200);
        order.autosorted = Some(Autosorted {
            to: "Receipts",
            moved: true,
        });
        cached.push(order);
        let mut learning = Learning::default();
        let _sorted = sort_into(&mut learning, &cached);
        assert_eq!(
            learning.accounts["me"]
                .folders
                .taught
                .get("INBOX-200@example")
                .map(String::as_str),
            Some("INBOX"),
            "moved back on purpose, though just in"
        );
    }

    #[test]
    fn mail_autosort_is_moving_isnt_learned_as_moved_back() {
        let mut cached = receipts(8);
        cached.extend(inbox_chatter(100, 10));
        let mut order = new_order(200);
        order.autosorted = Some(Autosorted {
            to: "Receipts",
            moved: false,
        });
        cached.push(order);
        let mut learning = Learning::default();
        let _sorted = sort_into(&mut learning, &cached);
        let taught = &learning.accounts["me"].folders.taught;
        assert!(
            !taught.contains_key("INBOX-200@example"),
            "still on its way"
        );
    }

    #[test]
    fn a_gmail_label_kept_beside_where_autosort_put_mail_isnt_learned() {
        let mut cached = receipts(8);
        cached.extend(inbox_chatter(100, 10));
        cached.extend((0..3).map(|i| mail("Work", Role::Other, i + 1, "boss@work.example", "Q3")));
        let autosorted = Some(Autosorted {
            to: "Receipts",
            moved: true,
        });
        // Labeled Work before autosort put it in Receipts: Gmail lists it in both.
        let mut labeled = new_order(200);
        labeled.mailbox = "Work";
        labeled.role = Role::Other;
        labeled.autosorted = autosorted;
        let mut put = labeled.clone();
        put.mailbox = "Receipts";
        cached.extend([labeled, put]);
        let mut learning = Learning::default();
        let _sorted = sort_into(&mut learning, &cached);
        let taught = &learning.accounts["me"].folders.taught;
        assert!(!taught.contains_key("INBOX-200@example"), "{taught:?}");
    }

    #[test]
    fn mail_autosorted_isnt_learned_from_a_label_before_its_back_in_the_inbox() {
        let mut cached = receipts(8);
        cached.extend(inbox_chatter(100, 10));
        cached.extend((0..3).map(|i| mail("Work", Role::Other, i + 1, "boss@work.example", "Q3")));
        // Moved, but Receipts not synced since: only its Work label's copy is cached.
        let mut labeled = new_order(200);
        labeled.mailbox = "Work";
        labeled.role = Role::Other;
        labeled.autosorted = Some(Autosorted {
            to: "Receipts",
            moved: true,
        });
        cached.push(labeled);
        let mut learning = Learning::default();
        let _sorted = sort_into(&mut learning, &cached);
        let taught = &learning.accounts["me"].folders.taught;
        assert!(!taught.contains_key("INBOX-200@example"), "{taught:?}");
    }

    #[test]
    fn mail_just_in_that_gmails_all_mail_lists_isnt_learned_as_archived() {
        let mut cached = receipts(8);
        cached.extend(inbox_chatter(100, 10));
        // All Mail synced before the inbox: only its copy is cached yet.
        let mut order = new_order(200);
        order.mailbox = "[Gmail]/All Mail";
        order.role = Role::Archive;
        cached.push(order.clone());
        let mut learning = Learning::default();
        let _sorted = sort_into(&mut learning, &cached);
        let taught = &learning.accounts["me"].folders.taught;
        assert!(!taught.contains_key("INBOX-200@example"), "{taught:?}");
        // Once the inbox's copy is cached, it's still waiting to be filed.
        order.mailbox = "INBOX";
        order.role = Role::Inbox;
        cached.push(order);
        let _sorted = sort_into(&mut learning, &cached);
        let taught = &learning.accounts["me"].folders.taught;
        assert!(!taught.contains_key("INBOX-200@example"), "{taught:?}");
    }

    #[test]
    fn mail_just_in_that_a_gmail_filter_labeled_isnt_learned_as_filed() {
        let mut cached = receipts(8);
        cached.extend(inbox_chatter(100, 10));
        cached.extend((0..3).map(|i| mail("Finance", Role::Other, i + 1, "bank@b.example", "Q3")));
        let mut listed = listed(&cached);
        if let Some(names) = listed.get_mut("me") {
            names.insert("[Gmail]/All Mail".to_owned());
        }
        // The filter's label synced before the inbox: only its copy is cached yet.
        let mut order = new_order(200);
        order.mailbox = "Finance";
        order.role = Role::Other;
        cached.push(order);
        let mut learning = Learning::default();
        let _sorted = sort(&mut learning, &examples(&cached), &listed, NOW);
        let taught = &learning.accounts["me"].folders.taught;
        assert!(!taught.contains_key("INBOX-200@example"), "{taught:?}");
        // Elsewhere than Gmail, mail filed is filed.
        let mut learning = Learning::default();
        let _sorted = sort_into(&mut learning, &cached);
        let taught = &learning.accounts["me"].folders.taught;
        assert!(taught.contains_key("INBOX-200@example"), "{taught:?}");
    }

    #[test]
    fn mail_just_in_filed_in_a_new_gmail_label_here_is_learned_at_once() {
        let mut cached = receipts(8);
        cached.extend(inbox_chatter(100, 10));
        let mut listed = listed(&cached);
        if let Some(names) = listed.get_mut("me") {
            names.insert("[Gmail]/All Mail".to_owned());
            names.insert("Bank".to_owned());
        }
        // A folder just made, and the bank's mail of the last day filed in it here.
        cached.extend((0..5).map(|i| {
            let mut filed = new_order(300 + i);
            filed.mailbox = "Bank";
            filed.role = Role::Other;
            filed.envelope.from.email = Some("alerts@bank.example".into());
            filed.envelope.subject = Some("Card payment".into());
            filed.moved_here = true;
            filed
        }));
        let mut learning = Learning::default();
        let _sorted = sort(&mut learning, &examples(&cached), &listed, NOW);
        let bank = learning.accounts["me"].folders.class("Bank").cloned();
        assert_eq!(
            bank.map(|c| c.messages),
            Some(5),
            "filed by hand, not by a filter"
        );
        let mut payment = new_order(400);
        payment.envelope.from.email = Some("alerts@bank.example".into());
        payment.envelope.subject = Some("Card payment".into());
        cached.push(payment);
        let tags = sort(&mut learning, &examples(&cached), &listed, NOW).tags;
        assert_eq!(tags.last().unwrap().suggested.as_deref(), Some("Bank"));
    }

    #[test]
    fn mail_just_in_filed_in_a_gmail_label_here_is_learned_there_whatever_other_label_it_has() {
        let mut cached = receipts(8);
        cached.extend(inbox_chatter(100, 10));
        cached.extend((0..3).map(|i| mail("Bank", Role::Other, i + 1, "a@bank.example", "Hi")));
        let mut listed = listed(&cached);
        if let Some(names) = listed.get_mut("me") {
            names.insert("[Gmail]/All Mail".to_owned());
        }
        // Labeled Receipts by a filter, then filed in Bank here: Gmail lists it in both.
        let mut labeled = new_order(200);
        labeled.mailbox = "Receipts";
        labeled.role = Role::Other;
        let mut filed = labeled.clone();
        filed.mailbox = "Bank";
        filed.moved_here = true;
        for copies in [[labeled.clone(), filed.clone()], [filed, labeled]] {
            let mut cached = cached.clone();
            cached.extend(copies);
            let mut learning = Learning::default();
            let _sorted = sort(&mut learning, &examples(&cached), &listed, NOW);
            let taught = &learning.accounts["me"].folders.taught;
            assert_eq!(
                taught.get("INBOX-200@example").map(String::as_str),
                Some("Bank"),
                "{taught:?}"
            );
            // A week on, the move is forgotten: it's still filed in Bank.
            for c in &mut cached {
                c.moved_here = false;
                c.envelope.date = c.envelope.date.map(|d| d - 7 * DAY);
            }
            let _sorted = sort(&mut learning, &examples(&cached), &listed, NOW);
            let taught = &learning.accounts["me"].folders.taught;
            assert_eq!(
                taught.get("INBOX-200@example").map(String::as_str),
                Some("Bank"),
                "{taught:?}"
            );
        }
    }

    #[test]
    fn mail_just_in_autosort_archived_here_isnt_learned() {
        let mut cached = receipts(8);
        cached.extend(inbox_chatter(100, 10));
        let mut archived = new_order(200);
        archived.mailbox = "Archive";
        archived.role = Role::Archive;
        archived.moved_here = true;
        archived.autosorted = Some(Autosorted {
            to: "Archive",
            moved: true,
        });
        cached.push(archived);
        let mut learning = Learning::default();
        let _sorted = sort_into(&mut learning, &cached);
        let taught = &learning.accounts["me"].folders.taught;
        assert!(!taught.contains_key("INBOX-200@example"), "{taught:?}");
    }

    #[test]
    fn mail_just_in_archived_here_is_learned_at_once() {
        let mut cached = receipts(8);
        cached.extend(inbox_chatter(100, 10));
        let mut archived = new_order(200);
        archived.mailbox = "Archive";
        archived.role = Role::Archive;
        cached.push(archived.clone());
        let mut learning = Learning::default();
        let _sorted = sort_into(&mut learning, &cached);
        let taught = &learning.accounts["me"].folders.taught;
        assert!(
            !taught.contains_key("INBOX-200@example"),
            "an archive may list the inbox's mail: {taught:?}"
        );
        archived.moved_here = true;
        cached.pop();
        cached.push(archived);
        let _sorted = sort_into(&mut learning, &cached);
        let taught = &learning.accounts["me"].folders.taught;
        assert_eq!(
            taught.get("INBOX-200@example").map(String::as_str),
            Some("Archive"),
            "archived here"
        );
    }

    #[test]
    fn mail_autosorted_to_gmails_archive_and_back_in_the_inbox_is_learned() {
        let mut cached = receipts(8);
        cached.extend(inbox_chatter(100, 10));
        let autosorted = Some(Autosorted {
            to: "[Gmail]/All Mail",
            moved: true,
        });
        // Back in the inbox: All Mail lists it still, as it lists every message.
        let mut back = new_order(200);
        back.autosorted = autosorted;
        let mut all_mail = back.clone();
        all_mail.mailbox = "[Gmail]/All Mail";
        all_mail.role = Role::Archive;
        cached.extend([back, all_mail]);
        let mut learning = Learning::default();
        let _sorted = sort_into(&mut learning, &cached);
        let taught = &learning.accounts["me"].folders.taught;
        assert_eq!(
            taught.get("INBOX-200@example").map(String::as_str),
            Some("INBOX")
        );
    }

    #[test]
    fn mail_just_in_isnt_learned_as_staying() {
        let mut cached = receipts(8);
        cached.extend(inbox_chatter(100, 10));
        cached.push(new_order(200));
        let mut learning = Learning::default();
        let _sorted = sort_into(&mut learning, &cached);
        let taught = &learning.accounts["me"].folders.taught;
        assert!(
            !taught.contains_key("INBOX-200@example"),
            "it may be filed yet"
        );
        assert_eq!(
            taught.get("INBOX-100@example").map(String::as_str),
            Some("INBOX")
        );
    }

    #[test]
    fn mail_kept_in_the_inbox_as_often_as_filed_stays() {
        let mut cached = receipts(6);
        cached.extend((0..6).map(|i| {
            let mut kept = new_order(300 + i);
            kept.envelope.date = Some(NOW - 10 * DAY);
            kept
        }));
        cached.extend(inbox_chatter(100, 6));
        cached.push(new_order(200));
        assert_eq!(
            sort_once(&cached).last().unwrap().suggested,
            None,
            "kept in the inbox as often as filed"
        );
    }

    #[test]
    fn what_was_learned_outlives_the_cache() {
        let mut cached = receipts(8);
        cached.extend(inbox_chatter(100, 10));
        let mut learning = Learning::default();
        let first = sort_into(&mut learning, &cached);
        assert!(!first.changes.is_empty(), "learned from the mail");
        // The receipts aged out of the cache; Receipts is still listed.
        let mut later = inbox_chatter(100, 10);
        later.push(new_order(200));
        let mut listed = listed(&later);
        listed.get_mut("me").unwrap().insert("Receipts".into());
        let again = sort(&mut learning, &examples(&later), &listed, NOW);
        assert_eq!(
            again.tags.last().unwrap().suggested.as_deref(),
            Some("Receipts")
        );
        assert!(again.changes.is_empty(), "nothing new: {:?}", again.changes);
    }

    #[test]
    fn mail_moved_is_taught_again() {
        let mut cached = receipts(8);
        cached.extend(inbox_chatter(100, 10));
        let mut learning = Learning::default();
        let _first = sort_into(&mut learning, &cached);
        // Filed in Receipts after all, here or by another app.
        let filed = cached.iter_mut().find(|c| c.envelope.uid == 100).unwrap();
        filed.mailbox = "Receipts";
        filed.role = Role::Other;
        filed.envelope.message_id = Some("INBOX-100@example".into());
        let sorted = sort_into(&mut learning, &cached);
        let folders = &learning.accounts["me"].folders;
        assert_eq!(folders.taught["INBOX-100@example"], "Receipts");
        let touched = &sorted.changes[&("me".to_owned(), Kind::Folders)];
        assert_eq!(
            touched.taught,
            HashSet::from(["INBOX-100@example".to_owned()])
        );
    }

    #[test]
    fn mail_moved_back_to_the_inbox_is_kept_there_at_once() {
        let mut cached = receipts(8);
        cached.extend(inbox_chatter(100, 10));
        let mut moved = new_order(200);
        moved.mailbox = "Receipts";
        moved.role = Role::Other;
        cached.push(moved);
        let mut learning = Learning::default();
        let _first = sort_into(&mut learning, &cached);
        let back = cached.last_mut().unwrap();
        back.mailbox = "INBOX";
        back.role = Role::Inbox;
        let _again = sort_into(&mut learning, &cached);
        let id = cached.last().unwrap().envelope.message_id.clone().unwrap();
        assert_eq!(
            learning.accounts["me"].folders.taught[&id], "INBOX",
            "just in, but moved back: it stays"
        );
    }

    #[test]
    fn a_mailbox_gone_is_forgotten() {
        let mut cached = receipts(8);
        cached.extend(inbox_chatter(100, 10));
        let mut learning = Learning::default();
        let _first = sort_into(&mut learning, &cached);
        let mut later = inbox_chatter(100, 10);
        later.push(new_order(200));
        let sorted = sort_into(&mut learning, &later);
        let folders = &learning.accounts["me"].folders;
        assert!(folders.class("Receipts").is_none(), "deleted on the server");
        assert!(folders.taught.values().all(|c| c != "Receipts"));
        assert!(sorted.tags.last().unwrap().suggested.is_none());
        let touched = &sorted.changes[&("me".to_owned(), Kind::Folders)];
        assert!(touched.removed.contains("Receipts"));
    }

    #[test]
    fn an_account_gone_is_forgotten() {
        let mut cached = receipts(8);
        cached.extend(inbox_chatter(100, 10));
        let mut learning = Learning::default();
        let _first = sort_into(&mut learning, &cached);
        let _gone = sort(&mut learning, &[], &HashMap::new(), NOW);
        assert!(learning.accounts.is_empty());
    }

    #[test]
    fn gmails_copy_in_all_mail_isnt_archiving() {
        // Gmail lists every message in All Mail too, the inbox's included.
        let mut cached = inbox_chatter(100, 10);
        for i in 0..8 {
            let mut inbox = new_order(i + 1);
            inbox.envelope.message_id = Some(format!("order-{i}@example"));
            let mut all = mail(
                "[Gmail]/All Mail",
                Role::Archive,
                i + 1,
                "orders@shop.example",
                "Your order receipt",
            );
            all.envelope.message_id = Some(format!("order-{i}@example"));
            cached.push(inbox);
            cached.push(all);
        }
        let tags = sort_once(&cached);
        assert!(
            tags.iter().all(|t| t.suggested.is_none()),
            "never archived, only listed twice: {tags:?}"
        );
    }

    #[test]
    fn mailboxes_the_app_keeps_arent_suggested() {
        let mut cached: Vec<Cached> = (0..8)
            .map(|i| {
                mail(
                    "[Gmail]/Starred",
                    Role::Other,
                    i + 1,
                    "orders@shop.example",
                    "Your order receipt",
                )
            })
            .chain((0..8).map(|i| {
                mail(
                    "Drafts",
                    Role::Other,
                    i + 1,
                    "orders@shop.example",
                    "Your order receipt",
                )
            }))
            .collect();
        cached.extend(inbox_chatter(100, 10));
        cached.push(new_order(200));
        assert_eq!(
            sort_once(&cached).last().unwrap().suggested,
            None,
            "not folders mail is filed in"
        );
    }

    /// Mail from the bank, starred, and newsletters, deleted: ten each.
    fn starred_and_deleted() -> Vec<Cached> {
        let starred = (0..MIN_IMPORTANCE).map(|i| {
            let mut m = mail(
                "Archive",
                Role::Archive,
                i + 1,
                "alerts@bank.example",
                "Account alert",
            );
            m.envelope.flagged = true;
            m
        });
        let deleted = (0..MIN_IMPORTANCE).map(|i| {
            mail(
                "Trash",
                Role::Trash,
                i + 1,
                "news@letters.example",
                "Weekly digest",
            )
        });
        starred.chain(deleted).collect()
    }

    #[test]
    fn mail_like_what_was_starred_is_important() {
        let mut cached = starred_and_deleted();
        cached.push(mail(
            "INBOX",
            Role::Inbox,
            1,
            "alerts@bank.example",
            "Account alert",
        ));
        cached.push(mail(
            "INBOX",
            Role::Inbox,
            2,
            "news@letters.example",
            "Weekly digest",
        ));
        let tags = sort_once(&cached);
        let n = tags.len();
        assert!(tags[n - 2].important, "like the starred: {:?}", tags[n - 2]);
        assert!(
            !tags[n - 1].important,
            "like the deleted: {:?}",
            tags[n - 1]
        );
    }

    #[test]
    fn what_a_class_alone_is_taught_waits_for_another() {
        // The starred mail is cached first, the deleted later.
        let mut cached = starred_and_deleted();
        let deleted = cached.split_off(usize::try_from(MIN_IMPORTANCE).unwrap());
        let mut learning = Learning::default();
        let first = sort_into(&mut learning, &cached);
        assert!(
            !first
                .changes
                .contains_key(&("me".to_owned(), Kind::Importance)),
            "nothing to tell it from: {:?}",
            first.changes
        );
        cached.extend(deleted);
        cached.push(mail(
            "INBOX",
            Role::Inbox,
            1,
            "alerts@bank.example",
            "Account alert",
        ));
        let tags = sort_into(&mut learning, &cached).tags;
        let last = tags.last().unwrap();
        assert!(last.important, "like the starred: {last:?}");
    }

    #[test]
    fn mail_deleted_in_gmail_is_deleted_though_all_mail_still_lists_it() {
        let mut cached = starred_and_deleted();
        let in_all_mail: Vec<Cached> = cached
            .iter()
            .filter(|c| c.role == Role::Trash)
            .map(|c| Cached {
                mailbox: "[Gmail]/All Mail",
                role: Role::Archive,
                envelope: c.envelope.clone(),
                autosorted: None,
                moved_here: false,
            })
            .collect();
        cached.extend(in_all_mail);
        let mut learning = Learning::default();
        let _sorting = sort_into(&mut learning, &cached);
        let learned = &learning.accounts["me"];
        let id = "Trash-1@example";
        assert_eq!(
            learned.importance.taught.get(id).map(String::as_str),
            Some(UNIMPORTANT)
        );
        assert_eq!(learned.folders.taught.get(id), None, "not archived");
    }

    #[test]
    fn mail_snoozed_in_gmail_isnt_archived_though_all_mail_lists_it() {
        let mut cached = receipts(8);
        cached.extend(inbox_chatter(100, 10));
        let snoozed = mail("Snoozed", Role::Snoozed, 1, "pal@friends.example", "Later");
        let all_mail = Cached {
            mailbox: "[Gmail]/All Mail",
            role: Role::Archive,
            envelope: snoozed.envelope.clone(),
            autosorted: None,
            moved_here: false,
        };
        cached.extend([snoozed, all_mail]);
        let mut learning = Learning::default();
        let _sorting = sort_into(&mut learning, &cached);
        let taught = &learning.accounts["me"].folders.taught;
        assert!(!taught.is_empty(), "learned from the rest");
        assert_eq!(taught.get("Snoozed-1@example"), None, "not archived");
    }

    #[test]
    fn starred_mail_isnt_tagged_important_too() {
        let mut cached = starred_and_deleted();
        let mut starred = mail(
            "INBOX",
            Role::Inbox,
            1,
            "alerts@bank.example",
            "Account alert",
        );
        starred.envelope.flagged = true;
        cached.push(starred);
        assert!(
            !sort_once(&cached).last().unwrap().important,
            "its star says so already"
        );
    }

    #[test]
    fn nothing_is_important_before_enough_was_learned() {
        let mut cached: Vec<Cached> = starred_and_deleted().into_iter().skip(1).collect();
        cached.push(mail(
            "INBOX",
            Role::Inbox,
            1,
            "alerts@bank.example",
            "Account alert",
        ));
        assert!(
            !sort_once(&cached).last().unwrap().important,
            "only nine starred"
        );
    }

    #[test]
    fn people_written_to_are_important_and_drafts_teach_nothing() {
        let mut cached = Vec::new();
        for i in 0..MIN_IMPORTANCE {
            let mut sent = mail("Sent", Role::Sent, i + 1, "me@home.example", "Re: lunch");
            sent.envelope.to = vec![Address {
                name: None,
                email: Some(format!("Pal{i}@friends.example")),
            }];
            cached.push(sent);
            let from = format!("pal{i}@friends.example");
            cached.push(mail("Archive", Role::Archive, i + 1, &from, "lunch"));
            let mut ignored = mail(
                "Archive",
                Role::Archive,
                100 + i,
                "promo@ads.example",
                "Big sale",
            );
            ignored.envelope.seen = false;
            ignored.envelope.date = Some(NOW - 30 * DAY);
            cached.push(ignored);
        }
        // A draft, from the account itself, starred: it isn't learned as important mail.
        let mut draft = mail("Drafts", Role::Other, 1, "me@home.example", "Big sale");
        draft.envelope.flagged = true;
        cached.push(draft);
        cached.push(mail(
            "INBOX",
            Role::Inbox,
            1,
            "pal3@friends.example",
            "dinner?",
        ));
        cached.push(mail(
            "INBOX",
            Role::Inbox,
            2,
            "promo@ads.example",
            "Big sale",
        ));
        let tags = sort_once(&cached);
        let n = tags.len();
        assert!(tags[n - 2].important, "a correspondent: {:?}", tags[n - 2]);
        assert!(
            !tags[n - 1].important,
            "left unread before: {:?}",
            tags[n - 1]
        );
    }
}
