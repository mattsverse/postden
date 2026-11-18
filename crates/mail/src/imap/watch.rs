//! Keeping an account's inbox in sync and reporting new mail as it arrives.

use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use async_imap::types::{Fetch, Flag};
use futures::TryStreamExt as _;
use postden_core::account::Account;
use postden_core::message::{Envelope, Mailbox, MailboxState, Role};
use tokio::sync::Notify;
use tokio::sync::mpsc::UnboundedSender;
use tokio::time::{sleep, timeout};

use super::connection::{Session, connect};
use super::envelope;
use crate::{Credentials, Error, Result};

/// The inbox's name on every server.
const INBOX: &str = "INBOX";
/// How long one sync (SELECT and FETCH) may take before the connection is presumed dead.
pub(super) const COMMAND_TIMEOUT: Duration = Duration::from_secs(120);
/// How long to IDLE before re-issuing it. RFC 2177 asks for under 29 minutes; shorter also gets
/// past NAT boxes that drop quiet connections, and notices a dead connection sooner.
const IDLE_TIMEOUT: Duration = Duration::from_mins(9);
/// How many of a mailbox's newest messages have their flags read again on each sync, so being
/// read or starred elsewhere shows here: what the list shows first.
const FLAGS_WINDOW: u32 = 500;
const MIN_BACKOFF: Duration = Duration::from_secs(5);
const MAX_BACKOFF: Duration = Duration::from_mins(5);

/// How [`watch`] syncs.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WatchConfig {
    /// How often to check for new mail when the server doesn't support IDLE.
    pub poll_interval: Duration,
    /// How many of the newest messages to fetch when nothing is cached yet.
    pub initial_fetch: u32,
    /// How often mailboxes other than the inbox are listed and synced.
    pub refresh_interval: Duration,
    /// Counts read marks and stars the server answered, shared with what asks for them: what a
    /// sync reads is older than those counted after it started.
    pub changes: Changes,
}

impl Default for WatchConfig {
    fn default() -> Self {
        Self {
            poll_interval: Duration::from_secs(60),
            initial_fetch: 200,
            refresh_interval: Duration::from_mins(5),
            changes: Changes::default(),
        }
    }
}

/// Counts read marks and stars as the server answers them, so what a sync read can be told
/// apart from what was changed after it started.
#[derive(Debug, Clone, Default)]
pub struct Changes(Arc<AtomicU64>);

impl Changes {
    /// The count now.
    #[must_use]
    pub fn now(&self) -> u64 {
        self.0.load(Ordering::SeqCst)
    }

    /// Counts one the server just answered, and returns its count.
    #[must_use]
    pub fn answered(&self) -> u64 {
        self.0.fetch_add(1, Ordering::SeqCst) + 1
    }
}

/// Every copy counts the same changes.
impl PartialEq for Changes {
    fn eq(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.0, &other.0)
    }
}

impl Eq for Changes {}

/// Asks a watcher or refresher to sync now, and numbers each asking, so what it reports as
/// [`Event::Synced`] says which askings it answered.
///
/// The numbers come from a count shared by every nudge made [`Nudge::new`] from it: a watcher
/// started afresh answers what its predecessor was asked.
#[derive(Debug, Default)]
pub struct Nudge {
    notify: Notify,
    asked: Arc<AtomicU64>,
}

impl Nudge {
    /// A nudge numbering its askings from `count`.
    #[must_use]
    pub fn new(count: Arc<AtomicU64>) -> Self {
        Self {
            notify: Notify::new(),
            asked: count,
        }
    }

    /// Asks for a sync now. Returns its number: an [`Event::Synced`] numbered as much or more
    /// answers it.
    pub fn ask(&self) -> u64 {
        let asked = self.asked.fetch_add(1, Ordering::SeqCst) + 1;
        self.notify.notify_one();
        asked
    }

    /// The number of the last asking, read as a sync starts: it answers that one and all before.
    pub(super) fn asked(&self) -> u64 {
        self.asked.load(Ordering::SeqCst)
    }

    /// Waits for the next asking, or returns at once for one made meanwhile.
    pub(super) async fn notified(&self) {
        self.notify.notified().await;
    }
}

/// What a watcher is doing.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Status {
    Connecting,
    /// Connected, and the server pushes new mail as it arrives (IMAP IDLE).
    Push,
    /// Connected, and checking for new mail on this interval because the server lacks IDLE.
    Polling(Duration),
    /// The connection failed and will be retried.
    Failed {
        error: String,
        retry_in: Duration,
    },
}

/// Something a watcher reports.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Event {
    /// The watcher of `account`'s inbox changed what it's doing.
    Status { account: String, status: Status },
    /// Messages fetched from `mailbox`, oldest first. Save them under `uid_validity`; if it differs from
    /// what was cached, the cache for that mailbox is stale and `messages` is the fresh start. That's
    /// also why this can be empty.
    Messages {
        account: String,
        mailbox: String,
        role: Role,
        uid_validity: u32,
        messages: Vec<Envelope>,
        /// [`WatchConfig::changes`] when the sync started: read marks and stars counted after
        /// are newer than what the messages say.
        asked: u64,
    },
    /// The account's mailboxes, as the server just listed them.
    Mailboxes {
        account: String,
        mailboxes: Vec<Mailbox>,
    },
    /// A sync of `mailbox` of `account` ended, whether it found anything or failed: it answered
    /// every [`Nudge::ask`] numbered up to `asked`. With no mailbox, it's every mailbox but the
    /// inbox: the refresher's round ended.
    Synced {
        account: String,
        mailbox: Option<String>,
        asked: u64,
    },
    /// Whether messages already cached from `mailbox` are read and starred, as the server says
    /// now. Only for the mailbox under `uid_validity`.
    Flags {
        account: String,
        mailbox: String,
        uid_validity: u32,
        flags: Vec<Flags>,
        /// As [`Event::Messages`]'s.
        asked: u64,
    },
}

/// Whether a message is read and starred.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Flags {
    pub uid: u32,
    pub seen: bool,
    pub flagged: bool,
}

/// What a [`sync`] found.
pub(super) struct Synced {
    pub(super) uid_validity: u32,
    /// The new messages, or `None` when nothing changed.
    pub(super) messages: Option<Vec<Envelope>>,
    /// The flags of the newest messages that were already there.
    pub(super) flags: Vec<Flags>,
    /// [`WatchConfig::changes`] when it started.
    pub(super) asked: u64,
}

impl Synced {
    /// What to report of it for `mailbox` of `account`, with `role`.
    pub(super) fn events(self, account: &str, mailbox: &str, role: Role) -> Vec<Event> {
        let mut events = Vec::new();
        if let Some(messages) = self.messages {
            events.push(Event::Messages {
                account: account.to_owned(),
                mailbox: mailbox.to_owned(),
                role,
                uid_validity: self.uid_validity,
                messages,
                asked: self.asked,
            });
        }
        if !self.flags.is_empty() {
            events.push(Event::Flags {
                account: account.to_owned(),
                mailbox: mailbox.to_owned(),
                uid_validity: self.uid_validity,
                flags: self.flags,
                asked: self.asked,
            });
        }
        events
    }
}

/// Watches `account`'s inbox until `events` is closed, sending what arrives.
///
/// Starts from `state`, what's already cached, so only newer mail is fetched. Asking `nudge` syncs
/// it now, rather than once the server reports a change or the next poll. Connection failures are
/// reported as [`Status::Failed`] and retried with backoff, or sooner when nudged, so this only
/// returns once nobody is listening any more.
///
/// `events` can carry any type an [`Event`] converts into, so a caller can share one channel between
/// watchers and other work.
pub async fn watch<T: From<Event>>(
    account: Account,
    credentials: Credentials,
    mut state: Option<MailboxState>,
    config: WatchConfig,
    nudge: Arc<Nudge>,
    events: UnboundedSender<T>,
) {
    let mut watcher = Watcher {
        account: &account,
        config: &config,
        nudge: &nudge,
        events: &events,
        backoff: MIN_BACKOFF,
        asked: 0,
    };
    loop {
        if !watcher.status(Status::Connecting) {
            return;
        }
        let error = match watcher.run(&credentials, &mut state).await {
            Ok(()) => return,
            Err(e) => e,
        };
        let retry_in = watcher.backoff;
        // Everything asked so far is answered, by the failure: even what woke an IDLE that then
        // failed, before a sync numbered it.
        watcher.asked = nudge.asked();
        if !watcher.synced()
            || !watcher.status(Status::Failed {
                error: error.to_string(),
                retry_in,
            })
        {
            return;
        }
        // A nudge, such as the user asking for a refresh, retries now.
        tokio::select! {
            () = sleep(retry_in) => {}
            () = nudge.notified() => {}
        }
        watcher.backoff = (retry_in.saturating_mul(2)).min(MAX_BACKOFF);
    }
}

struct Watcher<'a, T> {
    account: &'a Account,
    config: &'a WatchConfig,
    nudge: &'a Nudge,
    events: &'a UnboundedSender<T>,
    backoff: Duration,
    /// What the sync under way, or the failure, answers: see [`Nudge::asked`].
    asked: u64,
}

impl<T: From<Event>> Watcher<'_, T> {
    /// Reports a status change. Returns `false` when nobody is listening.
    fn status(&self, status: Status) -> bool {
        self.send(Event::Status {
            account: self.account.id.clone(),
            status,
        })
    }

    fn send(&self, event: Event) -> bool {
        self.events.send(event.into()).is_ok()
    }

    /// Reports that the inbox's sync ended. Returns `false` when nobody is listening.
    fn synced(&self) -> bool {
        self.send(Event::Synced {
            account: self.account.id.clone(),
            mailbox: Some(INBOX.to_owned()),
            asked: self.asked,
        })
    }

    /// One connection's lifetime. Returns `Ok` only once nobody is listening.
    async fn run(
        &mut self,
        credentials: &Credentials,
        state: &mut Option<MailboxState>,
    ) -> Result<()> {
        let mut session = connect(&self.account.imap, credentials).await?;
        self.backoff = MIN_BACKOFF;
        let push = with_timeout(session.capabilities()).await?.has_str("IDLE");
        let status = if push {
            Status::Push
        } else {
            Status::Polling(self.config.poll_interval)
        };
        if !self.status(status) {
            return Ok(());
        }
        loop {
            self.asked = self.nudge.asked();
            let synced = with_timeout(sync(&mut session, INBOX, self.config, state)).await?;
            for event in synced.events(&self.account.id, INBOX, Role::Inbox) {
                if !self.send(event) {
                    return Ok(());
                }
            }
            if !self.synced() || self.events.is_closed() {
                return Ok(());
            }
            session = if push {
                idle(session, self.nudge).await?
            } else {
                tokio::select! {
                    () = sleep(self.config.poll_interval) => {}
                    () = self.nudge.notified() => {}
                }
                session
            };
        }
    }
}

pub(super) async fn with_timeout<T>(
    f: impl Future<Output = Result<T, impl Into<Error>>>,
) -> Result<T> {
    timeout(COMMAND_TIMEOUT, f)
        .await
        .map_err(|_elapsed| Error::Timeout)?
        .map_err(Into::into)
}

/// Waits until the server reports a change, `nudge` is notified, or [`IDLE_TIMEOUT`] passes.
async fn idle(session: Session, nudge: &Nudge) -> Result<Session> {
    let mut handle = session.idle();
    with_timeout(handle.init()).await?;
    {
        let (wait, stop) = handle.wait_with_timeout(IDLE_TIMEOUT);
        tokio::pin!(wait);
        // Whatever woke us (new mail, a flag change, a nudge or the timeout), the next sync finds
        // out what changed.
        tokio::select! {
            waited = &mut wait => {
                waited?;
            }
            () = nudge.notified() => {
                // Dropping the stop source interrupts the wait, which then ends cleanly.
                drop(stop);
                wait.await?;
            }
        }
    }
    with_timeout(handle.done()).await
}

/// Fetches what's new in the mailbox since `state`, and advances `state` past it, and reads again
/// the flags of the newest [`FLAGS_WINDOW`] messages that were there before.
pub(super) async fn sync(
    session: &mut Session,
    name: &str,
    config: &WatchConfig,
    state: &mut Option<MailboxState>,
) -> Result<Synced> {
    // Before anything is read: what's changed after is newer than what's read.
    let asked = config.changes.now();
    let mailbox = session.select(name).await?;
    let uid_validity = mailbox
        .uid_validity
        .ok_or_else(|| Error::NoUidValidity(name.to_owned()))?;
    let plan = plan(*state, uid_validity, mailbox.exists, config.initial_fetch);
    let fetches: Vec<Fetch> = match plan {
        None => Vec::new(),
        Some(Plan::After(uid)) => {
            session
                .uid_fetch(format!("{}:*", uid.saturating_add(1)), envelope::QUERY)
                .await?
                .try_collect()
                .await?
        }
        Some(Plan::Newest { from_seq }) => {
            session
                .fetch(format!("{from_seq}:*"), envelope::QUERY)
                .await?
                .try_collect()
                .await?
        }
    };
    let after = match plan {
        Some(Plan::After(uid)) => uid,
        _ => 0,
    };
    let mut messages: Vec<Envelope> = fetches
        .iter()
        .filter_map(envelope::from_fetch)
        // `n:*` always matches the last message, even when its UID is below n.
        .filter(|m| m.uid > after)
        .collect();
    messages.sort_by_key(|m| m.uid);

    let reset = state.is_none_or(|s| s.uid_validity != uid_validity);
    // Those just fetched came with their flags, and after a reset nothing older is cached.
    let flags = match plan {
        Some(Plan::After(_)) if !reset => {
            let from = mailbox
                .exists
                .saturating_sub(FLAGS_WINDOW.saturating_sub(1))
                .max(1);
            let flagged: Vec<Fetch> = session
                .fetch(format!("{from}:*"), "(UID FLAGS)")
                .await?
                .try_collect()
                .await?;
            flagged
                .iter()
                .filter_map(|f| {
                    let uid = f.uid.filter(|&uid| uid <= after)?;
                    Some(Flags {
                        uid,
                        seen: f.flags().any(|f| f == Flag::Seen),
                        flagged: f.flags().any(|f| f == Flag::Flagged),
                    })
                })
                .collect()
        }
        _ => Vec::new(),
    };
    let last_uid = messages
        .last()
        .map(|m| m.uid)
        .or_else(|| state.filter(|_| !reset).and_then(|s| s.last_uid));
    *state = Some(MailboxState {
        uid_validity,
        last_uid,
    });
    Ok(Synced {
        uid_validity,
        messages: (reset || !messages.is_empty()).then_some(messages),
        flags,
        asked,
    })
}

/// What to fetch after selecting a mailbox.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Plan {
    /// Every message with a UID above this one.
    After(u32),
    /// The newest messages, from this sequence number on.
    Newest { from_seq: u32 },
}

/// Decides what to fetch, given what's cached and what the server reports on SELECT.
fn plan(
    state: Option<MailboxState>,
    uid_validity: u32,
    exists: u32,
    initial_fetch: u32,
) -> Option<Plan> {
    if exists == 0 {
        return None;
    }
    if let Some(MailboxState {
        uid_validity: cached,
        last_uid: Some(last),
    }) = state
        && cached == uid_validity
    {
        return Some(Plan::After(last));
    }
    let count = initial_fetch.min(exists);
    (count > 0).then(|| Plan::Newest {
        from_seq: exists - count + 1,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    const fn state(uid_validity: u32, last_uid: Option<u32>) -> MailboxState {
        MailboxState {
            uid_validity,
            last_uid,
        }
    }

    #[test]
    fn an_empty_mailbox_fetches_nothing() {
        assert_eq!(plan(None, 1, 0, 200), None);
        assert_eq!(plan(Some(state(1, Some(5))), 1, 0, 200), None);
    }

    #[test]
    fn a_first_sync_fetches_only_the_newest_messages() {
        assert_eq!(
            plan(None, 1, 1000, 200),
            Some(Plan::Newest { from_seq: 801 })
        );
        assert_eq!(plan(None, 1, 50, 200), Some(Plan::Newest { from_seq: 1 }));
        assert_eq!(plan(None, 1, 50, 0), None, "initial fetch disabled");
    }

    #[test]
    fn a_cached_mailbox_fetches_what_came_after() {
        assert_eq!(
            plan(Some(state(1, Some(41))), 1, 1000, 200),
            Some(Plan::After(41))
        );
    }

    #[test]
    fn a_changed_uid_validity_starts_over() {
        assert_eq!(
            plan(Some(state(1, Some(41))), 2, 1000, 200),
            Some(Plan::Newest { from_seq: 801 })
        );
    }

    #[test]
    fn a_mailbox_that_was_empty_starts_with_the_newest() {
        assert_eq!(
            plan(Some(state(1, None)), 1, 3, 200),
            Some(Plan::Newest { from_seq: 1 })
        );
    }
}
