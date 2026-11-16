//! The window's state, and everything that changes it.

use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use jiff::Zoned;
use postden_app::{
    Draft, Imported, Login, Mailroom, Passwords, Profile, Settings, Theme, Update, check_logins,
    eml_file_name, finish_google_sign_in, is_sealed, password_key, remove_account, save_account,
    signature, smtp_password_key, start_google_sign_in,
};
use postden_core::account::{Account, Signature};
use postden_core::message::{Body, Envelope, Mailbox, Role};
use postden_core::secrets::{SecretStore, SecretString};
use postden_mail::imap::{Status, WatchConfig};
use postden_secrets::Backend;
use postden_store::{Scope, Store, StoredMessage};
use postden_transfer::{Code, Offer};
use rune::{
    AppHandle, FileDialogOptions, ImageCache, PointerPress, Taskbar, TextEdit, Tray, Tx,
    WindowHandle,
};
use rune_kit::shell::PaneState;

use crate::account_settings::{AccountSettings, IMAGE_EXTENSIONS};
use crate::compose::{ComposeForm, Mode};
use crate::form::{AccountForm, GoogleSignIn, Step};
use crate::notifications::{self, Alert, Target};
use crate::search::{self, MailboxKey, MessageKey, SERVER_PAGE, Search};
use crate::transfer::{self, Transfer};
use crate::update::{self, Phase, Updater};

/// How many messages the list shows at first.
const LIST_LIMIT: usize = 300;
/// How many more messages the list shows, or fetches from each mailbox, once scrolled to its end.
const LIST_PAGE: usize = 100;

/// What the app runs on once started.
pub(crate) struct Services {
    pub(crate) store: Store,
    pub(crate) secrets: Box<dyn SecretStore>,
    /// Passwords come from a command and are never written.
    pub(crate) read_only: bool,
    pub(crate) mailroom: Mailroom,
}

/// A message open for reading.
pub(crate) struct Reader {
    /// The message as listed. Its `seen` follows the list.
    pub(crate) message: StoredMessage,
    /// The downloaded body, or why it couldn't be downloaded. `None` while downloading.
    pub(crate) body: Option<Result<Body, String>>,
    /// The body's HTML as Markdown, to show, converted once when it arrives.
    pub(crate) markdown: Option<String>,
}

impl Reader {
    fn is(&self, account: &str, mailbox: &str, uid: u32) -> bool {
        is_message(&self.message, account, mailbox, uid)
    }
}

/// The menu of a message in the list, opened by right-clicking it.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct MessageMenu {
    pub(crate) message: MessageKey,
    /// Where it was right-clicked, which the menu opens at.
    pub(crate) point: PointerPress,
    /// Its "Move to" submenu is open.
    pub(crate) move_open: bool,
}

impl MessageMenu {
    pub(crate) fn is(&self, account: &str, mailbox: &str, uid: u32) -> bool {
        let (a, b, u) = &self.message;
        a == account && b == mailbox && *u == uid
    }
}

/// The menu of a mailbox in the sidebar, opened by right-clicking it.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct MailboxMenu {
    pub(crate) mailbox: MailboxKey,
    /// Where it was right-clicked, which the menu opens at.
    pub(crate) point: PointerPress,
}

impl MailboxMenu {
    pub(crate) fn is(&self, account: &str, mailbox: &str) -> bool {
        let (a, b) = &self.mailbox;
        a == account && b == mailbox
    }
}

/// A body's HTML as Markdown, to show, if it has HTML with something in it.
fn markdown(body: &Body) -> Option<String> {
    body.html
        .as_deref()
        .map(postden_html::to_markdown)
        .filter(|md| !md.trim().is_empty())
}

/// Whose mailboxes an account reads: the same IMAP server and user is the same mailboxes, with
/// the same UIDs.
fn imap_owner(account: &Account) -> (String, u16, String) {
    (
        account.imap.host.to_ascii_lowercase(),
        account.imap.port,
        account.imap.username.clone(),
    )
}

/// What importing a profile did, in a sentence.
fn imported_notice(imported: &Imported) -> String {
    let count = |n: usize| format!("{n} account{}", if n == 1 { "" } else { "s" });
    let mut said = Vec::new();
    if !imported.added.is_empty() {
        said.push(format!("added {}", count(imported.added.len())));
    }
    if !imported.replaced.is_empty() {
        said.push(format!("replaced {}", count(imported.replaced.len())));
    }
    if !imported.kept.is_empty() {
        said.push(format!("kept {} as set up", count(imported.kept.len())));
    }
    let mut notice = if said.is_empty() {
        "Imported the settings".to_owned()
    } else {
        let said = said.join(", ");
        format!("Imported: {said}")
    };
    if !imported.without_secrets.is_empty() {
        notice.push_str("; their passwords come from the password command");
    }
    let failed = imported
        .failed
        .iter()
        .map(|(id, error)| format!("; couldn't import {id}: {error}"));
    notice.extend(failed);
    notice
}

fn is_message(m: &StoredMessage, account: &str, mailbox: &str, uid: u32) -> bool {
    m.account == account && m.mailbox == mailbox && m.envelope.uid == uid
}

/// What the message list shows.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Showing {
    /// Every account's mailbox with this role: all inboxes, or all Sent mailboxes.
    All(Role),
    /// One mailbox of one account.
    Mailbox { account: String, name: String },
}

impl Showing {
    /// An account's inbox.
    pub(crate) fn inbox(account: &str) -> Self {
        Self::Mailbox {
            account: account.to_owned(),
            name: INBOX.to_owned(),
        }
    }

    /// The account whose mail is shown, if just one's is.
    pub(crate) fn account(&self) -> Option<&str> {
        match self {
            Self::All(_) => None,
            Self::Mailbox { account, .. } => Some(account),
        }
    }

    pub(crate) fn is(&self, account: &str, name: &str) -> bool {
        matches!(self, Self::Mailbox { account: a, name: n } if a == account && n == name)
    }

    /// The cached mail this shows.
    fn scope(&self) -> Scope<'_> {
        match self {
            Self::All(role) => Scope::Role(*role),
            Self::Mailbox { account, name } => Scope::Mailbox { account, name },
        }
    }
}

/// What a search of every inbox looks through.
static ALL_INBOXES: Showing = Showing::All(Role::Inbox);

/// The inbox's name on every server.
pub(crate) const INBOX: &str = "INBOX";

/// Counts the window's closes and reopenings, so a check started when it closed only acts on that
/// close. Shared, not in [`Mail`], so the tray's thread bumps it the moment its icon is clicked,
/// before an update already queued could act on the close.
static CLOSES: AtomicU64 = AtomicU64::new(0);

/// The window is coming back: what its last close started no longer applies. Any thread.
pub(crate) fn reopened() {
    CLOSES.fetch_add(1, Ordering::AcqRel);
}

/// How long a notice shows before it closes on its own.
const NOTICE_SECS: u64 = 6;

/// How long after starting the first automatic update check runs, out of the way of the first sync.
const FIRST_UPDATE_CHECK: Duration = Duration::from_secs(10);
/// How often updates are checked for while the app runs.
const UPDATE_CHECK_EVERY: Duration = Duration::from_hours(24);

pub(crate) struct Mail {
    handle: Option<AppHandle<Self>>,
    /// The tray icon, kept for as long as the app runs. `None` where there's none, and until it
    /// has started.
    tray: Option<Tray>,
    /// The window, once it exists, for the unread count on the app's icon.
    window: Option<WindowHandle>,
    /// The unread count the app's icon shows, `None` until it's shown on the current window.
    badge: Option<usize>,
    /// The window has the keyboard focus: the user is looking at the mail, so new mail isn't
    /// announced with a system notification.
    focused: bool,
    /// The services, or why they couldn't start.
    pub(crate) services: Result<Services, String>,
    pub(crate) accounts: Vec<Account>,
    /// Each account's connection status, by account id, or why its watcher couldn't start.
    pub(crate) statuses: HashMap<String, Result<Status, String>>,
    /// What the list shows, and its mail.
    pub(crate) showing: Showing,
    pub(crate) messages: Vec<StoredMessage>,
    /// The search over the list. While it has a query, the list shows what matches.
    pub(crate) search: Search,
    /// How many messages the list may show. It grows as the list is scrolled to its end.
    list_limit: usize,
    /// The synced mailboxes whose mail the list shows.
    listed: Vec<MailboxKey>,
    /// Mailboxes whose older mail is being fetched.
    fetching_older: HashSet<MailboxKey>,
    /// Mailboxes with nothing older left on the server than what's cached.
    no_older: HashSet<MailboxKey>,
    /// Each account's mailboxes as the server last listed them, by account id.
    pub(crate) mailboxes: HashMap<String, Vec<Mailbox>>,
    /// Unread inbox messages, by account id.
    unread: HashMap<String, usize>,
    /// The latest thing worth telling the user, such as new mail arriving.
    /// It closes on its own after [`NOTICE_SECS`], or when its close button is pressed.
    pub(crate) notice: Option<String>,
    /// Counts notices, so a timer only closes the one it was started for.
    notice_id: u64,
    pub(crate) form: AccountForm,
    /// The account whose Remove button was pressed once and awaits a second press.
    pub(crate) confirm_remove: Option<String>,
    pub(crate) sidebar: PaneState,
    /// The message open in the main pane, if any.
    pub(crate) reader: Option<Reader>,
    /// Accounts whose mailboxes are hidden in the sidebar.
    pub(crate) collapsed: HashSet<String>,
    /// The toolbar's ⋯ menu is open.
    pub(crate) actions_open: bool,
    /// The menu of a message in the list, while it's open.
    pub(crate) message_menu: Option<MessageMenu>,
    /// The menu of a mailbox in the sidebar, while it's open.
    pub(crate) mailbox_menu: Option<MailboxMenu>,
    /// A reply asked for before its message was downloaded: the message, and whether to everyone.
    /// It starts once the message opens.
    pending_reply: Option<(MessageKey, bool)>,
    /// Messages being moved, left out of the list until the server answers.
    moving: HashSet<MessageKey>,
    /// Mailboxes being marked read, as asked with [`Mail::mark_all_seen`], until the server answers.
    marking_seen: HashSet<MailboxKey>,
    /// The message being written, shown over the reader and the list.
    pub(crate) compose: ComposeForm,
    /// The app's preferences, as last saved.
    pub(crate) settings: Settings,
    /// The settings page is shown over everything else in the main pane.
    pub(crate) settings_open: bool,
    /// One account's settings, shown in place of the settings page while it's open.
    pub(crate) account_settings: AccountSettings,
    /// Decodes the images shown, such as a signature's. `None` before the app starts.
    pub(crate) images: Option<ImageCache>,
    /// Checking for, announcing and installing new releases.
    pub(crate) updater: Updater,
    /// Exporting or importing every account and setting, in the settings.
    pub(crate) transfer: Transfer,
}

impl Mail {
    /// The state before [`Mail::start`] runs.
    pub(crate) fn not_started() -> Self {
        Self::failed(None, "starting…".into())
    }

    fn failed(handle: Option<AppHandle<Self>>, error: String) -> Self {
        Self {
            handle,
            tray: None,
            window: None,
            badge: None,
            focused: false,
            services: Err(error),
            accounts: Vec::new(),
            statuses: HashMap::new(),
            messages: Vec::new(),
            search: Search::default(),
            list_limit: LIST_LIMIT,
            listed: Vec::new(),
            fetching_older: HashSet::new(),
            no_older: HashSet::new(),
            showing: Showing::All(Role::Inbox),
            mailboxes: HashMap::new(),
            unread: HashMap::new(),
            notice: None,
            notice_id: 0,
            form: AccountForm::default(),
            confirm_remove: None,
            sidebar: PaneState::new(260.0).range(200.0, 420.0),
            reader: None,
            compose: ComposeForm::default(),
            actions_open: false,
            message_menu: None,
            mailbox_menu: None,
            pending_reply: None,
            moving: HashSet::new(),
            marking_seen: HashSet::new(),
            collapsed: HashSet::new(),
            settings: Settings::default(),
            settings_open: false,
            account_settings: AccountSettings::default(),
            images: None,
            updater: Updater::default(),
            transfer: Transfer::default(),
        }
    }

    /// Opens the store and starts watching every account. Failing that, the window says why.
    pub(crate) fn start(handle: AppHandle<Self>) -> Self {
        match Self::try_start(&handle) {
            Ok((services, settings)) => {
                let wake = handle.clone();
                let mut mail = Self::failed(Some(handle), String::new());
                // A decoded picture changes what the view shows (Markdown draws an image once it's
                // ready), so finishing one updates the state, which builds the view again.
                mail.images = Some(ImageCache::new(Arc::new(move || {
                    wake.update(|_: &mut Self, _| {});
                })));
                mail.services = Ok(services);
                mail.settings = settings;
                mail.reload();
                let failed = mail
                    .services
                    .as_mut()
                    .map(|s| s.mailroom.watch_all(&s.store, s.secrets.as_ref()));
                match failed {
                    Ok(Ok(failed)) => {
                        for (account, error) in failed {
                            mail.statuses.insert(account, Err(error.to_string()));
                        }
                    }
                    Ok(Err(e)) => mail.notify(e.to_string()),
                    Err(_) => {}
                }
                mail.form.open = mail.accounts.is_empty();
                mail.updater = Updater::new();
                if mail.settings.check_updates {
                    mail.schedule_update_check(FIRST_UPDATE_CHECK);
                }
                mail
            }
            Err(e) => Self::failed(Some(handle), e),
        }
    }

    /// Shows `notice` in place of the last one, and closes it after [`NOTICE_SECS`] unless a newer
    /// one replaced it by then.
    fn notify(&mut self, notice: String) {
        self.notice = Some(notice);
        self.notice_id += 1;
        let Some(handle) = &self.handle else { return };
        let (handle, id) = (handle.clone(), self.notice_id);
        // Without a thread the notice stays until it's closed by hand or replaced.
        drop(rune::task::spawn_after(
            "notice-timeout",
            Duration::from_secs(NOTICE_SECS),
            move || {
                handle.update(move |s: &mut Self, _| {
                    if s.notice_id == id {
                        s.notice = None;
                    }
                });
            },
        ));
    }

    /// Closes the notice now.
    pub(crate) fn dismiss_notice(&mut self) {
        self.notice = None;
    }

    fn try_start(handle: &AppHandle<Self>) -> Result<(Services, Settings), String> {
        let dir = postden_app::data_dir(None).map_err(|e| e.to_string())?;
        let database = postden_app::database_path(&dir);
        let store = Store::open(&database)
            .map_err(|e| format!("couldn't open {}: {e}", database.display()))?;
        let backend = postden_app::secrets_backend(None);
        let secrets = postden_secrets::open(&backend).map_err(|e| {
            format!("{e}. Without a keyring, set POSTDEN_PASSWORD_COMMAND to a command that prints the password.")
        })?;
        let settings = Settings::load(&store).map_err(|e| e.to_string())?;
        let config = WatchConfig {
            poll_interval: settings.poll_interval,
            ..WatchConfig::default()
        };
        let notify = handle.clone();
        let mailroom = Mailroom::start(&database, config, settings.prefetch, move |update| {
            notify.update(move |s: &mut Self, tx| s.apply(update, tx));
        })
        .map_err(|e| e.to_string())?;
        let services = Services {
            store,
            secrets,
            read_only: matches!(backend, Backend::Command { .. }),
            mailroom,
        };
        Ok((services, settings))
    }

    /// Rereads accounts and mail from the store.
    fn reload(&mut self) {
        let Ok(services) = &self.services else { return };
        let messages = if let Some(query) = self.search.query() {
            services
                .store
                .matching_messages(
                    self.listing().scope(),
                    |e| search::matches(e, query),
                    self.list_limit,
                )
                .map(|cached| self.search.merge(cached, self.list_limit))
                .and_then(|mut merged| {
                    // A server match the cache holds but whose headers don't match (its text
                    // did) shows as read or unread as the cache says, which follows the server.
                    for m in &mut merged {
                        let (a, b, uid) = (&m.account, &m.mailbox, m.envelope.uid);
                        if let Some(seen) = services.store.seen(a, b, uid)? {
                            m.envelope.seen = seen;
                        }
                    }
                    Ok(merged)
                })
        } else {
            match &self.showing {
                Showing::All(role) => services.store.recent_messages(*role, self.list_limit),
                Showing::Mailbox { account, name } => {
                    services
                        .store
                        .mailbox_messages(account, name, self.list_limit)
                }
            }
        };
        let mailboxes = services.store.accounts().and_then(|accounts| {
            accounts
                .into_iter()
                .map(|a| Ok((services.store.mailboxes(&a.id)?, a)))
                .collect::<Result<Vec<_>, _>>()
        });
        match (mailboxes, messages, services.store.unread_counts()) {
            (Ok(accounts), Ok(mut messages), Ok(unread)) => {
                messages.retain(|m| {
                    !self
                        .moving
                        .contains(&(m.account.clone(), m.mailbox.clone(), m.envelope.uid))
                });
                self.mailboxes = accounts
                    .iter()
                    .map(|(mailboxes, a)| (a.id.clone(), mailboxes.clone()))
                    .collect();
                self.accounts = accounts.into_iter().map(|(_, a)| a).collect();
                self.messages = messages;
                self.listed = self.listed_mailboxes();
                self.unread = unread;
                self.show_badge();
                if let Some(reader) = &mut self.reader {
                    let m = &reader.message;
                    let (account, mailbox, uid) = (&m.account, &m.mailbox, m.envelope.uid);
                    if let Some(listed) = self
                        .messages
                        .iter()
                        .find(|l| is_message(l, account, mailbox, uid))
                    {
                        reader.message = listed.clone();
                    }
                }
            }
            (Err(e), _, _) | (_, Err(e), _) | (_, _, Err(e)) => {
                self.notify(format!("couldn't read mail: {e}"));
            }
        }
    }

    fn apply(&mut self, update: Update, tx: &mut Tx) {
        if let Update::Mail {
            account,
            mailbox,
            first_sync: true,
            ..
        } = &update
        {
            // Started over: what was older is to be fetched again, and what a search found there
            // is gone.
            self.no_older.remove(&(account.clone(), mailbox.clone()));
            self.search.forget_mailbox(account, mailbox);
        }
        match update {
            Update::Status { account, status } => {
                self.statuses.insert(account, Ok(status));
            }
            Update::Mail {
                account,
                mailbox,
                role: Role::Inbox,
                count,
                first_sync,
                uid_validity,
                unread,
            } => {
                let target = Target {
                    account,
                    mailbox,
                    uid_validity,
                    uid: None,
                };
                self.inbox_mail(target, count, first_sync, &unread);
            }
            Update::CopyFailed { account, error } => self.copy_failed(&account, &error),
            Update::SaveFailed { account, error } => {
                self.notify(format!("{account}: couldn't save new mail: {error}"));
            }
            Update::Opened {
                account,
                mailbox,
                uid,
                body,
            } => self.opened(&account, &mailbox, uid, body, tx),
            // Sent mail is shown as it's listed, without a notice.
            Update::Mail { .. } => self.reload(),
            Update::Mailboxes { account } => self.mailboxes_listed(&account),
            Update::Seen {
                account,
                mailbox,
                uid,
                seen,
            } => {
                self.search.settle(&account, &mailbox, uid, Some(seen));
                self.reload();
            }
            Update::Older {
                account,
                mailbox,
                count,
                done,
            } => self.older_fetched(account, mailbox, count, done),
            Update::OlderFailed {
                account,
                mailbox,
                error,
            } => {
                let label = self.label(&account).to_owned();
                self.notify(format!("{label}: couldn't fetch older mail: {error}"));
                self.fetching_older.remove(&(account, mailbox));
            }
            Update::SeenFailed {
                account,
                mailbox,
                uid,
                error,
            } => self.seen_failed(&account, &mailbox, uid, &error),
            Update::AllSeen {
                account,
                mailbox,
                result,
            } => self.all_seen(&account, &mailbox, result),
            Update::Moved {
                account,
                mailbox,
                uid,
                to,
                result,
            } => self.moved(&account, &mailbox, uid, &to, result),
            Update::Exported {
                account,
                path,
                result,
            } => self.exported(&account, &path, result),
            Update::Sent { account, result } => self.sent(&account, result),
            Update::Searched {
                id,
                account,
                mailbox,
                result,
            } => self.searched(id, &account, &mailbox, result),
        }
    }

    /// The server listed an account's mailboxes. What a search found in one deleted or renamed
    /// on the server is gone.
    fn mailboxes_listed(&mut self, account: &str) {
        self.reload();
        let listed = self.mailboxes.get(account);
        let gone = |a: &str, m: &str| {
            a == account && !listed.is_some_and(|l| l.iter().any(|b| b.name == m))
        };
        if self.search.forget_gone(gone) {
            self.reload();
        }
    }

    /// Older mail was saved: `count` messages, and, if `done`, nothing older is left.
    fn older_fetched(&mut self, account: String, mailbox: String, count: usize, done: bool) {
        let key = (account, mailbox);
        self.fetching_older.remove(&key);
        if done {
            self.no_older.insert(key);
        }
        // Make room for what came, as it's older than everything listed.
        self.list_limit = self.list_limit.max(self.messages.len() + count);
        self.reload();
    }

    /// The server refused to mark a message read or unread, which shows as the store says again.
    fn seen_failed(&mut self, account: &str, mailbox: &str, uid: u32, error: &str) {
        self.search.settle(account, mailbox, uid, None);
        self.notify(format!("{account}: {error}"));
        self.reload();
    }

    /// The server marked a whole mailbox read, as asked with [`Mail::mark_all_seen`], or refused to.
    /// Every message below `up_to` was marked; with `None`, the mailbox started over meanwhile, so
    /// nothing listed was.
    fn all_seen(&mut self, account: &str, mailbox: &str, result: Result<Option<u32>, String>) {
        self.marking_seen
            .remove(&(account.to_owned(), mailbox.to_owned()));
        let name = self.mailbox_label(account, mailbox);
        // Started over meanwhile, the server still marked it all; the store refetches it as it is.
        if result.is_ok() {
            self.notify(format!("Marked {name} as read"));
        }
        match result {
            Ok(None) => {}
            Ok(Some(up_to)) => {
                self.search.set_mailbox_seen(account, mailbox, up_to);
                if let Some(reader) = &mut self.reader
                    && reader.message.account == account
                    && reader.message.mailbox == mailbox
                    && reader.message.envelope.uid < up_to
                {
                    reader.message.envelope.seen = true;
                }
            }
            Err(error) => {
                let label = self.label(account).to_owned();
                self.notify(format!("{label}: couldn't mark {name} as read: {error}"));
            }
        }
        self.reload();
    }

    /// The server moved a message asked for with [`Mail::move_message`], or refused to, which
    /// lists it where it was again.
    fn moved(
        &mut self,
        account: &str,
        mailbox: &str,
        uid: u32,
        to: &str,
        result: Result<bool, String>,
    ) {
        self.moving
            .remove(&(account.to_owned(), mailbox.to_owned(), uid));
        let to = self.mailbox_label(account, to);
        match result {
            Ok(current) => {
                // If the mailbox started over meanwhile, what a search found there under this
                // UID is another message.
                if current {
                    self.search.forget_message(account, mailbox, uid);
                }
                self.notify(format!("Moved to {to}"));
            }
            Err(error) => {
                let label = self.label(account).to_owned();
                self.notify(format!(
                    "{label}: couldn't move the message to {to}: {error}"
                ));
            }
        }
        self.reload();
    }

    /// A message asked for with [`Mail::open_message`] was downloaded, or couldn't be.
    fn opened(
        &mut self,
        account: &str,
        mailbox: &str,
        uid: u32,
        body: Result<Body, String>,
        tx: &mut Tx,
    ) {
        self.search
            .settle(account, mailbox, uid, body.is_ok().then_some(true));
        if let Some(reader) = &mut self.reader
            && reader.is(account, mailbox, uid)
        {
            reader.markdown = body.as_ref().ok().and_then(markdown);
            let downloaded = body.is_ok();
            reader.body = Some(body);
            // The server, not a click, brought it, which would land the text at once:
            // ask for its entrance anyway, so it fades in over the spinner.
            tx.animated();
            let waiting = self
                .pending_reply
                .take_if(|(key, _)| key.0 == account && key.1 == mailbox && key.2 == uid);
            // Failing to download, the reader says why, and a reply waits to be asked again. A
            // message started meanwhile isn't replaced.
            if let (Some((_, all)), true, false) = (waiting, downloaded, self.compose.open) {
                self.reply(all);
            }
        }
        // Read now, or, if the download failed, unread again as the store still says.
        self.reload();
    }

    /// The server finished searching a mailbox, for the search running now or one replaced.
    fn searched(
        &mut self,
        id: u64,
        account: &str,
        mailbox: &str,
        result: Result<postden_mail::imap::Found, String>,
    ) {
        if id != self.search.id {
            return;
        }
        let answer = result.as_ref().map_err(|_error| ()).map(|found| {
            let messages = found.messages.iter().map(|envelope| StoredMessage {
                account: account.to_owned(),
                mailbox: mailbox.to_owned(),
                envelope: envelope.clone(),
            });
            (messages.collect(), found.more)
        });
        if !self.search.answered(account, mailbox, answer) {
            return;
        }
        match result {
            Ok(_) => self.reload(),
            Err(error) => {
                let label = self.label(account).to_owned();
                self.notify(format!("{label}: couldn't search {mailbox}: {error}"));
            }
        }
    }

    /// What the list shows: what's selected, or, searching everywhere, every inbox.
    pub(crate) fn listing(&self) -> &Showing {
        if self.search.everywhere && self.search.is_active() {
            &ALL_INBOXES
        } else {
            &self.showing
        }
    }

    /// The query changed: matches it in the cache. What the server found for the last one no
    /// longer stands.
    pub(crate) fn search_changed(&mut self) {
        if !self.search.asked_server() {
            self.search.forget_server();
        }
        self.list_limit = LIST_LIMIT;
        self.reload();
    }

    /// Searches every inbox, or just what's selected.
    pub(crate) fn search_everywhere(&mut self, everywhere: bool) {
        if self.search.everywhere != everywhere {
            self.search.everywhere = everywhere;
            self.search.forget_server();
            self.list_limit = LIST_LIMIT;
            self.reload();
        }
    }

    /// Ends the search, showing the whole list again. Returns whether there was one.
    pub(crate) fn clear_search(&mut self) -> bool {
        if self.search.query.text.is_empty() {
            return false;
        }
        self.search.query.clear();
        self.search_changed();
        true
    }

    /// Puts the caret in the search field, back on the list.
    pub(crate) fn focus_search(&mut self) {
        self.reader = None;
        self.search.query.focused = true;
    }

    /// Asks the server of each mailbox searched for what matches, beyond what's cached and in the
    /// message text.
    pub(crate) fn search_server(&mut self) {
        let Some(query) = self.search.query().map(str::to_owned) else {
            return;
        };
        let mailboxes = self.mailboxes_of(self.listing());
        let Ok(services) = &self.services else { return };
        if mailboxes.is_empty() {
            self.notify(
                "Nothing here has synced yet, so there's nothing to search on the server".into(),
            );
            return;
        }
        self.search.forget_server();
        let mut failed = Vec::new();
        for key in mailboxes {
            match services.mailroom.search(
                &services.store,
                &key.0,
                &key.1,
                &query,
                self.search.id,
                SERVER_PAGE,
            ) {
                Ok(()) => {
                    self.search.pending.insert(key);
                }
                Err(e) => {
                    failed.push(format!("{}: {e}", self.label(&key.0)));
                    self.search.failed.insert(key);
                }
            }
        }
        self.search.asked = Some(query);
        // What the last search found goes until this one's answers come.
        self.reload();
        if !failed.is_empty() {
            self.notify(format!("Couldn't search the server. {}", failed.join("; ")));
        }
    }

    /// Starts a new message from the open message's account, else the selected one, else the first.
    pub(crate) fn compose(&mut self) {
        let account = self
            .reader
            .as_ref()
            .map(|r| r.message.account.clone())
            .or_else(|| self.showing.account().map(str::to_owned))
            .or_else(|| self.accounts.first().map(|a| a.id.clone()));
        if let Some(account) = account {
            // Writing this, a reply still waiting for its message to download no longer starts.
            self.pending_reply = None;
            self.form.open = false;
            self.settings_open = false;
            let draft = Draft {
                signature: self.signature_of(&account),
                ..Draft::new(&account)
            };
            self.compose = ComposeForm::new(draft, self.images.as_ref());
        }
    }

    /// The account's signature. Failing to read it, the message goes without, and says why.
    fn signature_of(&mut self, account: &str) -> Option<Signature> {
        let Ok(services) = &self.services else {
            return None;
        };
        match services.store.signature(account) {
            Ok(signature) => signature,
            Err(e) => {
                self.notify(format!("couldn't read the signature: {e}"));
                None
            }
        }
    }

    /// Sends the message being written from `account`, with its signature in place of the
    /// other's.
    pub(crate) fn compose_from(&mut self, account: &str) {
        if self.compose.account == account {
            return;
        }
        let signature = self.signature_of(account);
        account.clone_into(&mut self.compose.account);
        self.compose.set_signature(signature, self.images.as_ref());
    }

    /// Starts a reply to the open message, once it's downloaded.
    pub(crate) fn reply(&mut self, all: bool) {
        let Some(Reader {
            message,
            body: Some(Ok(body)),
            ..
        }) = &self.reader
        else {
            return;
        };
        let Some(account) = self.accounts.iter().find(|a| a.id == message.account) else {
            return;
        };
        let mut draft = Draft::reply(
            account,
            &message.envelope,
            body,
            all,
            &jiff::tz::TimeZone::system(),
        );
        draft.signature = self.signature_of(&draft.account);
        self.compose = ComposeForm::new(draft, self.images.as_ref());
    }

    /// Hands the message being written to the mailroom.
    pub(crate) fn send(&mut self) {
        let (Ok(services), c) = (&self.services, &mut self.compose) else {
            return;
        };
        if !c.open || c.sending {
            return;
        }
        let Some(account) = self.accounts.iter().find(|a| a.id == c.account) else {
            c.error = Some(format!("{} is no longer set up", c.account));
            return;
        };
        let result = c
            .draft()
            .outgoing(account)
            .and_then(|message| services.mailroom.send(&account.id, message));
        match result {
            Ok(()) => {
                c.sending = true;
                c.error = None;
            }
            Err(e) => c.error = Some(e.to_string()),
        }
    }

    /// The mailroom finished sending. Only one message is written at a time, so it's the one shown,
    /// if it's still shown.
    fn sent(&mut self, account: &str, result: Result<String, String>) {
        let label = self.label(account).to_owned();
        match result {
            Ok(_) => {
                if self.compose.is_sending() {
                    self.compose = ComposeForm::default();
                }
                self.notify(format!("Sent from {label}"));
            }
            Err(error) if self.compose.is_sending() => {
                self.compose.sending = false;
                self.compose.error = Some(format!("Couldn't send: {error}"));
            }
            Err(error) => self.notify(format!("{label}: couldn't send: {error}")),
        }
    }

    fn copy_failed(&mut self, account: &str, error: &str) {
        let label = self.label(account).to_owned();
        self.notify(format!(
            "{label}: sent, but couldn't file it in Sent: {error}"
        ));
    }

    pub(crate) fn discard(&mut self) {
        if self.compose.is_sending() {
            self.notify("Still sending; you'll be told how it went".into());
        }
        self.compose = ComposeForm::default();
    }

    /// Opens a message in the main pane, downloading it unless it was downloaded ahead, and
    /// marking it read. One the list doesn't show, such as one a search leaves out, opens from
    /// the cache.
    pub(crate) fn open_message(&mut self, account: &str, mailbox: &str, uid: u32) {
        self.pending_reply = None;
        let listed = self
            .messages
            .iter()
            .find(|m| is_message(m, account, mailbox, uid))
            .cloned();
        let cached = || {
            let services = self.services.as_ref().ok()?;
            services.store.message(account, mailbox, uid).ok().flatten()
        };
        let Some(message) = listed.or_else(cached) else {
            return;
        };
        let mut reader = Reader {
            message,
            body: None,
            markdown: None,
        };
        if let Ok(services) = &self.services {
            let opened = services
                .mailroom
                .open(&services.store, account, mailbox, uid);
            if let Err(e) = opened {
                reader.body = Some(Err(e.to_string()));
            } else {
                // Downloaded ahead, it shows at once; otherwise once it's downloaded.
                if let Ok(Some(body)) = opened {
                    reader.markdown = markdown(&body);
                    reader.body = Some(Ok(body));
                }
                // Shown read right away; the store catches up once the server agrees.
                reader.message.envelope.seen = true;
                if let Some(listed) = self
                    .messages
                    .iter_mut()
                    .find(|m| is_message(m, account, mailbox, uid))
                {
                    listed.envelope.seen = true;
                }
                self.search.set_seen(account, mailbox, uid, true);
            }
        }
        self.reader = Some(reader);
    }

    /// Opens a link from a message in the browser.
    pub(crate) fn open_link(&mut self, url: &str, tx: &mut Tx) {
        if let Err(e) = tx.open_url(url) {
            self.notify(format!("couldn't open {url}: {e}"));
        }
    }

    pub(crate) fn close_reader(&mut self) {
        self.reader = None;
        self.pending_reply = None;
    }

    /// Asks where to save a message as an `.eml` file, offering a name made of its subject, then
    /// downloads it there. It stays read or unread, as it was.
    pub(crate) fn export_message(tx: &mut Tx, message: &StoredMessage) {
        let options = FileDialogOptions {
            title: "Export the message".into(),
            file_name: Some(eml_file_name(message.envelope.subject.as_deref())),
            ..FileDialogOptions::default()
        }
        .filter("Email message", &["eml"]);
        let (account, mailbox, uid) = (
            message.account.clone(),
            message.mailbox.clone(),
            message.envelope.uid,
        );
        tx.save_file(options, move |s: &mut Self, result, _| match result {
            Ok(Some(path)) => s.export_to(&account, &mailbox, uid, path),
            Ok(None) => {}
            Err(e) => s.notify(format!("Couldn't open the file picker: {e}")),
        });
    }

    /// Downloads a message and saves it to `path` as an `.eml` file. A notice says how it went.
    fn export_to(&mut self, account: &str, mailbox: &str, uid: u32, path: PathBuf) {
        let Ok(services) = &self.services else { return };
        if let Err(e) = services
            .mailroom
            .export(&services.store, account, mailbox, uid, path)
        {
            let label = self.label(account).to_owned();
            self.notify(format!("{label}: couldn't export the message: {e}"));
        }
    }

    fn exported(&mut self, account: &str, path: &std::path::Path, result: Result<(), String>) {
        let name = path.file_name().map_or_else(
            || path.display().to_string(),
            |n| n.to_string_lossy().into_owned(),
        );
        let label = self.label(account).to_owned();
        self.notify(match result {
            Ok(()) => format!("Exported {name}"),
            Err(e) => format!("{label}: couldn't export {name}: {e}"),
        });
    }

    /// Flips the open message between read and unread.
    pub(crate) fn toggle_seen(&mut self) {
        let Some(reader) = &self.reader else { return };
        let m = &reader.message;
        let (account, mailbox, uid) = (m.account.clone(), m.mailbox.clone(), m.envelope.uid);
        self.set_seen(&account, &mailbox, uid, !m.envelope.seen);
    }

    /// Marks a message read or unread. It's shown so right away; the store catches up once the
    /// server agrees.
    pub(crate) fn set_seen(&mut self, account: &str, mailbox: &str, uid: u32, seen: bool) {
        let Ok(services) = &self.services else { return };
        if let Err(e) = services
            .mailroom
            .set_seen(&services.store, account, mailbox, uid, seen)
        {
            self.notify(e.to_string());
            return;
        }
        if let Some(reader) = &mut self.reader
            && reader.is(account, mailbox, uid)
        {
            reader.message.envelope.seen = seen;
        }
        if let Some(listed) = self
            .messages
            .iter_mut()
            .find(|l| is_message(l, account, mailbox, uid))
        {
            listed.envelope.seen = seen;
        }
        self.search.set_seen(account, mailbox, uid, seen);
    }

    /// Opens the menu of a message in the list at `point`, where it was right-clicked.
    pub(crate) fn open_message_menu(
        &mut self,
        account: &str,
        mailbox: &str,
        uid: u32,
        point: PointerPress,
    ) {
        self.message_menu = Some(MessageMenu {
            message: (account.to_owned(), mailbox.to_owned(), uid),
            point,
            move_open: false,
        });
    }

    pub(crate) fn close_message_menu(&mut self) {
        self.message_menu = None;
    }

    /// Shows or hides the message menu's "Move to" submenu.
    pub(crate) const fn toggle_move_menu(&mut self) {
        if let Some(menu) = &mut self.message_menu {
            menu.move_open = !menu.move_open;
        }
    }

    /// Opens the menu of a mailbox in the sidebar at `point`, where it was right-clicked.
    pub(crate) fn open_mailbox_menu(&mut self, account: &str, mailbox: &str, point: PointerPress) {
        self.mailbox_menu = Some(MailboxMenu {
            mailbox: (account.to_owned(), mailbox.to_owned()),
            point,
        });
    }

    pub(crate) fn close_mailbox_menu(&mut self) {
        self.mailbox_menu = None;
    }

    /// Marks every message in one of an account's mailboxes read, on the server too. It shows once
    /// the server agrees; until then the mailbox shows it's busy, and isn't marked again.
    pub(crate) fn mark_all_seen(&mut self, account: &str, mailbox: &str) {
        let key = (account.to_owned(), mailbox.to_owned());
        if self.marking_seen.contains(&key) {
            return;
        }
        let Ok(services) = &self.services else { return };
        if let Err(e) = services
            .mailroom
            .mark_all_seen(&services.store, account, mailbox)
        {
            let label = self.label(account).to_owned();
            self.notify(format!("{label}: couldn't mark the mailbox as read: {e}"));
            return;
        }
        self.marking_seen.insert(key);
    }

    /// Whether one of an account's mailboxes is being marked read, waiting on the server.
    pub(crate) fn marking_seen(&self, account: &str, mailbox: &str) -> bool {
        self.marking_seen
            .iter()
            .any(|(a, m)| a == account && m == mailbox)
    }

    /// Replies to a message, to its sender or everyone. It opens first, as the reply quotes it,
    /// and the reply starts once it's downloaded.
    pub(crate) fn reply_to(&mut self, account: &str, mailbox: &str, uid: u32, all: bool) {
        let open = self.reader.as_ref().filter(|r| r.is(account, mailbox, uid));
        match open.map(|r| &r.body) {
            Some(Some(Ok(_))) => return self.reply(all),
            // Still downloading: it replies once that's done.
            Some(None) => {}
            _ => self.open_message(account, mailbox, uid),
        }
        let open = self.reader.as_ref().filter(|r| r.is(account, mailbox, uid));
        match open.map(|r| &r.body) {
            // Downloaded ahead, it opened at once.
            Some(Some(Ok(_))) => self.reply(all),
            Some(None) => {
                self.pending_reply = Some(((account.to_owned(), mailbox.to_owned(), uid), all));
            }
            _ => {}
        }
    }

    /// Moves a message to another of its account's mailboxes. It leaves the list at once, and
    /// comes back if the server refuses.
    pub(crate) fn move_message(&mut self, account: &str, mailbox: &str, uid: u32, to: &str) {
        let Ok(services) = &self.services else { return };
        if let Err(e) = services
            .mailroom
            .move_message(&services.store, account, mailbox, uid, to)
        {
            let label = self.label(account).to_owned();
            self.notify(format!("{label}: couldn't move the message: {e}"));
            return;
        }
        self.moving
            .insert((account.to_owned(), mailbox.to_owned(), uid));
        if self
            .reader
            .as_ref()
            .is_some_and(|r| r.is(account, mailbox, uid))
        {
            self.reader = None;
        }
        self.reload();
    }

    /// The account's mailbox with `role`, as last listed, if it has one.
    pub(crate) fn mailbox_with_role(&self, account: &str, role: Role) -> Option<&Mailbox> {
        self.mailboxes
            .get(account)
            .and_then(|list| list.iter().find(|m| m.role == role))
    }

    /// The name to show for one of the account's mailboxes.
    fn mailbox_label(&self, account: &str, name: &str) -> String {
        self.mailboxes
            .get(account)
            .and_then(|list| list.iter().find(|m| m.name == name))
            .map_or_else(|| name.to_owned(), Mailbox::label)
    }

    /// Downloads the open message again, after a failure.
    pub(crate) fn retry_open(&mut self) {
        if let Some(r) = self.reader.take() {
            let m = r.message;
            self.open_message(&m.account, &m.mailbox, m.envelope.uid);
        }
    }

    /// The messages to list.
    pub(crate) fn shown(&self) -> impl Iterator<Item = &StoredMessage> {
        self.messages.iter()
    }

    /// Whether the list shows sent mail, so it says whom each message went to.
    pub(crate) fn showing_sent(&self) -> bool {
        match self.listing() {
            Showing::All(role) => *role == Role::Sent,
            Showing::Mailbox { account, name } => self
                .mailboxes
                .get(account)
                .and_then(|list| list.iter().find(|m| m.name == *name))
                .is_some_and(|m| m.role == Role::Sent),
        }
    }

    /// Mail was saved in an inbox, `count` messages of which `unread` are new and unread: the
    /// notice says so, the list shows it, and a system notification announces it.
    fn inbox_mail(&mut self, target: Target, count: usize, first_sync: bool, unread: &[Envelope]) {
        let label = self.label(&target.account).to_owned();
        self.notify(postden_app::mail_notice(
            &label,
            count,
            first_sync,
            &Zoned::now(),
        ));
        self.reload();
        self.announce(&label, target, unread);
    }

    /// Tells the user about mail that arrived unread in an inbox with a system notification,
    /// unless they're looking at the window or turned notifications off.
    fn announce(&self, label: &str, mut target: Target, unread: &[Envelope]) {
        let Ok(services) = &self.services else { return };
        let looking = self.focused && self.handle.as_ref().is_some_and(AppHandle::is_visible);
        if !self.settings.notify_new_mail || looking {
            return;
        }
        let Some((title, body)) = postden_app::new_mail_alert(label, unread) else {
            return;
        };
        target.uid = match unread {
            [only] => Some(only.uid),
            _ => None,
        };
        let alert = Alert {
            title,
            body,
            target,
        };
        notifications::post(&alert, &services.mailroom.handle());
    }

    /// The window gained or lost the keyboard focus.
    pub(crate) const fn focus_changed(&mut self, focused: bool, _: &mut Tx) {
        self.focused = focused;
    }

    /// A notification about new mail was clicked: the window comes back with the message open,
    /// or with the account's inbox listed for several, or once the mailbox started over (its
    /// UIDs may name other messages then). A search is ended, as it would hide the new mail. A message being written stays in front, as it does for everything else, and the
    /// new mail waits unread in the list.
    pub(crate) fn notification_clicked(&mut self, target: &Target) {
        self.bring_forward();
        #[cfg(target_os = "macos")]
        rune_notify::bring_to_front();
        if self.compose.open {
            return;
        }
        self.clear_search();
        let showing = if self.showing == ALL_INBOXES {
            ALL_INBOXES.clone()
        } else {
            Showing::inbox(&target.account)
        };
        self.show(showing);
        let uid_validity = self.services.as_ref().ok().and_then(|s| {
            let state = s.store.mailbox_state(&target.account, &target.mailbox);
            state.ok().flatten().map(|state| state.uid_validity)
        });
        if let Some(uid) = target.message(uid_validity) {
            self.open_message(&target.account, &target.mailbox, uid);
        }
    }

    /// The window was made, or made again: its icon shows the unread count.
    pub(crate) fn attach_window(&mut self, window: WindowHandle) {
        self.window = Some(window);
        self.badge = None;
        self.show_badge();
    }

    /// Shows how many inbox messages are unread on the app's icon: the Dock's on macOS, the
    /// taskbar button's on Windows. Linux has no badge.
    fn show_badge(&mut self) {
        let Some(window) = &self.window else { return };
        let unread = self.unread(None);
        if self.badge == Some(unread) {
            return;
        }
        let description = match unread {
            1 => "1 unread message".to_owned(),
            n => format!("{n} unread messages"),
        };
        let count = u32::try_from(unread).unwrap_or(u32::MAX);
        window.set_taskbar(Taskbar::new().badge(count, description));
        self.badge = Some(unread);
    }

    /// How many inbox messages are unread, in one account or all of them.
    pub(crate) fn unread(&self, account: Option<&str>) -> usize {
        account.map_or_else(
            || self.unread.values().sum(),
            |id| self.unread.get(id).copied().unwrap_or_default(),
        )
    }

    /// Shows or hides an account's mailboxes in the sidebar.
    pub(crate) fn toggle_mailboxes(&mut self, account: &str) {
        if !self.collapsed.remove(account) {
            self.collapsed.insert(account.to_owned());
        }
    }

    /// Lists `showing`. A mailbox other than an inbox is also refreshed from the server, as those
    /// are otherwise only synced every few minutes.
    pub(crate) fn show(&mut self, showing: Showing) {
        self.form.open = false;
        self.settings_open = false;
        self.reader = None;
        self.confirm_remove = None;
        if let (Ok(services), Showing::Mailbox { account, name }) = (&self.services, &showing)
            && name != INBOX
        {
            services.mailroom.refresh(account);
        }
        if self.showing != showing {
            // A search goes on in the mailbox now shown, unless it looks through every inbox.
            if !self.search.everywhere {
                self.search.forget_server();
            }
            self.showing = showing;
            self.list_limit = LIST_LIMIT;
            self.reload();
        }
    }

    /// The synced mailboxes whose mail [`Mail::showing`] lists.
    fn listed_mailboxes(&self) -> Vec<MailboxKey> {
        self.mailboxes_of(&self.showing)
    }

    /// The synced mailboxes whose mail `showing` lists.
    fn mailboxes_of(&self, showing: &Showing) -> Vec<MailboxKey> {
        let Ok(services) = &self.services else {
            return Vec::new();
        };
        match showing {
            Showing::Mailbox { account, name } => vec![(account.clone(), name.clone())],
            Showing::All(role) => self
                .accounts
                .iter()
                .filter_map(|a| {
                    let found = services.store.mailbox_for_role(&a.id, *role).ok()??;
                    Some((a.id.clone(), found.0))
                })
                .collect(),
        }
    }

    /// Whether the list has more to show past its end: cached, or still on a server.
    /// While searching, that's only more cached matches: the server is searched as a whole.
    pub(crate) fn has_older(&self) -> bool {
        if self.search.is_active() {
            return self.messages.len() >= self.list_limit;
        }
        self.messages.len() >= self.list_limit
            || !self.fetching_older.is_empty()
            || self.listed.iter().any(|m| !self.no_older.contains(m))
    }

    /// Whether older mail is being fetched for the list.
    pub(crate) fn fetching_older(&self) -> bool {
        !self.fetching_older.is_empty()
    }

    /// The list was scrolled to its end: shows more of what's cached, or, with nothing more cached,
    /// fetches older mail from each listed mailbox that has some.
    pub(crate) fn load_older(&mut self) {
        if !self.fetching_older.is_empty() {
            return;
        }
        let listed = self.messages.len();
        if listed >= self.list_limit {
            self.list_limit += LIST_PAGE;
            self.reload();
            if self.messages.len() > listed || self.search.is_active() {
                return;
            }
        }
        if self.search.is_active() {
            return;
        }
        let Ok(services) = &self.services else { return };
        for key in &self.listed {
            if self.no_older.contains(key) {
                continue;
            }
            // An account that isn't connected, or a mailbox with nothing cached, has nothing to
            // fetch from yet; its sync brings the newest first.
            if services
                .mailroom
                .fetch_older(&services.store, &key.0, &key.1, LIST_PAGE)
                .is_ok()
            {
                self.fetching_older.insert(key.clone());
            }
        }
    }

    /// The name to show for an account.
    pub(crate) fn label<'a>(&'a self, id: &'a str) -> &'a str {
        self.accounts
            .iter()
            .find(|a| a.id == id)
            .map_or(id, Account::label)
    }

    pub(crate) fn open_form(&mut self) {
        self.reader = None;
        self.settings_open = false;
        self.form = AccountForm {
            open: true,
            ..AccountForm::default()
        };
    }

    /// Moves the add-account form to `step`, starting it afresh.
    pub(crate) fn form_step(&mut self, step: Step) {
        self.form = AccountForm {
            open: true,
            step,
            ..AccountForm::default()
        };
        self.form.email.focused = step != Step::Choose;
    }

    /// Submits the form's current screen: signs in with Google, or checks the typed-in login.
    pub(crate) fn submit_form(&mut self, _: &mut Tx) {
        match self.form.step {
            Step::Choose => {}
            Step::Google => self.sign_in_with_google(),
            Step::Servers => self.check_servers(),
        }
    }

    /// Checks the typed-in login in the background, then saves the account if it works.
    fn check_servers(&mut self) {
        let (Ok(services), Some(handle)) = (&self.services, &self.handle) else {
            return;
        };
        if self.form.checking {
            return;
        }
        let account = match self
            .form
            .new_account()
            .and_then(|a| a.build().map_err(|e| e.to_string()))
        {
            Ok(account) => account,
            Err(e) => {
                self.form.error = Some(e);
                return;
            }
        };
        let password = if services.read_only {
            match services.secrets.get(&password_key(&account.id)) {
                Ok(Some(password)) => password,
                Ok(None) => {
                    self.form.error = Some(format!(
                        "the password command printed nothing for {}",
                        account.id
                    ));
                    return;
                }
                Err(e) => {
                    self.form.error = Some(e.to_string());
                    return;
                }
            }
        } else if self.form.password.text.is_empty() {
            self.form.error = Some("the password is missing".into());
            return;
        } else {
            SecretString::from(self.form.password.text.clone())
        };
        let smtp = if services.read_only {
            // A command may fail for a secret it doesn't have; then the IMAP one is used.
            services
                .secrets
                .get(&smtp_password_key(&account.id))
                .ok()
                .flatten()
        } else {
            (!self.form.smtp_password.text.is_empty())
                .then(|| SecretString::from(self.form.smtp_password.text.clone()))
        };
        let login = Login::from(Passwords {
            imap: password,
            smtp,
        });

        self.form.checking = true;
        self.form.error = None;
        let handle = handle.clone();
        services.mailroom.handle().spawn(async move {
            let result = check_logins(&account, &login)
                .await
                .map_err(|e| e.to_string());
            handle.update(move |s: &mut Self, _| s.login_checked(&account, &login, result));
        });
    }

    /// Signs in with Google in the browser, then checks the logins and saves the Gmail account.
    /// The email field, if filled, is suggested to Google; the name field names the account.
    fn sign_in_with_google(&mut self) {
        let (Ok(services), Some(handle)) = (&self.services, &self.handle) else {
            return;
        };
        if self.form.checking {
            return;
        }
        if services.read_only {
            self.form.error = Some(
                "Signing in with Google needs the OS keyring to keep the sign-in, but passwords \
                 come from POSTDEN_PASSWORD_COMMAND."
                    .into(),
            );
            return;
        }
        let optional = |t: &str| Some(t.trim().to_owned()).filter(|t| !t.is_empty());
        let hint = optional(&self.form.email.text);
        let name = optional(&self.form.name.text);
        let handle = handle.clone();
        let task = services.mailroom.handle().spawn(async move {
            let sign_in = match start_google_sign_in(hint.as_deref()).await {
                Ok(sign_in) => sign_in,
                Err(e) => {
                    let error = e.to_string();
                    handle.update(move |s: &mut Self, _| s.signed_in(Err(error)));
                    return;
                }
            };
            let url = sign_in.url().to_owned();
            handle.update(move |s: &mut Self, tx| s.open_sign_in(url, tx));
            let result = finish_google_sign_in(sign_in, name)
                .await
                .map_err(|e| e.to_string());
            handle.update(move |s: &mut Self, _| s.signed_in(result));
        });
        self.form.checking = true;
        self.form.error = None;
        self.form.google = Some(GoogleSignIn {
            task: task.abort_handle(),
            url: None,
        });
    }

    fn open_sign_in(&mut self, url: String, tx: &mut Tx) {
        let Some(google) = &mut self.form.google else {
            return;
        };
        if let Err(e) = tx.open_url(&url) {
            self.form.error = Some(format!("Couldn't open the browser: {e}"));
        }
        google.url = Some(url);
    }

    fn signed_in(&mut self, result: Result<(Account, Login), String>) {
        // Cancelled meanwhile.
        if self.form.google.take().is_none() {
            return;
        }
        match result {
            Ok((account, login)) => self.login_checked(&account, &login, Ok(())),
            Err(e) => {
                self.form.checking = false;
                self.form.error = Some(e);
            }
        }
    }

    /// Stops a Google sign-in that's waiting for the browser.
    pub(crate) fn cancel_sign_in(&mut self) {
        self.form.google = None;
        self.form.checking = false;
    }

    fn login_checked(&mut self, account: &Account, login: &Login, result: Result<(), String>) {
        self.form.checking = false;
        if let Err(e) = result {
            self.form.error = Some(e);
            return;
        }
        let Ok(services) = &mut self.services else {
            return;
        };
        let (id, label) = (account.id.clone(), account.label().to_owned());
        let saved = save_account(&services.store, services.secrets.as_ref(), account, login)
            .and_then(|()| {
                services
                    .mailroom
                    .watch(account, &services.store, services.secrets.as_ref())
            });
        if let Err(e) = saved {
            self.form.error = Some(e.to_string());
            return;
        }
        self.statuses.insert(id, Ok(Status::Connecting));
        self.notify(format!("Added {label}"));
        self.form = AccountForm::default();
        self.reload();
    }

    /// Shows the settings page. A message being written stays, and shows again once it closes,
    /// as do an account's settings that were changed but not saved.
    pub(crate) fn open_settings(&mut self) {
        self.form.open = false;
        self.confirm_remove = None;
        self.settings_open = true;
        if !self.account_settings.is_changed() {
            self.account_settings = AccountSettings::default();
        }
    }

    pub(crate) fn close_settings(&mut self) {
        self.settings_open = false;
        self.confirm_remove = None;
        self.cancel_transfer();
    }

    /// Starts exporting every account and setting: the settings ask for a passphrase.
    pub(crate) fn start_export(&mut self) {
        self.transfer = Transfer::export();
    }

    /// Stops exporting or importing, forgetting the passphrase typed. Work under way still
    /// finishes, and says how it went.
    pub(crate) fn cancel_transfer(&mut self) {
        if self.transfer.can_cancel() {
            self.transfer = Transfer::default();
        }
    }

    /// Starts sending every account and setting to another computer: the settings ask for a
    /// passphrase.
    pub(crate) fn start_send(&mut self) {
        self.transfer = Transfer::send();
    }

    /// Starts receiving accounts and settings from another computer: the settings ask for the
    /// code it shows.
    pub(crate) fn start_receive(&mut self) {
        self.transfer = Transfer::receive();
    }

    /// Locks every account and setting with the passphrase typed, then goes online to offer
    /// them to another computer, in the background: the settings show the code it types.
    pub(crate) fn send_profile(&mut self) {
        let (Ok(services), Some(handle)) = (&self.services, &self.handle) else {
            return;
        };
        if self.transfer.busy {
            return;
        }
        let passphrase = match self.transfer.new_passphrase() {
            Ok(passphrase) => passphrase,
            Err(e) => {
                self.transfer.error = Some(e);
                return;
            }
        };
        let profile = match Profile::collect(&services.store, services.secrets.as_ref()) {
            Ok(profile) => profile,
            Err(e) => {
                self.transfer.error = Some(format!("Couldn't send: {e}"));
                return;
            }
        };
        let count = profile.accounts.len();
        let handle = handle.clone();
        let task = services.mailroom.handle().spawn(async move {
            let task = tokio::task::id();
            let sealed = tokio::task::spawn_blocking(move || profile.seal(&passphrase)).await;
            let offer = match sealed {
                Ok(Ok(file)) => Offer::new(file).await.map_err(|e| e.to_string()),
                Ok(Err(e)) => Err(e.to_string()),
                Err(e) => Err(e.to_string()),
            };
            let delivered = match offer {
                Ok(offer) => {
                    let code = offer.code().to_string();
                    handle.update(move |s: &mut Self, _| s.offering(task, code));
                    offer.deliver().await.map_err(|e| e.to_string())
                }
                Err(e) => Err(e),
            };
            handle.update(move |s: &mut Self, _| s.profile_sent(task, delivered, count));
        });
        let t = &mut self.transfer;
        t.busy = true;
        t.error = None;
        t.online = Some(transfer::Online::new(task.abort_handle()));
    }

    /// The profile is locked and offered under `code`: the settings show it.
    fn offering(&mut self, task: tokio::task::Id, code: String) {
        let t = &mut self.transfer;
        if !t.is_online(task) {
            return;
        }
        let (transfer::Step::Send, Some(online)) = (t.step, &mut t.online) else {
            return;
        };
        online.code = Some(code);
        t.step = transfer::Step::Offering;
        t.busy = false;
        t.passphrase = TextEdit::default();
        t.confirm = TextEdit::default();
    }

    /// The other computer took the profile, or sending it failed.
    fn profile_sent(&mut self, task: tokio::task::Id, delivered: Result<(), String>, count: usize) {
        if !self.transfer.is_online(task) {
            return;
        }
        match delivered {
            Ok(()) => {
                self.transfer = Transfer::default();
                let s = if count == 1 { "" } else { "s" };
                self.notify(format!("Sent {count} account{s} to the other computer"));
            }
            Err(e) => {
                self.transfer = Transfer {
                    error: Some(format!("Couldn't send: {e}")),
                    ..Transfer::send()
                };
            }
        }
    }

    /// Dials the computer showing the code typed and downloads its profile, in the background,
    /// then asks for its passphrase.
    pub(crate) fn receive_profile(&mut self) {
        let (Ok(services), Some(handle)) = (&self.services, &self.handle) else {
            return;
        };
        let t = &mut self.transfer;
        if t.busy {
            return;
        }
        let code: Code = match t.code.text.parse() {
            Ok(code) => code,
            Err(e) => {
                t.error = Some(format!("{e}"));
                return;
            }
        };
        let handle = handle.clone();
        let task = services.mailroom.handle().spawn(async move {
            let task = tokio::task::id();
            let received = postden_transfer::receive(&code)
                .await
                .map_err(|e| e.to_string());
            handle.update(move |s: &mut Self, _| s.profile_received(task, received));
        });
        t.busy = true;
        t.error = None;
        t.online = Some(transfer::Online::new(task.abort_handle()));
    }

    fn profile_received(&mut self, task: tokio::task::Id, received: Result<Vec<u8>, String>) {
        let t = &mut self.transfer;
        if t.step != transfer::Step::Receive || !t.is_online(task) {
            return;
        }
        t.online = None;
        t.busy = false;
        match received {
            Ok(file) if is_sealed(&file) => {
                self.transfer = Transfer::unlock("the profile sent".into(), file);
            }
            Ok(_) => t.error = Some("the other computer didn't send a Postden profile".into()),
            Err(e) => t.error = Some(format!("Couldn't receive: {e}")),
        }
    }

    /// Asks where to save every account and setting, locked with the passphrase typed, then
    /// encrypts and writes them there in the background.
    pub(crate) fn export_profile(&mut self, tx: &mut Tx) {
        if self.transfer.busy {
            return;
        }
        let passphrase = match self.transfer.new_passphrase() {
            Ok(passphrase) => passphrase,
            Err(e) => {
                self.transfer.error = Some(e);
                return;
            }
        };
        let options = FileDialogOptions {
            title: "Export accounts and settings".into(),
            file_name: Some(format!(
                "Postden {}.{}",
                Zoned::now().strftime("%Y-%m-%d"),
                transfer::EXTENSION
            )),
            ..FileDialogOptions::default()
        }
        .filter("Postden profile", &[transfer::EXTENSION]);
        tx.save_file(options, move |s: &mut Self, result, _| match result {
            Ok(Some(path)) => s.write_profile(path, passphrase),
            Ok(None) => {}
            Err(e) => s.transfer.error = Some(format!("Couldn't open the file picker: {e}")),
        });
    }

    /// Gathers every account and setting, then encrypts them with `passphrase` and writes them
    /// to `path` in the background: encrypting takes a second on purpose, to slow down guessing.
    fn write_profile(&mut self, path: PathBuf, passphrase: SecretString) {
        let (Ok(services), Some(handle)) = (&self.services, &self.handle) else {
            return;
        };
        let profile = match Profile::collect(&services.store, services.secrets.as_ref()) {
            Ok(profile) => profile,
            Err(e) => {
                self.transfer.error = Some(format!("Couldn't export: {e}"));
                return;
            }
        };
        self.transfer.busy = true;
        self.transfer.error = None;
        let handle = handle.clone();
        services.mailroom.handle().spawn_blocking(move || {
            let written = profile
                .save(&path, &passphrase)
                .map(|()| profile.accounts.len())
                .map_err(|e| e.to_string());
            handle.update(move |s: &mut Self, _| s.profile_written(&path, written));
        });
    }

    fn profile_written(&mut self, path: &std::path::Path, written: Result<usize, String>) {
        self.transfer.busy = false;
        let name = path.file_name().map_or_else(
            || path.display().to_string(),
            |n| n.to_string_lossy().into_owned(),
        );
        match written {
            Ok(count) => {
                self.transfer = Transfer::default();
                let s = if count == 1 { "" } else { "s" };
                self.notify(format!("Exported {count} account{s} to {name}"));
            }
            Err(e) => self.transfer.error = Some(format!("Couldn't export to {name}: {e}")),
        }
    }

    /// Asks for a profile file to import, then for its passphrase.
    pub(crate) fn pick_profile(tx: &mut Tx) {
        let options = FileDialogOptions {
            title: "Import accounts and settings".into(),
            ..FileDialogOptions::default()
        }
        .filter("Postden profile", &[transfer::EXTENSION]);
        tx.open_file(options, |s: &mut Self, result, _| match result {
            Ok(Some(path)) => s.read_profile(&path),
            Ok(None) => {}
            Err(e) => s.notify(format!("Couldn't open the file picker: {e}")),
        });
    }

    /// Reads a profile file, to unlock with its passphrase.
    fn read_profile(&mut self, path: &std::path::Path) {
        let name = path.file_name().map_or_else(
            || path.display().to_string(),
            |n| n.to_string_lossy().into_owned(),
        );
        match std::fs::read(path) {
            Ok(file) if is_sealed(&file) => self.transfer = Transfer::unlock(name, file),
            Ok(_) => self.notify(format!("{name} isn't a Postden profile")),
            Err(e) => self.notify(format!("Couldn't read {name}: {e}")),
        }
    }

    /// Decrypts the picked profile with the passphrase typed, in the background, then shows
    /// what it holds.
    pub(crate) fn unlock_profile(&mut self) {
        let (Ok(services), Some(handle)) = (&self.services, &self.handle) else {
            return;
        };
        let t = &mut self.transfer;
        let (Some((_, file)), false) = (&t.file, t.busy) else {
            return;
        };
        let Some(passphrase) = t.typed_passphrase() else {
            t.error = Some("type the passphrase the profile was exported with".into());
            return;
        };
        let (file, handle) = (file.clone(), handle.clone());
        t.busy = true;
        t.error = None;
        services.mailroom.handle().spawn_blocking(move || {
            let opened = Profile::open(&file, &passphrase).map_err(|e| e.to_string());
            handle.update(move |s: &mut Self, _| s.profile_opened(opened));
        });
    }

    fn profile_opened(&mut self, opened: Result<Profile, String>) {
        self.transfer.busy = false;
        if self.transfer.step != transfer::Step::Unlock {
            return;
        }
        let Ok(services) = &self.services else { return };
        match opened.and_then(|p| {
            let existing = p.existing(&services.store).map_err(|e| e.to_string())?;
            Ok((p, existing))
        }) {
            Ok((profile, existing)) => self.transfer = Transfer::review(profile, existing),
            Err(e) => self.transfer.error = Some(e),
        }
    }

    /// Sets up the reviewed profile's accounts and, if chosen, its settings, then connects the
    /// accounts it set up.
    pub(crate) fn import_profile(&mut self) {
        let Ok(services) = &mut self.services else {
            return;
        };
        let Some(profile) = &self.transfer.profile else {
            return;
        };
        let options = self.transfer.options;
        // Where each account's mailboxes were, to tell which ones a replaced account left.
        let mailboxes_before: HashMap<String, (String, u16, String)> = self
            .accounts
            .iter()
            .map(|a| (a.id.clone(), imap_owner(a)))
            .collect();
        // Stopped before anything's replaced, so their syncs can't write what they fetched from
        // the old servers over the new ones' mailboxes; started again below.
        let replacing: Vec<String> = profile
            .accounts
            .iter()
            .map(|a| a.id.clone())
            .filter(|id| options.replace_accounts && mailboxes_before.contains_key(id))
            .collect();
        for id in &replacing {
            services.mailroom.unwatch(id);
        }
        let imported = profile.import(&mut services.store, services.secrets.as_ref(), options);
        let mut restart: Vec<String> = replacing;
        if let Ok(imported) = &imported {
            restart.extend(imported.added.iter().cloned());
        }
        let mut moved = Vec::new();
        for id in &restart {
            // A replaced account may have new servers: if it can't reconnect, it mustn't keep
            // checking and sending mail with the old ones.
            let watched = services
                .store
                .account(id)
                .map_err(postden_app::Error::from)
                .and_then(|a| {
                    let a =
                        a.ok_or_else(|| postden_app::Error::Invalid(format!("{id} is gone")))?;
                    if mailboxes_before
                        .get(id)
                        .is_some_and(|before| *before != imap_owner(&a))
                    {
                        moved.push(id.clone());
                    }
                    services
                        .mailroom
                        .watch(&a, &services.store, services.secrets.as_ref())
                });
            let status = watched
                .map(|()| Status::Connecting)
                .map_err(|e| e.to_string());
            self.statuses.insert(id.clone(), status);
        }
        let imported = match imported {
            Ok(imported) => imported,
            Err(e) => {
                // Nothing was imported: the profile can be imported again as it is.
                self.transfer.error = Some(format!("Couldn't import: {e}"));
                return;
            }
        };
        let settings = options
            .settings
            .then(|| Settings::load(&services.store))
            .transpose();
        self.transfer = Transfer::default();
        for id in &moved {
            self.forget_mail_of(id);
        }
        self.reload();
        match settings {
            Ok(Some(settings)) => self.use_settings(settings),
            Ok(None) => {}
            Err(e) => self.notify(format!("Couldn't read the imported settings: {e}")),
        }
        self.notify(imported_notice(&imported));
    }

    /// Switches to `settings`, as imported, doing what changing each by hand does.
    fn use_settings(&mut self, settings: Settings) {
        self.set_theme(settings.theme);
        self.set_poll_interval(settings.poll_interval);
        self.set_prefetch(settings.prefetch);
        self.set_notify_new_mail(settings.notify_new_mail);
        self.set_keep_running(settings.keep_running);
        self.set_check_updates(settings.check_updates);
    }

    /// Shows an account's settings: what it's called, the name its mail goes under, and its
    /// signature.
    pub(crate) fn open_account_settings(&mut self, id: &str) {
        let Some(account) = self.accounts.iter().find(|a| a.id == id).cloned() else {
            return;
        };
        if !self.account_settings.open || self.account_settings.account != id {
            let signature = self.signature_of(id);
            self.account_settings =
                AccountSettings::new(&account, signature.as_ref(), self.images.as_ref());
        }
        self.form.open = false;
        self.confirm_remove = None;
        self.settings_open = true;
    }

    /// Back to the settings page, dropping what wasn't saved.
    pub(crate) fn close_account_settings(&mut self) {
        self.account_settings = AccountSettings::default();
    }

    /// Saves the account's settings, then goes back to the settings page.
    pub(crate) fn save_account_settings(&mut self) {
        let (Ok(services), page) = (&mut self.services, &mut self.account_settings) else {
            return;
        };
        if !page.open {
            return;
        }
        let id = page.account.clone();
        let saved = services
            .store
            .set_names(&id, page.label().as_deref(), page.name().as_deref())
            .map_err(postden_app::Error::from)
            .and_then(|found| {
                if found {
                    signature::save_signature(&mut services.store, &id, page.built().as_ref())
                } else {
                    Err(postden_app::Error::Invalid(format!(
                        "{id} is no longer set up"
                    )))
                }
            });
        if let Err(e) = saved {
            page.error = Some(format!("Couldn't save: {e}"));
            return;
        }
        self.account_settings = AccountSettings::default();
        self.reload();
        let label = self.label(&id).to_owned();
        self.notify(format!("Saved {label}"));
    }

    /// Asks for image files to add to the signature being edited.
    pub(crate) fn pick_signature_image(tx: &mut Tx) {
        let options = FileDialogOptions {
            title: "Add an image to the signature".into(),
            ..FileDialogOptions::default()
        }
        .filter("PNG, JPEG or GIF image", &IMAGE_EXTENSIONS);
        tx.open_file(options, |s: &mut Self, result, _| match result {
            Ok(Some(path)) => s.add_signature_images(&[path]),
            Ok(None) => {}
            Err(e) => {
                s.account_settings.error = Some(format!("Couldn't open the file picker: {e}"));
            }
        });
    }

    /// Adds image files to the signature being edited, after its text or at the source's caret.
    pub(crate) fn add_signature_images(&mut self, paths: &[PathBuf]) {
        let page = &mut self.account_settings;
        page.dropping = false;
        for path in paths {
            match signature::read_image(path) {
                Ok(image) => page.add_image(image),
                Err(e) => page.error = Some(e.to_string()),
            }
        }
        page.refresh(self.images.as_ref());
    }

    /// Changes the signature being edited with `edit`, then draws its preview again.
    pub(crate) fn edit_signature(&mut self, edit: impl FnOnce(&mut AccountSettings)) {
        if self.account_settings.open {
            edit(&mut self.account_settings);
            self.account_settings.refresh(self.images.as_ref());
        }
    }

    /// Switches the signature being edited between the visual editor and its HTML source.
    pub(crate) fn set_signature_mode(&mut self, mode: Mode) {
        self.edit_signature(|page| page.set_mode(mode));
    }

    /// Whether to draw the window dark, given whether the system is.
    pub(crate) const fn is_dark(&self, system_dark: bool) -> bool {
        self.settings.theme.is_dark(system_dark)
    }

    pub(crate) fn set_theme(&mut self, theme: Theme) {
        if self.settings.theme != theme {
            self.settings.theme = theme;
            self.save_settings();
        }
    }

    /// Downloads the newest `count` messages of each inbox ahead of their opening, or, with 0,
    /// each message only once it's opened.
    pub(crate) fn set_prefetch(&mut self, count: usize) {
        let Ok(services) = &self.services else {
            return;
        };
        if services.mailroom.prefetch() == count {
            return;
        }
        services.mailroom.set_prefetch(count);
        self.settings.prefetch = count;
        self.save_settings();
    }

    /// Checks mail every `every` on servers that can't push it, reconnecting every account.
    pub(crate) fn set_poll_interval(&mut self, every: Duration) {
        let Ok(services) = &mut self.services else {
            return;
        };
        if services.mailroom.poll_interval() == every {
            return;
        }
        let restarted =
            services
                .mailroom
                .set_poll_interval(every, &services.store, services.secrets.as_ref());
        self.settings.poll_interval = every;
        match restarted {
            Ok(failed) => {
                for account in &self.accounts {
                    self.statuses
                        .insert(account.id.clone(), Ok(Status::Connecting));
                }
                for (account, error) in failed {
                    self.statuses.insert(account, Err(error.to_string()));
                }
            }
            Err(e) => self.notify(e.to_string()),
        }
        self.save_settings();
    }

    /// The window closed: the app quits, unless the setting says to keep running, the app
    /// started (otherwise the setting wasn't read, and there's no mail to keep checking), and
    /// something brings the window back: the tray icon, or the Dock on macOS.
    pub(crate) fn window_closed(&mut self, tx: &mut Tx) {
        reopened();
        let way_back = cfg!(target_os = "macos") || self.tray.is_some();
        if self.settings.keep_running && self.services.is_ok() && way_back {
            #[cfg(target_os = "linux")]
            self.quit_without_tray_host();
        } else {
            tx.quit();
        }
    }

    /// Quits if no tray host shows the tray icon, as on GNOME without an extension: Rune makes the
    /// icon either way. Asks off the UI thread, so the window is already gone when it quits, and
    /// stays if the window came back in the meantime.
    #[cfg(target_os = "linux")]
    fn quit_without_tray_host(&self) {
        let (Ok(services), Some(handle)) = (&self.services, &self.handle) else {
            return;
        };
        let (handle, id) = (handle.clone(), CLOSES.load(Ordering::Acquire));
        services.mailroom.handle().spawn(async move {
            if !crate::tray::host_registered().await {
                handle.update(move |_: &mut Self, tx| {
                    if CLOSES.load(Ordering::Acquire) == id {
                        tx.quit();
                    }
                });
            }
        });
    }

    /// The tray icon started: closing the window keeps the app running from now on.
    #[cfg_attr(
        target_os = "macos",
        expect(
            clippy::missing_const_for_fn,
            reason = "Rune's tray only drops its icon off macOS"
        )
    )]
    pub(crate) fn attach_tray(&mut self, tray: Tray) {
        self.tray = Some(tray);
    }

    /// Shows the window and brings it to the front. Returns whether it could ask to.
    pub(crate) fn bring_forward(&self) -> bool {
        self.handle.as_ref().is_some_and(|handle| {
            handle.open();
            true
        })
    }

    /// Announces new mail with system notifications, or stops.
    pub(crate) fn set_notify_new_mail(&mut self, on: bool) {
        if self.settings.notify_new_mail == on {
            return;
        }
        self.settings.notify_new_mail = on;
        self.save_settings();
    }

    /// Keeps the app running once its window closes, or has closing it quit.
    pub(crate) fn set_keep_running(&mut self, on: bool) {
        if self.settings.keep_running == on {
            return;
        }
        self.settings.keep_running = on;
        self.save_settings();
    }

    /// Turns automatic update checks on, checking now, or off.
    pub(crate) fn set_check_updates(&mut self, on: bool) {
        if self.settings.check_updates == on {
            return;
        }
        self.settings.check_updates = on;
        // Stops any timer already running; turning checks on starts a fresh one.
        self.updater.timer += 1;
        if on {
            self.check_for_updates(false);
        }
        self.save_settings();
    }

    /// Checks for updates after `delay`, then every [`UPDATE_CHECK_EVERY`], while automatic checks
    /// are on. Replaces any check scheduled before.
    fn schedule_update_check(&mut self, delay: Duration) {
        let Some(handle) = &self.handle else { return };
        self.updater.timer += 1;
        let (handle, id) = (handle.clone(), self.updater.timer);
        drop(rune::task::spawn_after("update-check", delay, move || {
            handle.update(move |s: &mut Self, _| {
                if s.updater.timer == id && s.settings.check_updates {
                    s.check_for_updates(false);
                }
            });
        }));
    }

    /// Asks GitHub for a newer release. `manual` when the user asked, which always shows what
    /// was found; automatic checks announce a release once, unless it was skipped.
    pub(crate) fn check_for_updates(&mut self, manual: bool) {
        let (Ok(services), Some(handle)) = (&self.services, &self.handle) else {
            return;
        };
        if !self.updater.can_check() {
            // Asked during an automatic check: that one's result is shown.
            if manual && let Phase::Checking { manual: m } = &mut self.updater.phase {
                *m = true;
            }
            return;
        }
        self.updater.phase = Phase::Checking { manual };
        let handle = handle.clone();
        services.mailroom.handle().spawn(async move {
            let result = postden_update::check(update::current())
                .await
                .map_err(|e| e.to_string());
            handle.update(move |s: &mut Self, _| s.update_checked(result));
        });
    }

    fn update_checked(&mut self, result: Result<Option<postden_update::Available>, String>) {
        let manual = matches!(self.updater.phase, Phase::Checking { manual: true });
        let announce = self
            .updater
            .checked(result, self.settings.skipped_update, Zoned::now());
        if announce {
            // Not over a message being written or an account being added, unless asked for.
            if manual || !(self.compose.open || self.form.open) {
                self.updater.dialog_open = true;
            } else if let Some(a) = &self.updater.available {
                self.notify(format!(
                    "Postden {} is available. See Settings to update.",
                    a.latest.version
                ));
            }
        }
        // A day after the last check, whichever started it; this replaces any timer running.
        if self.settings.check_updates {
            self.schedule_update_check(UPDATE_CHECK_EVERY);
        }
    }

    /// Shows the announcement of the update found, with its changelog.
    pub(crate) const fn open_update(&mut self) {
        if self.updater.available.is_some() {
            self.updater.dialog_open = true;
        }
    }

    /// Closes the announcement; the update stays available in the settings.
    pub(crate) const fn close_update(&mut self) {
        self.updater.dialog_open = false;
    }

    /// Stops announcing the update found; a newer one is announced again.
    pub(crate) fn skip_update(&mut self) {
        let Some(available) = &self.updater.available else {
            return;
        };
        self.settings.skipped_update = Some(available.latest.version);
        self.updater.dialog_open = false;
        self.save_settings();
    }

    /// Downloads and installs the update found, in the background.
    pub(crate) fn install_update(&mut self) {
        let (Ok(services), Some(handle)) = (&self.services, &self.handle) else {
            return;
        };
        if !self.updater.can_install() {
            return;
        }
        let Some(available) = &self.updater.available else {
            return;
        };
        let (release, install) = (available.latest.clone(), self.updater.install.clone());
        self.updater.phase = Phase::Downloading(None);
        let (progress, handle) = (handle.clone(), handle.clone());
        services.mailroom.handle().spawn(async move {
            let result = postden_update::install(&release, install, move |percent| {
                progress.update(move |s: &mut Self, _| {
                    if matches!(s.updater.phase, Phase::Downloading(_)) {
                        s.updater.phase = Phase::Downloading(Some(percent));
                    }
                });
            })
            .await
            .map_err(|e| e.to_string());
            handle.update(move |s: &mut Self, _| {
                s.updater.phase = match result {
                    Ok(()) => Phase::Ready,
                    Err(error) => Phase::InstallFailed(error),
                };
            });
        });
    }

    /// Restarts into the installed update.
    pub(crate) fn restart_to_update(&mut self, tx: &mut Tx) {
        if self.updater.phase == Phase::Ready {
            tx.relaunch();
        }
    }

    /// Opens the update's release page, for those who update by hand.
    pub(crate) fn open_release_page(&mut self, tx: &mut Tx) {
        let page = self.updater.available.as_ref().map_or_else(
            || {
                format!(
                    "https://github.com/{}/releases/latest",
                    postden_update::REPOSITORY
                )
            },
            |a| a.latest.page.clone(),
        );
        self.open_link(&page, tx);
    }

    fn save_settings(&mut self) {
        let Ok(services) = &self.services else { return };
        if let Err(e) = self.settings.save(&services.store) {
            self.notify(format!("couldn't save settings: {e}"));
        }
    }

    /// Removes an account on the second press of its Remove button.
    pub(crate) fn remove(&mut self, id: &str) {
        if self.confirm_remove.as_deref() != Some(id) {
            self.confirm_remove = Some(id.to_owned());
            return;
        }
        self.confirm_remove = None;
        let Ok(services) = &mut self.services else {
            return;
        };
        services.mailroom.unwatch(id);
        let notice = match remove_account(&services.store, services.secrets.as_ref(), id) {
            Ok(_) => format!("Removed {id}"),
            Err(e) => format!("Removed {id}, but: {e}"),
        };
        self.notify(notice);
        self.statuses.remove(id);
        if self.account_settings.account == id {
            self.account_settings = AccountSettings::default();
        }
        self.forget_mail_of(id);
        self.reload();
    }

    /// Stops showing account `id`'s mail, removed or now on another server or user, whose UIDs
    /// name other messages: its open message and menus, and what a server search found.
    fn forget_mail_of(&mut self, id: &str) {
        if self.showing.account() == Some(id) {
            self.showing = Showing::All(Role::Inbox);
        }
        // What the server found may be its mail.
        self.search.forget_server();
        self.search.forget_gone(|account, _| account == id);
        if self
            .reader
            .as_ref()
            .is_some_and(|r| r.message.account == id)
        {
            self.reader = None;
        }
        if self
            .message_menu
            .as_ref()
            .is_some_and(|m| m.message.0 == id)
        {
            self.message_menu = None;
        }
        if self
            .mailbox_menu
            .as_ref()
            .is_some_and(|m| m.mailbox.0 == id)
        {
            self.mailbox_menu = None;
        }
    }
}
