//! Watching every account's inbox in the background, refreshing its other mailboxes on a timer,
//! and saving what arrives; downloading the inbox's newest messages ahead of their opening, when
//! asked to; and opening, exporting, flagging, moving, snoozing and sending messages on the same
//! background runtime. Snoozed messages come back to the inbox unread once
//! [`Mailroom::wake_due`] finds them due.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::Duration;

use postden_core::account::{Account, Server};
use postden_core::message::{Body, Envelope, Mailbox, Outgoing, Role};
use postden_core::secrets::SecretStore;
use postden_mail::imap::{
    Event, Flags, Found, Older, Prefetched, SNOOZED_NAME, Status, WatchConfig, download_message,
    fetch_older, mark_all_seen, move_message, open_message, prefetch_messages, refresh,
    restore_message, save_sent_copy, search, set_flagged, set_seen, snooze_message, wake_message,
    watch,
};
use postden_mail::{Credentials, smtp};
use postden_store::{Snooze, Store, Written};
use tokio::runtime::Runtime;
use tokio::sync::Notify;
use tokio::sync::mpsc::{UnboundedSender, WeakUnboundedSender, unbounded_channel};
use tokio::task::AbortHandle;

use crate::{Error, Result, credentials};

/// The inbox's name on every server, the only mailbox whose messages are downloaded ahead.
const INBOX: &str = "INBOX";
/// How many messages one connection downloads ahead. Each batch is saved, and opens without
/// waiting, before the next is asked for, under the setting and login as they are then.
const PREFETCH_BATCH: usize = 20;
/// The largest message downloaded ahead, in bytes. Larger ones, mostly their attachments, are
/// only downloaded once opened.
const PREFETCH_MAX_SIZE: u32 = 5 * 1024 * 1024;

/// Something the UI should show. By the time it's sent, the mail is already in the store.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Update {
    /// An account's connection changed.
    Status { account: String, status: Status },
    /// The server listed an account's mailboxes. They're in the store.
    Mailboxes { account: String },
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
    /// saved: the cache holds a mailbox's newest mail without gaps, and matches can be anywhere.
    /// If the mailbox started over (a new UIDVALIDITY) since the search began, it failed: its
    /// UIDs would name other messages now.
    Searched {
        /// The id the search was started with.
        id: u64,
        account: String,
        mailbox: String,
        result: Result<Found, String>,
    },
    /// Fetched mail couldn't be saved.
    SaveFailed { account: String, error: String },
    /// A message asked for with [`Mailroom::open`] was downloaded, or couldn't be. Once downloaded
    /// it's read, on the server and in the store.
    Opened {
        account: String,
        mailbox: String,
        /// The mailbox's UIDVALIDITY it was downloaded under: the body is of the message `uid`
        /// named then.
        uid_validity: u32,
        uid: u32,
        body: Result<Body, String>,
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
    /// A message handed to [`Mailroom::send`] was accepted by the account's SMTP server, or wasn't.
    /// On success, it's the `Message-ID` it was sent with.
    Sent {
        account: String,
        result: Result<String, String>,
    },
    /// A sent message couldn't be filed in the account's Sent mailbox. It was still sent.
    CopyFailed { account: String, error: String },
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
        /// When it was answered, as [`WatchConfig::changes`] counts: downloaded, it's read.
        at: u64,
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
    /// Download the account's newest inbox messages not downloaded yet, as many as asked, and
    /// drop the others downloaded before.
    Prefetch {
        account: String,
    },
    /// Messages asked for by [`Job::Prefetch`] were downloaded, or couldn't be.
    Prefetched {
        account: String,
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
        | Event::Flags { account, .. } => account,
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
            | Self::Searched { account, .. } => Some(account),
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
                ..
            } => Self::Opened {
                account,
                mailbox,
                uid_validity,
                uid,
                body: replaced(),
                at,
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
    nudge: Arc<Notify>,
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
    /// How many of each inbox's newest messages to download ahead.
    prefetch: Arc<AtomicUsize>,
    /// Each watched account's IMAP server and credentials, for downloading ahead, with the
    /// watch they're for: one lookup gets both, so a download is never numbered as a watch
    /// on other servers.
    logins: Logins,
    /// Each watched account's current watch: see [`Job::Watching`].
    watches: Watches,
    /// The number of the last watch started.
    last_watch: u64,
    /// The accounts whose due snoozes are being brought back, not to be asked for twice at once.
    waking: Arc<Mutex<HashSet<String>>>,
    /// Snoozed messages being moved back by [`Mailroom::restore`], by account and Message-ID:
    /// their snoozes are kept until they're moved, but not brought back meanwhile.
    unsnoozing: Arc<Mutex<HashSet<(String, String)>>>,
}

type Logins = Arc<Mutex<HashMap<String, (Server, Credentials, u64)>>>;
type Watches = Arc<Mutex<HashMap<String, u64>>>;

impl Mailroom {
    /// Starts the background runtime and the thread that saves mail into the database at `database`.
    /// The newest `prefetch` messages of each inbox are downloaded ahead of their opening.
    ///
    /// # Errors
    ///
    /// Returns an error when the database can't be opened or a thread can't be started.
    pub fn start(
        database: &Path,
        config: WatchConfig,
        prefetch: usize,
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
        let logins = Logins::default();
        let watches = Watches::default();
        let mut prefetcher = Prefetcher {
            handle: runtime.handle().clone(),
            jobs: jobs.downgrade(),
            logins: Arc::clone(&logins),
            watches: Arc::clone(&watches),
            count: Arc::clone(&prefetch),
            busy: HashSet::new(),
            skipped: HashSet::new(),
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
            logins,
            watches,
            last_watch: 0,
            waking: Arc::default(),
            unsnoozing: Arc::default(),
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

    /// Has the store thread download ahead what `account`'s inbox lacks.
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
        let nudge = Arc::new(Notify::new());
        let watcher = self.handle.spawn(watch(
            account.clone(),
            imap_credentials.clone(),
            inbox,
            self.config.clone(),
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
    /// the server. Any other is downloaded in the background, which marks it read, arriving as
    /// [`Update::Opened`], and `None` is returned.
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
        self.handle.spawn(async move {
            let body = open_message(&server, &credentials, &mailbox, uid_validity, uid)
                .await
                .map_err(|e| e.to_string());
            // Nobody is listening only as the app quits.
            let _ignored = jobs.send(Job::Opened {
                account,
                mailbox,
                uid_validity,
                uid,
                body,
                at: changes.answered(),
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
    /// it's listed there. The result arrives as [`Update::Moved`].
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
        let (account, mailbox, to) = (account.to_owned(), mailbox.to_owned(), to.to_owned());
        self.handle.spawn(async move {
            let result = move_message(&server, &credentials, &mailbox, uid_validity, uid, &to)
                .await
                .map_err(|e| e.to_string());
            // The inbox's watcher sees mail moved there on its own; the refresher syncs the rest.
            if result.is_ok() {
                nudge.notify_one();
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
            .await
            .map_err(|e| e.to_string());
            if result.is_ok() {
                nudge.notify_one();
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
        });
        Ok(())
    }

    /// Moves a message back from the account's mailbox `mailbox`, where it was moved or snoozed
    /// to, to `to`, where it was, in the background, as when that's undone. It's found by
    /// `message_id`, as its UID there isn't known, and stays read or unread as it is. A message
    /// snoozed until `snoozed` is no longer snoozed once it's moved back; until then, and if it
    /// can't be, its snooze is kept, but it isn't brought back. The result arrives as
    /// [`Update::Restored`].
    ///
    /// # Errors
    ///
    /// As [`Mailroom::open`], and when the message has no Message-ID to find it by.
    pub fn restore(
        &self,
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
        let (server, credentials) = (watched.server.clone(), watched.credentials.clone());
        let jobs = Asking {
            jobs: self.jobs.clone(),
            watch: watched.number,
        };
        let nudge = Arc::clone(&watched.nudge);
        // Not while snoozes are being brought back, which may have taken it already; and not
        // brought back while it's being moved.
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
            )
            .await
            .map_err(|e| e.to_string());
            if result.is_ok() {
                nudge.notify_one();
            }
            // Bringing it back once due finds nothing to bring if it was moved.
            if let Some(unsnoozing) = unsnoozing {
                unsnoozing
                    .lock()
                    .unwrap_or_else(PoisonError::into_inner)
                    .remove(&reserved);
            }
            let _ignored = jobs.send(Job::Restored(restoring, result));
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
                        nudge.notify_one();
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
    /// interval. Does nothing for an account that isn't watched.
    pub fn refresh(&self, account: &str) {
        if let Some(w) = self.watchers.get(account) {
            w.nudge.notify_one();
        }
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
        self.handle.spawn(async move {
            let sent = match smtp::send(&smtp, &smtp_credentials, &message).await {
                Ok(sent) => sent,
                Err(e) => {
                    let _ignored = jobs.send(Job::Done(Update::Sent {
                        account,
                        result: Err(e.to_string()),
                    }));
                    return;
                }
            };
            let _ignored = jobs.send(Job::Done(Update::Sent {
                account: account.clone(),
                result: Ok(sent.message_id.clone()),
            }));
            match save_sent_copy(&imap, &credentials, &sent.message_id, &sent.copy).await {
                // Fetch it from Sent now, so it's listed there right away.
                Ok(()) => nudge.notify_one(),
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

impl Drop for Mailroom {
    fn drop(&mut self) {
        // Don't wait for connections to wind down: the app may be quitting.
        if let Some(runtime) = self.runtime.take() {
            runtime.shutdown_background();
        }
    }
}

/// Downloads inbox messages ahead of their opening, for the store thread, which alone knows
/// which are cached.
struct Prefetcher {
    handle: tokio::runtime::Handle,
    /// Weak, as the store thread it lives on ends only once every sender is dropped.
    jobs: WeakUnboundedSender<Job>,
    logins: Logins,
    watches: Watches,
    count: Arc<AtomicUsize>,
    /// The accounts with a batch being downloaded ahead: one at a time each, so only one
    /// connection per account downloads ahead, and nothing is asked for twice.
    busy: HashSet<String>,
    /// Messages a download ahead left out, by account, watch, UIDVALIDITY and UID, not to be
    /// asked for again: those too large, and those no longer on the server, whose envelopes
    /// stay cached as the watcher only fetches newer mail. By watch, as a replaced account's
    /// UIDs name another server's messages: they're checked again once watched anew.
    skipped: HashSet<(String, u64, u32, u32)>,
}

impl Prefetcher {
    /// Saves what a job brought and says what the UI should show, then downloads ahead what
    /// the account's inbox lacks if the job may have changed that.
    #[expect(
        clippy::significant_drop_tightening,
        reason = "a watch's number is held until what it brought is written"
    )]
    fn save(&mut self, store: &mut Store, job: Job) -> Vec<Update> {
        // Held from checking a watch's number until what it brought is written: unwatching
        // waits for it, so once `Mailroom::unwatch` returns, nothing of that watch is written.
        let watches = Arc::clone(&self.watches);
        let current = watches.lock().unwrap_or_else(PoisonError::into_inner);
        let is_current = |account: &str, watch| current.get(account) == Some(&watch);
        let job = match job {
            Job::Asked { watch, job } if job.account().is_some_and(|a| !is_current(a, watch)) => {
                job.superseded()
            }
            Job::Asked { job, .. } => *job,
            Job::Watching { watch, event } if !is_current(event_account(&event), watch) => {
                return Vec::new();
            }
            Job::Watching { event, .. } => Job::Watched(event),
            job => job,
        };
        let (account, updates) = match job {
            Job::Prefetch { account } => (account, Vec::new()),
            Job::Prefetched {
                account,
                watch,
                uid_validity,
                uids,
                result,
            } => {
                self.busy.remove(&account);
                // Failing, it's tried again with the next new mail or at the next start. Asked
                // of mailboxes the account no longer has, it's for no one.
                let Ok(prefetched) = result else {
                    return Vec::new();
                };
                if current.get(&account) != Some(&watch) {
                    return Vec::new();
                }
                let updates =
                    self.save_prefetched(store, &account, watch, uid_validity, &uids, &prefetched);
                (account, updates)
            }
            Job::Watched(Event::Messages {
                ref account,
                ref mailbox,
                ..
            }) if mailbox == INBOX => (account.clone(), save(store, job)),
            Job::Opened {
                account,
                mailbox,
                uid_validity,
                uid,
                body: Ok(body),
                at,
            } if mailbox == INBOX && self.count.load(Ordering::Relaxed) > 0 => {
                let mut updates = Vec::new();
                // Kept like one downloaded ahead, so it opens again without waiting; unless the
                // mailbox started over meanwhile, as its UID names another message now.
                let kept = store
                    .mailbox_state(&account, &mailbox)
                    .map_err(Error::from)
                    .and_then(|state| {
                        if state.is_some_and(|s| s.uid_validity == uid_validity) {
                            store.save_body(&account, &mailbox, uid, &body)?;
                        }
                        Ok(())
                    });
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
                    at,
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

    fn save_prefetched(
        &mut self,
        store: &Store,
        account: &str,
        watch: u64,
        uid_validity: u32,
        asked: &[u32],
        prefetched: &Prefetched,
    ) -> Vec<Update> {
        let result = (|| -> Result<()> {
            // Started over meanwhile, the UIDs name other messages.
            let current = is_current(store, account, INBOX, uid_validity)?;
            if !current {
                return Ok(());
            }
            for (uid, body) in &prefetched.bodies {
                store.save_body(account, INBOX, *uid, body)?;
            }
            let downloaded: HashSet<u32> = prefetched.bodies.iter().map(|(uid, _)| *uid).collect();
            self.skipped.extend(
                asked
                    .iter()
                    .filter(|uid| !downloaded.contains(uid))
                    .map(|&uid| (account.to_owned(), watch, uid_validity, uid)),
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

    /// Drops what's downloaded ahead of `account`'s inbox but its newest messages, and starts
    /// downloading those it lacks.
    fn prefetch(&mut self, store: &Store, account: &str) -> Result<()> {
        let count = self.count.load(Ordering::Relaxed);
        store.trim_bodies(account, INBOX, count)?;
        // Downloading a batch: the next is asked for once it's back, as things are then.
        if count == 0 || self.busy.contains(account) {
            return Ok(());
        }
        let Some(state) = store.mailbox_state(account, INBOX)? else {
            return Ok(());
        };
        let Some((server, credentials, watch)) = self
            .logins
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .get(account)
            .cloned()
        else {
            return Ok(());
        };
        let uid_validity = state.uid_validity;
        let uids: Vec<u32> = store
            .missing_bodies(account, INBOX, count)?
            .into_iter()
            .filter(|&uid| {
                !self
                    .skipped
                    .contains(&(account.to_owned(), watch, uid_validity, uid))
            })
            .take(PREFETCH_BATCH)
            .collect();
        if uids.is_empty() {
            return Ok(());
        }
        // The mailroom is going away: nobody would save what comes back.
        let Some(jobs) = self.jobs.upgrade() else {
            return Ok(());
        };
        self.busy.insert(account.to_owned());
        let account = account.to_owned();
        self.handle.spawn(async move {
            let result = prefetch_messages(
                &server,
                &credentials,
                INBOX,
                uid_validity,
                &uids,
                PREFETCH_MAX_SIZE,
            )
            .await
            .map_err(|e| e.to_string());
            let _ignored = jobs.send(Job::Prefetched {
                account,
                watch,
                uid_validity,
                uids,
                result,
            });
        });
        Ok(())
    }
}

/// Saves what a job brought, and says what the UI should show.
fn save(store: &mut Store, job: Job) -> Vec<Update> {
    match job {
        Job::Watched(Event::Status { account, status }) => vec![Update::Status { account, status }],
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
            at,
        } => opened(store, (account, mailbox, uid_validity), uid, body, at),
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
        store
            .remove_message(&account, &mailbox, uid)
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

/// A message was downloaded, and so marked read on the server, or couldn't be: read in the
/// store too, unless its mailbox, given with the UIDVALIDITY it was downloaded under, started
/// over since.
fn opened(
    store: &Store,
    (account, mailbox, uid_validity): (String, String, u32),
    uid: u32,
    body: Result<Body, String>,
    at: u64,
) -> Vec<Update> {
    let mut updates = Vec::new();
    // Unless the mailbox started over meanwhile: its UID names another message now.
    let marked = || {
        if is_current(store, &account, &mailbox, uid_validity)? {
            store.set_seen(&account, &mailbox, uid, true)?;
            store.written(&account, &mailbox, (uid_validity, uid), read(at));
        }
        Ok::<_, postden_store::Error>(())
    };
    if body.is_ok()
        && let Err(e) = marked()
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
        Job::Watched(Event::Messages {
            account: "me@example.com".into(),
            mailbox: "INBOX".into(),
            role: Role::Inbox,
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
                })
                .collect(),
            asked: 0,
        })
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
            busy: HashSet::new(),
            skipped: HashSet::new(),
        };
        ((runtime, jobs), prefetcher)
    }

    fn prefetched(uid_validity: u32, uids: &[u32], too_big: &[u32]) -> Job {
        Job::Prefetched {
            account: "me@example.com".into(),
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
            prefetcher
                .skipped
                .contains(&("me@example.com".to_owned(), 1, 1, 3)),
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
            uid_validity: 1,
            uids: vec![1, 2],
            result: Ok(Prefetched {
                bodies: vec![(1, Body::default())],
                too_big: Vec::new(),
            }),
        };
        prefetcher.save(&mut store, job);
        assert!(
            prefetcher
                .skipped
                .contains(&("me@example.com".to_owned(), 1, 1, 2)),
            "skipped, not retried in a loop"
        );
        let failed = Job::Prefetched {
            watch: 1,
            account: "me@example.com".into(),
            uid_validity: 1,
            uids: vec![3],
            result: Err("offline".into()),
        };
        prefetcher.save(&mut store, failed);
        assert!(
            !prefetcher
                .skipped
                .contains(&("me@example.com".to_owned(), 1, 1, 3)),
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
            at: 0,
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
    fn dropping_the_mailroom_ends_the_store_thread() {
        let dir = std::env::temp_dir().join(format!("postden-mailroom-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let (ended, done) = std::sync::mpsc::channel::<()>();
        // The callback goes with the store thread: once it's dropped, the thread ended.
        let mailroom = Mailroom::start(&dir.join("db"), WatchConfig::default(), 50, move |_| {
            let _ = &ended;
        })
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
            at: 0,
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
                at: 0,
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
