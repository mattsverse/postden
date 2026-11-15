//! Watching every account's inbox in the background, refreshing its other mailboxes on a timer,
//! and saving what arrives; downloading the inbox's newest messages ahead of their opening, when
//! asked to; and opening, exporting, flagging, moving and sending messages on the same background
//! runtime.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::Duration;

use postden_core::account::{Account, Server};
use postden_core::message::{Body, Envelope, Outgoing, Role};
use postden_core::secrets::SecretStore;
use postden_mail::imap::{
    Event, Found, Older, Prefetched, Status, WatchConfig, download_message, fetch_older,
    mark_all_seen, move_message, open_message, prefetch_messages, refresh, save_sent_copy, search,
    set_seen, watch,
};
use postden_mail::{Credentials, smtp};
use postden_store::Store;
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
    },
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
        error: String,
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
        Ok(Some(up_to))
    });
    Update::AllSeen {
        account,
        mailbox,
        result,
    }
}

/// Work for the store thread: what watchers report, and results of work done for the UI.
#[derive(Debug)]
enum Job {
    Watched(Event),
    Opened {
        account: String,
        mailbox: String,
        /// The mailbox's UIDVALIDITY the download was made under.
        uid_validity: u32,
        uid: u32,
        body: Result<Body, String>,
    },
    Seen {
        account: String,
        mailbox: String,
        uid: u32,
        seen: bool,
        result: Result<(), String>,
    },
    AllSeen {
        account: String,
        mailbox: String,
        uid_validity: u32,
        /// The mailbox's UIDNEXT when it was marked: every message below it is read.
        result: Result<u32, String>,
    },
    Moved(Moving, Result<(), String>),
    /// Work with nothing to save, such as sending or exporting a message: its update goes to the
    /// UI as it is, in turn with the others.
    Done(Update),
    Older {
        account: String,
        mailbox: String,
        uid_validity: u32,
        result: Result<Older, String>,
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
    /// The inbox's watcher and the other mailboxes' refresher.
    tasks: [AbortHandle; 2],
    /// Wakes the refresher.
    nudge: Arc<Notify>,
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
    /// Each watched account's IMAP server and credentials, for downloading ahead.
    logins: Logins,
}

type Logins = Arc<Mutex<HashMap<String, (Server, Credentials)>>>;

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
        let mut prefetcher = Prefetcher {
            handle: runtime.handle().clone(),
            jobs: jobs.downgrade(),
            logins: Arc::clone(&logins),
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
        let nudge = Arc::new(Notify::new());
        let watcher = self.handle.spawn(watch(
            account.clone(),
            imap_credentials.clone(),
            inbox,
            self.config.clone(),
            self.jobs.clone(),
        ));
        let refresher = self.handle.spawn(refresh(
            account.clone(),
            imap_credentials.clone(),
            cached,
            self.config.clone(),
            Arc::clone(&nudge),
            self.jobs.clone(),
        ));
        self.watchers.insert(
            account.id.clone(),
            Watched {
                server: account.imap.clone(),
                credentials: imap_credentials.clone(),
                smtp: account.smtp.clone(),
                smtp_credentials,
                tasks: [watcher.abort_handle(), refresher.abort_handle()],
                nudge,
            },
        );
        self.logins
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .insert(account.id.clone(), (account.imap.clone(), imap_credentials));
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
        self.watchers.remove(account).is_some_and(|w| {
            w.tasks.iter().for_each(AbortHandle::abort);
            true
        })
    }

    /// Opens a message and marks it read. One downloaded ahead is returned at once, and marked
    /// read in the background, arriving as [`Update::Seen`] or [`Update::SeenFailed`]. Any
    /// other is downloaded in the background, arriving as [`Update::Opened`], and `None` is
    /// returned.
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
    ) -> Result<Option<Body>> {
        if let Some(body) = store.body(account, mailbox, uid)? {
            if store.seen(account, mailbox, uid)? == Some(false) {
                // Shown all the same while disconnected; it stays unread until opened online.
                let _ignored = self.set_seen(store, account, mailbox, uid, true);
            }
            return Ok(Some(body));
        }
        let (server, credentials, uid_validity) = self.target(store, account, mailbox)?;
        let (account, mailbox) = (account.to_owned(), mailbox.to_owned());
        let jobs = self.jobs.clone();
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
            });
        });
        Ok(None)
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
        let (server, credentials, uid_validity) = self.target(store, account, mailbox)?;
        let (account, mailbox) = (account.to_owned(), mailbox.to_owned());
        let jobs = self.jobs.clone();
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
    /// [`Update::SeenFailed`].
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
    ) -> Result<()> {
        let (server, credentials, uid_validity) = self.target(store, account, mailbox)?;
        let (account, mailbox) = (account.to_owned(), mailbox.to_owned());
        let jobs = self.jobs.clone();
        self.handle.spawn(async move {
            let result = set_seen(&server, &credentials, &mailbox, uid_validity, uid, seen)
                .await
                .map_err(|e| e.to_string());
            let _ignored = jobs.send(Job::Seen {
                account,
                mailbox,
                uid,
                seen,
                result,
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
        let (server, credentials, uid_validity) = self.target(store, account, mailbox)?;
        let (account, mailbox) = (account.to_owned(), mailbox.to_owned());
        let jobs = self.jobs.clone();
        self.handle.spawn(async move {
            let result = mark_all_seen(&server, &credentials, &mailbox, uid_validity)
                .await
                .map_err(|e| e.to_string());
            let _ignored = jobs.send(Job::AllSeen {
                account,
                mailbox,
                uid_validity,
                result,
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
    ) -> Result<()> {
        let (server, credentials, uid_validity) = self.target(store, account, mailbox)?;
        let nudge = Arc::clone(&self.watched(account)?.nudge);
        let (account, mailbox, to) = (account.to_owned(), mailbox.to_owned(), to.to_owned());
        let jobs = self.jobs.clone();
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
            };
            let _ignored = jobs.send(Job::Moved(moving, result));
        });
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
        let (server, credentials, uid_validity) = self.target(store, account, mailbox)?;
        // With nothing cached, the next sync fetches the newest; there's no "older" yet.
        let Some(before) = store.oldest_uid(account, mailbox)? else {
            return Err(Error::Invalid(format!("{mailbox} has nothing cached yet")));
        };
        let (account, mailbox) = (account.to_owned(), mailbox.to_owned());
        let jobs = self.jobs.clone();
        self.handle.spawn(async move {
            let result = fetch_older(&server, &credentials, &mailbox, uid_validity, before, count)
                .await
                .map_err(|e| e.to_string());
            let _ignored = jobs.send(Job::Older {
                account,
                mailbox,
                uid_validity,
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
        let (server, credentials, uid_validity) = self.target(store, account, mailbox)?;
        let (account, mailbox, query) = (account.to_owned(), mailbox.to_owned(), query.to_owned());
        let jobs = self.jobs.clone();
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

    /// Where a message lives: its account's server and credentials, and the mailbox's UIDVALIDITY.
    fn target(
        &self,
        store: &Store,
        account: &str,
        mailbox: &str,
    ) -> Result<(Server, Credentials, u32)> {
        let watched = self.watched(account)?;
        let state = store
            .mailbox_state(account, mailbox)?
            .ok_or_else(|| Error::Invalid(format!("{mailbox} hasn't synced yet")))?;
        Ok((
            watched.server.clone(),
            watched.credentials.clone(),
            state.uid_validity,
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
    count: Arc<AtomicUsize>,
    /// The accounts with a batch being downloaded ahead: one at a time each, so only one
    /// connection per account downloads ahead, and nothing is asked for twice.
    busy: HashSet<String>,
    /// Messages a download ahead left out, by account, UIDVALIDITY and UID, not to be asked for
    /// again: those too large, and those no longer on the server, whose envelopes stay cached
    /// as the watcher only fetches newer mail.
    skipped: HashSet<(String, u32, u32)>,
}

impl Prefetcher {
    /// Saves what a job brought and says what the UI should show, then downloads ahead what
    /// the account's inbox lacks if the job may have changed that.
    fn save(&mut self, store: &mut Store, job: Job) -> Vec<Update> {
        let (account, updates) = match job {
            Job::Prefetch { account } => (account, Vec::new()),
            Job::Prefetched {
                account,
                uid_validity,
                uids,
                result,
            } => {
                self.busy.remove(&account);
                // Failing, it's tried again with the next new mail or at the next start.
                let Ok(prefetched) = result else {
                    return Vec::new();
                };
                let updates =
                    self.save_prefetched(store, &account, uid_validity, &uids, &prefetched);
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
        uid_validity: u32,
        asked: &[u32],
        prefetched: &Prefetched,
    ) -> Vec<Update> {
        let result = (|| -> Result<()> {
            // Started over meanwhile, the UIDs name other messages.
            let current = store
                .mailbox_state(account, INBOX)?
                .is_some_and(|s| s.uid_validity == uid_validity);
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
                    .map(|&uid| (account.to_owned(), uid_validity, uid)),
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
        let Some((server, credentials)) = self
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
                    .contains(&(account.to_owned(), uid_validity, uid))
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
            // A refresher stopped as its account was removed can still have a listing in flight.
            match store.account(&account).and_then(|a| {
                a.map(|_| store.save_mailbox_list(&account, &mailboxes))
                    .transpose()
            }) {
                Ok(Some(())) => vec![Update::Mailboxes { account }],
                Ok(None) => Vec::new(),
                Err(e) => vec![Update::SaveFailed {
                    account,
                    error: e.to_string(),
                }],
            }
        }
        Job::Watched(Event::Messages {
            account,
            mailbox,
            role,
            uid_validity,
            messages,
        }) => save_messages(store, account, mailbox, role, uid_validity, &messages)
            .into_iter()
            .collect(),
        Job::Opened {
            account,
            mailbox,
            uid,
            body,
            ..
        } => {
            let mut updates = Vec::new();
            if body.is_ok()
                && let Err(e) = store.set_seen(&account, &mailbox, uid, true)
            {
                updates.push(Update::SaveFailed {
                    account: account.clone(),
                    error: e.to_string(),
                });
            }
            updates.push(Update::Opened {
                account,
                mailbox,
                uid,
                body,
            });
            updates
        }
        Job::Seen {
            account,
            mailbox,
            uid,
            seen,
            result,
        } => {
            let result = result.and_then(|()| {
                store
                    .set_seen(&account, &mailbox, uid, seen)
                    .map_err(|e| e.to_string())
            });
            vec![match result {
                Ok(_) => Update::Seen {
                    account,
                    mailbox,
                    uid,
                    seen,
                },
                Err(error) => Update::SeenFailed {
                    account,
                    mailbox,
                    uid,
                    error,
                },
            }]
        }
        Job::AllSeen {
            account,
            mailbox,
            uid_validity,
            result,
        } => vec![all_seen(store, account, mailbox, uid_validity, result)],
        Job::Moved(moving, result) => vec![moved(store, moving, result)],
        Job::Done(update) => vec![update],
        Job::Older {
            account,
            mailbox,
            uid_validity,
            result,
        } => vec![save_older(store, account, mailbox, uid_validity, result)],
        Job::Searched {
            id,
            account,
            mailbox,
            uid_validity,
            result,
        } => vec![searched(store, id, account, mailbox, uid_validity, result)],
        // The prefetcher handles these before they get here.
        Job::Prefetch { .. } | Job::Prefetched { .. } => Vec::new(),
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
    Update::Moved {
        account,
        mailbox,
        uid,
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
    account: String,
    mailbox: String,
    uid_validity: u32,
    result: Result<Older, String>,
) -> Update {
    let saved = result.and_then(|older| {
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
    account: String,
    mailbox: String,
    role: Role,
    uid_validity: u32,
    messages: &[Envelope],
) -> Option<Update> {
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
                })
                .collect(),
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
            count: Arc::new(AtomicUsize::new(count)),
            busy: HashSet::new(),
            skipped: HashSet::new(),
        };
        ((runtime, jobs), prefetcher)
    }

    fn prefetched(uid_validity: u32, uids: &[u32], too_big: &[u32]) -> Job {
        Job::Prefetched {
            account: "me@example.com".into(),
            uid_validity,
            uids: uids.iter().chain(too_big).copied().collect(),
            result: Ok(Prefetched {
                bodies: uids.iter().map(|&uid| (uid, Body::default())).collect(),
                too_big: too_big.to_vec(),
            }),
        }
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
                .contains(&("me@example.com".to_owned(), 1, 3)),
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
                .contains(&("me@example.com".to_owned(), 1, 2)),
            "skipped, not retried in a loop"
        );
        let failed = Job::Prefetched {
            account: "me@example.com".into(),
            uid_validity: 1,
            uids: vec![3],
            result: Err("offline".into()),
        };
        prefetcher.save(&mut store, failed);
        assert!(
            !prefetcher
                .skipped
                .contains(&("me@example.com".to_owned(), 1, 3)),
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
            uid: 1,
            seen: false,
            result,
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
}
