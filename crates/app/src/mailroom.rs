//! Watching every account's inbox in the background, refreshing its other mailboxes on a timer,
//! and saving what arrives; downloading the newest messages of the inbox, and of the mailboxes
//! chosen, ahead of their opening, when asked to; and opening, exporting, flagging, moving, snoozing and sending messages on the same
//! background runtime. Snoozed messages come back to the inbox unread once
//! [`Mailroom::wake_due`] finds them due.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::Duration;

use postden_core::account::{Account, Server};
use postden_core::message::{Address, Body, Envelope, Mailbox, Outgoing, Role};
use postden_core::secrets::SecretStore;
use postden_mail::imap::{
    Event, Flags, Found, Nudge, Older, Prefetched, SNOOZED_NAME, Status, Threading, Unread,
    UnreadIn, Verdict, WatchConfig, create_mailbox, delete_mailbox, download_message, fetch_older,
    fetch_threading, fetch_unread, fetch_unread_each, mark_all_seen, move_message, open_message,
    prefetch_messages, refresh, restore_message, save_sent_copy, search, set_flagged, set_seen,
    snooze_message, wake_message, watch,
};
use postden_mail::{Credentials, smtp};
use postden_store::{Snooze, Store, Written};
use tokio::runtime::Runtime;
use tokio::sync::mpsc::{UnboundedSender, WeakUnboundedSender, unbounded_channel};
use tokio::task::AbortHandle;

use crate::{Error, Result, credentials};

/// The inbox's name on every server, whose messages are downloaded ahead, as are those of the
/// mailboxes [`Store::prefetch_mailboxes`] holds.
const INBOX: &str = "INBOX";
/// How many messages one connection downloads ahead. Each batch is saved, and opens without
/// waiting, before the next is asked for, under the setting and login as they are then.
const PREFETCH_BATCH: usize = 20;
/// The largest message downloaded ahead, in bytes. Larger ones, mostly their attachments, are
/// only downloaded once opened.
const PREFETCH_MAX_SIZE: u32 = 5 * 1024 * 1024;
/// How many of a mailbox's unread messages older than what's cached are kept cached, the newest:
/// as many as a first sync fetches. Each search for them fetches only those not cached yet.
const UNREAD_KEPT: usize = 200;
/// How many messages cached before threads were fetched have theirs read at once: their
/// `In-Reply-To` and `References` headers, a few hundred bytes each.
const THREADING_BATCH: usize = 500;

/// Something the UI should show. By the time it's sent, the mail is already in the store.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Update {
    /// An account's connection changed.
    Status { account: String, status: Status },
    /// The server listed an account's mailboxes. They're in the store.
    Mailboxes { account: String },
    /// A sync of one of an account's mailboxes ended, with what it found already saved, or
    /// failed: it answered every refresh numbered up to `asked`, as [`Mailroom::refresh`] and
    /// [`Mailroom::refresh_inbox`] number them. With no mailbox, it's every mailbox but the
    /// inbox.
    Synced {
        account: String,
        mailbox: Option<String>,
        asked: u64,
    },
    /// Mail was saved. `first_sync` is true when this is the mailbox's first fetch (or a restart after
    /// its UIDVALIDITY changed), so the messages aren't "new", just newly cached.
    Mail {
        account: String,
        mailbox: String,
        role: Role,
        count: usize,
        first_sync: bool,
        /// The mailbox's UIDVALIDITY the messages were saved under: their UIDs name them only
        /// while it holds.
        uid_validity: u32,
        /// The new messages that arrived unread, oldest first, to tell the user about. Empty on a
        /// first sync.
        unread: Vec<Envelope>,
        /// The Message-IDs of the messages saved, of those that have one.
        message_ids: Vec<String>,
    },
    /// Cached messages in a mailbox were read, unread, starred or unstarred elsewhere, and the
    /// store says so now.
    FlagsChanged { account: String, mailbox: String },
    /// Mail older than what was cached, asked for with [`Mailroom::fetch_older`], was saved.
    /// `done` says the mailbox has nothing older left on the server.
    Older {
        account: String,
        mailbox: String,
        count: usize,
        done: bool,
    },
    /// Fetching older mail failed.
    OlderFailed {
        account: String,
        mailbox: String,
        error: String,
    },
    /// A search asked for with [`Mailroom::search`] finished in one mailbox. What it found isn't
    /// saved: the cache holds a mailbox's newest mail, and matches can be anywhere.
    /// If the mailbox started over (a new UIDVALIDITY) since the search began, it failed: its
    /// UIDs would name other messages now.
    Searched {
        /// The id the search was started with.
        id: u64,
        account: String,
        mailbox: String,
        result: Result<Found, String>,
    },
    /// A mailbox's unread mail was looked for, with [`Mailroom::find_unread`] or
    /// [`Mailroom::find_unread_each`], and saved: the newest older than what was cached, how many
    /// more there are, and what's cached read or not as the server says; or it couldn't be looked
    /// for. `result` holds how many messages were added or changed, and by how much the unread
    /// mail that isn't cached did.
    Unread {
        account: String,
        mailbox: String,
        result: Result<usize, String>,
        /// It was asked for again while it looked, so what it found may be older than what
        /// asked: ask again.
        again: bool,
    },
    /// Fetched mail couldn't be saved.
    SaveFailed { account: String, error: String },
    /// Messages asked for with [`Mailroom::peek`] were downloaded, and their text is in the
    /// store, to show where a search found them.
    Peeked { account: String, mailbox: String },
    /// Where messages cached before threads were fetched sit in their threads was read, and is
    /// in the store: the threads [`Store::thread`] gives may have grown.
    Threaded { account: String },
    /// A message asked for with [`Mailroom::open`] was downloaded, or couldn't be. Once downloaded
    /// it's read, on the server and in the store, unless the server wouldn't mark it: `marked`.
    Opened {
        account: String,
        mailbox: String,
        /// The mailbox's UIDVALIDITY it was downloaded under: the body is of the message `uid`
        /// named then.
        uid_validity: u32,
        uid: u32,
        body: Result<Body, String>,
        /// Whether the server agreed to mark it read: it can refuse, as on a shared mailbox, and
        /// the message stays as it was.
        marked: bool,
    },
    /// A message asked for with [`Mailroom::export`] was saved to `path` as an `.eml` file, or
    /// couldn't be.
    Exported {
        account: String,
        path: PathBuf,
        result: Result<(), String>,
    },
    /// A message was marked read or unread, on the server and in the store.
    Seen {
        account: String,
        mailbox: String,
        uid: u32,
        seen: bool,
    },
    /// Marking a message read or unread failed. The store still says what it did before.
    SeenFailed {
        account: String,
        mailbox: String,
        uid: u32,
        /// What it was to be marked.
        seen: bool,
        error: String,
    },
    /// A message was starred or unstarred, as asked with [`Mailroom::set_flagged`], on the server
    /// and in the store; or it wasn't, and the store still says what it did before.
    Flagged {
        account: String,
        mailbox: String,
        /// The mailbox's UIDVALIDITY the star was asked under: `uid` named the message then.
        uid_validity: u32,
        uid: u32,
        flagged: bool,
        result: Result<(), String>,
    },
    /// Every message in a mailbox was marked read, as asked with [`Mailroom::mark_all_seen`], on
    /// the server and in the store; or it wasn't, and the store still says what it did before.
    /// Marked, it's the UID every message below was marked under, the mailbox's UIDNEXT then; or
    /// `None` if the mailbox started over meanwhile, as its UIDs name other messages now.
    AllSeen {
        account: String,
        mailbox: String,
        result: Result<Option<u32>, String>,
    },
    /// A message asked for with [`Mailroom::move_message`] was moved to the account's mailbox `to`
    /// and dropped from the store, to be cached in `to` once that mailbox syncs; or it wasn't, and
    /// the store still holds it where it was. Moved, it says whether `uid` still named it when
    /// the move was answered: not if `mailbox` started over meanwhile, as its UIDs name other
    /// messages now.
    Moved {
        account: String,
        mailbox: String,
        uid: u32,
        to: String,
        result: Result<bool, String>,
    },
    /// A message asked for with [`Mailroom::snooze`] was moved to the account's Snoozed mailbox
    /// `to`, to come back at `until`, in Unix seconds; or the move failed, and it may not have
    /// been. The snooze is kept either way: waking forgets it if the message isn't there. Moved,
    /// it says whether `uid` still named it, as [`Update::Moved`].
    Snoozed {
        account: String,
        mailbox: String,
        uid: u32,
        to: String,
        until: i64,
        result: Result<bool, String>,
    },
    /// A snoozed message was due and is back in the inbox, unread, or wasn't in `mailbox` any
    /// more; either way it's no longer snoozed. It's left `mailbox`, where it had `uids`, none if
    /// that started over since. Failing, it stays snoozed and is tried again by the next
    /// [`Mailroom::wake_due`].
    Woke {
        account: String,
        mailbox: String,
        uids: Vec<u32>,
        result: Result<(), String>,
    },
    /// A message asked for with [`Mailroom::restore`] was moved back from `mailbox` to `to`, where
    /// it was; or it wasn't, and it's still in `mailbox`, snoozed again if it was snoozed. Moved,
    /// it says whether it was found in `mailbox` at all: it had `uids` there, none if it had been
    /// moved on meanwhile or `mailbox` started over since.
    Restored {
        account: String,
        mailbox: String,
        /// The message's Message-ID, which it was found by.
        message_id: String,
        uids: Vec<u32>,
        to: String,
        result: Result<bool, String>,
    },
    /// Every downloaded message was dropped, as asked with [`Mailroom::clear_downloaded`]: how
    /// many, or why they couldn't be.
    Cleared(Result<usize, String>),
    /// The opened messages kept couldn't be dropped once [`Mailroom::set_keep_opened`] stopped
    /// keeping them.
    ReleaseFailed(String),
    /// A message handed to [`Mailroom::send`] was accepted by the account's SMTP server, or wasn't.
    /// On success, it's the `Message-ID` it was sent with.
    Sent {
        account: String,
        result: Result<String, String>,
        /// Who it went to, in To, Cc and Bcc.
        recipients: Vec<Address>,
    },
    /// A sent message couldn't be filed in the account's Sent mailbox. It was still sent.
    CopyFailed { account: String, error: String },
    /// A mailbox asked for with [`Mailroom::create_mailbox`] was made on the server, to be listed
    /// with the account's mailboxes as [`Update::Mailboxes`] next; or it wasn't.
    MailboxCreated {
        account: String,
        name: String,
        result: Result<(), String>,
    },
    /// A mailbox asked for with [`Mailroom::delete_mailbox`] was deleted on the server, its cached
    /// mail to be dropped once the account's mailboxes are listed as [`Update::Mailboxes`] next;
    /// or it wasn't. `id` is the one it was asked with.
    MailboxDeleted {
        id: u64,
        account: String,
        name: String,
        result: Result<(), String>,
    },
}

/// How long a mailbox stays reserved after a move or deletion the server didn't answer: as long
/// as an IMAP command is waited for again.
const UNCONFIRMED_HOLD: Duration = Duration::from_secs(120);

/// A mailbox on an IMAP server and user, whichever account reads it: two accounts on the same
/// login read the same mailboxes.
type Place = (String, u16, String, String);

fn place(server: &Server, mailbox: &str) -> Place {
    (
        server.host.to_ascii_lowercase(),
        server.port,
        server.username.clone(),
        mailbox.to_owned(),
    )
}

/// What's under way in each mailbox: messages being moved, moved back or snoozed into or out of
/// it, and whether it's being deleted. Neither starts while the other is: deleting it would take
/// the messages moved in with it, or before they're moved out.
#[derive(Debug, Default)]
struct Traffic {
    moving: HashMap<Place, usize>,
    deleting: HashSet<Place>,
}

type Traffics = Arc<Mutex<Traffic>>;

/// Messages being moved between two mailboxes, or a mailbox being deleted, held in
/// [`Mailroom::traffic`] until it's dropped, as its job ends.
struct Reserved {
    traffic: Traffics,
    places: Vec<Place>,
    deleting: bool,
}

impl Reserved {
    /// A message being moved from the mailbox at `from` to the one at `to`, unless either is
    /// being deleted.
    fn moving(traffic: &Traffics, from: Place, to: Place) -> Result<Self> {
        let mut t = traffic.lock().unwrap_or_else(PoisonError::into_inner);
        if t.deleting.contains(&from) || t.deleting.contains(&to) {
            return Err(Error::Invalid(
                "that mailbox is being deleted, so nothing's moved there or from it".to_owned(),
            ));
        }
        for place in [&from, &to] {
            *t.moving.entry(place.clone()).or_default() += 1;
        }
        drop(t);
        Ok(Self {
            traffic: Arc::clone(traffic),
            places: vec![from, to],
            deleting: false,
        })
    }

    /// The mailbox at `place` being deleted, unless a message is being moved into or out of it,
    /// or it's being deleted already.
    fn deleting(traffic: &Traffics, place: Place) -> Result<Self> {
        let mut t = traffic.lock().unwrap_or_else(PoisonError::into_inner);
        if t.moving.contains_key(&place) {
            return Err(Error::Invalid(
                "a message is still being moved into or out of it; try again once it's moved"
                    .to_owned(),
            ));
        }
        if !t.deleting.insert(place.clone()) {
            return Err(Error::Invalid("it's being deleted already".to_owned()));
        }
        drop(t);
        Ok(Self {
            traffic: Arc::clone(traffic),
            places: vec![place],
            deleting: true,
        })
    }

    /// Lets go once what it was held for surely ended: at once, unless the server didn't answer
    /// `result`, when it may still be doing it, so as long again as it was waited for.
    fn release<T>(
        self,
        result: &std::result::Result<T, postden_mail::Error>,
    ) -> impl Future<Output = ()> + use<T> {
        let unanswered = result
            .as_ref()
            .err()
            .is_some_and(postden_mail::Error::may_have_happened);
        async move {
            if unanswered {
                tokio::time::sleep(UNCONFIRMED_HOLD).await;
            }
            drop(self);
        }
    }
}

impl Drop for Reserved {
    fn drop(&mut self) {
        let mut t = self.traffic.lock().unwrap_or_else(PoisonError::into_inner);
        for place in &self.places {
            if self.deleting {
                t.deleting.remove(place);
            } else if let Some(n) = t.moving.get_mut(place) {
                *n -= 1;
                if *n == 0 {
                    t.moving.remove(place);
                }
            }
        }
    }
}

/// Marks a mailbox read in the store once the server did, up to the UIDNEXT it was marked under.
fn all_seen(
    store: &Store,
    account: String,
    mailbox: String,
    uid_validity: u32,
    result: Result<u32, String>,
    at: u64,
) -> Update {
    let result = result.and_then(|up_to| {
        // Started over meanwhile, the cached UIDs name other messages than were marked.
        let current = store
            .mailbox_state(&account, &mailbox)
            .map_err(|e| e.to_string())?
            .is_some_and(|state| state.uid_validity == uid_validity);
        if !current {
            return Ok(None);
        }
        store
            .set_all_seen(&account, &mailbox, up_to)
            .map_err(|e| e.to_string())?;
        store.written_all_read(&account, &mailbox, (uid_validity, up_to), at);
        Ok(Some(up_to))
    });
    Update::AllSeen {
        account,
        mailbox,
        result,
    }
}

/// A read mark to read, answered at `at`.
const fn read(at: u64) -> Written {
    Written {
        at,
        seen: Some(true),
        flagged: None,
    }
}

/// Work for the store thread: what watchers report, and results of work done for the UI.
#[derive(Debug)]
enum Job {
    Watched(Event),
    /// What the watchers of one [`Mailroom::watch`] report, numbered as that watch: saved only
    /// while it's the account's current one. Work a replaced or stopped watcher queued, before
    /// it was stopped, is dropped, as it's about mailboxes the account may no longer have.
    Watching {
        watch: u64,
        event: Event,
    },
    /// What came of something asked of the server, such as opening or moving a message,
    /// numbered as the watch whose server it was asked of. Under a watch that's no longer the
    /// account's current one, it's taken as failed, and nothing of it saved: the account may
    /// have other mailboxes now.
    Asked {
        watch: u64,
        job: Box<Self>,
    },
    Opened {
        account: String,
        mailbox: String,
        /// The mailbox's UIDVALIDITY the download was made under.
        uid_validity: u32,
        uid: u32,
        body: Result<Body, String>,
        /// Whether the server agreed to mark it read: it can refuse, as on a shared mailbox, and
        /// the message still opens, as it was.
        marked: bool,
        /// When it was answered, as [`WatchConfig::changes`] counts.
        at: u64,
        /// How many times downloaded messages were cleared when it was asked: cleared since,
        /// it's shown but not kept.
        cleared: u64,
    },
    Seen {
        account: String,
        mailbox: String,
        /// The mailbox's UIDVALIDITY the mark was asked under.
        uid_validity: u32,
        uid: u32,
        seen: bool,
        result: Result<(), String>,
        /// When it was answered, as [`WatchConfig::changes`] counts.
        at: u64,
    },
    Flagged {
        account: String,
        mailbox: String,
        /// The mailbox's UIDVALIDITY the star was asked under.
        uid_validity: u32,
        uid: u32,
        flagged: bool,
        result: Result<(), String>,
        /// When it was answered, as [`WatchConfig::changes`] counts.
        at: u64,
    },
    AllSeen {
        account: String,
        mailbox: String,
        uid_validity: u32,
        /// The mailbox's UIDNEXT when it was marked: every message below it is read.
        result: Result<u32, String>,
        /// When it was answered, as [`WatchConfig::changes`] counts.
        at: u64,
    },
    Moved(Moving, Result<(), String>),
    /// A message was moved back where it was, or wasn't: the UIDVALIDITY of the mailbox it was in
    /// and the UIDs it had there, or `None` if that mailbox couldn't be opened.
    Restored(Restoring, Result<Option<(u32, Vec<u32>)>, String>),
    /// A snoozed message was brought back, or wasn't: the UIDVALIDITY of the mailbox it waited in
    /// and the UIDs it had there, or `None` if that mailbox couldn't be opened.
    Woke(Snooze, Result<Option<(u32, Vec<u32>)>, String>),
    /// Work with nothing to save, such as sending or exporting a message: its update goes to the
    /// UI as it is, in turn with the others.
    Done(Update),
    Older {
        account: String,
        mailbox: String,
        uid_validity: u32,
        result: Result<Older, String>,
        /// [`WatchConfig::changes`] when it was asked: read marks and stars counted after are
        /// newer than what the messages say.
        asked: u64,
    },
    Searched {
        id: u64,
        account: String,
        mailbox: String,
        uid_validity: u32,
        result: Result<Found, String>,
    },
    /// What [`Mailroom::find_unread`] found below `below`.
    Unread {
        account: String,
        mailbox: String,
        uid_validity: u32,
        below: u32,
        result: Result<Unread, String>,
        /// As [`Job::Older`]'s.
        asked: u64,
        /// As [`Update::Unread`]'s.
        again: bool,
    },
    /// Drop every downloaded message, then download ahead anew.
    Clear,
    /// Stop keeping opened messages, unless they're kept again by then.
    Release,
    /// Download the newest messages not downloaded yet of the account's inbox and the mailboxes
    /// chosen, as many as asked, and drop the others downloaded before.
    Prefetch {
        account: String,
    },
    /// Messages asked for by [`Mailroom::peek`] were downloaded, or couldn't be: as
    /// [`Job::Opened`], `cleared` says whether downloaded messages were cleared since.
    Peeked {
        account: String,
        mailbox: String,
        uid_validity: u32,
        cleared: u64,
        result: Result<Prefetched, String>,
    },
    /// Where messages cached before threads were fetched sit in their threads was read, or
    /// couldn't be.
    Threaded {
        account: String,
        mailbox: String,
        /// The account's watch they were asked for under: see [`Job::Watching`].
        watch: u64,
        uid_validity: u32,
        /// Every message asked for, found or not.
        uids: Vec<u32>,
        result: Result<Vec<Threading>, String>,
    },
    /// Messages asked for by [`Job::Prefetch`] were downloaded, or couldn't be.
    Prefetched {
        account: String,
        mailbox: String,
        /// The account's watch they were asked for under: see [`Job::Watching`].
        watch: u64,
        uid_validity: u32,
        /// Every message asked for, downloaded or not.
        uids: Vec<u32>,
        result: Result<Prefetched, String>,
    },
}

/// A message being moved, as asked of [`Mailroom::move_message`].
#[derive(Debug)]
struct Moving {
    account: String,
    mailbox: String,
    /// The mailbox's UIDVALIDITY the move was made under.
    uid_validity: u32,
    uid: u32,
    to: String,
    /// Moved to be snoozed: when it's due back.
    snooze: Option<i64>,
}

/// A message being moved back where it was, as asked of [`Mailroom::restore`].
#[derive(Debug)]
struct Restoring {
    account: String,
    /// Where it is.
    mailbox: String,
    message_id: String,
    /// Where it goes back to.
    to: String,
    /// It was snoozed until then; the snooze is forgotten once it's moved back.
    snoozed: Option<i64>,
}

/// Whether a message can be found again on the server by its Message-ID, `message_id`: it has
/// one, and it's printable ASCII (RFC 5322 section 3.6.4), so it can be searched for.
#[must_use]
pub fn is_searchable(message_id: &str) -> bool {
    !message_id.is_empty() && message_id.chars().all(|c| c.is_ascii_graphic())
}

/// The UIDVALIDITY to ask the server under: `expected`, the one a UID was known under, if given
/// and still `cached`, else `cached`.
fn under(expected: Option<u32>, cached: u32, mailbox: &str) -> Result<u32> {
    match expected {
        Some(expected) if expected != cached => {
            Err(Error::Invalid(format!("{mailbox} changed on the server")))
        }
        _ => Ok(cached),
    }
}

/// Whether the account's `mailbox` is cached under `uid_validity`, so a UID known under it still
/// names the same message.
fn is_current(
    store: &Store,
    account: &str,
    mailbox: &str,
    uid_validity: u32,
) -> std::result::Result<bool, postden_store::Error> {
    Ok(store
        .mailbox_state(account, mailbox)?
        .is_some_and(|s| s.uid_validity == uid_validity))
}

/// Whose watch an event is from.
fn event_account(event: &Event) -> &str {
    match event {
        Event::Status { account, .. }
        | Event::Messages { account, .. }
        | Event::Mailboxes { account, .. }
        | Event::Synced { account, .. }
        | Event::Flags { account, .. } => account,
    }
}

/// `job`, failed, as its account was replaced while it was asked: as [`Job::superseded`], and a
/// search for unread mail, with nothing to save, goes to the UI as it is, still asking to be
/// searched again if it was.
fn superseded(job: Job) -> Job {
    match job {
        // Asked for again meanwhile, as by the new watch's sync, it's still looked for again.
        Job::Unread {
            account,
            mailbox,
            again,
            ..
        } => Job::Done(Update::Unread {
            account,
            mailbox,
            result: replaced(),
            again,
        }),
        // Its text is the old server's, which a message the new one numbers alike isn't.
        Job::Peeked {
            account,
            mailbox,
            uid_validity,
            cleared,
            ..
        } => Job::Peeked {
            account,
            mailbox,
            uid_validity,
            cleared,
            result: replaced(),
        },
        job => job.superseded(),
    }
}

/// What a request asked of an account's server ends as when the account was replaced meanwhile.
fn replaced<T>() -> Result<T, String> {
    Err("the account was replaced while this was under way".to_owned())
}

impl Job {
    /// The account something asked of the server was about.
    fn account(&self) -> Option<&str> {
        match self {
            Self::Opened { account, .. }
            | Self::Seen { account, .. }
            | Self::Flagged { account, .. }
            | Self::AllSeen { account, .. }
            | Self::Older { account, .. }
            | Self::Searched { account, .. }
            | Self::Peeked { account, .. }
            | Self::Unread { account, .. } => Some(account),
            Self::Moved(moving, _) => Some(&moving.account),
            Self::Woke(snooze, _) => Some(&snooze.account),
            Self::Restored(restoring, _) => Some(&restoring.account),
            _ => None,
        }
    }

    /// This, failed, as its account was replaced while it was asked: nothing of it is saved.
    fn superseded(self) -> Self {
        match self {
            Self::Opened {
                account,
                mailbox,
                uid_validity,
                uid,
                at,
                cleared,
                ..
            } => Self::Opened {
                account,
                mailbox,
                uid_validity,
                uid,
                body: replaced(),
                marked: false,
                at,
                cleared,
            },
            Self::Seen {
                account,
                mailbox,
                uid_validity,
                uid,
                seen,
                at,
                ..
            } => Self::Seen {
                account,
                mailbox,
                uid_validity,
                uid,
                seen,
                result: replaced(),
                at,
            },
            Self::AllSeen {
                account,
                mailbox,
                uid_validity,
                at,
                ..
            } => Self::AllSeen {
                account,
                mailbox,
                uid_validity,
                result: replaced(),
                at,
            },
            Self::Flagged {
                account,
                mailbox,
                uid_validity,
                uid,
                flagged,
                at,
                ..
            } => Self::Flagged {
                account,
                mailbox,
                uid_validity,
                uid,
                flagged,
                result: replaced(),
                at,
            },
            Self::Moved(moving, _) => Self::Moved(moving, replaced()),
            Self::Restored(restoring, _) => Self::Restored(restoring, replaced()),
            Self::Woke(snooze, _) => Self::Woke(snooze, replaced()),
            Self::Older {
                account,
                mailbox,
                uid_validity,
                asked,
                ..
            } => Self::Older {
                account,
                mailbox,
                uid_validity,
                result: replaced(),
                asked,
            },
            Self::Searched {
                id,
                account,
                mailbox,
                uid_validity,
                ..
            } => Self::Searched {
                id,
                account,
                mailbox,
                uid_validity,
                result: replaced(),
            },
            job => job,
        }
    }
}

impl From<Event> for Job {
    fn from(event: Event) -> Self {
        Self::Watched(event)
    }
}

/// An account being watched, with what's needed to do more work for it.
#[derive(Debug)]
struct Watched {
    server: Server,
    credentials: Credentials,
    smtp: Server,
    smtp_credentials: Credentials,
    /// The inbox's watcher, the other mailboxes' refresher, and what numbers what they report.
    tasks: [AbortHandle; 3],
    /// Wakes the refresher.
    nudge: Arc<Nudge>,
    /// Wakes the inbox's watcher.
    inbox_nudge: Arc<Nudge>,
    /// Which watch of the account this is: see [`Job::Watching`].
    number: u64,
}

/// The store thread's queue, for the result of something asked of one watch's server: see
/// [`Job::Asked`].
struct Asking {
    jobs: UnboundedSender<Job>,
    watch: u64,
}

impl Asking {
    /// Whether it was sent: not only as the app quits.
    fn send(&self, job: Job) -> bool {
        self.jobs
            .send(Job::Asked {
                watch: self.watch,
                job: Box::new(job),
            })
            .is_ok()
    }
}

/// Runs a watcher for each account's inbox and a refresher for its other mailboxes on a background
/// runtime, and saves their mail on a thread of its own.
///
/// Each [`Update`] goes to the `notify` callback given to [`Mailroom::start`], from that background
/// thread. Dropping the mailroom stops every watcher.
#[derive(Debug)]
pub struct Mailroom {
    /// Only taken by `drop`, to shut it down without waiting.
    runtime: Option<Runtime>,
    handle: tokio::runtime::Handle,
    jobs: UnboundedSender<Job>,
    watchers: HashMap<String, Watched>,
    config: WatchConfig,
    /// How many of the newest messages of each inbox, and each mailbox chosen, to download ahead.
    prefetch: Arc<AtomicUsize>,
    /// Whether messages, once opened, stay downloaded.
    keep_opened: Arc<AtomicBool>,
    /// How many times downloaded messages were cleared: see [`Job::Opened`].
    cleared: Arc<AtomicU64>,
    /// Each watched account's IMAP server and credentials, for downloading ahead, with the
    /// watch they're for: one lookup gets both, so a download is never numbered as a watch
    /// on other servers.
    logins: Logins,
    /// Each watched account's current watch: see [`Job::Watching`].
    watches: Watches,
    /// The number of the last watch started.
    last_watch: u64,
    /// Numbers every refresh asked for, of any account: see [`Nudge`].
    refreshes: Arc<AtomicU64>,
    /// The accounts whose due snoozes are being brought back, not to be asked for twice at once.
    waking: Arc<Mutex<HashSet<String>>>,
    /// Messages on their way into each mailbox, and the mailboxes being deleted: see [`Traffic`].
    traffic: Traffics,
    /// Snoozed messages being moved back by [`Mailroom::restore`], by account and Message-ID:
    /// their snoozes are kept until they're moved, but not brought back meanwhile.
    unsnoozing: Arc<Mutex<HashSet<(String, String)>>>,
    /// The mailboxes being searched for unread mail by [`Mailroom::find_unread`], not to be
    /// searched twice at once: see [`Finding`].
    finding_unread: Finding,
    /// The accounts whose mailboxes [`Mailroom::find_unread_each`] is looking in, each with
    /// whether it was asked for again meanwhile.
    unread_rounds: Arc<Mutex<HashMap<String, bool>>>,
}

/// The mailboxes being searched for unread mail, by account and name, each with whether it was
/// asked for again meanwhile: what that search read may be older than what asked.
#[derive(Debug, Clone, Default)]
struct Finding(Arc<Mutex<HashMap<(String, String), bool>>>);

impl Finding {
    /// Starts a search of `key`. Returns `false` if one is under way: it's searched again after.
    fn start(&self, key: &(String, String)) -> bool {
        let mut finding = self.0.lock().unwrap_or_else(PoisonError::into_inner);
        if let Some(again) = finding.get_mut(key) {
            *again = true;
            return false;
        }
        finding.insert(key.clone(), false);
        true
    }

    /// Ends the search of `key`. Returns whether it was asked for again meanwhile.
    fn end(&self, key: &(String, String)) -> bool {
        self.0
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .remove(key)
            .unwrap_or(false)
    }
}

type Logins = Arc<Mutex<HashMap<String, (Server, Credentials, u64)>>>;
type Watches = Arc<Mutex<HashMap<String, u64>>>;

impl Mailroom {
    /// Starts the background runtime and the thread that saves mail into the database at `database`.
    /// The newest `prefetch` messages of each inbox are downloaded ahead of their opening, and
    /// with `keep_opened`, any message opened stays downloaded.
    ///
    /// # Errors
    ///
    /// Returns an error when the database can't be opened or a thread can't be started.
    pub fn start(
        database: &Path,
        config: WatchConfig,
        prefetch: usize,
        keep_opened: bool,
        notify: impl Fn(Update) + Send + 'static,
    ) -> Result<Self> {
        let runtime = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(2)
            .thread_name("postden-mail")
            .enable_all()
            .build()?;
        // A connection of its own: SQLite connections can't be shared across threads.
        let mut store = Store::open(database)?;
        let (jobs, mut received) = unbounded_channel();
        let prefetch = Arc::new(AtomicUsize::new(prefetch));
        let keep_opened = Arc::new(AtomicBool::new(keep_opened));
        let cleared = Arc::new(AtomicU64::new(0));
        let logins = Logins::default();
        let watches = Watches::default();
        let mut prefetcher = Prefetcher {
            handle: runtime.handle().clone(),
            jobs: jobs.downgrade(),
            logins: Arc::clone(&logins),
            watches: Arc::clone(&watches),
            count: Arc::clone(&prefetch),
            keep_opened: Arc::clone(&keep_opened),
            cleared: Arc::clone(&cleared),
            busy: HashSet::new(),
            skipped: HashSet::new(),
            failed: HashSet::new(),
            threading: HashSet::new(),
        };
        std::thread::Builder::new()
            .name("postden-store".into())
            .spawn(move || {
                while let Some(job) = received.blocking_recv() {
                    for update in prefetcher.save(&mut store, job) {
                        notify(update);
                    }
                }
            })?;
        Ok(Self {
            handle: runtime.handle().clone(),
            runtime: Some(runtime),
            jobs,
            watchers: HashMap::new(),
            config,
            prefetch,
            keep_opened,
            cleared,
            logins,
            watches,
            last_watch: 0,
            refreshes: Arc::default(),
            waking: Arc::default(),
            traffic: Arc::default(),
            unsnoozing: Arc::default(),
            finding_unread: Finding::default(),
            unread_rounds: Arc::default(),
        })
    }

    /// How many of each inbox's newest messages are downloaded ahead of their opening.
    #[must_use]
    pub fn prefetch(&self) -> usize {
        self.prefetch.load(Ordering::Relaxed)
    }

    /// Downloads the newest `count` messages of each inbox ahead of their opening, in the
    /// background, and drops those downloaded before that are no longer among them. With 0,
    /// messages are only downloaded once opened.
    pub fn set_prefetch(&self, count: usize) {
        self.prefetch.store(count, Ordering::Relaxed);
        for account in self.watchers.keys() {
            self.prefetch_for(account);
        }
    }

    /// Whether messages, once opened, stay downloaded.
    #[must_use]
    pub fn keep_opened(&self) -> bool {
        self.keep_opened.load(Ordering::Relaxed)
    }

    /// Keeps messages downloaded once they're opened, or stops: then, in the background, those
    /// kept are dropped but for those among the newest downloaded ahead. Failing, that arrives
    /// as [`Update::ReleaseFailed`].
    pub fn set_keep_opened(&self, on: bool) {
        self.keep_opened.store(on, Ordering::Relaxed);
        if !on {
            // On the store thread, after any opened message it's keeping meanwhile.
            let _ignored = self.jobs.send(Job::Release);
        }
    }

    /// Drops every downloaded message, of every account, whether downloaded ahead or kept once
    /// opened, in the background, then downloads ahead anew as the setting says. Messages
    /// being opened meanwhile are shown but not kept. How many were dropped arrives as
    /// [`Update::Cleared`].
    pub fn clear_downloaded(&self) {
        self.cleared.fetch_add(1, Ordering::Relaxed);
        let _ignored = self.jobs.send(Job::Clear);
    }

    /// Downloads ahead, in the background, what `account`'s inbox and the mailboxes chosen lack,
    /// and drops what was downloaded ahead of those no longer chosen: for after
    /// [`Store::set_prefetch_mailbox`] changed which.
    pub fn prefetch_mailboxes_changed(&self, account: &str) {
        self.prefetch_for(account);
    }

    /// Has the store thread download ahead what `account`'s inbox and mailboxes chosen lack.
    fn prefetch_for(&self, account: &str) {
        let _ignored = self.jobs.send(Job::Prefetch {
            account: account.to_owned(),
        });
    }

    /// How often watchers check for new mail when the server doesn't push it.
    #[must_use]
    pub const fn poll_interval(&self) -> Duration {
        self.config.poll_interval
    }

    /// Checks for new mail every `every` on servers that don't push it, restarting every account's
    /// watchers to apply it. Returns the accounts that couldn't be restarted, and why.
    ///
    /// # Errors
    ///
    /// Returns an error when the accounts can't be read.
    pub fn set_poll_interval(
        &mut self,
        every: Duration,
        store: &Store,
        secrets: &dyn SecretStore,
    ) -> Result<Vec<(String, Error)>> {
        if self.config.poll_interval == every {
            return Ok(Vec::new());
        }
        self.config.poll_interval = every;
        self.watch_all(store, secrets)
    }

    /// Starts watching every account in `store`. Returns the accounts that couldn't be started, and
    /// why, such as a missing password or Google sign-in.
    ///
    /// # Errors
    ///
    /// Returns an error when the accounts can't be read.
    pub fn watch_all(
        &mut self,
        store: &Store,
        secrets: &dyn SecretStore,
    ) -> Result<Vec<(String, Error)>> {
        let mut failed = Vec::new();
        for account in store.accounts()? {
            let id = account.id.clone();
            if let Err(e) = self.watch(&account, store, secrets) {
                failed.push((id, e));
            }
        }
        Ok(failed)
    }

    /// Starts watching `account`'s inbox and refreshing its other mailboxes, replacing any watchers it
    /// had. Only mail newer than what `store` already holds is fetched.
    ///
    /// # Errors
    ///
    /// Returns an error when its passwords, tokens or sync state can't be read.
    pub fn watch(
        &mut self,
        account: &Account,
        store: &Store,
        secrets: &dyn SecretStore,
    ) -> Result<()> {
        // Read now, not when sending: the keyring may prompt.
        let (imap_credentials, smtp_credentials) = credentials(secrets, account)?;
        let mut cached = store.mailbox_states(&account.id)?;
        let inbox = cached.remove("INBOX");
        self.unwatch(&account.id);
        self.last_watch += 1;
        let number = self.last_watch;
        self.watches
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .insert(account.id.clone(), number);
        let (events, mut reported) = unbounded_channel::<Event>();
        let jobs = self.jobs.clone();
        let numberer = self.handle.spawn(async move {
            while let Some(event) = reported.recv().await {
                if jobs
                    .send(Job::Watching {
                        watch: number,
                        event,
                    })
                    .is_err()
                {
                    break;
                }
            }
        });
        let nudge = Arc::new(Nudge::new(Arc::clone(&self.refreshes)));
        let inbox_nudge = Arc::new(Nudge::new(Arc::clone(&self.refreshes)));
        let watcher = self.handle.spawn(watch(
            account.clone(),
            imap_credentials.clone(),
            inbox,
            self.config.clone(),
            Arc::clone(&inbox_nudge),
            events.clone(),
        ));
        let refresher = self.handle.spawn(refresh(
            account.clone(),
            imap_credentials.clone(),
            cached,
            self.config.clone(),
            Arc::clone(&nudge),
            events,
        ));
        self.watchers.insert(
            account.id.clone(),
            Watched {
                server: account.imap.clone(),
                credentials: imap_credentials.clone(),
                smtp: account.smtp.clone(),
                smtp_credentials,
                tasks: [
                    watcher.abort_handle(),
                    refresher.abort_handle(),
                    numberer.abort_handle(),
                ],
                nudge,
                inbox_nudge,
                number,
            },
        );
        self.logins
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .insert(
                account.id.clone(),
                (account.imap.clone(), imap_credentials, number),
            );
        // What's cached may already be all there is: no new mail would ask for it.
        self.prefetch_for(&account.id);
        Ok(())
    }

    /// Stops watching an account. Returns whether it was being watched.
    pub fn unwatch(&mut self, account: &str) -> bool {
        self.logins
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .remove(account);
        // What its watchers queued already is dropped: see `Job::Watching`. This waits for
        // anything of theirs being written, so none is once it returns.
        self.watches
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .remove(account);
        self.watchers.remove(account).is_some_and(|w| {
            w.tasks.iter().for_each(AbortHandle::abort);
            true
        })
    }

    /// Opens a message, and marks it read. One downloaded ahead is returned at once, and marked
    /// read in the background, arriving as [`Update::Seen`] or [`Update::SeenFailed`], unless
    /// `mark` isn't set, as when that's asked already; the `bool` says whether it was asked of
    /// the server. Any other is downloaded in the background and arrives as [`Update::Opened`],
    /// with whether the server agreed to mark it read, and `None` is returned.
    ///
    /// # Errors
    ///
    /// Returns an error, and does nothing, when the message must be downloaded but the account
    /// isn't watched or the mailbox never synced, or when the store can't be read.
    pub fn open(
        &self,
        store: &Store,
        account: &str,
        mailbox: &str,
        uid: u32,
        mark: bool,
    ) -> Result<(Option<Body>, bool)> {
        // Downloaded ahead, it's kept now it's opened, past its turn among the newest: before
        // it's read, so it isn't trimmed in between.
        if self.keep_opened() {
            store.keep_body(account, mailbox, uid)?;
        }
        if let Some(body) = store.body(account, mailbox, uid)? {
            // Shown all the same while disconnected; it stays unread until opened online.
            let marking = mark
                && store.seen(account, mailbox, uid)? == Some(false)
                && self
                    .set_seen(store, account, mailbox, uid, true, None)
                    .is_ok();
            return Ok((Some(body), marking));
        }
        let (server, credentials, uid_validity, jobs) = self.target(store, account, mailbox)?;
        let (account, mailbox) = (account.to_owned(), mailbox.to_owned());
        let changes = self.config.changes.clone();
        let cleared = self.cleared.load(Ordering::Relaxed);
        self.handle.spawn(async move {
            let opened = open_message(&server, &credentials, &mailbox, uid_validity, uid)
                .await
                .map_err(|e| e.to_string());
            let marked = matches!(opened, Ok((_, true)));
            let body = opened.map(|(body, _)| body);
            // Nobody is listening only as the app quits.
            let _ignored = jobs.send(Job::Opened {
                account,
                mailbox,
                uid_validity,
                uid,
                body,
                marked,
                at: changes.answered(),
                cleared,
            });
        });
        Ok((None, false))
    }

    /// Downloads a message whole in the background and writes it to `path` as an `.eml` file,
    /// replacing what's there. It stays read or unread, as it was. The result arrives as
    /// [`Update::Exported`].
    ///
    /// # Errors
    ///
    /// As [`Mailroom::open`].
    pub fn export(
        &self,
        store: &Store,
        account: &str,
        mailbox: &str,
        uid: u32,
        path: PathBuf,
    ) -> Result<()> {
        let (server, credentials, uid_validity, jobs) = self.target(store, account, mailbox)?;
        let (account, mailbox) = (account.to_owned(), mailbox.to_owned());
        self.handle.spawn(async move {
            let result =
                match download_message(&server, &credentials, &mailbox, uid_validity, uid).await {
                    Ok(raw) => {
                        let to = path.clone();
                        tokio::task::spawn_blocking(move || std::fs::write(to, raw))
                            .await
                            .map_err(|e| e.to_string())
                            .and_then(|written| written.map_err(|e| e.to_string()))
                    }
                    Err(e) => Err(e.to_string()),
                };
            let _ignored = jobs.send(Job::Done(Update::Exported {
                account,
                path,
                result,
            }));
        });
        Ok(())
    }

    /// Marks a message read or unread in the background. The result arrives as [`Update::Seen`] or
    /// [`Update::SeenFailed`]. With `uid_validity`, only under it: see [`Mailroom::target_under`].
    ///
    /// # Errors
    ///
    /// As [`Mailroom::open`].
    pub fn set_seen(
        &self,
        store: &Store,
        account: &str,
        mailbox: &str,
        uid: u32,
        seen: bool,
        uid_validity: Option<u32>,
    ) -> Result<()> {
        let (server, credentials, uid_validity, jobs) =
            self.target_under(store, account, mailbox, uid_validity)?;
        let (account, mailbox) = (account.to_owned(), mailbox.to_owned());
        let changes = self.config.changes.clone();
        self.handle.spawn(async move {
            let result = set_seen(&server, &credentials, &mailbox, uid_validity, uid, seen)
                .await
                .map_err(|e| e.to_string());
            let _ignored = jobs.send(Job::Seen {
                account,
                mailbox,
                uid_validity,
                uid,
                seen,
                result,
                at: changes.answered(),
            });
        });
        Ok(())
    }

    /// Stars a message, or takes its star away, in the background. The result arrives as
    /// [`Update::Flagged`].
    ///
    /// # Errors
    ///
    /// As [`Mailroom::open`].
    pub fn set_flagged(
        &self,
        store: &Store,
        account: &str,
        mailbox: &str,
        uid: u32,
        flagged: bool,
        uid_validity: Option<u32>,
    ) -> Result<()> {
        let (server, credentials, uid_validity, jobs) =
            self.target_under(store, account, mailbox, uid_validity)?;
        let (account, mailbox) = (account.to_owned(), mailbox.to_owned());
        let changes = self.config.changes.clone();
        self.handle.spawn(async move {
            let result = set_flagged(&server, &credentials, &mailbox, uid_validity, uid, flagged)
                .await
                .map_err(|e| e.to_string());
            let _ignored = jobs.send(Job::Flagged {
                account,
                mailbox,
                uid_validity,
                uid,
                flagged,
                result,
                at: changes.answered(),
            });
        });
        Ok(())
    }

    /// Marks every message in a mailbox read in the background, including those not cached. The
    /// result arrives as [`Update::AllSeen`].
    ///
    /// # Errors
    ///
    /// As [`Mailroom::open`].
    pub fn mark_all_seen(&self, store: &Store, account: &str, mailbox: &str) -> Result<()> {
        let (server, credentials, uid_validity, jobs) = self.target(store, account, mailbox)?;
        let (account, mailbox) = (account.to_owned(), mailbox.to_owned());
        let changes = self.config.changes.clone();
        self.handle.spawn(async move {
            let result = mark_all_seen(&server, &credentials, &mailbox, uid_validity)
                .await
                .map_err(|e| e.to_string());
            let _ignored = jobs.send(Job::AllSeen {
                account,
                mailbox,
                uid_validity,
                result,
                at: changes.answered(),
            });
        });
        Ok(())
    }

    /// Moves a message to the account's mailbox `to` in the background, then syncs the mailboxes so
    /// it's listed there. Moved into or out of Junk, it's marked as spam or not on the way
    /// ([`Verdict`]). The result arrives as [`Update::Moved`].
    ///
    /// # Errors
    ///
    /// As [`Mailroom::open`].
    pub fn move_message(
        &self,
        store: &Store,
        account: &str,
        mailbox: &str,
        uid: u32,
        to: &str,
        uid_validity: Option<u32>,
    ) -> Result<()> {
        let (server, credentials, uid_validity, jobs) =
            self.target_under(store, account, mailbox, uid_validity)?;
        let nudge = Arc::clone(&self.watched(account)?.nudge);
        let verdict = verdict(store, account, mailbox, to)?;
        let reserved =
            Reserved::moving(&self.traffic, place(&server, mailbox), place(&server, to))?;
        let (account, mailbox, to) = (account.to_owned(), mailbox.to_owned(), to.to_owned());
        self.handle.spawn(async move {
            let result = move_message(
                &server,
                &credentials,
                &mailbox,
                uid_validity,
                uid,
                &to,
                verdict,
            )
            .await;
            let release = reserved.release(&result);
            let result = result.map_err(|e| e.to_string());
            // The inbox's watcher sees mail moved there on its own; the refresher syncs the rest.
            if result.is_ok() {
                let _asked = nudge.ask();
            }
            let moving = Moving {
                account,
                mailbox,
                uid_validity,
                uid,
                to,
                snooze: None,
            };
            let _ignored = jobs.send(Job::Moved(moving, result));
            release.await;
        });
        Ok(())
    }

    /// Snoozes a message until `until`, in Unix seconds: moves it to the account's Snoozed
    /// mailbox in the background, creating one if there's none, as [`Mailroom::move_message`].
    /// The result arrives as [`Update::Snoozed`]. Once due, [`Mailroom::wake_due`] brings it back.
    ///
    /// `message` is as listed: it's found again by its Message-ID. The snooze is saved
    /// before the message is moved, and kept whatever the move's result, which may be unknown:
    /// if the message never reached the Snoozed mailbox, waking it finds nothing there and just
    /// forgets it.
    ///
    /// # Errors
    ///
    /// As [`Mailroom::open`], and when the message has no Message-ID to find it by once moved, or
    /// is snoozed already.
    pub fn snooze(
        &self,
        store: &Store,
        account: &str,
        mailbox: &str,
        message: &Envelope,
        until: i64,
        uid_validity: Option<u32>,
    ) -> Result<()> {
        let uid = message.uid;
        let snoozed = store
            .mailboxes(account)?
            .into_iter()
            .find(|m| m.role == Role::Snoozed)
            .map(|m| m.name);
        if snoozed.as_deref() == Some(mailbox) {
            return Err(Error::Invalid("the message is snoozed already".to_owned()));
        }
        let message_id = message
            .message_id
            .as_deref()
            .filter(|id| is_searchable(id))
            .map(str::to_owned)
            .ok_or_else(|| {
                Error::Invalid("the message has no Message-ID to find it by later".to_owned())
            })?;
        let (server, credentials, uid_validity, jobs) =
            self.target_under(store, account, mailbox, uid_validity)?;
        let nudge = Arc::clone(&self.watched(account)?.nudge);
        let create = snoozed.is_none();
        let to = snoozed.unwrap_or_else(|| SNOOZED_NAME.to_owned());
        let reserved =
            Reserved::moving(&self.traffic, place(&server, mailbox), place(&server, &to))?;
        store.snooze(&Snooze {
            account: account.to_owned(),
            message_id,
            mailbox: to.clone(),
            until,
        })?;
        let (account, mailbox) = (account.to_owned(), mailbox.to_owned());
        self.handle.spawn(async move {
            let result = snooze_message(
                &server,
                &credentials,
                &mailbox,
                uid_validity,
                uid,
                &to,
                create,
            )
            .await;
            let release = reserved.release(&result);
            let result = result.map_err(|e| e.to_string());
            if result.is_ok() {
                let _asked = nudge.ask();
            }
            let moving = Moving {
                account,
                mailbox,
                uid_validity,
                uid,
                to,
                snooze: Some(until),
            };
            let _ignored = jobs.send(Job::Moved(moving, result));
            release.await;
        });
        Ok(())
    }

    /// Moves a message back from the account's mailbox `mailbox`, where it was moved or snoozed
    /// to, to `to`, where it was, in the background, as when that's undone. It's found by
    /// `message_id`, as its UID there isn't known, and stays read or unread as it is; out of or
    /// back into Junk, it's marked as spam or not, as [`Mailroom::move_message`]. A message
    /// snoozed until `snoozed` is no longer snoozed once it's moved back; until then, and if it
    /// can't be, its snooze is kept, but it isn't brought back. The result arrives as
    /// [`Update::Restored`].
    ///
    /// # Errors
    ///
    /// As [`Mailroom::open`], and when the message has no Message-ID to find it by.
    pub fn restore(
        &self,
        store: &Store,
        account: &str,
        mailbox: &str,
        message_id: &str,
        to: &str,
        snoozed: Option<i64>,
    ) -> Result<()> {
        if !is_searchable(message_id) {
            return Err(Error::Invalid(
                "the message has no Message-ID to find it by".to_owned(),
            ));
        }
        let watched = self.watched(account)?;
        let verdict = verdict(store, account, mailbox, to)?;
        let (server, credentials) = (watched.server.clone(), watched.credentials.clone());
        let jobs = Asking {
            jobs: self.jobs.clone(),
            watch: watched.number,
        };
        let nudge = Arc::clone(&watched.nudge);
        // Not while snoozes are being brought back, which may have taken it already; and not
        // brought back while it's being moved.
        // First, so a refusal leaves nothing taken.
        let moving = Reserved::moving(
            &self.traffic,
            place(&watched.server, mailbox),
            place(&watched.server, to),
        )?;
        let reserved = (account.to_owned(), message_id.to_owned());
        if snoozed.is_some() {
            let busy = || {
                Error::Invalid(
                    "snoozed mail is being brought back; try again in a moment".to_owned(),
                )
            };
            if self
                .waking
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .contains(account)
            {
                return Err(busy());
            }
            if !self
                .unsnoozing
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .insert(reserved.clone())
            {
                return Err(busy());
            }
        }
        let unsnoozing = snoozed.map(|_| Arc::clone(&self.unsnoozing));
        let restoring = Restoring {
            account: account.to_owned(),
            mailbox: mailbox.to_owned(),
            message_id: message_id.to_owned(),
            to: to.to_owned(),
            snoozed,
        };
        self.handle.spawn(async move {
            let result = restore_message(
                &server,
                &credentials,
                &restoring.mailbox,
                &restoring.message_id,
                &restoring.to,
                verdict,
            )
            .await;
            let release = moving.release(&result);
            let result = result.map_err(|e| e.to_string());
            if result.is_ok() {
                let _asked = nudge.ask();
            }
            // Bringing it back once due finds nothing to bring if it was moved.
            if let Some(unsnoozing) = unsnoozing {
                unsnoozing
                    .lock()
                    .unwrap_or_else(PoisonError::into_inner)
                    .remove(&reserved);
            }
            let _ignored = jobs.send(Job::Restored(restoring, result));
            release.await;
        });
        Ok(())
    }

    /// Brings every snooze due by `now`, in Unix seconds, back to its account's inbox in the
    /// background, unread. Each arrives as [`Update::Woke`]. An account's are brought back one
    /// at a time, on one connection at a time, so many due at once don't crowd the server. Those
    /// of accounts not watched, or with snoozes being brought back already, wait for a later call.
    ///
    /// # Errors
    ///
    /// Returns an error when the store can't be read.
    pub fn wake_due(&self, store: &Store, now: i64) -> Result<()> {
        let mut due: HashMap<String, Vec<Snooze>> = HashMap::new();
        let unsnoozing = self
            .unsnoozing
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone();
        for snooze in store.due_snoozes(now)? {
            if unsnoozing.contains(&(snooze.account.clone(), snooze.message_id.clone())) {
                continue;
            }
            due.entry(snooze.account.clone()).or_default().push(snooze);
        }
        for (account, snoozes) in due {
            let Ok(watched) = self.watched(&account) else {
                continue;
            };
            if !self
                .waking
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .insert(account.clone())
            {
                continue;
            }
            let (server, credentials) = (watched.server.clone(), watched.credentials.clone());
            let jobs = Asking {
                jobs: self.jobs.clone(),
                watch: watched.number,
            };
            let nudge = Arc::clone(&watched.nudge);
            let waking = Arc::clone(&self.waking);
            self.handle.spawn(async move {
                for snooze in snoozes {
                    let result = wake_message(
                        &server,
                        &credentials,
                        &snooze.mailbox,
                        &snooze.message_id,
                        INBOX,
                    )
                    .await
                    .map_err(|e| e.to_string());
                    if result.is_ok() {
                        let _asked = nudge.ask();
                    }
                    let _ignored = jobs.send(Job::Woke(snooze, result));
                }
                waking
                    .lock()
                    .unwrap_or_else(PoisonError::into_inner)
                    .remove(&account);
            });
        }
        Ok(())
    }

    /// Fetches up to `count` messages from `mailbox` older than the oldest cached, in the background.
    /// The result arrives as [`Update::Older`] or [`Update::OlderFailed`].
    ///
    /// # Errors
    ///
    /// As [`Mailroom::open`].
    pub fn fetch_older(
        &self,
        store: &Store,
        account: &str,
        mailbox: &str,
        count: usize,
    ) -> Result<()> {
        let (server, credentials, uid_validity, jobs) = self.target(store, account, mailbox)?;
        // With nothing cached, the next sync fetches the newest; there's no "older" yet.
        let Some(before) = store.oldest_uid(account, mailbox)? else {
            return Err(Error::Invalid(format!("{mailbox} has nothing cached yet")));
        };
        let (account, mailbox) = (account.to_owned(), mailbox.to_owned());
        // Before anything is read: what's changed after is newer than what's read.
        let asked = self.config.changes.now();
        self.handle.spawn(async move {
            let result = fetch_older(&server, &credentials, &mailbox, uid_validity, before, count)
                .await
                .map_err(|e| e.to_string());
            let _ignored = jobs.send(Job::Older {
                account,
                mailbox,
                uid_validity,
                result,
                asked,
            });
        });
        Ok(())
    }

    /// Looks in `mailbox` on the server, in the background, for every message it has unread: it
    /// caches the newest [`UNREAD_KEPT`] of those older than what's cached, which a sync never
    /// fetches, counts the rest, and has what's cached read or not as the server says now. The
    /// result arrives as [`Update::Unread`]. Asked while it's already looking there, it looks
    /// again after, as that update says.
    ///
    /// # Errors
    ///
    /// As [`Mailroom::open`].
    pub fn find_unread(&self, store: &Store, account: &str, mailbox: &str) -> Result<()> {
        let (server, credentials, uid_validity, jobs) = self.target(store, account, mailbox)?;
        let below = store.oldest_uid(account, mailbox)?.unwrap_or(0);
        let cached = store.uids_below(account, mailbox, below)?;
        let key = (account.to_owned(), mailbox.to_owned());
        if !self.finding_unread.start(&key) {
            return Ok(());
        }
        // Before anything is read: what's changed after is newer than what's read.
        let asked = self.config.changes.now();
        let finding = self.finding_unread.clone();
        self.handle.spawn(async move {
            let (account, mailbox) = key.clone();
            let result = fetch_unread(
                &server,
                &credentials,
                &mailbox,
                uid_validity,
                below,
                &cached,
                UNREAD_KEPT,
            )
            .await
            .map_err(|e| e.to_string());
            // Ended before it's saved, so asking again once it's saved starts another.
            let again = finding.end(&key);
            let _ignored = jobs.send(Job::Unread {
                account,
                mailbox,
                uid_validity,
                below,
                result,
                asked,
                again,
            });
        });
        Ok(())
    }

    /// As [`Mailroom::find_unread`] for each of `account`'s `mailboxes`, one after the other over
    /// one connection, so an account with many mailboxes doesn't open as many at once. Each
    /// result arrives as its own [`Update::Unread`]. Those not synced yet, or already being looked
    /// in, are left out: the latter look again after. Asked while it's already looking in the
    /// account's mailboxes, the last result says to look again, as `again`, once. It stops once
    /// the account's watch does.
    ///
    /// # Errors
    ///
    /// Returns an error when the account isn't connected, or the store can't be read.
    pub fn find_unread_each(
        &self,
        store: &Store,
        account: &str,
        mailboxes: &[String],
    ) -> Result<()> {
        let watched = self.watched(account)?;
        let (server, credentials) = (watched.server.clone(), watched.credentials.clone());
        let asking = Asking {
            jobs: self.jobs.clone(),
            watch: watched.number,
        };
        // Read before any is claimed, so a failed read leaves none claimed.
        let synced = unread_targets(store, account, mailboxes)?;
        let account = account.to_owned();
        let rounds = Arc::clone(&self.unread_rounds);
        {
            let mut running = rounds.lock().unwrap_or_else(PoisonError::into_inner);
            if let Some(again) = running.get_mut(&account) {
                *again = true;
                return Ok(());
            }
            running.insert(account.clone(), false);
        }
        let wanted: Vec<UnreadIn> = synced
            .into_iter()
            .filter(|m| {
                self.finding_unread
                    .start(&(account.clone(), m.mailbox.clone()))
            })
            .collect();
        // Whether it was asked for again meanwhile.
        let end = move |account: &str| {
            rounds
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .remove(account)
                .unwrap_or(false)
        };
        let Some(last) = wanted.last().map(|m| m.mailbox.clone()) else {
            end(&account);
            return Ok(());
        };
        let changes = self.config.changes.clone();
        let (finding, live) = (self.finding_unread.clone(), Arc::clone(&self.watches));
        self.handle.spawn(async move {
            let current = || {
                live.lock()
                    .unwrap_or_else(PoisonError::into_inner)
                    .get(&account)
                    == Some(&asking.watch)
            };
            let mut held = None;
            fetch_unread_each(
                &server,
                &credentials,
                &wanted,
                (UNREAD_KEPT, &changes),
                current,
                |m, asked, result| {
                    let key = (account.clone(), m.mailbox.clone());
                    // Ended before it's saved, so asking again once it's saved starts another.
                    let job = Job::Unread {
                        account: account.clone(),
                        mailbox: m.mailbox.clone(),
                        uid_validity: m.uid_validity,
                        below: m.below,
                        result,
                        asked,
                        again: finding.end(&key),
                    };
                    if m.mailbox == last {
                        held = Some(job);
                    } else {
                        let _ignored = asking.send(job);
                    }
                },
            )
            .await;
            // The round ends once its connection is closed, so another doesn't open beside it;
            // the last result says whether it was asked for again meanwhile.
            let asked_again = end(&account);
            if let Some(mut job) = held {
                if let Job::Unread { again, .. } = &mut job {
                    *again |= asked_again;
                }
                let _ignored = asking.send(job);
            }
        });
        Ok(())
    }

    /// Downloads messages `uids` of `account`'s `mailbox` in the background, without marking
    /// them read, so what they say can be shown, as where a search found them in their text.
    /// They arrive as [`Update::Peeked`].
    ///
    /// # Errors
    ///
    /// As [`Mailroom::open`].
    pub fn peek(&self, store: &Store, account: &str, mailbox: &str, uids: Vec<u32>) -> Result<()> {
        if uids.is_empty() {
            return Ok(());
        }
        let (server, credentials, uid_validity, jobs) = self.target(store, account, mailbox)?;
        let (account, mailbox) = (account.to_owned(), mailbox.to_owned());
        let cleared = self.cleared.load(Ordering::Relaxed);
        self.handle.spawn(async move {
            let result = prefetch_messages(
                &server,
                &credentials,
                &mailbox,
                uid_validity,
                &uids,
                PREFETCH_MAX_SIZE,
            )
            .await
            .map_err(|e| e.to_string());
            let _ignored = jobs.send(Job::Peeked {
                account,
                mailbox,
                uid_validity,
                cleared,
                result,
            });
        });
        Ok(())
    }

    /// Searches `mailbox` on the server in the background for the newest `count` messages holding
    /// every word of `query`. The result arrives as [`Update::Searched`], carrying `id`.
    ///
    /// # Errors
    ///
    /// As [`Mailroom::open`].
    pub fn search(
        &self,
        store: &Store,
        account: &str,
        mailbox: &str,
        query: &str,
        id: u64,
        count: usize,
    ) -> Result<()> {
        let (server, credentials, uid_validity, jobs) = self.target(store, account, mailbox)?;
        let (account, mailbox, query) = (account.to_owned(), mailbox.to_owned(), query.to_owned());
        self.handle.spawn(async move {
            let result = search(&server, &credentials, &mailbox, uid_validity, &query, count)
                .await
                .map_err(|e| e.to_string());
            let _ignored = jobs.send(Job::Searched {
                id,
                account,
                mailbox,
                uid_validity,
                result,
            });
        });
        Ok(())
    }

    /// Lists and syncs `account`'s mailboxes other than the inbox now, rather than at the next
    /// interval. Returns the refresh's number, which an [`Update::Synced`] numbered as much or
    /// more answers; `None`, doing nothing, for an account that isn't watched.
    pub fn refresh(&self, account: &str) -> Option<u64> {
        self.watchers.get(account).map(|w| w.nudge.ask())
    }

    /// Syncs `account`'s inbox now, rather than once the server reports a change or at the next
    /// poll. Returns the refresh's number, as [`Mailroom::refresh`] does.
    pub fn refresh_inbox(&self, account: &str) -> Option<u64> {
        self.watchers.get(account).map(|w| w.inbox_nudge.ask())
    }

    /// Creates a mailbox called `name` on `account`'s server in the background, then lists the
    /// account's mailboxes so it shows. The result arrives as [`Update::MailboxCreated`].
    ///
    /// # Errors
    ///
    /// Returns an error, and creates nothing, when the account isn't watched.
    pub fn create_mailbox(&self, account: &str, name: &str) -> Result<()> {
        let watched = self.watched(account)?;
        let (server, credentials) = (watched.server.clone(), watched.credentials.clone());
        let nudge = Arc::clone(&watched.nudge);
        let (account, name) = (account.to_owned(), name.to_owned());
        let jobs = self.jobs.clone();
        self.handle.spawn(async move {
            let result = create_mailbox(&server, &credentials, &name)
                .await
                .map_err(|e| e.to_string());
            if result.is_ok() {
                let _asked = nudge.ask();
            }
            let _ignored = jobs.send(Job::Done(Update::MailboxCreated {
                account,
                name,
                result,
            }));
        });
        Ok(())
    }

    /// Deletes the mailbox `name`, as the server lists it, and the mail in it from `account`'s
    /// server in the background, then lists the account's mailboxes, deleted or not, so it's
    /// gone, cached mail and all. `uid_validity` is the mailbox's when it was asked to be
    /// deleted: another mailbox under its name since isn't deleted. The result arrives as
    /// [`Update::MailboxDeleted`], with `id`, which tells answers to requests for the same mailbox
    /// apart.
    ///
    /// # Errors
    ///
    /// Returns an error, and deletes nothing, when the account isn't watched, or a message is on
    /// its way into the mailbox, moved or moved back there through any account on its login.
    pub fn delete_mailbox(
        &self,
        account: &str,
        name: &str,
        uid_validity: u32,
        id: u64,
    ) -> Result<()> {
        let watched = self.watched(account)?;
        let (server, credentials) = (watched.server.clone(), watched.credentials.clone());
        let deleting = Reserved::deleting(&self.traffic, place(&server, name))?;
        let nudge = Arc::clone(&watched.nudge);
        let (account, name) = (account.to_owned(), name.to_owned());
        let jobs = self.jobs.clone();
        self.handle.spawn(async move {
            let result = delete_mailbox(&server, &credentials, &name, uid_validity).await;
            // Sent, but not answered: the server may still be deleting it. Nothing's moved there
            // until it surely isn't.
            let release = deleting.release(&result);
            let result = result.map_err(|e| e.to_string());
            // Refused too: what's on the server may have changed since it was last listed.
            let _asked = nudge.ask();
            let _ignored = jobs.send(Job::Done(Update::MailboxDeleted {
                id,
                account,
                name,
                result,
            }));
            release.await;
        });
        Ok(())
    }

    /// Sends `message` from `account` in the background, then files it in the account's Sent mailbox.
    /// The result arrives as [`Update::Sent`], and a failure to file it as [`Update::CopyFailed`].
    ///
    /// # Errors
    ///
    /// Returns an error, and sends nothing, when the account isn't watched.
    pub fn send(&self, account: &str, message: Outgoing) -> Result<()> {
        let watched = self.watched(account)?;
        let (smtp, smtp_credentials) = (watched.smtp.clone(), watched.smtp_credentials.clone());
        let (imap, credentials) = (watched.server.clone(), watched.credentials.clone());
        let nudge = Arc::clone(&watched.nudge);
        let account = account.to_owned();
        let jobs = self.jobs.clone();
        let recipients: Vec<Address> = [&message.to, &message.cc, &message.bcc]
            .into_iter()
            .flatten()
            .cloned()
            .collect();
        self.handle.spawn(async move {
            let sent = match smtp::send(&smtp, &smtp_credentials, &message).await {
                Ok(sent) => sent,
                Err(e) => {
                    let _ignored = jobs.send(Job::Done(Update::Sent {
                        account,
                        result: Err(e.to_string()),
                        recipients,
                    }));
                    return;
                }
            };
            let _ignored = jobs.send(Job::Done(Update::Sent {
                account: account.clone(),
                result: Ok(sent.message_id.clone()),
                recipients,
            }));
            match save_sent_copy(&imap, &credentials, &sent.message_id, &sent.copy).await {
                // Fetch it from Sent now, so it's listed there right away.
                Ok(()) => {
                    let _asked = nudge.ask();
                }
                Err(e) => {
                    let _ignored = jobs.send(Job::Done(Update::CopyFailed {
                        account,
                        error: e.to_string(),
                    }));
                }
            }
        });
        Ok(())
    }

    fn watched(&self, account: &str) -> Result<&Watched> {
        self.watchers
            .get(account)
            .ok_or_else(|| Error::Invalid(format!("{account} isn't connected")))
    }

    /// Where a message lives: its account's server and credentials, and the mailbox's
    /// UIDVALIDITY; and where to send what comes of asking that server.
    fn target(
        &self,
        store: &Store,
        account: &str,
        mailbox: &str,
    ) -> Result<(Server, Credentials, u32, Asking)> {
        self.target_under(store, account, mailbox, None)
    }

    /// As [`Mailroom::target`], but under `expected`, the UIDVALIDITY the message's UID was
    /// known under, if given: refused if the mailbox started over since, and the server is
    /// asked under it, so it refuses too if it started over meanwhile.
    fn target_under(
        &self,
        store: &Store,
        account: &str,
        mailbox: &str,
        expected: Option<u32>,
    ) -> Result<(Server, Credentials, u32, Asking)> {
        let watched = self.watched(account)?;
        let state = store
            .mailbox_state(account, mailbox)?
            .ok_or_else(|| Error::Invalid(format!("{mailbox} hasn't synced yet")))?;
        let uid_validity = under(expected, state.uid_validity, mailbox)?;
        Ok((
            watched.server.clone(),
            watched.credentials.clone(),
            uid_validity,
            Asking {
                jobs: self.jobs.clone(),
                watch: watched.number,
            },
        ))
    }

    /// The background runtime, for other mail work such as checking a login.
    #[must_use]
    pub fn handle(&self) -> tokio::runtime::Handle {
        self.handle.clone()
    }
}

/// What [`Mailroom::find_unread_each`] looks in: each of the account's `mailboxes` that synced,
/// once.
fn unread_targets(store: &Store, account: &str, mailboxes: &[String]) -> Result<Vec<UnreadIn>> {
    let mut synced: Vec<UnreadIn> = Vec::new();
    for mailbox in mailboxes {
        if synced.iter().any(|m| m.mailbox == *mailbox) {
            continue;
        }
        if let Some(state) = store.mailbox_state(account, mailbox)? {
            // With nothing cached, there's nothing older to fetch, but there's still a count.
            let below = store.oldest_uid(account, mailbox)?.unwrap_or(0);
            synced.push(UnreadIn {
                mailbox: mailbox.clone(),
                uid_validity: state.uid_validity,
                below,
                cached: store.uids_below(account, mailbox, below)?,
            });
        }
    }
    Ok(synced)
}

/// What moving a message from the account's mailbox `from` to `to` says about whether it's spam,
/// by their roles as last listed.
fn verdict(store: &Store, account: &str, from: &str, to: &str) -> Result<Option<Verdict>> {
    let mailboxes = store.mailboxes(account)?;
    let role = |name: &str| {
        mailboxes
            .iter()
            .find(|m| m.name == name)
            .map_or(Role::Other, |m| m.role)
    };
    Ok(Verdict::of_move(role(from), role(to)))
}

impl Drop for Mailroom {
    fn drop(&mut self) {
        // Don't wait for connections to wind down: the app may be quitting.
        if let Some(runtime) = self.runtime.take() {
            runtime.shutdown_background();
        }
    }
}

/// Downloads the messages of the inbox, and of the mailboxes chosen, ahead of their opening, for
/// the store thread, which alone knows which are cached.
struct Prefetcher {
    handle: tokio::runtime::Handle,
    /// Weak, as the store thread it lives on ends only once every sender is dropped.
    jobs: WeakUnboundedSender<Job>,
    logins: Logins,
    watches: Watches,
    count: Arc<AtomicUsize>,
    /// Whether messages, once opened, stay downloaded, in any mailbox.
    keep_opened: Arc<AtomicBool>,
    /// How many times downloaded messages were cleared: see [`Job::Opened`].
    cleared: Arc<AtomicU64>,
    /// The accounts with a batch being downloaded ahead: one at a time each, of any of its
    /// mailboxes, so only one connection per account downloads ahead, and nothing is asked for
    /// twice.
    busy: HashSet<String>,
    /// Messages a download ahead left out, by account, watch, mailbox, UIDVALIDITY and UID, not to be
    /// asked for again: those too large, and those no longer on the server, whose envelopes
    /// stay cached as the watcher only fetches newer mail. By watch, as a replaced account's
    /// UIDs name another server's messages: they're checked again once watched anew.
    skipped: HashSet<(String, u64, String, u32, u32)>,
    /// The mailboxes whose last batch failed, by account and watch, passed over so the others
    /// are downloaded ahead meanwhile: each is tried again once the account's mail syncs, or
    /// it's asked to download ahead again.
    failed: HashSet<(String, u64, String)>,
    /// The accounts whose messages cached before threads were fetched are having theirs read:
    /// one batch at a time each.
    threading: HashSet<String>,
}

impl Prefetcher {
    /// Saves what a job brought and says what the UI should show, as [`Prefetcher::save_job`];
    /// and once an account's mail synced, reads where its messages cached before threads were
    /// fetched sit in theirs.
    fn save(&mut self, store: &mut Store, job: Job) -> Vec<Update> {
        let synced = match &job {
            Job::Watched(Event::Synced { account, .. })
            | Job::Watching {
                event: Event::Synced { account, .. },
                ..
            } => Some(account.clone()),
            _ => None,
        };
        let mut updates = match job {
            Job::Threaded {
                account,
                mailbox,
                watch,
                uid_validity,
                uids,
                result,
            } => {
                return self.save_threading(
                    store,
                    (account, mailbox),
                    (watch, uid_validity),
                    &uids,
                    result,
                );
            }
            job => self.save_job(store, job),
        };
        if let Some(account) = synced {
            self.thread_ahead(store, &account, &mut updates);
        }
        updates
    }

    /// Saves where messages `uids` sit in their threads, as [`Prefetcher::thread_ahead`] read
    /// it, unless the account was replaced meanwhile, then reads the next batch. Failing, it's
    /// read again once the account's mail syncs.
    fn save_threading(
        &mut self,
        store: &mut Store,
        (account, mailbox): (String, String),
        (watch, uid_validity): (u64, u32),
        uids: &[u32],
        result: Result<Vec<Threading>, String>,
    ) -> Vec<Update> {
        self.threading.remove(&account);
        let Ok(found) = result else {
            return Vec::new();
        };
        // Held from checking the watch until it's written, as in `save_job`: unwatching waits
        // for it, so nothing read under a replaced account is written over its successor's.
        let watches = Arc::clone(&self.watches);
        let current = watches.lock().unwrap_or_else(PoisonError::into_inner);
        if current.get(&account) != Some(&watch) {
            return Vec::new();
        }
        let saved = store.save_threading((&account, &mailbox, uid_validity), uids, &found);
        drop(current);
        if let Err(e) = saved {
            return vec![Update::SaveFailed {
                account,
                error: e.to_string(),
            }];
        }
        let mut updates = vec![Update::Threaded {
            account: account.clone(),
        }];
        self.thread_ahead(store, &account, &mut updates);
        updates
    }

    /// Reads where a batch of `account`'s messages cached before threads were fetched sit in
    /// theirs, from the first of its mailboxes that has any, unless a batch is under way.
    fn thread_ahead(&mut self, store: &Store, account: &str, updates: &mut Vec<Update>) {
        if self.threading.contains(account) {
            return;
        }
        let Some((server, credentials, watch)) = self
            .logins
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .get(account)
            .cloned()
        else {
            return;
        };
        let next = (|| -> postden_store::Result<Option<(String, u32, Vec<u32>)>> {
            for (mailbox, state) in store.mailbox_states(account)? {
                let uids = store.unthreaded(account, &mailbox, THREADING_BATCH)?;
                if !uids.is_empty() {
                    return Ok(Some((mailbox, state.uid_validity, uids)));
                }
            }
            Ok(None)
        })();
        let (mailbox, uid_validity, uids) = match next {
            Ok(Some(next)) => next,
            Ok(None) => return,
            Err(e) => {
                updates.push(Update::SaveFailed {
                    account: account.to_owned(),
                    error: e.to_string(),
                });
                return;
            }
        };
        // The mailroom is going away: nobody would save what comes back.
        let Some(jobs) = self.jobs.upgrade() else {
            return;
        };
        self.threading.insert(account.to_owned());
        let account = account.to_owned();
        self.handle.spawn(async move {
            let result = fetch_threading(&server, &credentials, &mailbox, uid_validity, &uids)
                .await
                .map_err(|e| e.to_string());
            let _ignored = jobs.send(Job::Threaded {
                account,
                mailbox,
                watch,
                uid_validity,
                uids,
                result,
            });
        });
    }

    /// Saves what a job brought and says what the UI should show, then downloads ahead what
    /// the account's inbox and mailboxes chosen lack if the job may have changed that.
    #[expect(
        clippy::significant_drop_tightening,
        reason = "a watch's number is held until what it brought is written"
    )]
    fn save_job(&mut self, store: &mut Store, job: Job) -> Vec<Update> {
        // Held from checking a watch's number until what it brought is written: unwatching
        // waits for it, so once `Mailroom::unwatch` returns, nothing of that watch is written.
        let watches = Arc::clone(&self.watches);
        let current = watches.lock().unwrap_or_else(PoisonError::into_inner);
        let is_current = |account: &str, watch| current.get(account) == Some(&watch);
        // Read once, so whether an opened message is kept and how agree, however it's switched.
        let keep = self.keep_opened.load(Ordering::Relaxed);
        let job = match job {
            Job::Asked { watch, job } if job.account().is_some_and(|a| !is_current(a, watch)) => {
                superseded(*job)
            }
            Job::Asked { job, .. } => *job,
            Job::Watching { watch, event } if !is_current(event_account(&event), watch) => {
                return Vec::new();
            }
            Job::Watching { event, .. } => Job::Watched(event),
            job => job,
        };
        let (account, updates) = match job {
            Job::Clear => return self.clear(store),
            Job::Release => return self.release(store),
            Job::Prefetch { account } => {
                self.failed.retain(|(a, ..)| *a != account);
                (account, Vec::new())
            }
            job @ Job::Peeked { .. } => {
                save_peeked(store, job, self.cleared.load(Ordering::Relaxed))
            }
            Job::Prefetched {
                account,
                mailbox,
                watch,
                uid_validity,
                uids,
                result,
            } => {
                self.busy.remove(&account);
                // Asked of mailboxes the account no longer has, it's for no one.
                if current.get(&account) != Some(&watch) {
                    return Vec::new();
                }
                let updates = self.save_prefetched(
                    store,
                    (&account, &mailbox),
                    (watch, uid_validity),
                    &uids,
                    result,
                );
                (account, updates)
            }
            // New mail where it's downloaded ahead, or a sync while something failed: the
            // server answers again, so what failed is tried again.
            Job::Watched(ref event) if self.resumes(store, event) => {
                let account = event_account(event).to_owned();
                self.failed.retain(|(a, ..)| *a != account);
                (account, save(store, job))
            }
            Job::Opened {
                account,
                mailbox,
                uid_validity,
                uid,
                body: Ok(body),
                marked,
                at,
                cleared,
            } if self.keeps_opened(store, &account, &mailbox, keep)
                && cleared == self.cleared.load(Ordering::Relaxed) =>
            {
                // Downloaded, the server answers again.
                self.failed.retain(|(a, ..)| *a != account);
                let mut updates = Vec::new();
                let kept =
                    keep_opened_body(store, (&account, &mailbox, uid_validity, uid), &body, keep);
                if let Err(e) = kept {
                    updates.push(Update::SaveFailed {
                        account: account.clone(),
                        error: e.to_string(),
                    });
                }
                let opened = Job::Opened {
                    account: account.clone(),
                    mailbox,
                    uid_validity,
                    uid,
                    body: Ok(body),
                    marked,
                    at,
                    cleared,
                };
                updates.extend(save(store, opened));
                (account, updates)
            }
            job => return save(store, job),
        };
        let mut updates = updates;
        if let Err(e) = self.prefetch(store, &account) {
            updates.push(Update::SaveFailed {
                account,
                error: e.to_string(),
            });
        }
        updates
    }

    /// Whether `event` may change what an account downloads ahead: new mail in its inbox or a
    /// mailbox chosen, or a sync while one of its mailboxes failed.
    fn resumes(&self, store: &Store, event: &Event) -> bool {
        match event {
            Event::Messages {
                account, mailbox, ..
            } => prefetches(store, account, mailbox),
            Event::Synced { account, .. } => self.failed.iter().any(|(a, ..)| a == account),
            _ => false,
        }
    }

    /// Drops every downloaded message, then downloads ahead anew for each account logged in,
    /// trying again the mailboxes that failed.
    fn clear(&mut self, store: &Store) -> Vec<Update> {
        self.failed.clear();
        let mut updates = vec![Update::Cleared(
            store.clear_bodies().map_err(|e| e.to_string()),
        )];
        let accounts: Vec<String> = self
            .logins
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .keys()
            .cloned()
            .collect();
        for account in accounts {
            if let Err(e) = self.prefetch(store, &account) {
                updates.push(Update::SaveFailed {
                    account,
                    error: e.to_string(),
                });
            }
        }
        updates
    }

    /// Stops keeping opened messages, of every account, logged in or not: all are dropped but
    /// those among the newest downloaded ahead of each inbox and mailbox chosen. Unless they're
    /// kept again by now.
    fn release(&self, store: &mut Store) -> Vec<Update> {
        if self.keep_opened.load(Ordering::Relaxed) {
            return Vec::new();
        }
        let count = self.count.load(Ordering::Relaxed);
        match store.release_kept_bodies(INBOX, count) {
            Ok(_) => Vec::new(),
            Err(e) => vec![Update::ReleaseFailed(e.to_string())],
        }
    }

    /// Whether a message opened in `account`'s `mailbox` is to stay downloaded: when opened
    /// messages are `kept`, or, in the inbox or a mailbox chosen, while their newest are
    /// downloaded ahead.
    fn keeps_opened(&self, store: &Store, account: &str, mailbox: &str, kept: bool) -> bool {
        kept || (self.count.load(Ordering::Relaxed) > 0 && prefetches(store, account, mailbox))
    }

    fn save_prefetched(
        &mut self,
        store: &Store,
        (account, mailbox): (&str, &str),
        (watch, uid_validity): (u64, u32),
        asked: &[u32],
        result: Result<Prefetched, String>,
    ) -> Vec<Update> {
        // Failing, it's tried again once the account's mail syncs, or at the next start; the
        // account's other mailboxes are downloaded meanwhile.
        let Ok(prefetched) = result else {
            self.failed
                .insert((account.to_owned(), watch, mailbox.to_owned()));
            return Vec::new();
        };
        let result = (|| -> Result<()> {
            // Started over meanwhile, the UIDs name other messages.
            let current = is_current(store, account, mailbox, uid_validity)?;
            if !current {
                return Ok(());
            }
            // No longer chosen by the time each is written, it's not saved.
            for (uid, body) in &prefetched.bodies {
                store.save_body_ahead((account, mailbox, *uid), body, INBOX)?;
            }
            let downloaded: HashSet<u32> = prefetched.bodies.iter().map(|(uid, _)| *uid).collect();
            self.skipped.extend(
                asked
                    .iter()
                    .filter(|uid| !downloaded.contains(uid))
                    .map(|&uid| {
                        (
                            account.to_owned(),
                            watch,
                            mailbox.to_owned(),
                            uid_validity,
                            uid,
                        )
                    }),
            );
            Ok(())
        })();
        match result {
            Ok(()) => Vec::new(),
            Err(e) => vec![Update::SaveFailed {
                account: account.to_owned(),
                error: e.to_string(),
            }],
        }
    }

    /// Drops what's downloaded ahead of `account`'s inbox and mailboxes chosen but their newest
    /// messages, and starts downloading those they lack: the inbox's first.
    fn prefetch(&mut self, store: &Store, account: &str) -> Result<()> {
        let count = self.count.load(Ordering::Relaxed);
        let mut mailboxes = vec![INBOX.to_owned()];
        let mut chosen: Vec<String> = store
            .prefetch_mailboxes()?
            .into_iter()
            .filter(|(a, name)| a == account && name != INBOX)
            .map(|(_, name)| name)
            .collect();
        chosen.sort();
        mailboxes.extend(chosen);
        for mailbox in &mailboxes {
            store.trim_bodies(account, mailbox, count)?;
        }
        // Downloading a batch: the next is asked for once it's back, as things are then.
        if count == 0 || self.busy.contains(account) {
            return Ok(());
        }
        let Some((server, credentials, watch)) = self
            .logins
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .get(account)
            .cloned()
        else {
            return Ok(());
        };
        for mailbox in mailboxes {
            if self
                .failed
                .contains(&(account.to_owned(), watch, mailbox.clone()))
            {
                continue;
            }
            let Some(state) = store.mailbox_state(account, &mailbox)? else {
                continue;
            };
            let uid_validity = state.uid_validity;
            let uids: Vec<u32> = store
                .missing_bodies(account, &mailbox, count)?
                .into_iter()
                .filter(|&uid| {
                    !self.skipped.contains(&(
                        account.to_owned(),
                        watch,
                        mailbox.clone(),
                        uid_validity,
                        uid,
                    ))
                })
                .take(PREFETCH_BATCH)
                .collect();
            if uids.is_empty() {
                continue;
            }
            // The mailroom is going away: nobody would save what comes back.
            let Some(jobs) = self.jobs.upgrade() else {
                return Ok(());
            };
            self.busy.insert(account.to_owned());
            let (server, credentials, account) = (server, credentials, account.to_owned());
            self.handle.spawn(async move {
                let result = prefetch_messages(
                    &server,
                    &credentials,
                    &mailbox,
                    uid_validity,
                    &uids,
                    PREFETCH_MAX_SIZE,
                )
                .await
                .map_err(|e| e.to_string());
                let _ignored = jobs.send(Job::Prefetched {
                    account,
                    mailbox,
                    watch,
                    uid_validity,
                    uids,
                    result,
                });
            });
            return Ok(());
        }
        Ok(())
    }
}

/// Saves what [`Mailroom::peek`] downloaded, unless the mailbox started over meanwhile, as
/// its UIDs name other messages then; still unread. Nor, downloaded messages cleared since it was asked for (`cleared` times now), is it kept.
fn save_peeked(store: &Store, job: Job, cleared_now: u64) -> (String, Vec<Update>) {
    let Job::Peeked {
        account,
        mailbox,
        uid_validity,
        cleared,
        result,
    } = job
    else {
        return (String::new(), Vec::new());
    };
    if cleared != cleared_now {
        return (account, Vec::new());
    }
    let updates = peeked_updates(store, (&account, &mailbox), uid_validity, result);
    (account, updates)
}

/// What saving what [`Mailroom::peek`] downloaded says to the UI.
fn peeked_updates(
    store: &Store,
    (account, mailbox): (&str, &str),
    uid_validity: u32,
    result: Result<Prefetched, String>,
) -> Vec<Update> {
    let Ok(peeked) = result else {
        return Vec::new();
    };
    let saved = (|| -> Result<()> {
        if !is_current(store, account, mailbox, uid_validity)? {
            return Ok(());
        }
        // Kept, as one opened is: downloading ahead trims what it didn't download itself, and
        // the search showing them is still on screen.
        for (uid, body) in &peeked.bodies {
            store.save_body(account, mailbox, *uid, body, true)?;
        }
        Ok(())
    })();
    match saved {
        Ok(()) => vec![Update::Peeked {
            account: account.to_owned(),
            mailbox: mailbox.to_owned(),
        }],
        Err(e) => vec![Update::SaveFailed {
            account: account.to_owned(),
            error: e.to_string(),
        }],
    }
}

/// Caches the body of a message opened, `kept`, or like one downloaded ahead, so it opens again
/// without waiting; unless the mailbox started over meanwhile, as its UID names another message
/// now, or, not kept, it's no longer downloaded ahead by the time it's written.
fn keep_opened_body(
    store: &Store,
    (account, mailbox, uid_validity, uid): (&str, &str, u32, u32),
    body: &Body,
    kept: bool,
) -> Result<()> {
    let state = store.mailbox_state(account, mailbox)?;
    if state.is_some_and(|s| s.uid_validity == uid_validity) {
        if kept {
            store.save_body(account, mailbox, uid, body, true)?;
        } else {
            store.save_body_ahead((account, mailbox, uid), body, INBOX)?;
        }
    }
    Ok(())
}

/// Whether `account`'s `mailbox` has its newest messages downloaded ahead: the inbox's always,
/// and those of the mailboxes chosen. Unreadable, only the inbox's.
fn prefetches(store: &Store, account: &str, mailbox: &str) -> bool {
    mailbox == INBOX
        || store
            .prefetch_mailboxes()
            .is_ok_and(|on| on.contains(&(account.to_owned(), mailbox.to_owned())))
}

/// Saves what a job brought, and says what the UI should show.
fn save(store: &mut Store, job: Job) -> Vec<Update> {
    match job {
        Job::Watched(Event::Status { account, status }) => vec![Update::Status { account, status }],
        Job::Watched(Event::Synced {
            account,
            mailbox,
            asked,
        }) => vec![Update::Synced {
            account,
            mailbox,
            asked,
        }],
        Job::Watched(Event::Mailboxes { account, mailboxes }) => {
            save_mailboxes(store, account, &mailboxes)
                .into_iter()
                .collect()
        }
        Job::Watched(Event::Messages {
            account,
            mailbox,
            role,
            uid_validity,
            messages,
            asked,
        }) => save_messages(
            store,
            (account, mailbox, role),
            (uid_validity, asked),
            messages,
        )
        .into_iter()
        .collect(),
        Job::Watched(Event::Flags {
            account,
            mailbox,
            uid_validity,
            flags,
            asked,
        }) => save_flags(store, account, mailbox, uid_validity, &flags, asked)
            .into_iter()
            .collect(),
        job @ (Job::Opened { .. }
        | Job::Seen { .. }
        | Job::Flagged { .. }
        | Job::AllSeen { .. }) => save_answered(store, job),
        Job::Moved(moving, result) => vec![moved(store, moving, result)],
        Job::Restored(restoring, result) => vec![restored(store, restoring, result)],
        Job::Woke(snooze, result) => vec![woke(store, &snooze, result)],
        Job::Done(update) => vec![update],
        Job::Older {
            account,
            mailbox,
            uid_validity,
            result,
            asked,
        } => vec![save_older(
            store,
            (account, mailbox, uid_validity),
            result,
            asked,
        )],
        Job::Searched {
            id,
            account,
            mailbox,
            uid_validity,
            result,
        } => vec![searched(store, id, account, mailbox, uid_validity, result)],
        Job::Unread {
            account,
            mailbox,
            uid_validity,
            below,
            result,
            asked,
            again,
        } => vec![save_unread(
            store,
            (account, mailbox, uid_validity),
            below,
            result,
            (asked, again),
        )],
        // Numbered and downloading-ahead jobs, which `Prefetcher::save` handles first.
        _ => Vec::new(),
    }
}

/// Saves what the server answered about a message being opened, marked or starred, or a
/// mailbox marked read, as [`save`].
fn save_answered(store: &Store, job: Job) -> Vec<Update> {
    match job {
        Job::Opened {
            account,
            mailbox,
            uid_validity,
            uid,
            body,
            marked,
            at,
            ..
        } => opened(
            store,
            (account, mailbox, uid_validity),
            uid,
            (body, marked),
            at,
        ),
        Job::Seen {
            account,
            mailbox,
            uid_validity,
            uid,
            seen,
            result,
            at,
        } => vec![marked(
            store,
            (account, mailbox, uid_validity),
            uid,
            seen,
            result,
            at,
        )],
        Job::Flagged {
            account,
            mailbox,
            uid_validity,
            uid,
            flagged,
            result,
            at,
        } => vec![starred(
            store,
            (account, mailbox, uid_validity),
            uid,
            flagged,
            result,
            at,
        )],
        Job::AllSeen {
            account,
            mailbox,
            uid_validity,
            result,
            at,
        } => vec![all_seen(store, account, mailbox, uid_validity, result, at)],
        _ => Vec::new(),
    }
}

/// A message was moved on the server, or wasn't: once it was, it leaves the store. If its
/// mailbox started over since the move was made, its UIDs name other messages and it was
/// refetched without this one, so nothing is dropped.
fn moved(store: &mut Store, moving: Moving, result: Result<(), String>) -> Update {
    let Moving {
        account,
        mailbox,
        uid_validity,
        uid,
        to,
        snooze,
    } = moving;
    let result = result.and_then(|()| {
        let current = store
            .mailbox_state(&account, &mailbox)
            .map_err(|e| e.to_string())?
            .map(|s| s.uid_validity);
        if current != Some(uid_validity) {
            return Ok(false);
        }
        // Put away in the Trash or Junk, a copy Gmail's All Mail still lists wasn't archived.
        store
            .put_away(&account, &mailbox, uid, &to)
            .and_then(|()| store.remove_message(&account, &mailbox, uid))
            .map(|_| true)
            .map_err(|e| e.to_string())
    });
    let Some(until) = snooze else {
        return Update::Moved {
            account,
            mailbox,
            uid,
            to,
            result,
        };
    };
    Update::Snoozed {
        account,
        mailbox,
        uid,
        to,
        until,
        result,
    }
}

/// A snoozed message was brought back to the inbox, or wasn't: once it was, it's no longer
/// snoozed, and leaves the mailbox it waited in, unless that started over since.
fn woke(
    store: &mut Store,
    snooze: &Snooze,
    result: Result<Option<(u32, Vec<u32>)>, String>,
) -> Update {
    let mut left = Vec::new();
    let result = result.and_then(|found| {
        let saved = (|| {
            store.unsnooze(&snooze.account, &snooze.message_id, snooze.until)?;
            let Some((uid_validity, uids)) = found else {
                return Ok(());
            };
            let current = is_current(store, &snooze.account, &snooze.mailbox, uid_validity)?;
            if current {
                for uid in uids {
                    store.remove_message(&snooze.account, &snooze.mailbox, uid)?;
                    left.push(uid);
                }
            }
            Ok::<_, postden_store::Error>(())
        })();
        saved.map_err(|e| e.to_string())
    });
    Update::Woke {
        account: snooze.account.clone(),
        mailbox: snooze.mailbox.clone(),
        uids: left,
        result,
    }
}

/// Saves an account's mailboxes as the server listed them.
fn save_mailboxes(store: &mut Store, account: String, mailboxes: &[Mailbox]) -> Option<Update> {
    // A refresher stopped as its account was removed can still have a listing in flight.
    match store.account(&account).and_then(|a| {
        a.map(|_| store.save_mailbox_list(&account, mailboxes))
            .transpose()
    }) {
        Ok(Some(())) => Some(Update::Mailboxes { account }),
        Ok(None) => None,
        Err(e) => Some(Update::SaveFailed {
            account,
            error: e.to_string(),
        }),
    }
}

/// Saves whether cached messages are read and starred, as a sync found them, and says so if
/// any changed.
fn save_flags(
    store: &mut Store,
    account: String,
    mailbox: String,
    uid_validity: u32,
    flags: &[Flags],
    asked: u64,
) -> Option<Update> {
    let flags: Vec<_> = flags.iter().map(|f| (f.uid, f.seen, f.flagged)).collect();
    match store.set_flags(&account, &mailbox, uid_validity, asked, &flags) {
        Ok(0) => None,
        Ok(_) => Some(Update::FlagsChanged { account, mailbox }),
        Err(e) => Some(Update::SaveFailed {
            account,
            error: e.to_string(),
        }),
    }
}

/// A message was downloaded, or couldn't be. It's read in the store once the server agreed to
/// mark it, unless its mailbox, given with the UIDVALIDITY it was downloaded under, started over
/// since.
fn opened(
    store: &Store,
    (account, mailbox, uid_validity): (String, String, u32),
    uid: u32,
    (body, marked): (Result<Body, String>, bool),
    at: u64,
) -> Vec<Update> {
    let mut updates = Vec::new();
    // Unless the mailbox started over meanwhile: its UID names another message now.
    let read_here = || {
        if is_current(store, &account, &mailbox, uid_validity)? {
            store.set_seen(&account, &mailbox, uid, true)?;
            store.written(&account, &mailbox, (uid_validity, uid), read(at));
        }
        Ok::<_, postden_store::Error>(())
    };
    // Read once downloaded, unless the server wouldn't mark it: then it's as it was.
    if body.is_ok()
        && marked
        && let Err(e) = read_here()
    {
        updates.push(Update::SaveFailed {
            account: account.clone(),
            error: e.to_string(),
        });
    }
    updates.push(Update::Opened {
        account,
        mailbox,
        uid_validity,
        uid,
        marked: marked && body.is_ok(),
        body,
    });
    updates
}

/// Marks a message read or unread in the store once the server did.
fn marked(
    store: &Store,
    (account, mailbox, uid_validity): (String, String, u32),
    uid: u32,
    seen: bool,
    result: Result<(), String>,
    at: u64,
) -> Update {
    let result = result.and_then(|()| {
        // Unless the mailbox started over meanwhile: its UID names another message now.
        if is_current(store, &account, &mailbox, uid_validity).map_err(|e| e.to_string())? {
            store
                .set_seen(&account, &mailbox, uid, seen)
                .map_err(|e| e.to_string())?;
            let written = Written {
                at,
                seen: Some(seen),
                flagged: None,
            };
            store.written(&account, &mailbox, (uid_validity, uid), written);
        }
        Ok(())
    });
    match result {
        Ok(()) => Update::Seen {
            account,
            mailbox,
            uid,
            seen,
        },
        Err(error) => Update::SeenFailed {
            account,
            mailbox,
            uid,
            seen,
            error,
        },
    }
}

/// Stars or unstars a message in the store once the server did, unless its mailbox, given with
/// the UIDVALIDITY it was starred under, started over since: its UID names another message now.
fn starred(
    store: &Store,
    (account, mailbox, uid_validity): (String, String, u32),
    uid: u32,
    flagged: bool,
    result: Result<(), String>,
    at: u64,
) -> Update {
    let result = result.and_then(|()| {
        let saved = (|| {
            let current = is_current(store, &account, &mailbox, uid_validity)?;
            if current {
                store.set_flagged(&account, &mailbox, uid, flagged)?;
                let written = Written {
                    at,
                    seen: None,
                    flagged: Some(flagged),
                };
                store.written(&account, &mailbox, (uid_validity, uid), written);
            }
            Ok::<_, postden_store::Error>(())
        })();
        saved.map_err(|e| e.to_string())
    });
    Update::Flagged {
        account,
        mailbox,
        uid_validity,
        uid,
        flagged,
        result,
    }
}

/// A message was moved back where it was, or wasn't. Moved, or no longer there, it leaves the
/// mailbox it was in, unless that started over since, and a snooze it had is forgotten; not, the
/// snooze is kept.
fn restored(
    store: &mut Store,
    restoring: Restoring,
    result: Result<Option<(u32, Vec<u32>)>, String>,
) -> Update {
    let Restoring {
        account,
        mailbox,
        message_id,
        to,
        snoozed,
    } = restoring;
    let mut left = Vec::new();
    let result = match result {
        Ok(found) => {
            let saved = (|| {
                if let Some(until) = snoozed {
                    store.unsnooze(&account, &message_id, until)?;
                }
                let Some((uid_validity, uids)) = found else {
                    return Ok(false);
                };
                let moved = !uids.is_empty();
                if moved {
                    // Back in the Trash or Junk, or out of them, as Gmail's All Mail can't say.
                    store.put_away_message(&account, &message_id, &to)?;
                }
                let current = is_current(store, &account, &mailbox, uid_validity)?;
                if current {
                    for uid in uids {
                        store.remove_message(&account, &mailbox, uid)?;
                        left.push(uid);
                    }
                }
                Ok::<_, postden_store::Error>(moved)
            })();
            saved.map_err(|e| e.to_string())
        }
        Err(error) => Err(error),
    };
    Update::Restored {
        account,
        mailbox,
        message_id,
        uids: left,
        to,
        result,
    }
}

/// What a search found, unless its mailbox started over since. Jobs are saved in order, so a
/// restart saved before this is seen here, and one saved after reaches the UI after it.
fn searched(
    store: &Store,
    id: u64,
    account: String,
    mailbox: String,
    uid_validity: u32,
    result: Result<Found, String>,
) -> Update {
    let current = store
        .mailbox_state(&account, &mailbox)
        .map_err(|e| e.to_string())
        .map(|state| state.map(|s| s.uid_validity));
    let result = match current {
        Ok(Some(current)) if current == uid_validity => result,
        Ok(_) => Err(format!("{mailbox} changed on the server; search again")),
        Err(error) => Err(error),
    };
    Update::Searched {
        id,
        account,
        mailbox,
        result,
    }
}

fn save_older(
    store: &mut Store,
    (account, mailbox, uid_validity): (String, String, u32),
    result: Result<Older, String>,
    asked: u64,
) -> Update {
    let saved = result.and_then(|mut older| {
        // Read or starred since it was asked, that's newer than what it fetched.
        store.with_written(
            &account,
            &mailbox,
            (uid_validity, asked),
            &mut older.messages,
        );
        store
            .save_older_messages(&account, &mailbox, uid_validity, &older.messages)
            .map_err(|e| e.to_string())
            // The mailbox started over (or went) meanwhile: its next sync brings the newest again.
            .map(|saved| {
                if saved {
                    older
                } else {
                    Older {
                        messages: Vec::new(),
                        done: false,
                    }
                }
            })
    });
    match saved {
        Ok(older) => Update::Older {
            account,
            mailbox,
            count: older.messages.len(),
            // Nothing came back, so asking again would bring nothing either.
            done: older.done || older.messages.is_empty(),
        },
        Err(error) => Update::OlderFailed {
            account,
            mailbox,
            error,
        },
    }
}

fn save_unread(
    store: &mut Store,
    (account, mailbox, uid_validity): (String, String, u32),
    below: u32,
    result: Result<Unread, String>,
    (asked, again): (u64, bool),
) -> Update {
    let result = result.and_then(|unread| {
        store
            .save_unread(
                &account,
                &mailbox,
                (uid_validity, asked),
                (below, unread.upto),
                &unread.unseen,
                &unread.messages,
            )
            .map_err(|e| e.to_string())
            // The mailbox started over (or went) meanwhile: its next sync is looked after anew.
            .map(Option::unwrap_or_default)
    });
    Update::Unread {
        account,
        mailbox,
        result,
        again,
    }
}

fn save_messages(
    store: &mut Store,
    (account, mailbox, role): (String, String, Role),
    (uid_validity, asked): (u32, u64),
    mut messages: Vec<Envelope>,
) -> Option<Update> {
    // Read or starred since the sync started, that's newer than what it fetched.
    store.with_written(&account, &mailbox, (uid_validity, asked), &mut messages);
    let messages = messages.as_slice();
    let result = (|| -> Result<Option<bool>> {
        // A watcher stopped as its account was removed can still have mail in flight.
        if store.account(&account)?.is_none() {
            return Ok(None);
        }
        let first_sync = store
            .mailbox_state(&account, &mailbox)?
            .is_none_or(|s| s.uid_validity != uid_validity);
        store.save_messages(&account, &mailbox, role, uid_validity, messages)?;
        Ok(Some(first_sync))
    })();
    match result {
        Ok(None) => None,
        Ok(Some(first_sync)) => Some(Update::Mail {
            account,
            mailbox,
            role,
            count: messages.len(),
            first_sync,
            uid_validity,
            unread: if first_sync {
                Vec::new()
            } else {
                messages.iter().filter(|m| !m.seen).cloned().collect()
            },
            message_ids: messages
                .iter()
                .filter_map(|m| m.message_id.clone())
                .collect(),
        }),
        Err(e) => Some(Update::SaveFailed {
            account,
            error: e.to_string(),
        }),
    }
}

#[cfg(test)]
mod tests {
    use postden_core::account::{Auth, Security, Server};
    use postden_core::message::{Address, Envelope};

    use super::*;

    fn account() -> Account {
        let server = Server {
            host: "localhost".into(),
            port: 1,
            security: Security::None,
            username: "me".into(),
        };
        Account {
            id: "me@example.com".into(),
            display_name: None,
            label: None,
            imap: server.clone(),
            smtp: server,
            auth: Auth::Password,
        }
    }

    fn messages(uid_validity: u32, uids: &[u32]) -> Job {
        messages_in("INBOX", Role::Inbox, uid_validity, uids)
    }

    fn messages_in(mailbox: &str, role: Role, uid_validity: u32, uids: &[u32]) -> Job {
        Job::Watched(Event::Messages {
            account: "me@example.com".into(),
            mailbox: mailbox.into(),
            role,
            uid_validity,
            messages: uids
                .iter()
                .map(|&uid| Envelope {
                    uid,
                    message_id: None,
                    from: Address::default(),
                    to: Vec::new(),
                    subject: None,
                    date: None,
                    seen: false,
                    flagged: false,
                    in_reply_to: None,
                    references: Vec::new(),
                })
                .collect(),
            asked: 0,
        })
    }

    #[test]
    fn a_mailbox_isnt_deleted_while_mail_is_moved_in_or_out_nor_the_other_way() {
        let server = |host: &str| Server {
            host: host.into(),
            port: 993,
            security: Security::Tls,
            username: "me".into(),
        };
        let traffic = Traffics::default();
        let at = |host: &str, mailbox: &str| place(&server(host), mailbox);
        let work = || at("IMAP.example.com", "Work");
        let inbox = || at("imap.example.com", "INBOX");
        // Into it, and out of it, through another account on the same login.
        let into = Reserved::moving(&traffic, inbox(), work()).unwrap();
        let out = Reserved::moving(&traffic, at("imap.example.com", "Work"), inbox()).unwrap();
        assert!(Reserved::deleting(&traffic, work()).is_err());
        assert!(Reserved::deleting(&traffic, at("other.example", "Work")).is_ok());
        drop(into);
        assert!(Reserved::deleting(&traffic, work()).is_err());
        drop(out);
        let deleting = Reserved::deleting(&traffic, work()).unwrap();
        assert!(Reserved::moving(&traffic, inbox(), work()).is_err());
        assert!(Reserved::moving(&traffic, work(), inbox()).is_err());
        assert!(Reserved::deleting(&traffic, work()).is_err());
        // Nothing's taken by a refusal.
        assert!(Reserved::deleting(&traffic, inbox()).is_ok());
        drop(deleting);
        assert!(Reserved::moving(&traffic, inbox(), work()).is_ok());
    }

    #[test]
    fn the_first_fetch_is_a_sync_and_later_ones_are_new_mail() {
        let mut store = Store::open_in_memory().unwrap();
        store.save_account(&account()).unwrap();
        let first = save(&mut store, messages(1, &[1, 2])).remove(0);
        assert!(
            matches!(
                first,
                Update::Mail {
                    count: 2,
                    first_sync: true,
                    ref unread,
                    ..
                } if unread.is_empty()
            ),
            "got {first:?}"
        );
        let next = save(&mut store, messages(1, &[3])).remove(0);
        assert!(
            matches!(
                next,
                Update::Mail {
                    count: 1,
                    first_sync: false,
                    ref unread,
                    ..
                } if unread.iter().map(|m| m.uid).eq([3])
            ),
            "got {next:?}"
        );
        assert_eq!(store.recent_messages(Role::Inbox, 10).unwrap().len(), 3);
    }

    #[test]
    fn new_mail_already_read_is_not_unread() {
        let mut store = Store::open_in_memory().unwrap();
        store.save_account(&account()).unwrap();
        save(&mut store, messages(1, &[1]));
        let mut job = messages(1, &[2, 3]);
        if let Job::Watched(Event::Messages { messages, .. }) = &mut job
            && let Some(read) = messages.first_mut()
        {
            read.seen = true;
        }
        let update = save(&mut store, job).remove(0);
        assert!(
            matches!(
                update,
                Update::Mail { count: 2, ref unread, .. } if unread.iter().map(|m| m.uid).eq([3])
            ),
            "got {update:?}"
        );
    }

    #[test]
    fn older_mail_is_saved_without_counting_as_new() {
        let mut store = Store::open_in_memory().unwrap();
        store.save_account(&account()).unwrap();
        save(&mut store, messages(1, &[10]));
        let older: Vec<Envelope> = [4, 5]
            .into_iter()
            .map(|uid| Envelope {
                uid,
                message_id: None,
                from: Address::default(),
                to: Vec::new(),
                subject: None,
                date: None,
                seen: false,
                flagged: false,
                in_reply_to: None,
                references: Vec::new(),
            })
            .collect();
        let job = |uid_validity, older: &[Envelope]| Job::Older {
            account: "me@example.com".into(),
            mailbox: "INBOX".into(),
            uid_validity,
            result: Ok(Older {
                messages: older.to_vec(),
                done: false,
            }),
            asked: 0,
        };

        let updates = save(&mut store, job(1, &older));
        assert!(
            matches!(
                updates.as_slice(),
                [Update::Older {
                    count: 2,
                    done: false,
                    ..
                }]
            ),
            "got {updates:?}"
        );
        assert_eq!(store.recent_messages(Role::Inbox, 10).unwrap().len(), 3);

        let stale = save(&mut store, job(2, &older));
        assert!(
            matches!(
                stale.as_slice(),
                [Update::Older {
                    count: 0,
                    done: true,
                    ..
                }]
            ),
            "got {stale:?}"
        );
        let empty = save(&mut store, job(1, &[]));
        assert!(
            matches!(empty.as_slice(), [Update::Older { done: true, .. }]),
            "nothing came back, so nothing more to ask for: {empty:?}"
        );
    }

    #[test]
    fn an_unread_search_asked_for_while_under_way_runs_again_after() {
        let finding = Finding::default();
        let inbox = ("me@example.com".to_owned(), "INBOX".to_owned());
        assert!(finding.start(&inbox));
        assert!(!finding.start(&inbox), "under way");
        assert!(!finding.start(&inbox), "asked twice, once more is enough");
        assert!(finding.end(&inbox), "asked for again");
        assert!(finding.start(&inbox), "that search");
        assert!(!finding.end(&inbox), "nothing asked meanwhile");
    }

    #[test]
    fn a_message_the_server_wont_mark_read_opens_and_stays_unread() {
        let mut store = Store::open_in_memory().unwrap();
        store.save_account(&account()).unwrap();
        save(&mut store, messages(1, &[1]));
        let updates = save(
            &mut store,
            Job::Opened {
                account: "me@example.com".into(),
                mailbox: "INBOX".into(),
                uid_validity: 1,
                uid: 1,
                body: Ok(Body::default()),
                marked: false,
                at: 1,
                cleared: 0,
            },
        );
        assert!(
            matches!(updates.as_slice(), [Update::Opened { body: Ok(_), .. }]),
            "{updates:?}"
        );
        assert_eq!(
            store.seen("me@example.com", "INBOX", 1).unwrap(),
            Some(false)
        );
    }

    #[test]
    fn a_search_that_found_nothing_unread_counts_as_looked_in() {
        let mut store = Store::open_in_memory().unwrap();
        store.save_account(&account()).unwrap();
        save(&mut store, messages(1, &[1, 2]));
        assert!(!store.unread_searched("me@example.com", "INBOX").unwrap());
        // Cached from its first message, and nothing unread.
        let skipped = Job::Unread {
            account: "me@example.com".into(),
            mailbox: "INBOX".into(),
            uid_validity: 1,
            below: 1,
            result: Ok(Unread::default()),
            asked: 0,
            again: false,
        };
        save(&mut store, skipped);
        assert!(store.unread_searched("me@example.com", "INBOX").unwrap());
        assert_eq!(
            store.seen("me@example.com", "INBOX", 1).unwrap(),
            Some(false)
        );
    }

    #[test]
    fn an_unread_search_from_a_replaced_watch_still_runs_again_if_asked() {
        let job = |again| Job::Unread {
            account: "me@example.com".into(),
            mailbox: "INBOX".into(),
            uid_validity: 1,
            below: 10,
            result: Ok(Unread {
                upto: 10,
                unseen: vec![3],
                messages: Vec::new(),
            }),
            asked: 0,
            again,
        };
        for again in [false, true] {
            assert!(
                matches!(
                    superseded(job(again)),
                    Job::Done(Update::Unread { result: Err(_), again: a, .. }) if a == again
                ),
                "again: {again}"
            );
        }
    }

    #[test]
    fn text_peeked_from_a_replaced_watch_isnt_saved() {
        let job = Job::Peeked {
            account: "me@example.com".into(),
            mailbox: "INBOX".into(),
            uid_validity: 1,
            cleared: 0,
            result: Ok(Prefetched::default()),
        };
        assert!(job.account().is_some(), "checked against its watch");
        assert!(matches!(
            superseded(job),
            Job::Peeked { result: Err(_), .. }
        ));
    }

    #[test]
    fn search_results_from_before_a_mailbox_restarted_are_refused() {
        let mut store = Store::open_in_memory().unwrap();
        store.save_account(&account()).unwrap();
        save(&mut store, messages(1, &[1]));
        let job = |uid_validity| Job::Searched {
            id: 3,
            account: "me@example.com".into(),
            mailbox: "INBOX".into(),
            uid_validity,
            result: Ok(Found {
                messages: Vec::new(),
                more: false,
            }),
        };
        assert!(
            matches!(
                save(&mut store, job(1)).as_slice(),
                [Update::Searched {
                    id: 3,
                    result: Ok(_),
                    ..
                }]
            ),
            "same UIDVALIDITY"
        );
        save(&mut store, messages(2, &[1]));
        let stale = save(&mut store, job(1));
        assert!(
            matches!(stale.as_slice(), [Update::Searched { result: Err(_), .. }]),
            "got {stale:?}"
        );
    }

    /// A prefetcher downloading the newest `count`, with no account logged in, so it never
    /// connects; and the runtime it would download on, never run, and the channel it reports on.
    fn prefetcher(count: usize) -> ((Runtime, UnboundedSender<Job>), Prefetcher) {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .build()
            .unwrap();
        let (jobs, received) = unbounded_channel::<Job>();
        drop(received);
        let prefetcher = Prefetcher {
            handle: runtime.handle().clone(),
            jobs: jobs.downgrade(),
            logins: Logins::default(),
            watches: Arc::new(Mutex::new(HashMap::from([(
                "me@example.com".to_owned(),
                1,
            )]))),
            count: Arc::new(AtomicUsize::new(count)),
            keep_opened: Arc::new(AtomicBool::new(false)),
            cleared: Arc::new(AtomicU64::new(0)),
            busy: HashSet::new(),
            skipped: HashSet::new(),
            failed: HashSet::new(),
            threading: HashSet::new(),
        };
        ((runtime, jobs), prefetcher)
    }

    fn prefetched(uid_validity: u32, uids: &[u32], too_big: &[u32]) -> Job {
        prefetched_in("INBOX", uid_validity, uids, too_big)
    }

    fn prefetched_in(mailbox: &str, uid_validity: u32, uids: &[u32], too_big: &[u32]) -> Job {
        Job::Prefetched {
            account: "me@example.com".into(),
            mailbox: mailbox.into(),
            watch: 1,
            uid_validity,
            uids: uids.iter().chain(too_big).copied().collect(),
            result: Ok(Prefetched {
                bodies: uids.iter().map(|&uid| (uid, Body::default())).collect(),
                too_big: too_big.to_vec(),
            }),
        }
    }

    #[test]
    fn threads_read_under_a_replaced_watch_are_not_saved() {
        let mut store = Store::open_in_memory().unwrap();
        store.save_account(&account()).unwrap();
        let (_runtime, mut prefetcher) = prefetcher(0);
        prefetcher.save(&mut store, messages(1, &[2]));
        let threaded = |watch| Job::Threaded {
            account: "me@example.com".into(),
            mailbox: "INBOX".into(),
            watch,
            uid_validity: 1,
            uids: vec![2],
            result: Ok(vec![(2, Some("parent@example.com".into()), Vec::new())]),
        };
        let parent = |store: &Store| {
            store
                .message("me@example.com", "INBOX", 2)
                .unwrap()
                .and_then(|m| m.envelope.in_reply_to)
        };
        // Read under watch 2, while the account is watched as 1: of another server.
        prefetcher.save(&mut store, threaded(2));
        assert_eq!(parent(&store), None, "replaced");
        let updates = prefetcher.save(&mut store, threaded(1));
        assert!(
            matches!(&updates[..], [Update::Threaded { .. }]),
            "got {updates:?}"
        );
        assert_eq!(parent(&store).as_deref(), Some("parent@example.com"));
    }

    #[test]
    fn only_the_current_watch_of_an_account_is_saved() {
        let mut store = Store::open_in_memory().unwrap();
        store.save_account(&account()).unwrap();
        let (_runtime, mut prefetcher) = prefetcher(0);
        let watching = |watch, uid| match messages(1, &[uid]) {
            Job::Watched(event) => Job::Watching { watch, event },
            other => other,
        };
        let saved = |store: &Store, uid| {
            store
                .message("me@example.com", "INBOX", uid)
                .unwrap()
                .is_some()
        };

        // Not watched (any more): what its watchers queued is dropped.
        prefetcher.watches.lock().unwrap().clear();
        prefetcher.save(&mut store, watching(1, 1));
        assert!(!saved(&store, 1), "stopped");

        prefetcher
            .watches
            .lock()
            .unwrap()
            .insert("me@example.com".to_owned(), 2);
        prefetcher.save(&mut store, watching(1, 1));
        assert!(!saved(&store, 1), "replaced: another server's mailboxes");
        prefetcher.save(&mut store, watching(2, 2));
        assert!(saved(&store, 2), "the current watch");

        // Asked of the old server: failed, and nothing of it saved.
        let seen = |watch| Job::Asked {
            watch,
            job: Box::new(Job::Seen {
                account: "me@example.com".into(),
                mailbox: "INBOX".into(),
                uid_validity: 1,
                uid: 2,
                seen: true,
                result: Ok(()),
                at: 0,
            }),
        };
        let is_seen = |store: &Store| store.seen("me@example.com", "INBOX", 2).unwrap();
        let updates = prefetcher.save(&mut store, seen(1));
        assert!(
            matches!(&updates[..], [Update::SeenFailed { error, .. }] if error.contains("replaced")),
            "got {updates:?}"
        );
        assert_eq!(is_seen(&store), Some(false));
        prefetcher.save(&mut store, seen(2));
        assert_eq!(is_seen(&store), Some(true), "asked of the current watch");

        let mut old = prefetched(1, &[2], &[]);
        if let Job::Prefetched { watch, .. } = &mut old {
            *watch = 1;
        }
        prefetcher.save(&mut store, old);
        assert_eq!(store.body("me@example.com", "INBOX", 2).unwrap(), None);
    }

    #[test]
    fn bodies_downloaded_ahead_are_kept_only_under_their_uid_validity() {
        let mut store = Store::open_in_memory().unwrap();
        store.save_account(&account()).unwrap();
        let (_runtime, mut prefetcher) = prefetcher(10);
        prefetcher.save(&mut store, messages(1, &[1, 2, 3]));
        prefetcher.busy.insert("me@example.com".to_owned());
        let body = |store: &Store, uid| store.body("me@example.com", "INBOX", uid).unwrap();

        assert_eq!(
            prefetcher.save(&mut store, prefetched(1, &[1, 2], &[3])),
            []
        );
        assert!(body(&store, 1).is_some() && body(&store, 2).is_some());
        assert!(prefetcher.busy.is_empty(), "no longer downloading");
        assert!(
            prefetcher.skipped.contains(&(
                "me@example.com".to_owned(),
                1,
                "INBOX".to_owned(),
                1,
                3
            )),
            "too large, not asked for again"
        );

        // Started over: UID 3 names another message, and the bodies went with the old ones.
        prefetcher.save(&mut store, messages(2, &[3]));
        assert!(body(&store, 1).is_none());
        prefetcher.save(&mut store, prefetched(1, &[3], &[]));
        assert!(
            body(&store, 3).is_none(),
            "downloaded under the old UIDVALIDITY"
        );
    }

    #[test]
    fn mailboxes_chosen_are_downloaded_ahead_after_the_inbox() {
        let mut store = Store::open_in_memory().unwrap();
        store.save_account(&account()).unwrap();
        let (_runtime, mut prefetcher) = prefetcher(10);
        prefetcher.logins.lock().unwrap().insert(
            "me@example.com".to_owned(),
            (
                account().imap,
                Credentials::Password(String::from("secret").into()),
                1,
            ),
        );
        let in_mailbox = |mailbox, uids: &[u32]| messages_in(mailbox, Role::Other, 1, uids);
        let downloaded = |mailbox, uids: &[u32]| prefetched_in(mailbox, 1, uids, &[]);
        let body = |store: &Store, mailbox, uid| {
            store
                .body("me@example.com", mailbox, uid)
                .unwrap()
                .is_some()
        };
        store
            .set_prefetch_mailbox("me@example.com", "Receipts", true)
            .unwrap();
        prefetcher.save(&mut store, in_mailbox("News", &[1, 2]));
        assert!(prefetcher.busy.is_empty(), "not chosen");
        prefetcher.save(&mut store, messages(1, &[1]));
        prefetcher.save(&mut store, in_mailbox("Receipts", &[1, 2]));
        prefetcher.save(&mut store, downloaded("INBOX", &[1]));
        assert!(
            prefetcher.busy.contains("me@example.com"),
            "the inbox done, Receipts started"
        );
        prefetcher.save(&mut store, downloaded("Receipts", &[1, 2]));
        assert!(prefetcher.busy.is_empty(), "nothing more to download");
        assert!(body(&store, "INBOX", 1));
        assert!(body(&store, "Receipts", 1) && body(&store, "Receipts", 2));

        // No longer chosen: what's downloaded ahead goes, and what arrives late isn't saved.
        store
            .set_prefetch_mailbox("me@example.com", "Receipts", false)
            .unwrap();
        assert!(!body(&store, "Receipts", 1));
        prefetcher.save(&mut store, downloaded("Receipts", &[1, 2]));
        assert!(!body(&store, "Receipts", 1));
        assert!(body(&store, "INBOX", 1), "the inbox's stay");
    }

    #[test]
    fn a_mailbox_that_fails_is_passed_over_then_tried_again() {
        let mut store = Store::open_in_memory().unwrap();
        store.save_account(&account()).unwrap();
        let (_runtime, mut prefetcher) = prefetcher(10);
        prefetcher.logins.lock().unwrap().insert(
            "me@example.com".to_owned(),
            (
                account().imap,
                Credentials::Password(String::from("secret").into()),
                1,
            ),
        );
        let in_mailbox = |mailbox, uids: &[u32]| messages_in(mailbox, Role::Other, 1, uids);
        let downloaded = |mailbox, uids: &[u32]| prefetched_in(mailbox, 1, uids, &[]);
        let failed = || Job::Prefetched {
            account: "me@example.com".into(),
            mailbox: "Archive".into(),
            watch: 1,
            uid_validity: 1,
            uids: vec![1],
            result: Err("offline".into()),
        };
        let busy = |prefetcher: &Prefetcher| prefetcher.busy.contains("me@example.com");
        for mailbox in ["Archive", "Receipts"] {
            store
                .set_prefetch_mailbox("me@example.com", mailbox, true)
                .unwrap();
        }
        prefetcher.save(&mut store, messages(1, &[1]));
        prefetcher.save(&mut store, in_mailbox("Archive", &[1]));
        prefetcher.save(&mut store, in_mailbox("Receipts", &[1]));
        prefetcher.save(&mut store, downloaded("INBOX", &[1]));
        assert!(busy(&prefetcher), "the inbox done, Archive started");

        // Failing, it doesn't hold up the next.
        prefetcher.save(&mut store, failed());
        assert!(busy(&prefetcher), "Receipts started");
        prefetcher.save(&mut store, downloaded("Receipts", &[1]));
        assert!(!busy(&prefetcher), "Archive isn't asked for again");

        // New mail tries it again.
        prefetcher.save(&mut store, in_mailbox("Receipts", &[2]));
        assert!(busy(&prefetcher), "asked for again once mail synced");
        prefetcher.save(&mut store, failed());
        prefetcher.save(&mut store, downloaded("Receipts", &[2]));
        assert!(!busy(&prefetcher));

        // So does a sync without new mail.
        let synced = Job::Watching {
            watch: 1,
            event: Event::Synced {
                account: "me@example.com".into(),
                mailbox: Some("Archive".into()),
                asked: 0,
            },
        };
        prefetcher.save(&mut store, synced);
        assert!(busy(&prefetcher), "asked for again once synced");
        prefetcher.save(&mut store, failed());
        assert!(!busy(&prefetcher));

        // And a message opened, downloaded from the server.
        let opened = Job::Opened {
            account: "me@example.com".into(),
            mailbox: "INBOX".into(),
            uid_validity: 1,
            uid: 1,
            body: Ok(Body::default()),
            marked: false,
            at: 1,
            cleared: 0,
        };
        prefetcher.save(&mut store, opened);
        assert!(busy(&prefetcher), "asked for again once a message opened");
        prefetcher.save(&mut store, failed());
        assert!(!busy(&prefetcher));

        // And clearing what's downloaded, new mail or not.
        prefetcher.save(&mut store, Job::Clear);
        assert!(prefetcher.failed.is_empty());
        assert!(busy(&prefetcher), "downloading anew");
    }

    #[test]
    fn one_batch_at_a_time_is_downloaded_ahead_as_the_setting_is_then() {
        let mut store = Store::open_in_memory().unwrap();
        store.save_account(&account()).unwrap();
        let (_runtime, mut prefetcher) = prefetcher(100);
        prefetcher.logins.lock().unwrap().insert(
            "me@example.com".to_owned(),
            (
                account().imap,
                Credentials::Password(String::from("secret").into()),
                1,
            ),
        );
        let uids: Vec<u32> = (1..=50).collect();
        prefetcher.save(&mut store, messages(1, &uids));
        assert!(
            prefetcher.busy.contains("me@example.com"),
            "a batch started"
        );
        // More mail meanwhile doesn't start a second batch.
        prefetcher.save(&mut store, messages(1, &[51]));
        assert_eq!(prefetcher.busy.len(), 1);

        // Turned off while the batch downloaded: once it's back, no other starts.
        prefetcher.count.store(0, Ordering::Relaxed);
        let newest: Vec<u32> = (32..=51).rev().collect();
        prefetcher.save(&mut store, prefetched(1, &newest, &[]));
        assert!(prefetcher.busy.is_empty(), "nothing more asked for");
        assert_eq!(
            store
                .missing_bodies("me@example.com", "INBOX", 100)
                .unwrap()
                .len(),
            51,
            "and what came back was dropped"
        );
    }

    #[test]
    fn a_message_gone_from_the_server_is_not_asked_for_again() {
        let mut store = Store::open_in_memory().unwrap();
        store.save_account(&account()).unwrap();
        let (_runtime, mut prefetcher) = prefetcher(10);
        prefetcher.save(&mut store, messages(1, &[1, 2]));
        // UID 2 was asked for, but expunged: the server returned only UID 1.
        let job = Job::Prefetched {
            watch: 1,
            account: "me@example.com".into(),
            mailbox: "INBOX".into(),
            uid_validity: 1,
            uids: vec![1, 2],
            result: Ok(Prefetched {
                bodies: vec![(1, Body::default())],
                too_big: Vec::new(),
            }),
        };
        prefetcher.save(&mut store, job);
        assert!(
            prefetcher.skipped.contains(&(
                "me@example.com".to_owned(),
                1,
                "INBOX".to_owned(),
                1,
                2
            )),
            "skipped, not retried in a loop"
        );
        let failed = Job::Prefetched {
            watch: 1,
            account: "me@example.com".into(),
            mailbox: "INBOX".into(),
            uid_validity: 1,
            uids: vec![3],
            result: Err("offline".into()),
        };
        prefetcher.save(&mut store, failed);
        assert!(
            !prefetcher.skipped.contains(&(
                "me@example.com".to_owned(),
                1,
                "INBOX".to_owned(),
                1,
                3
            )),
            "a failure is tried again later"
        );
    }

    #[test]
    fn asking_for_fewer_drops_the_older_bodies_downloaded_ahead() {
        let mut store = Store::open_in_memory().unwrap();
        store.save_account(&account()).unwrap();
        let (_runtime, mut prefetcher) = prefetcher(3);
        prefetcher.save(&mut store, messages(1, &[1, 2, 3]));
        prefetcher.save(&mut store, prefetched(1, &[1, 2, 3], &[]));
        let cached = |store: &Store| {
            (1..=3)
                .filter(|&uid| {
                    store
                        .body("me@example.com", "INBOX", uid)
                        .unwrap()
                        .is_some()
                })
                .count()
        };
        assert_eq!(cached(&store), 3);
        prefetcher.count.store(1, Ordering::Relaxed);
        let prefetch = || Job::Prefetch {
            account: "me@example.com".into(),
        };
        prefetcher.save(&mut store, prefetch());
        assert_eq!(cached(&store), 1, "only the newest");
        prefetcher.count.store(0, Ordering::Relaxed);
        prefetcher.save(&mut store, prefetch());
        assert_eq!(cached(&store), 0, "off, nothing is kept");
    }

    #[test]
    fn an_opened_inbox_message_is_kept_only_while_downloading_ahead() {
        let mut store = Store::open_in_memory().unwrap();
        store.save_account(&account()).unwrap();
        let opened = |uid_validity| Job::Opened {
            account: "me@example.com".into(),
            mailbox: "INBOX".into(),
            uid_validity,
            uid: 1,
            body: Ok(Body::default()),
            marked: true,
            at: 0,
            cleared: 0,
        };
        let (_runtime, mut off) = prefetcher(0);
        off.save(&mut store, messages(1, &[1]));
        off.save(&mut store, opened(1));
        assert!(store.body("me@example.com", "INBOX", 1).unwrap().is_none());
        let (_runtime, mut on) = prefetcher(5);
        on.save(&mut store, opened(2));
        assert!(
            store.body("me@example.com", "INBOX", 1).unwrap().is_none(),
            "downloaded under another UIDVALIDITY"
        );
        let updates = on.save(&mut store, opened(1));
        assert!(
            matches!(updates.as_slice(), [Update::Opened { body: Ok(_), .. }]),
            "got {updates:?}"
        );
        assert!(store.body("me@example.com", "INBOX", 1).unwrap().is_some());
    }

    #[test]
    fn opened_messages_are_kept_in_any_mailbox_past_the_newest() {
        let (mut store, _runtime, mut prefetcher) = keeping();
        prefetcher.save(&mut store, opening("Archive", 0));
        assert!(cached(&store, "Archive"), "not only the inbox's");
        // UID 1 isn't among the one newest downloaded ahead, but stays.
        prefetcher.save(&mut store, opening("INBOX", 0));
        assert!(cached(&store, "INBOX"));
        prefetcher.save(
            &mut store,
            Job::Prefetch {
                account: "me@example.com".into(),
            },
        );
        assert!(cached(&store, "INBOX"), "not trimmed");
    }

    /// The store, with an archived message 1 and inbox messages 1 and 2, and a prefetcher
    /// downloading the newest one ahead and keeping opened messages.
    fn keeping() -> (Store, (Runtime, UnboundedSender<Job>), Prefetcher) {
        let mut store = Store::open_in_memory().unwrap();
        store.save_account(&account()).unwrap();
        let (runtime, mut prefetcher) = prefetcher(1);
        prefetcher.keep_opened.store(true, Ordering::Relaxed);
        let mut archived = messages(1, &[1]);
        if let Job::Watched(Event::Messages { mailbox, role, .. }) = &mut archived {
            *mailbox = "Archive".into();
            *role = Role::Archive;
        }
        prefetcher.save(&mut store, archived);
        prefetcher.save(&mut store, messages(1, &[1, 2]));
        (store, runtime, prefetcher)
    }

    /// Message 1 of `mailbox` downloaded, asked for before `cleared` clears.
    fn opening(mailbox: &str, cleared: u64) -> Job {
        Job::Opened {
            account: "me@example.com".into(),
            mailbox: mailbox.into(),
            uid_validity: 1,
            uid: 1,
            body: Ok(Body::default()),
            marked: true,
            at: 0,
            cleared,
        }
    }

    fn cached(store: &Store, mailbox: &str) -> bool {
        store.body("me@example.com", mailbox, 1).unwrap().is_some()
    }

    #[test]
    fn opened_messages_are_released_after_those_being_kept() {
        let (mut store, _runtime, mut prefetcher) = keeping();
        // Kept as the setting was when its turn came, then turned off: released after.
        prefetcher.save(&mut store, opening("Archive", 0));
        prefetcher.save(&mut store, opening("INBOX", 0));
        prefetcher.keep_opened.store(false, Ordering::Relaxed);
        assert_eq!(prefetcher.save(&mut store, Job::Release), []);
        assert!(!cached(&store, "Archive"));
        assert!(!cached(&store, "INBOX"), "not among the newest one");

        // Turned on again before the release's turn: still kept.
        prefetcher.keep_opened.store(true, Ordering::Relaxed);
        prefetcher.save(&mut store, opening("Archive", 0));
        prefetcher.save(&mut store, Job::Release);
        assert!(cached(&store, "Archive"));
    }

    #[test]
    fn text_peeked_before_a_clear_isnt_kept() {
        let (mut store, _runtime, mut prefetcher) = keeping();
        let peeked = |cleared| Job::Peeked {
            account: "me@example.com".into(),
            mailbox: "Archive".into(),
            uid_validity: 1,
            cleared,
            result: Ok(Prefetched {
                bodies: vec![(1, Body::default())],
                too_big: Vec::new(),
            }),
        };
        prefetcher.cleared.fetch_add(1, Ordering::Relaxed);
        prefetcher.save(&mut store, peeked(0));
        assert!(!cached(&store, "Archive"), "asked for before the clear");
        prefetcher.save(&mut store, peeked(1));
        assert!(cached(&store, "Archive"));
    }

    #[test]
    fn a_message_opened_before_a_clear_is_shown_but_not_kept() {
        let (mut store, _runtime, mut prefetcher) = keeping();
        prefetcher.save(&mut store, opening("Archive", 0));
        prefetcher.cleared.fetch_add(1, Ordering::Relaxed);
        let updates = prefetcher.save(&mut store, Job::Clear);
        assert!(
            matches!(updates.as_slice(), [Update::Cleared(Ok(1))]),
            "got {updates:?}"
        );
        assert!(!cached(&store, "Archive"));

        let updates = prefetcher.save(&mut store, opening("Archive", 0));
        assert!(
            matches!(updates.as_slice(), [Update::Opened { body: Ok(_), .. }]),
            "got {updates:?}"
        );
        assert!(!cached(&store, "Archive"), "asked before the clear");
        prefetcher.save(&mut store, opening("Archive", 1));
        assert!(cached(&store, "Archive"), "asked after");
    }

    #[test]
    fn dropping_the_mailroom_ends_the_store_thread() {
        let dir = std::env::temp_dir().join(format!("postden-mailroom-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let (ended, done) = std::sync::mpsc::channel::<()>();
        // The callback goes with the store thread: once it's dropped, the thread ended.
        let mailroom = Mailroom::start(
            &dir.join("db"),
            WatchConfig::default(),
            50,
            false,
            move |_| {
                let _ = &ended;
            },
        )
        .unwrap();
        drop(mailroom);
        let result = done.recv_timeout(Duration::from_secs(5));
        let _ignored = std::fs::remove_dir_all(&dir);
        assert_eq!(
            result,
            Err(std::sync::mpsc::RecvTimeoutError::Disconnected),
            "still running"
        );
    }

    #[test]
    fn mail_for_a_removed_account_is_dropped() {
        let mut store = Store::open_in_memory().unwrap();
        assert_eq!(save(&mut store, messages(1, &[1])), vec![]);
    }

    #[test]
    fn an_opened_message_is_read_but_a_failed_open_changes_nothing() {
        let mut store = Store::open_in_memory().unwrap();
        store.save_account(&account()).unwrap();
        save(&mut store, messages(1, &[1]));
        let opened = |body| Job::Opened {
            account: "me@example.com".into(),
            mailbox: "INBOX".into(),
            uid_validity: 1,
            uid: 1,
            body,
            marked: true,
            at: 0,
            cleared: 0,
        };
        let seen = |store: &Store| {
            store.recent_messages(Role::Inbox, 1).unwrap()[0]
                .envelope
                .seen
        };

        save(&mut store, opened(Err("offline".into())));
        assert!(!seen(&store), "still unread");
        let updates = save(&mut store, opened(Ok(Body::default())));
        assert!(
            matches!(updates.as_slice(), [Update::Opened { body: Ok(_), .. }]),
            "got {updates:?}"
        );
        assert!(seen(&store), "read once opened");
    }

    #[test]
    fn marking_unread_only_sticks_when_the_server_agreed() {
        let mut store = Store::open_in_memory().unwrap();
        store.save_account(&account()).unwrap();
        save(&mut store, messages(1, &[1]));
        store.set_seen("me@example.com", "INBOX", 1, true).unwrap();
        let job = |result| Job::Seen {
            account: "me@example.com".into(),
            mailbox: "INBOX".into(),
            uid_validity: 1,
            uid: 1,
            seen: false,
            result,
            at: 0,
        };
        let seen = |store: &Store| {
            store.recent_messages(Role::Inbox, 1).unwrap()[0]
                .envelope
                .seen
        };

        let failed = save(&mut store, job(Err("offline".into())));
        assert!(matches!(failed.as_slice(), [Update::SeenFailed { .. }]));
        assert!(seen(&store));
        let done = save(&mut store, job(Ok(())));
        assert!(matches!(
            done.as_slice(),
            [Update::Seen { seen: false, .. }]
        ));
        assert!(!seen(&store));
    }

    #[test]
    fn a_mailbox_marked_read_is_read_in_the_store_only_under_its_uid_validity() {
        let mut store = Store::open_in_memory().unwrap();
        store.save_account(&account()).unwrap();
        save(&mut store, messages(1, &[1, 2]));
        let job = |uid_validity, result| Job::AllSeen {
            account: "me@example.com".into(),
            mailbox: "INBOX".into(),
            uid_validity,
            result,
            at: 0,
        };
        let seen = |store: &Store, uid| store.seen("me@example.com", "INBOX", uid).unwrap();

        let failed = save(&mut store, job(1, Err("offline".into())));
        assert!(matches!(
            failed.as_slice(),
            [Update::AllSeen { result: Err(_), .. }]
        ));
        assert_eq!(seen(&store, 1), Some(false));
        let stale = save(&mut store, job(2, Ok(3)));
        assert!(matches!(
            stale.as_slice(),
            [Update::AllSeen {
                result: Ok(None),
                ..
            }]
        ));
        assert_eq!(seen(&store, 1), Some(false), "another UIDVALIDITY's UIDs");
        let done = save(&mut store, job(1, Ok(2)));
        assert!(matches!(
            done.as_slice(),
            [Update::AllSeen {
                result: Ok(Some(2)),
                ..
            }]
        ));
        assert_eq!(seen(&store, 1), Some(true));
        assert_eq!(seen(&store, 2), Some(false), "came after it was marked");
    }

    #[test]
    fn a_moved_message_leaves_the_store_only_once_the_server_moved_it() {
        let mut store = Store::open_in_memory().unwrap();
        store.save_account(&account()).unwrap();
        save(&mut store, messages(1, &[1]));
        let job = |result| {
            let moving = Moving {
                account: "me@example.com".into(),
                mailbox: "INBOX".into(),
                uid_validity: 1,
                uid: 1,
                to: "Archive".into(),
                snooze: None,
            };
            Job::Moved(moving, result)
        };
        let cached = |store: &Store| store.recent_messages(Role::Inbox, 10).unwrap().len();

        let failed = save(&mut store, job(Err("offline".into())));
        assert!(
            matches!(failed.as_slice(), [Update::Moved { result: Err(_), .. }]),
            "got {failed:?}"
        );
        assert_eq!(cached(&store), 1, "still where it was");
        let moved = save(&mut store, job(Ok(())));
        assert!(
            matches!(
                moved.as_slice(),
                [Update::Moved { to, result: Ok(true), .. }] if to == "Archive"
            ),
            "got {moved:?}"
        );
        assert_eq!(cached(&store), 0);

        // The mailbox started over before the move was answered: UID 1 is another message now.
        save(&mut store, messages(2, &[1]));
        let stale = save(&mut store, job(Ok(())));
        assert!(
            matches!(
                stale.as_slice(),
                [Update::Moved {
                    result: Ok(false),
                    ..
                }]
            ),
            "it went through, but UID 1 no longer names it: {stale:?}"
        );
        assert_eq!(cached(&store), 1, "the message now at UID 1 stays");
        save(&mut store, messages(2, &[1]));
        assert_eq!(cached(&store), 1, "and later syncs don't skip it");
    }

    fn snooze(until: i64) -> Snooze {
        Snooze {
            account: "me@example.com".into(),
            message_id: "a@example.com".into(),
            mailbox: "Snoozed".into(),
            until,
        }
    }

    #[test]
    fn a_snooze_is_kept_whatever_came_of_its_move() {
        let mut store = Store::open_in_memory().unwrap();
        store.save_account(&account()).unwrap();
        save(&mut store, messages(1, &[1, 2]));
        let job = |uid, result| {
            let moving = Moving {
                account: "me@example.com".into(),
                mailbox: "INBOX".into(),
                uid_validity: 1,
                uid,
                to: "Snoozed".into(),
                snooze: Some(100),
            };
            Job::Moved(moving, result)
        };

        store.snooze(&snooze(100)).unwrap();
        let failed = save(&mut store, job(1, Err("offline".into())));
        assert!(
            matches!(
                failed.as_slice(),
                [Update::Snoozed {
                    until: 100,
                    result: Err(_),
                    ..
                }]
            ),
            "got {failed:?}"
        );
        // It may have moved all the same; if not, waking it finds nothing and forgets it.
        assert_eq!(store.due_snoozes(100).unwrap(), vec![snooze(100)]);

        let moved = save(&mut store, job(2, Ok(())));
        assert!(
            matches!(
                moved.as_slice(),
                [Update::Snoozed { to, result: Ok(true), .. }] if to == "Snoozed"
            ),
            "got {moved:?}"
        );
        assert_eq!(store.due_snoozes(100).unwrap(), vec![snooze(100)]);
        assert_eq!(
            store.recent_messages(Role::Inbox, 10).unwrap().len(),
            1,
            "it left the inbox"
        );
    }

    #[test]
    fn a_woken_message_is_no_longer_snoozed_or_listed_where_it_waited() {
        let mut store = Store::open_in_memory().unwrap();
        store.save_account(&account()).unwrap();
        let waiting = Job::Watched(Event::Messages {
            account: "me@example.com".into(),
            mailbox: "Snoozed".into(),
            role: Role::Snoozed,
            uid_validity: 7,
            messages: vec![Envelope {
                uid: 3,
                message_id: Some("a@example.com".into()),
                from: Address::default(),
                to: Vec::new(),
                subject: None,
                date: None,
                seen: true,
                flagged: false,
                in_reply_to: None,
                references: Vec::new(),
            }],
            asked: 0,
        });
        save(&mut store, waiting);
        store.snooze(&snooze(100)).unwrap();
        let cached = |store: &Store| store.recent_messages(Role::Snoozed, 10).unwrap().len();

        let failed = save(&mut store, Job::Woke(snooze(100), Err("offline".into())));
        assert!(
            matches!(failed.as_slice(), [Update::Woke { result: Err(_), .. }]),
            "got {failed:?}"
        );
        assert_eq!(
            store.due_snoozes(100).unwrap(),
            vec![snooze(100)],
            "tried again later"
        );
        assert_eq!(cached(&store), 1);

        let woke = save(&mut store, Job::Woke(snooze(100), Ok(Some((7, vec![3])))));
        assert!(
            matches!(woke.as_slice(), [Update::Woke { uids, result: Ok(()), .. }] if uids == &[3]),
            "got {woke:?}"
        );
        assert_eq!(store.due_snoozes(100).unwrap(), vec![]);
        assert_eq!(cached(&store), 0, "it left the Snoozed mailbox");

        // A Snoozed mailbox the server won't open, as one never created, holds nothing to wake.
        store.snooze(&snooze(200)).unwrap();
        let missing = save(&mut store, Job::Woke(snooze(200), Ok(None)));
        assert!(
            matches!(missing.as_slice(), [Update::Woke { result: Ok(()), .. }]),
            "got {missing:?}"
        );
        assert_eq!(
            store.due_snoozes(200).unwrap(),
            vec![],
            "forgotten, not retried"
        );
    }

    #[test]
    fn a_star_only_sticks_when_the_server_agreed() {
        let mut store = Store::open_in_memory().unwrap();
        store.save_account(&account()).unwrap();
        save(&mut store, messages(1, &[1]));
        let job = |result| Job::Flagged {
            account: "me@example.com".into(),
            mailbox: "INBOX".into(),
            uid_validity: 1,
            uid: 1,
            flagged: true,
            result,
            at: 0,
        };
        let flagged = |store: &Store| {
            store.recent_messages(Role::Inbox, 1).unwrap()[0]
                .envelope
                .flagged
        };

        let failed = save(&mut store, job(Err("offline".into())));
        assert!(matches!(
            failed.as_slice(),
            [Update::Flagged { result: Err(_), .. }]
        ));
        assert!(!flagged(&store));
        let done = save(&mut store, job(Ok(())));
        assert!(matches!(
            done.as_slice(),
            [Update::Flagged {
                flagged: true,
                result: Ok(()),
                ..
            }]
        ));
        assert!(flagged(&store));
    }

    #[test]
    fn a_message_moved_back_leaves_where_it_was_moved_and_its_snooze_goes_only_then() {
        let mut store = Store::open_in_memory().unwrap();
        store.save_account(&account()).unwrap();
        let archived = Job::Watched(Event::Messages {
            account: "me@example.com".into(),
            mailbox: "Snoozed".into(),
            role: Role::Snoozed,
            uid_validity: 7,
            messages: vec![Envelope {
                uid: 3,
                message_id: Some("a@example.com".into()),
                from: Address::default(),
                to: Vec::new(),
                subject: None,
                date: None,
                seen: true,
                flagged: false,
                in_reply_to: None,
                references: Vec::new(),
            }],
            asked: 0,
        });
        save(&mut store, archived);
        let job = |result| {
            let restoring = Restoring {
                account: "me@example.com".into(),
                mailbox: "Snoozed".into(),
                message_id: "a@example.com".into(),
                to: "INBOX".into(),
                snoozed: Some(100),
            };
            Job::Restored(restoring, result)
        };
        let cached = |store: &Store| store.recent_messages(Role::Snoozed, 10).unwrap().len();

        store.snooze(&snooze(100)).unwrap();
        let failed = save(&mut store, job(Err("offline".into())));
        assert!(
            matches!(failed.as_slice(), [Update::Restored { result: Err(_), .. }]),
            "got {failed:?}"
        );
        assert_eq!(
            store.due_snoozes(100).unwrap(),
            vec![snooze(100)],
            "still snoozed, as it's still waiting"
        );
        assert_eq!(cached(&store), 1);

        let gone = save(&mut store, job(Ok(Some((7, Vec::new())))));
        assert!(
            matches!(
                gone.as_slice(),
                [Update::Restored {
                    result: Ok(false),
                    ..
                }]
            ),
            "not there any more: {gone:?}"
        );
        assert_eq!(
            store.due_snoozes(100).unwrap(),
            vec![],
            "nothing to bring back"
        );
        store.snooze(&snooze(100)).unwrap();
        let restored = save(&mut store, job(Ok(Some((7, vec![3])))));
        assert!(
            matches!(
                restored.as_slice(),
                [Update::Restored { uids, to, result: Ok(true), .. }] if uids == &[3] && to == "INBOX"
            ),
            "got {restored:?}"
        );
        assert_eq!(cached(&store), 0, "it left where it was moved");
        assert_eq!(store.due_snoozes(100).unwrap(), vec![], "and isn't snoozed");
    }

    #[test]
    fn a_uid_is_asked_under_the_uid_validity_it_was_known_under() {
        assert_eq!(under(None, 7, "INBOX").unwrap(), 7);
        assert_eq!(under(Some(7), 7, "INBOX").unwrap(), 7);
        assert!(
            under(Some(6), 7, "INBOX").is_err(),
            "started over: UID may name another message"
        );
    }

    #[test]
    fn a_read_mark_answered_after_its_mailbox_started_over_isnt_saved() {
        let mut store = Store::open_in_memory().unwrap();
        store.save_account(&account()).unwrap();
        save(&mut store, messages(2, &[1]));
        let marked = save(
            &mut store,
            Job::Seen {
                account: "me@example.com".into(),
                mailbox: "INBOX".into(),
                uid_validity: 1,
                uid: 1,
                seen: true,
                result: Ok(()),
                at: 0,
            },
        );
        assert!(matches!(marked.as_slice(), [Update::Seen { .. }]));
        assert_eq!(
            store.seen("me@example.com", "INBOX", 1).unwrap(),
            Some(false),
            "UID 1 is another message under UIDVALIDITY 2"
        );
    }

    #[test]
    fn a_download_answered_after_its_mailbox_started_over_isnt_marked_read() {
        let mut store = Store::open_in_memory().unwrap();
        store.save_account(&account()).unwrap();
        save(&mut store, messages(2, &[1]));
        let opened = save(
            &mut store,
            Job::Opened {
                account: "me@example.com".into(),
                mailbox: "INBOX".into(),
                uid_validity: 1,
                uid: 1,
                body: Ok(Body::default()),
                marked: true,
                at: 0,
                cleared: 0,
            },
        );
        assert!(matches!(opened.as_slice(), [Update::Opened { .. }]));
        assert_eq!(
            store.seen("me@example.com", "INBOX", 1).unwrap(),
            Some(false),
            "UID 1 is another message under UIDVALIDITY 2"
        );
    }

    #[test]
    fn flags_read_before_a_star_was_answered_dont_take_it_away() {
        let mut store = Store::open_in_memory().unwrap();
        store.save_account(&account()).unwrap();
        save(&mut store, messages(1, &[1, 2]));
        let flags = |flagged, asked| {
            Job::Watched(Event::Flags {
                account: "me@example.com".into(),
                mailbox: "INBOX".into(),
                uid_validity: 1,
                flags: [1, 2]
                    .map(|uid| Flags {
                        uid,
                        seen: true,
                        flagged,
                    })
                    .to_vec(),
                asked,
            })
        };
        let flagged = |store: &Store, uid| {
            store
                .recent_messages(Role::Inbox, 10)
                .unwrap()
                .into_iter()
                .find(|m| m.envelope.uid == uid)
                .unwrap()
                .envelope
                .flagged
        };
        // A sync starts (count 4), a star is answered (5) and saved, then what the sync read is,
        // whatever order they reach the store in.
        save(
            &mut store,
            Job::Flagged {
                account: "me@example.com".into(),
                mailbox: "INBOX".into(),
                uid_validity: 1,
                uid: 1,
                flagged: true,
                result: Ok(()),
                at: 5,
            },
        );
        save(&mut store, flags(false, 4));
        assert!(
            flagged(&store, 1),
            "the star is newer than what the sync read"
        );
        assert_eq!(
            store.seen("me@example.com", "INBOX", 2).unwrap(),
            Some(true),
            "the rest of what it read is saved"
        );
        // A sync started after the star has the last word.
        save(&mut store, flags(false, 5));
        assert!(!flagged(&store, 1), "unstarred elsewhere since");
    }

    #[test]
    fn a_message_fetched_before_its_star_was_answered_is_saved_starred() {
        let mut store = Store::open_in_memory().unwrap();
        store.save_account(&account()).unwrap();
        save(&mut store, messages(1, &[1]));
        // UID 2 was found by searching the server, and starred, before the sync that fetched it
        // (started at count 4) was saved.
        save(
            &mut store,
            Job::Flagged {
                account: "me@example.com".into(),
                mailbox: "INBOX".into(),
                uid_validity: 1,
                uid: 2,
                flagged: true,
                result: Ok(()),
                at: 5,
            },
        );
        let mut fetched = messages(1, &[2]);
        if let Job::Watched(Event::Messages { asked, .. }) = &mut fetched {
            *asked = 4;
        }
        save(&mut store, fetched);
        let starred = store
            .recent_messages(Role::Inbox, 10)
            .unwrap()
            .into_iter()
            .any(|m| m.envelope.uid == 2 && m.envelope.flagged);
        assert!(starred);
    }

    #[test]
    fn a_star_from_before_its_mailbox_started_over_isnt_put_on_the_message_with_its_uid_now() {
        let mut store = Store::open_in_memory().unwrap();
        store.save_account(&account()).unwrap();
        save(&mut store, messages(1, &[1]));
        // A sync starts (count 4) and finds INBOX started over; meanwhile UID 1 is starred under
        // the old UIDVALIDITY (5), then the sync's messages are saved.
        save(
            &mut store,
            Job::Flagged {
                account: "me@example.com".into(),
                mailbox: "INBOX".into(),
                uid_validity: 1,
                uid: 1,
                flagged: true,
                result: Ok(()),
                at: 5,
            },
        );
        let mut started_over = messages(2, &[1]);
        if let Job::Watched(Event::Messages { asked, .. }) = &mut started_over {
            *asked = 4;
        }
        save(&mut store, started_over);
        let starred = store.recent_messages(Role::Inbox, 10).unwrap()[0]
            .envelope
            .flagged;
        assert!(!starred, "UID 1 names another message under UIDVALIDITY 2");
    }

    #[test]
    fn older_mail_fetched_before_its_star_was_answered_is_saved_starred() {
        let mut store = Store::open_in_memory().unwrap();
        store.save_account(&account()).unwrap();
        save(&mut store, messages(1, &[5]));
        // Older mail is asked for (count 4); UID 1, found by searching the server, is starred
        // (5); a sync started after the star (5) is saved; then the older mail, as read before
        // the star, is.
        save(
            &mut store,
            Job::Flagged {
                account: "me@example.com".into(),
                mailbox: "INBOX".into(),
                uid_validity: 1,
                uid: 1,
                flagged: true,
                result: Ok(()),
                at: 5,
            },
        );
        let older = Envelope {
            uid: 1,
            message_id: None,
            from: Address::default(),
            to: Vec::new(),
            subject: None,
            date: Some(1),
            seen: true,
            flagged: false,
            in_reply_to: None,
            references: Vec::new(),
        };
        save(
            &mut store,
            Job::Watched(Event::Flags {
                account: "me@example.com".into(),
                mailbox: "INBOX".into(),
                uid_validity: 1,
                flags: vec![Flags {
                    uid: 5,
                    seen: true,
                    flagged: false,
                }],
                asked: 5,
            }),
        );
        save(
            &mut store,
            Job::Older {
                account: "me@example.com".into(),
                mailbox: "INBOX".into(),
                uid_validity: 1,
                result: Ok(Older {
                    messages: vec![older],
                    done: true,
                }),
                asked: 4,
            },
        );
        let starred = store
            .recent_messages(Role::Inbox, 10)
            .unwrap()
            .into_iter()
            .any(|m| m.envelope.uid == 1 && m.envelope.flagged);
        assert!(starred, "the star is newer than what the fetch read");
    }
}
