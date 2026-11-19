//! The window's state, and everything that changes it.

use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};

use jiff::{Timestamp, Zoned};
use postden_app::{
    Autosort, Draft, Imported, Login, Mailroom, Passwords, Profile, Settings, Sorted, Sorter, Tags,
    Theme, Update, check_logins, contacts, eml_file_name, finish_google_sign_in, import, is_sealed,
    password_key, remove_account, save_account, signature, smtp_password_key, start_google_sign_in,
};
use postden_core::account::{Account, Signature};
use postden_core::message::{Address, Body, Envelope, Mailbox, Role};
use postden_core::secrets::{SecretStore, SecretString};
use postden_mail::imap::{Status, WatchConfig, has_kept_name};
use postden_secrets::Backend;
use postden_store::{Scope, Store, StoredMessage};
use postden_transfer::{Code, Offer};
use postden_update::Track;
use rune::{
    AppHandle, FileDialogOptions, ImageCache, PointerPress, Taskbar, TextEdit, Tray, Tx,
    WindowHandle,
};
use rune_kit::shell::PaneState;

use crate::account_settings::{AccountSettings, IMAGE_EXTENSIONS};
use crate::commands::{self, Command, Palette, PaletteMode, Shortcut};
use crate::compose::{ComposeForm, Mode, Recipients};
use crate::form::{AccountForm, AppAccounts, GoogleSignIn, Imports, Step, Unreadable, is_set_up};
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
    /// Where the store's database is, for the sorter's connection to it.
    database: PathBuf,
    /// Sorting the inbox, while the settings say to.
    sorter: Option<Sorter>,
}

/// A message open for reading.
pub(crate) struct Reader {
    /// The message as listed. Its `seen` follows the list.
    pub(crate) message: StoredMessage,
    /// The downloaded body, or why it couldn't be downloaded. `None` while downloading.
    pub(crate) body: Option<Result<Body, String>>,
    /// The body's HTML as Markdown, to show, converted once when it arrives.
    pub(crate) markdown: Option<String>,
    /// The UIDVALIDITY its mailbox was cached under when it opened: a download made under
    /// another is of another message.
    uid_validity: Option<u32>,
    /// Whether it was read before it opened, shown read since: as it shows again when the server
    /// won't mark it.
    seen_before: bool,
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
    /// Its "Snooze" submenu is open.
    pub(crate) snooze_open: bool,
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

/// The menu of an account in the sidebar, opened by right-clicking it.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct AccountMenu {
    pub(crate) account: String,
    /// Where it was right-clicked, which the menu opens at.
    pub(crate) point: PointerPress,
}

/// The dialog naming a mailbox to create on an account, opened from the account's menu.
#[derive(Debug, Clone, Default, PartialEq)]
pub(crate) struct NewMailbox {
    pub(crate) open: bool,
    pub(crate) account: String,
    pub(crate) name: TextEdit,
    /// The name asked for, while the server hasn't answered.
    pub(crate) creating: Option<String>,
    /// Why the server refused the last name asked for.
    pub(crate) error: Option<String>,
}

/// The dialog confirming a mailbox is to be deleted, with the mail in it, opened from the
/// mailbox's menu.
#[derive(Debug, Clone, Default, PartialEq)]
pub(crate) struct DeleteMailbox {
    /// The mailbox to delete, while the dialog is open.
    pub(crate) mailbox: Option<MailboxKey>,
    /// Its UIDVALIDITY when the dialog opened, if it was synced: the server deletes no other
    /// mailbox under its name since.
    uid_validity: Option<u32>,
    /// Why it can't be deleted: it wasn't synced, or the server refused. The dialog then says so,
    /// and only closes; opened again, it's on the mailbox as it is then.
    pub(crate) error: Option<String>,
}

/// A mailbox asked to be deleted, whose server hasn't answered.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Deleting {
    /// The number it was asked with.
    id: u64,
    /// The UIDVALIDITY it was asked under: another mailbox made under its name since has another.
    uid_validity: u32,
    /// The name it showed with, which its answer says it by: listed again meanwhile, it may be
    /// gone from what's listed.
    label: String,
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

/// What a notice adds when Z undoes what it tells of.
const fn undo_hint(undoable: bool) -> &'static str {
    if undoable { " · Z to undo" } else { "" }
}

/// Every account and setting, to export or send, and what sorting mail learned if
/// `with_sorting`.
fn collect_profile(services: &Services, with_sorting: bool) -> postden_app::Result<Profile> {
    let profile = Profile::collect(&services.store, services.secrets.as_ref())?;
    if with_sorting {
        profile.with_sorting(&services.store)
    } else {
        Ok(profile)
    }
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
    match &imported.sorting {
        Some(Ok(())) => notice.push_str("; sorting uses what it learned there"),
        Some(Err(e)) => notice.extend(["; couldn't import what sorting learned: ", e]),
        None => {}
    }
    let failed = imported
        .failed
        .iter()
        .map(|(id, error)| format!("; couldn't import {id}: {error}"));
    notice.extend(failed);
    notice
}

/// How many messages autosort moves at once in each account.
const AUTOSORT_AT_ONCE: usize = 3;

/// Starts sorting the mail cached in `database`: what it says of the inbox shows as it's done.
fn start_sorter(handle: &AppHandle<Mail>, database: &std::path::Path) -> Result<Sorter, String> {
    let handle = handle.clone();
    Sorter::start(database, move |generation, tags| {
        handle.update(move |s: &mut Mail, _| s.sorted(generation, tags));
    })
    .map_err(|e| format!("couldn't start sorting mail: {e}"))
}

/// Whether an update changes the mail cached, or what's known of it, so what sorting learns from.
/// Not a snoozed message woken: what it leaves changes no tag, and the inbox copy it gets back
/// comes with its own update, before which Gmail's All Mail copy would seem archived.
const fn changes_mail(update: &Update) -> bool {
    matches!(
        update,
        Update::Mailboxes { .. }
            | Update::Mail { .. }
            | Update::FlagsChanged { .. }
            | Update::Older { .. }
            | Update::Unread {
                result: Ok(1..),
                ..
            }
            | Update::Opened { .. }
            | Update::Seen { .. }
            | Update::Flagged { .. }
            | Update::AllSeen { .. }
            | Update::Moved { .. }
            | Update::Snoozed { .. }
            | Update::Restored { .. }
            | Update::Cleared(_)
    )
}

fn is_message(m: &StoredMessage, account: &str, mailbox: &str, uid: u32) -> bool {
    m.account == account && m.mailbox == mailbox && m.envelope.uid == uid
}

/// Whether the list shows `m`: any message, or, showing only unread mail, an unread one or one it
/// listed already while a message is open, so one read there doesn't leave it from under J and
/// K; but not one put away, read as shown.
fn lists(
    m: &StoredMessage,
    unread_only: bool,
    still_listed: &HashSet<MessageKey>,
    put_away: &HashSet<MessageKey>,
) -> bool {
    if !unread_only {
        return true;
    }
    if still_listed.is_empty() && put_away.is_empty() {
        return !m.envelope.seen;
    }
    let key = message_key(m);
    !put_away.contains(&key) && (!m.envelope.seen || still_listed.contains(&key))
}

/// A part of the settings page, picked with the tabs at its top.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(crate) enum SettingsSection {
    /// How Postden looks and behaves: its theme, notifications, closing and updates.
    #[default]
    General,
    /// The accounts set up, and moving them to another computer.
    Accounts,
    /// How often mail is checked, and how much is downloaded ahead.
    Advanced,
}

impl SettingsSection {
    pub(crate) const ALL: [Self; 3] = [Self::General, Self::Accounts, Self::Advanced];

    pub(crate) const fn label(self) -> &'static str {
        match self {
            Self::General => "General",
            Self::Accounts => "Accounts",
            Self::Advanced => "Advanced",
        }
    }
}

/// What the message list shows.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
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

/// Counts the add-account form's openings, for [`Imports::session`].
static FORM_OPENINGS: AtomicU64 = AtomicU64::new(0);

/// The window is coming back: what its last close started no longer applies. Any thread.
pub(crate) fn reopened() {
    CLOSES.fetch_add(1, Ordering::AcqRel);
}

/// How a message is answered: a reply to its sender or everyone, or a forward.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Answer {
    /// A reply to the sender, or whoever the message asks replies to go to.
    Reply,
    /// A reply to everyone the message went to.
    ReplyAll,
    /// The message passed on to whoever it's sent to.
    Forward,
}

/// A move asked for a message while a read mark or star asked for it was still unanswered: it's
/// made once that's answered, as the change would be lost with the UID it was asked under.
#[derive(Debug, Clone)]
enum Deferred {
    /// To the account's mailbox.
    Move(String),
    /// Snoozed until then.
    Snooze(Box<StoredMessage>, Timestamp),
}

/// What Z undoes: the last thing done to a message from the window.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Undo {
    /// The message `from` names was moved, or snoozed until `snoozed`, to its account's mailbox
    /// `to`, where it's found again by its Message-ID. It can be moved back once the server
    /// moved it: until then, `done` isn't set.
    Moved {
        from: MessageKey,
        to: String,
        message_id: String,
        snoozed: Option<i64>,
        done: bool,
    },
    /// The message was marked read or unread: `seen` is what it was. It's marked back once the
    /// server marked it (`done`), and only while its mailbox is still under `uid_validity`, as its
    /// UID names another message otherwise.
    Seen {
        message: MessageKey,
        uid_validity: u32,
        seen: bool,
        done: bool,
    },
    /// The message was starred or unstarred: `flagged` is what it was. As [`Undo::Seen`].
    Flagged {
        message: MessageKey,
        uid_validity: u32,
        flagged: bool,
        done: bool,
    },
}

impl Undo {
    /// The account of the message it's about.
    fn account(&self) -> &str {
        match self {
            Self::Moved { from: key, .. }
            | Self::Seen { message: key, .. }
            | Self::Flagged { message: key, .. } => &key.0,
        }
    }
}

/// A message being moved back by undoing, to `mailbox`, found by its Message-ID.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Restoring {
    account: String,
    mailbox: String,
    message_id: String,
}

/// How long a notice shows before it closes on its own.
const NOTICE_SECS: u64 = 6;

/// How long after starting the first automatic update check runs, out of the way of the first sync.
const FIRST_UPDATE_CHECK: Duration = Duration::from_secs(10);
/// How often updates are checked for while the app runs.
const UPDATE_CHECK_EVERY: Duration = Duration::from_hours(24);
/// How soon after G its second key must come.
const GO_WITHIN: Duration = Duration::from_secs(2);
/// How often snoozed messages are checked for being due, so one comes back within this of its time.
const WAKE_EVERY: Duration = Duration::from_secs(30);

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
    /// What the list shows while it shows only unread mail and a message is open: read ones
    /// among them stay listed while it's open. See [`lists`].
    still_listed: HashSet<MessageKey>,
    /// Messages shown read and gone back from while the list shows only unread mail: they leave
    /// it at once, though the store says they're unread until the server agrees, wherever the list
    /// goes meanwhile. Each comes back once no read mark for it is on its way, unless it's read.
    put_away: HashSet<MessageKey>,
    /// Read marks still on their way for a mailbox that started over since: their UIDs may name
    /// other messages now, so they don't put those away. Forgotten once answered, or once a read
    /// mark is asked for the message anew.
    stale_marks: HashSet<MessageKey>,
    /// The synced mailboxes whose mail the list shows.
    listed: Vec<MailboxKey>,
    /// Mailboxes whose older mail is being fetched.
    fetching_older: HashSet<MailboxKey>,
    /// Mailboxes with nothing older left on the server than what's cached.
    no_older: HashSet<MailboxKey>,
    /// Each account's mailboxes as the server last listed them, by account id.
    pub(crate) mailboxes: HashMap<String, Vec<Mailbox>>,
    /// Unread messages, by account id and mailbox name: those cached, and those the server last
    /// had unread that aren't.
    unread: HashMap<MailboxKey, usize>,
    /// The mailboxes whose unread mail was searched for on the server: their count in `unread`
    /// is known, even when it's none. The others' may only not be counted yet.
    counted: HashSet<MailboxKey>,
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
    /// Accounts whose group of mailboxes with nothing unread is open in the sidebar, while the
    /// settings say to group them.
    read_open: HashSet<String>,
    /// Whether the mailbox shown, and the one whose menu is open, sat in their account's group of
    /// mailboxes with nothing unread when picked: they stay there, or out of it, until left.
    held: HashMap<MailboxKey, bool>,
    /// The toolbar's ⋯ menu is open.
    pub(crate) actions_open: bool,
    /// The menu of a message in the list, while it's open.
    pub(crate) message_menu: Option<MessageMenu>,
    /// The menu of a mailbox in the sidebar, while it's open.
    pub(crate) mailbox_menu: Option<MailboxMenu>,
    /// The menu of an account in the sidebar, while it's open.
    pub(crate) account_menu: Option<AccountMenu>,
    /// The dialog creating a mailbox.
    pub(crate) new_mailbox: NewMailbox,
    pub(crate) delete_mailbox: DeleteMailbox,
    /// Mailboxes asked to be deleted, whose server hasn't answered, whether their dialog is open
    /// or not, each with the number it was asked with.
    deleting_mailboxes: HashMap<MailboxKey, Deleting>,
    /// The number the next mailbox asked to be deleted is asked with.
    next_delete: u64,
    /// A reply asked for before its message was downloaded: the message, and whether to everyone.
    /// It starts once the message opens.
    pending_reply: Option<(MessageKey, Answer)>,
    /// Messages being moved, left out of the list until the server answers.
    moving: HashSet<MessageKey>,
    /// Mailboxes being marked read, as asked with [`Mail::mark_all_seen`], until the server answers.
    marking_seen: HashSet<MailboxKey>,
    /// Mailboxes being refreshed, as asked with [`Mail::refresh_mailbox`], with the number of the
    /// refresh asked for: each shows it's busy until a sync answering it ends.
    refreshing: HashMap<MailboxKey, u64>,
    /// Accounts whose mailboxes other than the inbox are refreshed with [`Mail::refresh_all`],
    /// with the refresh's number, until the round answering it ends: what its listing finds
    /// shows it's busy too.
    refreshing_all: HashMap<String, u64>,
    /// The message being written, shown over the reader and the list.
    pub(crate) compose: ComposeForm,
    /// The app's preferences, as last saved.
    pub(crate) settings: Settings,
    /// The settings page is shown over everything else in the main pane.
    pub(crate) settings_open: bool,
    /// The part of the settings page shown. It stays as picked when the page closes.
    pub(crate) settings_section: SettingsSection,
    /// One account's settings, shown in place of the settings page while it's open.
    pub(crate) account_settings: AccountSettings,
    /// Decodes the images shown, such as a signature's. `None` before the app starts.
    pub(crate) images: Option<ImageCache>,
    /// Checking for, announcing and installing new releases.
    pub(crate) updater: Updater,
    /// Exporting or importing every account and setting, in the settings.
    pub(crate) transfer: Transfer,
    /// The command palette (⌘K).
    pub(crate) palette: Palette,
    /// The message under the list's cursor, which J and K move.
    pub(crate) cursor: Option<MessageKey>,
    /// When G was pressed: the next key, soon after, says where to go.
    go_pressed: Option<Instant>,
    /// What Z undoes, if anything.
    pub(crate) undo: Option<Undo>,
    /// What sorting last said of each inbox message, by message: where it seems to belong, and
    /// whether it seems important. Empty while mail isn't sorted.
    tags: HashMap<MessageKey, Tags>,
    /// The UIDVALIDITY each mailbox with tags was sorted under, by account and mailbox: a
    /// suggested move is made only while it holds, as the tags' UIDs name their messages only
    /// then.
    tagged_under: HashMap<(String, String), u32>,
    /// The mailboxes inbox mail is moved to by itself once sorting says it goes there, by
    /// account and name.
    pub(crate) autosort_to: HashSet<(String, String)>,
    /// The messages autosort is moving, by message, each with its Message-ID, until the server
    /// answers.
    autosorting: HashMap<MessageKey, String>,
    /// The message open, or whose menu is open, that autosort left alone, if it did: it's
    /// sorted again once that's closed.
    autosort_waits_for: Option<MessageKey>,
    /// A message being moved back by undoing: the list's cursor goes to it once it's listed
    /// there again, unless another mailbox is shown meanwhile.
    restoring: Option<Restoring>,
    /// Messages being moved back by undoing, which aren't announced as new mail when they arrive.
    returning: Vec<Restoring>,
    /// The moves being undone, which Z can undo again if moving the message back fails.
    undoing: Vec<Undo>,
    /// Messages opened while a mark to unread asked for them was on its way, as after U: they're
    /// marked read once nothing asked for them is unanswered, under the UIDVALIDITY they were
    /// opened under, whether still open or not.
    read_later: HashMap<MessageKey, Option<u32>>,
    /// Stars asked of the server and not answered yet, as asked: shown over what the cache
    /// still says, which changes only once the server agrees.
    starring: HashMap<MessageKey, bool>,
    /// Messages whose last read mark asked of the server, still unanswered, was to unread.
    unreading: HashSet<MessageKey>,
    /// Moves waiting for a read mark or star to their message to be answered, by message, with
    /// the UIDVALIDITY its mailbox was cached under then.
    deferred: HashMap<MessageKey, (Deferred, Option<u32>)>,
    /// Messages being moved back by undoing, by the mailbox they're leaving: they aren't changed
    /// meanwhile, as the change would be lost with their UID there.
    leaving: Vec<Restoring>,
    /// How many read marks (`false`) or stars (`true`) asked of the server for a message it
    /// hasn't answered yet: Z waits for them all.
    unanswered: HashMap<(MessageKey, bool), usize>,
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
            still_listed: HashSet::new(),
            put_away: HashSet::new(),
            stale_marks: HashSet::new(),
            listed: Vec::new(),
            fetching_older: HashSet::new(),
            no_older: HashSet::new(),
            showing: Showing::All(Role::Inbox),
            mailboxes: HashMap::new(),
            unread: HashMap::new(),
            counted: HashSet::new(),
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
            account_menu: None,
            new_mailbox: NewMailbox::default(),
            delete_mailbox: DeleteMailbox::default(),
            deleting_mailboxes: HashMap::new(),
            next_delete: 0,
            pending_reply: None,
            moving: HashSet::new(),
            marking_seen: HashSet::new(),
            refreshing: HashMap::new(),
            refreshing_all: HashMap::new(),
            collapsed: HashSet::new(),
            read_open: HashSet::new(),
            held: HashMap::new(),
            settings: Settings::default(),
            settings_open: false,
            settings_section: SettingsSection::default(),
            account_settings: AccountSettings::default(),
            images: None,
            updater: Updater::default(),
            transfer: Transfer::default(),
            palette: Palette::default(),
            cursor: None,
            go_pressed: None,
            undo: None,
            tags: HashMap::new(),
            tagged_under: HashMap::new(),
            autosort_to: HashSet::new(),
            autosorting: HashMap::new(),
            autosort_waits_for: None,
            restoring: None,
            returning: Vec::new(),
            undoing: Vec::new(),
            leaving: Vec::new(),
            deferred: HashMap::new(),
            read_later: HashMap::new(),
            unreading: HashSet::new(),
            starring: HashMap::new(),
            unanswered: HashMap::new(),
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
                mail.load_autosort();
                mail.reload();
                mail.sort_mail();
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
                if mail.accounts.is_empty() {
                    mail.open_form();
                }
                // What came due while the app was closed comes back now, then the rest on time.
                mail.wake_snoozed();
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
        // What autosort was moving when the app last quit was never answered: it's left where
        // it is.
        store.settle_autosorts().map_err(|e| e.to_string())?;
        let config = WatchConfig {
            poll_interval: settings.poll_interval,
            ..WatchConfig::default()
        };
        let notify = handle.clone();
        let mailroom = Mailroom::start(
            &database,
            config,
            settings.prefetch,
            settings.keep_opened,
            move |update| {
                notify.update(move |s: &mut Self, tx| {
                    // Mail coming in can add rows above the palette's highlight (Reply, once a
                    // message downloads), which the list's scroll anchor doesn't follow.
                    let before = s.palette_highlight();
                    // What sorting learns from changed: it learns again.
                    if changes_mail(&update) {
                        s.sort_mail();
                    }
                    s.apply(update, tx);
                    if let (Some((was, _)), Some((row, key))) = (before, s.palette_highlight())
                        && was != row
                    {
                        tx.scroll_into_view(key);
                    }
                });
            },
        )
        .map_err(|e| e.to_string())?;
        let sorter = settings
            .sort_mail
            .then(|| start_sorter(handle, &database))
            .transpose()?;
        let services = Services {
            store,
            secrets,
            read_only: matches!(backend, Backend::Command { .. }),
            mailroom,
            database,
            sorter,
        };
        Ok((services, settings))
    }

    /// Whether `m` is in what the list shows: its one mailbox, or a mailbox with its role.
    fn in_listing(&self, m: &StoredMessage) -> bool {
        match self.listing() {
            Showing::Mailbox { account, name } => m.account == *account && m.mailbox == *name,
            Showing::All(role) => self
                .mailboxes
                .get(&m.account)
                .and_then(|list| list.iter().find(|b| b.name == m.mailbox))
                .is_some_and(|b| b.role == *role),
        }
    }

    /// What the list shows, read from the store: what's selected, or what matches the search,
    /// all of it or only unread mail.
    fn read_list(&self, store: &Store) -> postden_store::Result<Vec<StoredMessage>> {
        let unread_only = self.settings.unread_only;
        // Not one being moved either, so it doesn't take the place of one listed.
        let listed = |m: &StoredMessage| {
            lists(m, unread_only, &self.still_listed, &self.put_away)
                && (self.moving.is_empty() || !self.moving.contains(&message_key(m)))
        };
        let mut found = if let Some(query) = self.search.query() {
            let cached = store.matching_messages(
                self.listing().scope(),
                |m| search::matches(&m.envelope, query) && listed(m),
                self.list_limit,
            )?;
            // Cut to the limit only once what the server found is filtered too, so read matches
            // don't take the place of unread ones.
            let mut merged = self.search.merge(cached, usize::MAX);
            // A server match the cache holds but whose headers don't match (its text did) shows
            // as read or unread, and starred or not, as the cache says, which follows the server.
            for m in &mut merged {
                let (a, b, uid) = (&m.account, &m.mailbox, m.envelope.uid);
                if let Some(cached) = store.message(a, b, uid)? {
                    m.envelope.seen = cached.envelope.seen;
                    m.envelope.flagged = cached.envelope.flagged;
                }
            }
            merged
        } else if unread_only {
            // As many more as are put away or being moved, which the store still has unread, to
            // fill the list.
            let hidden = self.put_away.len() + self.moving.len();
            let limit = self.list_limit.saturating_add(hidden);
            let mut unread = store.unread_messages(self.showing.scope(), limit)?;
            // Read ones listed already stay; they're no more than the list showed.
            let known: HashSet<MessageKey> = unread.iter().map(message_key).collect();
            for key in self.still_listed.difference(&known) {
                // Not one from elsewhere, as one open from a search of every inbox.
                if let Some(m) = store.message(&key.0, &key.1, key.2)?
                    && self.in_listing(&m)
                {
                    unread.push(m);
                }
            }
            search::newest_first(&mut unread);
            unread
        } else {
            match &self.showing {
                Showing::All(role) => store.recent_messages(*role, self.list_limit)?,
                Showing::Mailbox { account, name } => {
                    store.mailbox_messages(account, name, self.list_limit)?
                }
            }
        };
        // The message open stays listed, past the end if need be, as when the list starts again
        // shorter, so J and K go on from it: listed already, still in what's listed and matching
        // the search.
        let open = self.reader.as_ref().map(|r| message_key(&r.message));
        if let Some((a, b, uid)) = open.clone().filter(|key| {
            self.find_listed(key).is_some() && !found.iter().any(|m| message_key(m) == *key)
        }) && let Some(m) = store.message(&a, &b, uid)?
            && self.in_listing(&m)
            && self
                .search
                .query()
                .is_none_or(|query| search::matches(&m.envelope, query))
        {
            found.push(m);
            search::newest_first(&mut found);
        }
        found.retain(listed);
        let past = found
            .iter()
            .skip(self.list_limit)
            .position(|m| Some(message_key(m)) == open);
        let kept = past.map(|at| found.swap_remove(self.list_limit + at));
        found.truncate(self.list_limit);
        found.extend(kept);
        Ok(found)
    }

    /// Rereads accounts and mail from the store.
    fn reload(&mut self) {
        let Ok(services) = &self.services else { return };
        let unread_only = self.settings.unread_only;
        // Read ones stay listed only while a message is open: back in the list, by whatever way,
        // what's read leaves it. The one open stays, so J and K go on from it.
        match self.reader.as_ref().map(|r| message_key(&r.message)) {
            Some(open) if unread_only => {
                self.still_listed.insert(open);
            }
            _ => self.still_listed.clear(),
        }
        let (unanswered, read_later) = (&self.unanswered, &self.read_later);
        self.stale_marks.retain(|key| {
            unanswered.contains_key(&(key.clone(), false)) || read_later.contains_key(key)
        });
        // Read now, or gone, or with no read mark left on its way, as once answered, a message
        // put away no longer is: unread as the store says, it's listed again.
        let still: HashSet<MessageKey> = self
            .put_away
            .iter()
            .filter(|key| self.marking_read(key))
            .filter(|(a, b, uid)| {
                services
                    .store
                    .seen(a, b, *uid)
                    .is_ok_and(|seen| seen == Some(false))
            })
            .cloned()
            .collect();
        self.put_away = still;
        let before: Vec<MessageKey> = if unread_only {
            self.messages.iter().map(message_key).collect()
        } else {
            Vec::new()
        };
        let messages = self.read_list(&services.store);
        let mailboxes = services.store.accounts().and_then(|accounts| {
            accounts
                .into_iter()
                .map(|a| Ok((services.store.mailboxes(&a.id)?, a)))
                .collect::<Result<Vec<_>, _>>()
        });
        match (
            mailboxes,
            messages,
            services.store.unread_counts(),
            services.store.counted_mailboxes(),
        ) {
            (Ok(accounts), Ok(mut messages), Ok(unread), Ok(counted)) => {
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
                for m in &mut messages {
                    self.shown_as_asked(m);
                }
                self.messages = messages;
                if unread_only {
                    // Kept while hidden too, as while being moved, or past the list's end.
                    self.still_listed
                        .extend(self.messages.iter().map(message_key));
                    self.cursor_past_left(&before);
                }
                self.listed = self.listed_mailboxes();
                self.unread = unread;
                self.counted = counted;
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
                    } else if let Some(&flagged) = self.starring.get(&message_key(&reader.message))
                    {
                        reader.message.envelope.flagged = flagged;
                    }
                }
            }
            (Err(e), _, _, _) | (_, Err(e), _, _) | (_, _, Err(e), _) | (_, _, _, Err(e)) => {
                self.notify(format!("couldn't read mail: {e}"));
            }
        }
    }

    /// One of an account's mailboxes started over: what was older is to be fetched again, and
    /// what a search found there is gone. A message open from there closes, and a star on its
    /// way there isn't shown, as their UIDs may name other messages now.
    fn started_over(&mut self, account: &str, mailbox: &str) {
        self.no_older
            .remove(&(account.to_owned(), mailbox.to_owned()));
        self.search.forget_mailbox(account, mailbox);
        self.starring
            .retain(|key, _| key.0 != account || key.1 != mailbox);
        self.tags
            .retain(|key, _| key.0 != account || key.1 != mailbox);
        self.still_listed
            .retain(|key| key.0 != account || key.1 != mailbox);
        let here = |key: &MessageKey| key.0 == account && key.1 == mailbox;
        let asked = self
            .unanswered
            .keys()
            .filter(|(key, star)| !star && here(key));
        let later = self.read_later.keys().filter(|key| here(key));
        let stale: Vec<MessageKey> = asked.map(|(key, _)| key).chain(later).cloned().collect();
        self.stale_marks.extend(stale);
        if self
            .reader
            .as_ref()
            .is_some_and(|r| r.message.account == account && r.message.mailbox == mailbox)
        {
            // What was read in other mailboxes stays put away; this one's marks are stale now.
            self.leave_reader();
            self.pending_reply = None;
        }
    }

    /// Shows `m` with a star on its way as asked, until the server answers.
    fn shown_as_asked(&self, m: &mut StoredMessage) {
        if let Some(&flagged) = self.starring.get(&message_key(m)) {
            m.envelope.flagged = flagged;
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
            self.started_over(account, mailbox);
        }
        match update {
            update @ (Update::Status { .. } | Update::Synced { .. }) => self.watching(update),
            Update::Mail {
                account,
                mailbox,
                role: Role::Inbox,
                count,
                first_sync,
                uid_validity,
                unread,
                message_ids,
            } => {
                let target = Target {
                    account,
                    mailbox,
                    uid_validity,
                    uid: None,
                };
                self.inbox_mail(target, count, first_sync, &unread, &message_ids);
            }
            Update::CopyFailed { account, error } => self.copy_failed(&account, &error),
            Update::SaveFailed { account, error } => {
                self.notify(format!("{account}: couldn't save new mail: {error}"));
            }
            Update::Cleared(cleared) => self.cleared(cleared),
            Update::ReleaseFailed(error) => {
                self.notify(format!("Couldn't drop the opened messages kept: {error}"));
            }
            update @ Update::Opened { .. } => self.opened_update(update, tx),
            // Sent mail is shown as it's listed, without a notice.
            Update::Mail { .. } => self.reload(),
            update @ Update::Unread { .. } => self.unread_found(&update),
            Update::Mailboxes { account } => self.mailboxes_listed(&account),
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
            update @ (Update::Snoozed { .. } | Update::Woke { .. }) => self.snoozed(update),
            Update::Exported {
                account,
                path,
                result,
            } => self.exported(&account, &path, result),
            update @ Update::Sent { .. } => self.sent(update),
            Update::MailboxCreated {
                account,
                name,
                result,
            } => self.mailbox_created(&account, &name, result),
            update @ Update::MailboxDeleted { .. } => self.mailbox_deleted(update),
            Update::Searched {
                id,
                account,
                mailbox,
                result,
            } => self.searched(id, &account, &mailbox, result),
            update @ (Update::Seen { .. }
            | Update::SeenFailed { .. }
            | Update::Flagged { .. }
            | Update::FlagsChanged { .. }
            | Update::Restored { .. }) => self.message_changed(update),
        }
        self.follow_restored(tx);
    }

    /// Every downloaded message was dropped, or couldn't be.
    fn cleared(&mut self, cleared: Result<usize, String>) {
        self.notify(match cleared {
            Ok(0) => "No downloaded messages to clear.".to_owned(),
            Ok(1) => "Cleared 1 downloaded message.".to_owned(),
            Ok(n) => format!("Cleared {n} downloaded messages."),
            Err(e) => format!("Couldn't clear the downloaded messages: {e}"),
        });
    }

    /// The server listed an account's mailboxes. What a search found in one deleted or renamed
    /// on the server is gone.
    fn mailboxes_listed(&mut self, account: &str) {
        self.reload();
        // Listed by a refresh of every mailbox, or a round before it: either way its round syncs
        // them all next, whether they were listed before or not.
        if let Some(&asked) = self.refreshing_all.get(account) {
            let listed = self.mailboxes.get(account).map_or(&[][..], Vec::as_slice);
            for mailbox in listed
                .iter()
                .filter(|m| !m.name.eq_ignore_ascii_case(INBOX))
            {
                let key = (account.to_owned(), mailbox.name.clone());
                let n = self.refreshing.entry(key).or_insert(asked);
                *n = (*n).max(asked);
            }
        }
        let listed = self.mailboxes.get(account);
        let gone = |a: &str, m: &str| {
            a == account && !listed.is_some_and(|l| l.iter().any(|b| b.name == m))
        };
        // Deleted, here or elsewhere, or renamed: what showed it shows the inbox instead.
        let left = |a: &str, m: &str| !m.eq_ignore_ascii_case(INBOX) && gone(a, m);
        let shown = match &self.showing {
            Showing::Mailbox { account: a, name } if left(a, name) => Some(name.clone()),
            _ => None,
        };
        let read = (self.reader.as_ref())
            .map(|r| &r.message)
            .filter(|m| left(&m.account, &m.mailbox))
            .map(|m| m.mailbox.clone());
        let forgot = self.search.forget_gone(gone);
        for m in shown.iter().chain(&read) {
            self.leave_gone_mailbox(account, m);
        }
        if forgot {
            self.reload();
        }
        // A mailbox to delete that's gone, or now kept for special mail, isn't deleted: unless
        // it's being deleted, whose answer closes the dialog, or the dialog already says why it
        // can't be, which stays for the user to close.
        if let Some((a, m)) = self.delete_mailbox.mailbox.clone()
            && a == account
            && self.delete_mailbox.error.is_none()
            && !self
                .deleting_mailboxes
                .contains_key(&(a.clone(), m.clone()))
            && !self.can_delete_mailbox(&a, &m)
        {
            self.close_delete_mailbox();
            let label = self.label(&a).to_owned();
            self.notify(format!(
                "{label}: {} changed on the server, so it wasn't deleted",
                self.mailbox_label(&a, &m)
            ));
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
                // Asked for at once, they all leave a list of unread mail.
                self.still_listed
                    .retain(|(a, b, uid)| a != account || b != mailbox || *uid >= up_to);
                if let Some(reader) = &mut self.reader
                    && reader.message.account == account
                    && reader.message.mailbox == mailbox
                    && reader.message.envelope.uid < up_to
                {
                    reader.message.envelope.seen = true;
                    // Read now, should a read mark asked as it opened be refused after.
                    reader.seen_before = true;
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
        let key = (account.to_owned(), mailbox.to_owned(), uid);
        self.moving.remove(&key);
        let undoable = self.moved_for_undo(&key, result.is_ok());
        let autosorted = self.autosort_answered(&key, result.is_ok());
        let to = self.mailbox_label(account, to);
        match result {
            Ok(current) => {
                // If the mailbox started over meanwhile, what a search found there under this
                // UID is another message.
                if current {
                    self.search.forget_message(account, mailbox, uid);
                }
                if autosorted {
                    self.notify(format!("Autosorted a new message to {to}"));
                } else {
                    self.notify(format!("Moved to {to}{}", undo_hint(undoable)));
                }
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

    /// [`Update::Opened`], as [`Mail::opened`].
    fn opened_update(&mut self, update: Update, tx: &mut Tx) {
        if let Update::Opened {
            account,
            mailbox,
            uid_validity,
            uid,
            body,
            marked,
        } = update
        {
            self.opened(&(account, mailbox, uid), uid_validity, (body, marked), tx);
        }
    }

    /// A message asked for with [`Mail::open_message`] was downloaded, or couldn't be.
    fn opened(
        &mut self,
        key: &MessageKey,
        uid_validity: u32,
        (body, marked): (Result<Body, String>, bool),
        tx: &mut Tx,
    ) {
        let (account, mailbox, uid) = (key.0.as_str(), key.1.as_str(), key.2);
        // Downloaded and marked, it's read; refused, it's as it was last confirmed.
        let read = (body.is_ok() && marked).then_some(true);
        self.search.settle(account, mailbox, uid, read);
        // Not downloaded, it isn't read either.
        if body.is_err() {
            self.read_later.remove(key);
        }
        // Refused, it shows as it was before it opened, shown read since: first, so what the
        // store says as the mark is answered below wins.
        if body.is_ok()
            && !marked
            && let Some(reader) = &mut self.reader
            && reader.is(account, mailbox, uid)
            && reader.uid_validity.is_none_or(|v| v == uid_validity)
        {
            reader.message.envelope.seen = reader.seen_before;
        }
        // Its read mark is answered; a download failing isn't a U refused, so the undo stays,
        // and is done once nothing else is awaited, as a U refused would have dropped it.
        if self.answered(key, false) == 0
            && let Some(Undo::Seen { message, done, .. }) = &mut self.undo
            && message == key
        {
            *done = true;
        }
        // Downloaded under another UIDVALIDITY, it's another message than the one open now.
        if let Some(reader) = &mut self.reader
            && reader.is(account, mailbox, uid)
            && reader.uid_validity.is_none_or(|v| v == uid_validity)
        {
            reader.markdown = body.as_ref().ok().and_then(markdown);
            let downloaded = body.is_ok();
            if let Some(read) = read {
                reader.message.envelope.seen = read;
            }
            reader.body = Some(body);
            // The server, not a click, brought it, which would land the text at once:
            // ask for its entrance anyway, so it fades in over the spinner.
            tx.animated();
            let waiting = self
                .pending_reply
                .take_if(|(key, _)| key.0 == account && key.1 == mailbox && key.2 == uid);
            // Failing to download, the reader says why, and a reply waits to be asked again. A
            // message started meanwhile isn't replaced.
            if let (Some((_, answer)), true, false) = (waiting, downloaded, self.compose.open) {
                self.answer(answer);
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
        self.clear_listed();
        self.reload();
    }

    /// Searches every inbox, or just what's selected.
    pub(crate) fn search_everywhere(&mut self, everywhere: bool) {
        if self.search.everywhere != everywhere {
            self.search.everywhere = everywhere;
            self.search.forget_server();
            self.list_limit = LIST_LIMIT;
            self.clear_listed();
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
        self.close_reader();
        self.search.query.focused = true;
    }

    /// Opens the palette afresh, with nothing typed, or closes it.
    pub(crate) fn toggle_palette(&mut self) {
        if self.palette.open {
            self.close_palette();
        } else {
            self.open_palette(PaletteMode::Commands);
        }
    }

    /// Opens the palette afresh on `mode`, with nothing typed.
    pub(crate) fn open_palette(&mut self, mode: PaletteMode) {
        // Menus left open would sit over it.
        self.actions_open = false;
        self.message_menu = None;
        self.mailbox_menu = None;
        self.account_menu = None;
        self.updater.channel_open = false;
        self.palette = Palette {
            open: true,
            mode,
            query: TextEdit::new(""),
            active: None,
        };
        self.palette.query.focused = true;
    }

    pub(crate) const fn close_palette(&mut self) {
        self.palette.open = false;
    }

    /// What's typed in the palette changed: the best match is highlighted.
    pub(crate) fn palette_changed(&mut self) {
        self.palette.active = None;
    }

    /// Moves the highlight `by` rows, wrapping around the ends. Returns the row it's on.
    pub(crate) fn move_palette(&mut self, by: isize) -> usize {
        let shown = self.palette.shown(self);
        if shown.is_empty() {
            return 0;
        }
        let at = self.palette.active_row(&shown).cast_signed() + by;
        let row = at.rem_euclid(shown.len().cast_signed()).cast_unsigned();
        self.palette.active = shown.into_iter().nth(row).map(|e| e.command);
        row
    }

    /// The palette's highlighted row and its key, while it's open with rows.
    fn palette_highlight(&self) -> Option<(usize, String)> {
        if !self.palette.open {
            return None;
        }
        let shown = self.palette.shown(self);
        let row = self.palette.active_row(&shown);
        Some((row, shown.get(row)?.command.key()))
    }

    /// Runs `command` from the palette, or the highlighted one (Return) for `None`, closing it.
    pub(crate) fn run_palette(&mut self, command: Option<Command>, tx: &mut Tx) {
        let shown = self.palette.shown(self);
        let row = self.palette.active_row(&shown);
        let entry = match command {
            Some(c) => shown.into_iter().find(|e| e.command == c),
            None => shown.into_iter().nth(row),
        };
        let Some(entry) = entry else { return };
        self.close_palette();
        entry.command.run(self, tx);
    }

    #[cfg(test)]
    /// Runs the command whose shortcut `ev` is, if one applies here. G on its own starts a
    /// "go to", and the key after it is taken whether it goes anywhere or not.
    pub(crate) fn run_shortcut(&mut self, ev: &rune::KeyEvent, tx: &mut Tx) -> bool {
        let go = self.take_go();
        self.run_keys(ev, go, tx)
    }

    /// Whether G was pressed just before this key, forgetting it: whatever handles the key, G
    /// only says where the next one goes.
    pub(crate) fn take_go(&mut self) -> bool {
        self.go_pressed
            .take()
            .is_some_and(|at| at.elapsed() < GO_WITHIN)
    }

    /// Forgets a G pressed before, when the key after it went elsewhere.
    pub(crate) const fn forget_go(&mut self) {
        self.go_pressed = None;
    }

    /// [`Mail::run_shortcut`], `go` when G came just before. While a menu is open, only ⌘
    /// shortcuts count: keys on their own are the menu's.
    pub(crate) fn run_keys(&mut self, ev: &rune::KeyEvent, go: bool, tx: &mut Tx) -> bool {
        let menu = self.actions_open
            || self.message_menu.is_some()
            || self.mailbox_menu.is_some()
            || self.account_menu.is_some();
        let counts =
            |k: &Shortcut| !menu || matches!(k, Shortcut::Command { .. } | Shortcut::Sidebar);
        let commands = commands::available(self);
        let pressed = commands.iter().find(|c| {
            c.shortcuts(self)
                .iter()
                .any(|k| counts(k) && k.matches(ev, go))
        });
        if let Some(command) = pressed.cloned() {
            // A menu open (⌘N over it) would come back once what the command shows closes.
            self.actions_open = false;
            self.message_menu = None;
            self.mailbox_menu = None;
            self.account_menu = None;
            self.close_palette();
            command.run(self, tx);
            return true;
        }
        let goes = |c: &Command| {
            c.shortcuts(self)
                .iter()
                .any(|k| matches!(k, Shortcut::Go(_)))
        };
        if !go && !menu && Shortcut::Key('g').matches(ev, false) && commands.iter().any(goes) {
            self.go_pressed = Some(Instant::now());
            return true;
        }
        go
    }

    /// The message the keyboard acts on: the open one, else the one under the list's cursor.
    pub(crate) fn focused_message(&self) -> Option<&StoredMessage> {
        match &self.reader {
            Some(r) => Some(&r.message),
            None => self.find_listed(self.cursor.as_ref()?),
        }
    }

    fn find_listed(&self, key: &MessageKey) -> Option<&StoredMessage> {
        self.shown().find(|m| message_key(m) == *key)
    }

    /// The message `key` names, open or listed.
    pub(crate) fn find_message(&self, key: &MessageKey) -> Option<&StoredMessage> {
        self.reader
            .as_ref()
            .map(|r| &r.message)
            .filter(|m| message_key(m) == *key)
            .or_else(|| self.find_listed(key))
    }

    /// Moves the list's cursor `by` rows, stopping at the ends; with nothing under it, it starts
    /// at the top. While a message is open, the next or previous one opens instead.
    pub(crate) fn move_cursor(&mut self, by: isize, tx: &mut Tx) {
        let keys: Vec<MessageKey> = self.shown().map(message_key).collect();
        let Some(last) = keys.len().checked_sub(1) else {
            return;
        };
        let open = self.reader.as_ref().map(|r| message_key(&r.message));
        let from = open.clone().or_else(|| self.cursor.clone());
        let at = from.and_then(|k| keys.iter().position(|x| *x == k));
        let row = match at {
            Some(i) => i.saturating_add_signed(by).min(last),
            // An open message the list doesn't show has no next one.
            None if open.is_some() => return,
            None => 0,
        };
        let Some(key) = keys.get(row).cloned() else {
            return;
        };
        tx.scroll_into_view(row_key(&key));
        if open.is_some() && at != Some(row) {
            self.open_message(&key.0, &key.1, key.2);
        }
        self.cursor = Some(key);
    }

    /// Scrolls the list to its cursor, as the list shows again.
    pub(crate) fn reveal_cursor(&self, tx: &mut Tx) {
        if let Some(key) = &self.cursor {
            tx.scroll_into_view(row_key(key));
        }
    }

    /// Puts the list's cursor on its first row, if it has one.
    pub(crate) fn cursor_to_first(&mut self, tx: &mut Tx) -> bool {
        let Some(first) = self.shown().next().map(message_key) else {
            return false;
        };
        tx.scroll_into_view(row_key(&first));
        self.cursor = Some(first);
        true
    }

    /// Opens the message under the list's cursor.
    pub(crate) fn open_cursor(&mut self) {
        if let Some(m) = self.focused_message() {
            let (account, mailbox, uid) = (m.account.clone(), m.mailbox.clone(), m.envelope.uid);
            self.open_message(&account, &mailbox, uid);
        }
    }

    /// A message was moved, or snoozed, and leaves the list at once. A cursor on it (or on it
    /// open) moves to the row after it, else the one before, and an open one closes.
    pub(crate) fn left_list(&mut self, key: MessageKey) {
        let open = self.reader.as_ref().map(|r| message_key(&r.message));
        if self.cursor.as_ref() == Some(&key) || open.as_ref() == Some(&key) {
            let keys: Vec<MessageKey> = self.shown().map(message_key).collect();
            let at = keys.iter().position(|k| *k == key);
            self.cursor = at
                .and_then(|i| keys.get(i + 1).or_else(|| keys.get(i.checked_sub(1)?)))
                .cloned();
        }
        if open.as_ref() == Some(&key) {
            self.leave_reader();
        }
        self.moving.insert(key);
        self.reload();
    }

    /// Replies to or forwards the focused message: the open one once it's downloaded, or the one
    /// under the list's cursor, which opens first.
    pub(crate) fn answer_focused(&mut self, answer: Answer) {
        if self.reader.is_some() {
            return self.answer(answer);
        }
        if let Some(key) = self
            .cursor
            .clone()
            .filter(|k| self.find_listed(k).is_some())
        {
            self.answer_message(&key.0, &key.1, key.2, answer);
        }
    }

    /// Snoozes the message `key` names until `until`.
    pub(crate) fn snooze_key(&mut self, key: &MessageKey, until: Timestamp) {
        let Some(message) = self.find_message(key).cloned() else {
            return;
        };
        self.snooze_message(&message, until);
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

    /// Suggests the contacts the recipient being typed in `field` could be, if contacts are
    /// remembered. Failing to read them, none are suggested.
    pub(crate) fn suggest(&mut self, field: Recipients) {
        let Ok(services) = &self.services else {
            return;
        };
        if !self.settings.remember_contacts {
            self.compose.close_suggestions();
            return;
        }
        let edit = self.compose.recipients(field);
        match contacts::suggestions(&services.store, &edit.text, edit.focus) {
            Ok(found) => self.compose.suggest(field, found),
            Err(_unreadable) => self.compose.close_suggestions(),
        }
    }

    /// The keys of a recipients field while it suggests contacts: ↑ and ↓ pick one, Return or
    /// Tab puts it in, and Escape stops suggesting. Whether the key was taken.
    pub(crate) fn suggestion_key(
        &mut self,
        field: Recipients,
        ev: &rune::KeyEvent,
        tx: &mut Tx,
    ) -> bool {
        let m = ev.mods;
        if self.compose.suggested(field).is_empty() || m.cmd || m.ctrl || m.alt || m.shift {
            return false;
        }
        match ev.key {
            rune::Key::Up | rune::Key::Down => {
                let by = if ev.key == rune::Key::Up { -1 } else { 1 };
                self.compose.move_suggestion(field, by);
                tx.scroll_into_view(field.suggestion_key(self.compose.suggestions.active));
                true
            }
            rune::Key::Enter | rune::Key::Tab => {
                let active = self.compose.suggestions.active;
                self.compose.choose_suggestion(field, active)
            }
            rune::Key::Escape => {
                self.compose.close_suggestions();
                true
            }
            _ => false,
        }
    }

    /// Closes the From picker for a key that went past it, as after Tab left its search. The key
    /// still does what it does (⌘W closes the window), so an update of its own builds the view.
    pub(crate) fn close_from_picker(&mut self) {
        self.compose.from.close();
        if let Some(handle) = &self.handle {
            handle.update(|_: &mut Self, _| {});
        }
    }

    /// Starts a reply to the open message, once it's downloaded.
    pub(crate) fn reply(&mut self, all: bool) {
        self.answer(if all { Answer::ReplyAll } else { Answer::Reply });
    }

    /// Starts a reply to, or a forward of, the open message, once it's downloaded. Its
    /// attachments aren't forwarded, and a notice says so.
    pub(crate) fn answer(&mut self, answer: Answer) {
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
        let tz = jiff::tz::TimeZone::system();
        let mut draft = match answer {
            Answer::Forward => Draft::forward(&account.id, &message.envelope, body, &tz),
            _ => Draft::reply(
                account,
                &message.envelope,
                body,
                answer == Answer::ReplyAll,
                &tz,
            ),
        };
        let left_behind = (answer == Answer::Forward && !body.attachments.is_empty())
            .then(|| body.attachments.join(", "));
        draft.signature = self.signature_of(&draft.account);
        self.compose = ComposeForm::new(draft, self.images.as_ref());
        if let Some(names) = left_behind {
            self.notify(format!("Attachments aren't forwarded yet: {names}"));
        }
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
    fn sent(&mut self, update: Update) {
        let Update::Sent {
            account,
            result,
            recipients,
        } = update
        else {
            return;
        };
        let label = self.label(&account).to_owned();
        match result {
            Ok(_) => {
                self.remember(&recipients);
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

    /// Remembers who mail was sent to, if contacts are remembered.
    fn remember(&mut self, recipients: &[Address]) {
        let Ok(services) = &mut self.services else {
            return;
        };
        if !self.settings.remember_contacts {
            return;
        }
        if let Err(e) = contacts::remember(
            &mut services.store,
            recipients,
            Timestamp::now().as_second(),
        ) {
            self.notify(format!("Couldn't remember who it went to: {e}"));
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
        // Opening marks it read, which would be lost once it's moved back.
        if self.moving_back(&(account.to_owned(), mailbox.to_owned(), uid)) {
            return;
        }
        self.put_away
            .remove(&(account.to_owned(), mailbox.to_owned(), uid));
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
        let Some(mut message) = listed.or_else(cached) else {
            return;
        };
        self.shown_as_asked(&mut message);
        // Back in the list, the cursor is on it.
        self.cursor = Some((account.to_owned(), mailbox.to_owned(), uid));
        // A read mark already on its way isn't asked for twice: one failing would be taken for
        // the other. One to unread, as after U, is followed by one to read once nothing for
        // the message is awaited.
        let key = (account.to_owned(), mailbox.to_owned(), uid);
        let uid_validity = self.uid_validity(account, mailbox);
        let mark = !self.unanswered.contains_key(&(key.clone(), false));
        // Marked read now, an older note to do so later is superseded.
        let later = self.unreading.contains(&key);
        if mark {
            self.read_later.remove(&key);
        }
        // To be marked read later, it is as this message; a read mark from before its mailbox
        // started over, still on its way, isn't asked again, and doesn't put it away.
        if later {
            self.stale_marks.remove(&key);
        }
        let mut reader = Reader {
            seen_before: message.envelope.seen,
            message,
            body: None,
            markdown: None,
            uid_validity,
        };
        let mut started = true;
        if let Ok(services) = &self.services {
            let opened = services
                .mailroom
                .open(&services.store, account, mailbox, uid, mark);
            if let Err(e) = opened {
                reader.body = Some(Err(e.to_string()));
                started = false;
            } else {
                // Downloaded ahead, it shows at once; otherwise once it's downloaded.
                let key = (account.to_owned(), mailbox.to_owned(), uid);
                match opened {
                    Ok((Some(body), marking)) => {
                        reader.markdown = markdown(&body);
                        reader.body = Some(Ok(body));
                        // Marked read on the server, which answers as for U.
                        if marking {
                            self.asked(&key, false);
                            self.unreading.remove(&key);
                        }
                    }
                    // Marked read once downloaded, which answers as `Update::Opened`.
                    Ok((None, _)) => {
                        self.asked(&key, false);
                        self.unreading.remove(&key);
                    }
                    Err(_) => {}
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
        // Not opened, it isn't read either.
        if started && later {
            self.read_later.insert(key, uid_validity);
        }
        // What autosort left alone while another was open may go now.
        if self
            .autosort_waits_for
            .take_if(|waiting| *waiting != message_key(&reader.message))
            .is_some()
        {
            self.sort_mail();
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
        self.leave_reader();
        self.pending_reply = None;
        self.forget_read();
    }

    /// Closes the open message, however the window leaves it. What was read meanwhile and is still
    /// being marked so is put away, for a list of only unread mail: the messages listed, as those J
    /// and K went through, and the one open, listed or not, as when opened from a notification.
    fn leave_reader(&mut self) {
        let Some(reader) = self.reader.take() else {
            return;
        };
        // What autosort left alone while it was open may go now.
        if self.autosort_waits_for.take().is_some() {
            self.sort_mail();
        }
        // Whether the list shows only unread mail or not, as it may again before they're answered.
        // Those kept listed too, though pushed past the list's end since.
        let listed = self.messages.iter().map(message_key);
        let kept = self.still_listed.iter().cloned();
        let marking: Vec<MessageKey> = listed
            .chain(kept)
            .chain([message_key(&reader.message)])
            .filter(|key| self.marking_read(key))
            .collect();
        self.put_away.extend(marking);
    }

    /// Showing only unread mail, the read messages it kept listed leave it, as after reading
    /// them, with those whose read mark is on its way to the server. One opened without asking
    /// the server, as while it can't be reached, is still unread and stays.
    fn forget_read(&mut self) {
        if self.settings.unread_only {
            self.reload();
        }
    }

    /// The list goes on to show something else: what it kept listed while a message was open
    /// is forgotten, but what was read there and is still being marked so is put away; all but
    /// the one still open, which stays listed for J and K until it's left.
    fn clear_listed(&mut self) {
        if self.settings.unread_only {
            let open = self.reader.as_ref().map(|r| message_key(&r.message));
            let marking: Vec<MessageKey> = self
                .still_listed
                .iter()
                .filter(|key| Some(*key) != open.as_ref() && self.marking_read(key))
                .cloned()
                .collect();
            self.put_away.extend(marking);
        }
        self.still_listed.clear();
    }

    /// Whether a read mark for `key` is on its way to the server, or is to follow one to unread.
    fn marking_read(&self, key: &MessageKey) -> bool {
        let asked =
            self.unanswered.contains_key(&(key.clone(), false)) && !self.unreading.contains(key);
        !self.stale_marks.contains(key) && (asked || self.read_later.contains_key(key))
    }

    /// Showing only unread mail, a cursor on a message that left the list moves to the row after
    /// it in the list `before`, still listed, else the one before.
    fn cursor_past_left(&mut self, before: &[MessageKey]) {
        let Some(cursor) = self.cursor.clone() else {
            return;
        };
        if self.find_listed(&cursor).is_some() {
            return;
        }
        let Some(at) = before.iter().position(|k| *k == cursor) else {
            return;
        };
        let still = |k: &&MessageKey| self.find_listed(k).is_some();
        let after = before
            .get(at + 1..)
            .and_then(|rest| rest.iter().find(still));
        self.cursor = after
            .or_else(|| before.get(..at)?.iter().rev().find(still))
            .cloned();
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

    /// Flips the focused message between read and unread.
    pub(crate) fn toggle_seen(&mut self) {
        let Some(m) = self.focused_message() else {
            return;
        };
        let (key, seen) = (message_key(m), m.envelope.seen);
        self.mark_seen(key, !seen);
    }

    /// Marks a message read or unread, as asked from the window, so Z marks it back.
    pub(crate) fn mark_seen(&mut self, key: MessageKey, seen: bool) {
        if self.still_saving(&key, false) {
            return;
        }
        let uid_validity = self.uid_validity(&key.0, &key.1);
        if self.set_seen(&key.0, &key.1, key.2, seen, uid_validity) {
            self.did(uid_validity.map(|uid_validity| Undo::Seen {
                message: key,
                uid_validity,
                seen: !seen,
                done: false,
            }));
        }
    }

    /// Stars a message or takes its star away, as asked from the window, so Z does the opposite.
    pub(crate) fn star(&mut self, key: MessageKey, flagged: bool) {
        if self.still_saving(&key, true) {
            return;
        }
        let uid_validity = self.uid_validity(&key.0, &key.1);
        if self.set_flagged(&key.0, &key.1, key.2, flagged, uid_validity) {
            self.did(uid_validity.map(|uid_validity| Undo::Flagged {
                message: key,
                uid_validity,
                flagged: !flagged,
                done: false,
            }));
        }
    }

    /// Whether the server hasn't answered the last read mark, or with `star` star, asked for
    /// `key`'s message: another isn't asked for until it has, so they're made in turn, and a
    /// notice says so.
    fn still_saving(&mut self, key: &MessageKey, star: bool) -> bool {
        let waiting = self.unanswered.contains_key(&(key.clone(), star));
        if waiting {
            self.notify("Still waiting for the server about this message".into());
        }
        waiting || self.moving_back(key)
    }

    /// Something was done from the window, which Z undoes with `undo`, or can't: what was done
    /// before, and moves back that fail, aren't undone any more.
    fn did(&mut self, undo: Option<Undo>) {
        self.undo = undo;
        self.undoing.clear();
        // Nor does the cursor go to a message being moved back; it isn't new mail all the same.
        self.restoring = None;
    }

    /// The UIDVALIDITY a mailbox is cached under, if it's cached.
    fn uid_validity(&self, account: &str, mailbox: &str) -> Option<u32> {
        let services = self.services.as_ref().ok()?;
        let state = services.store.mailbox_state(account, mailbox).ok()??;
        Some(state.uid_validity)
    }

    /// The server starred the message `key` names, with `star`, or else marked it: `Ok` with
    /// what it is now, or refused, `Err` with what it was asked to be. Once it answered every
    /// change asked for, Z can change it back; the change Z undoes refused, there's nothing to
    /// undo.
    fn changed_for_undo(&mut self, key: &MessageKey, star: bool, answer: Result<bool, bool>) {
        let waiting = self.answered(key, star);
        if star != matches!(self.undo, Some(Undo::Flagged { .. })) {
            return;
        }
        let Some(
            Undo::Seen {
                message,
                done,
                seen: was,
                ..
            }
            | Undo::Flagged {
                message,
                done,
                flagged: was,
                ..
            },
        ) = &mut self.undo
        else {
            return;
        };
        if message != key {
            return;
        }
        // Another change refused, such as opening it marking it read, isn't the one undone,
        // which a refusal of would have dropped it: answered too, if nothing else is awaited.
        let undone_refused = answer.is_err_and(|asked| asked != *was);
        if undone_refused {
            self.undo = None;
        } else if waiting == 0 {
            *done = true;
        }
    }

    /// The open message, if it's in the account's `mailbox`, shows as read or unread and starred
    /// or not as the store now says, unless a change to it is still on its way to the server.
    fn reader_flags_changed(&mut self, account: &str, mailbox: &str) {
        let open = self
            .reader
            .as_ref()
            .filter(|r| r.message.account == account && r.message.mailbox == mailbox)
            .map(|r| message_key(&r.message));
        if let Some(key) = open {
            self.settle_reader(&key);
        }
    }

    /// The open message, if `key` names it, shows as the store says, once no change to it is on
    /// its way to the server: one made elsewhere meanwhile isn't missed.
    fn settle_reader(&mut self, key: &MessageKey) {
        if self.pending(key) {
            return;
        }
        let Some(reader) = self
            .reader
            .as_mut()
            .filter(|r| message_key(&r.message) == *key)
        else {
            return;
        };
        let Ok(services) = &self.services else { return };
        if let Ok(Some(cached)) = services.store.message(&key.0, &key.1, key.2) {
            reader.message.envelope.seen = cached.envelope.seen;
            reader.message.envelope.flagged = cached.envelope.flagged;
        }
    }

    /// Whether `key`'s message is being moved back by undoing, so it isn't changed meanwhile,
    /// and a notice says so.
    fn moving_back(&mut self, key: &MessageKey) -> bool {
        let id = self
            .find_message(key)
            .or_else(|| self.search.found.iter().find(|m| message_key(m) == *key))
            .and_then(|m| m.envelope.message_id.clone());
        let leaving = id.is_some_and(|id| {
            self.leaving
                .iter()
                .any(|r| r.account == key.0 && r.mailbox == key.1 && r.message_id == id)
        });
        if leaving {
            self.notify("The message is being moved back".into());
        }
        leaving
    }

    /// The server answered a read mark, or with `star` a star, asked for `key`'s message. Once
    /// none is left, the open message shows as the store says, a read mark waiting to follow
    /// is asked for, and then a move waiting for them is made. Returns how many of that kind are
    /// still unanswered.
    fn answered(&mut self, key: &MessageKey, star: bool) -> usize {
        let asked = (key.clone(), star);
        let waiting = self
            .unanswered
            .get(&asked)
            .map_or(0, |n| n.saturating_sub(1));
        if waiting == 0 {
            self.unanswered.remove(&asked);
            if !star {
                self.unreading.remove(key);
            }
        } else {
            self.unanswered.insert(asked, waiting);
        }
        self.settle_reader(key);
        // Opened meanwhile, it's marked read now: asked whatever it shows, as one found only by
        // searching the server has no cached copy to say. A move waiting goes once that's
        // answered too.
        if !self.pending(key)
            && let Some(uid_validity) = self.read_later.remove(key)
        {
            self.set_seen(&key.0, &key.1, key.2, true, uid_validity);
        }
        if !self.pending(key) {
            self.send_deferred(key);
        }
        waiting
    }

    /// Whether a read mark or star asked for `key`'s message hasn't been answered yet.
    fn pending(&self, key: &MessageKey) -> bool {
        self.unanswered.contains_key(&(key.clone(), false))
            || self.unanswered.contains_key(&(key.clone(), true))
    }

    /// Makes the move that waited for `key`'s message, if any. Refused, or once its mailbox
    /// started over (its UID may name another message now), the message is listed where it
    /// was again, if it's still there.
    fn send_deferred(&mut self, key: &MessageKey) {
        let Some((deferred, asked_under)) = self.deferred.remove(key) else {
            return;
        };
        let (account, mailbox, uid) = (&key.0, &key.1, key.2);
        let current = self.uid_validity(account, mailbox);
        let deleting = match &deferred {
            Deferred::Move(to) => self.deleting_destination(account, to),
            Deferred::Snooze(..) => None,
        };
        let sent = match (&self.services, &deferred) {
            (Err(e), _) => Err(e.clone()),
            (_, Deferred::Move(_)) if let Some(why) = deleting => Err(why),
            _ if current.is_none() || current != asked_under => Err(format!(
                "{} changed on the server",
                self.mailbox_label(account, mailbox)
            )),
            (Ok(services), Deferred::Move(to)) => services
                .mailroom
                .move_message(&services.store, account, mailbox, uid, to, asked_under)
                .map_err(|e| e.to_string()),
            (Ok(services), Deferred::Snooze(message, until)) => services
                .mailroom
                .snooze(
                    &services.store,
                    account,
                    mailbox,
                    &message.envelope,
                    until.as_second(),
                    asked_under,
                )
                .map_err(|e| e.to_string()),
        };
        if let Err(e) = sent {
            let label = self.label(account).to_owned();
            let what = match deferred {
                Deferred::Move(_) => "move",
                Deferred::Snooze(..) => "snooze",
            };
            self.notify(format!("{label}: couldn't {what} the message: {e}"));
            self.moving.remove(key);
            self.moved_for_undo(key, false);
            self.reload();
        }
    }

    /// Counts a read mark, or with `star` a star, asked of the server for `key`'s message.
    fn asked(&mut self, key: &MessageKey, star: bool) {
        *self.unanswered.entry((key.clone(), star)).or_default() += 1;
        // Asked now, a read mark is this message's, not one from before its mailbox started over.
        if !star {
            self.stale_marks.remove(key);
        }
    }

    /// Stars the focused message, or takes its star away.
    pub(crate) fn toggle_star(&mut self) {
        let Some(m) = self.focused_message() else {
            return;
        };
        let (key, flagged) = (message_key(m), m.envelope.flagged);
        self.star(key, !flagged);
    }

    /// Stars a message, or takes its star away. It's shown so right away; the store catches up
    /// once the server agrees. With `uid_validity`, as [`Mail::set_seen`]. Returns whether it
    /// was asked of the server.
    pub(crate) fn set_flagged(
        &mut self,
        account: &str,
        mailbox: &str,
        uid: u32,
        flagged: bool,
        uid_validity: Option<u32>,
    ) -> bool {
        let Ok(services) = &self.services else {
            return false;
        };
        if let Err(e) = services.mailroom.set_flagged(
            &services.store,
            account,
            mailbox,
            uid,
            flagged,
            uid_validity,
        ) {
            self.notify(e.to_string());
            return false;
        }
        let key = (account.to_owned(), mailbox.to_owned(), uid);
        self.asked(&key, true);
        self.starring.insert(key, flagged);
        if let Some(reader) = &mut self.reader
            && reader.is(account, mailbox, uid)
        {
            reader.message.envelope.flagged = flagged;
        }
        if let Some(listed) = self
            .messages
            .iter_mut()
            .find(|l| is_message(l, account, mailbox, uid))
        {
            listed.envelope.flagged = flagged;
        }
        self.search.set_flagged(account, mailbox, uid, flagged);
        true
    }

    /// The server starred or unstarred a message, asked under `uid_validity`, or refused to,
    /// which shows as the store says again; but not on a message its UID names since its
    /// mailbox started over.
    fn flagged(
        &mut self,
        key: &MessageKey,
        uid_validity: u32,
        flagged: bool,
        result: Result<(), String>,
    ) {
        let (account, mailbox, uid) = (key.0.as_str(), key.1.as_str(), key.2);
        // Answered: the store says what the server did. One star is on its way at a time.
        self.starring.remove(key);
        match result {
            Ok(()) => self.changed_for_undo(key, true, Ok(flagged)),
            Err(error) => {
                self.changed_for_undo(key, true, Err(flagged));
                let label = self.label(account).to_owned();
                let what = if flagged { "star" } else { "unstar" };
                self.notify(format!("{label}: couldn't {what} the message: {error}"));
                if self.uid_validity(account, mailbox) == Some(uid_validity) {
                    self.search.set_flagged(account, mailbox, uid, !flagged);
                    if let Some(reader) = &mut self.reader
                        && reader.is(account, mailbox, uid)
                    {
                        reader.message.envelope.flagged = !flagged;
                    }
                }
            }
        }
        self.search.star_answered(account, mailbox, uid);
        self.reload();
    }

    /// Marks a message read or unread. It's shown so right away; the store catches up once the
    /// server agrees. With `uid_validity`, the UIDVALIDITY the UID was known under, it's refused
    /// if the mailbox started over since. Returns whether it was asked of the server.
    pub(crate) fn set_seen(
        &mut self,
        account: &str,
        mailbox: &str,
        uid: u32,
        seen: bool,
        uid_validity: Option<u32>,
    ) -> bool {
        // Asked for anew, a read mark waiting to follow an earlier one isn't wanted any more.
        let key = (account.to_owned(), mailbox.to_owned(), uid);
        self.read_later.remove(&key);
        let Ok(services) = &self.services else {
            return false;
        };
        if let Err(e) =
            services
                .mailroom
                .set_seen(&services.store, account, mailbox, uid, seen, uid_validity)
        {
            self.notify(e.to_string());
            return false;
        }
        self.asked(&key, false);
        if seen {
            self.unreading.remove(&key);
        } else {
            self.unreading.insert(key);
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
        true
    }

    /// Opens the menu of a message in the list at `point`, where it was right-clicked.
    pub(crate) fn open_message_menu(
        &mut self,
        account: &str,
        mailbox: &str,
        uid: u32,
        point: PointerPress,
    ) {
        self.forget_go();
        self.message_menu = Some(MessageMenu {
            message: (account.to_owned(), mailbox.to_owned(), uid),
            point,
            move_open: false,
            snooze_open: false,
        });
    }

    pub(crate) fn close_message_menu(&mut self) {
        self.message_menu = None;
        // What autosort left alone while its menu was open may go now.
        if self.autosort_waits_for.take().is_some() {
            self.sort_mail();
        }
        // Escape in the menu doesn't reach the window: a G pressed before goes nowhere.
        self.forget_go();
    }

    /// Shows or hides the message menu's "Move to" submenu.
    pub(crate) const fn toggle_move_menu(&mut self) {
        if let Some(menu) = &mut self.message_menu {
            menu.move_open = !menu.move_open;
            menu.snooze_open = false;
        }
    }

    /// Shows or hides the message menu's "Snooze" submenu.
    pub(crate) const fn toggle_snooze_menu(&mut self) {
        if let Some(menu) = &mut self.message_menu {
            menu.snooze_open = !menu.snooze_open;
            menu.move_open = false;
        }
    }

    /// Opens the menu of a mailbox in the sidebar at `point`, where it was right-clicked.
    pub(crate) fn open_mailbox_menu(&mut self, account: &str, mailbox: &str, point: PointerPress) {
        self.forget_go();
        let grouped = self.in_open_group(account, mailbox);
        self.mailbox_menu = Some(MailboxMenu {
            mailbox: (account.to_owned(), mailbox.to_owned()),
            point,
        });
        self.hold(account, mailbox, grouped);
    }

    pub(crate) fn close_mailbox_menu(&mut self) {
        self.mailbox_menu = None;
        self.forget_go();
    }

    /// Opens the menu of an account in the sidebar at `point`, where it was right-clicked.
    pub(crate) fn open_account_menu(&mut self, account: &str, point: PointerPress) {
        self.forget_go();
        self.account_menu = Some(AccountMenu {
            account: account.to_owned(),
            point,
        });
    }

    pub(crate) fn close_account_menu(&mut self) {
        self.account_menu = None;
        self.forget_go();
    }

    /// Opens the dialog naming a mailbox to create on `account`, with nothing typed.
    pub(crate) fn open_new_mailbox(&mut self, account: &str) {
        self.close_account_menu();
        let mut name = TextEdit::new("");
        name.focused = true;
        self.new_mailbox = NewMailbox {
            open: true,
            account: account.to_owned(),
            name,
            creating: None,
            error: None,
        };
    }

    /// Closes the mailbox dialog. A mailbox already asked for is still made, and a notice says so.
    pub(crate) fn close_new_mailbox(&mut self) {
        self.new_mailbox = NewMailbox::default();
    }

    /// Asks the account's server for a mailbox called what's typed in the dialog. The dialog
    /// stays open, and says it's busy, until the server answers.
    pub(crate) fn create_mailbox(&mut self) {
        let dialog = &mut self.new_mailbox;
        let name = dialog.name.text.trim().to_owned();
        if !dialog.open || name.is_empty() || dialog.creating.is_some() {
            return;
        }
        let Ok(services) = &self.services else { return };
        match services.mailroom.create_mailbox(&dialog.account, &name) {
            Ok(()) => {
                dialog.creating = Some(name);
                dialog.error = None;
            }
            Err(e) => dialog.error = Some(e.to_string()),
        }
    }

    /// The server made the mailbox `name` asked for with [`Mail::create_mailbox`], or refused to.
    /// Made, the dialog closes and the mailbox shows once the account's mailboxes are listed
    /// again; refused, the dialog says why, so another name can be tried.
    fn mailbox_created(&mut self, account: &str, name: &str, result: Result<(), String>) {
        let d = &mut self.new_mailbox;
        let asked = d.open && d.account == account && d.creating.as_deref() == Some(name);
        match (asked, result) {
            (true, Ok(())) => self.close_new_mailbox(),
            (true, Err(e)) => {
                d.creating = None;
                d.error = Some(e);
                // Back to the name, to fix it.
                d.name.focused = true;
            }
            // The dialog was closed meanwhile.
            (false, Ok(())) => {
                let label = self.label(account).to_owned();
                self.notify(format!("{label}: created the mailbox {name}"));
            }
            (false, Err(e)) => {
                let label = self.label(account).to_owned();
                self.notify(format!("{label}: couldn't create the mailbox {name}: {e}"));
            }
        }
    }

    /// Opens the dialog confirming the mailbox `mailbox` of `account` is to be deleted. Asked to
    /// be deleted already, it says it's busy.
    pub(crate) fn open_delete_mailbox(&mut self, account: &str, mailbox: &str) {
        self.close_mailbox_menu();
        let state = self.services.as_ref().map_or(Ok(None), |services| {
            services.store.mailbox_state(account, mailbox)
        });
        let shown = self.mailbox_label(account, mailbox);
        // Without its UIDVALIDITY, another mailbox made under its name meanwhile couldn't be told.
        let (uid_validity, error) = match state {
            Ok(Some(state)) => (Some(state.uid_validity), self.snoozed_in(account, mailbox)),
            Ok(None) => (
                None,
                Some(format!("{shown} hasn't synced yet. Try again once it has.")),
            ),
            Err(e) => (
                None,
                Some(format!("Couldn't read what's known of {shown}: {e}")),
            ),
        };
        self.delete_mailbox = DeleteMailbox {
            mailbox: Some((account.to_owned(), mailbox.to_owned())),
            uid_validity,
            error,
        };
    }

    /// Closes the delete dialog. A mailbox already asked to be deleted still is, and a notice
    /// says so.
    pub(crate) fn close_delete_mailbox(&mut self) {
        self.delete_mailbox = DeleteMailbox::default();
    }

    /// Whether accounts `a` and `b` read the same mailboxes: they're one, or log in as the same
    /// user of the same IMAP server.
    fn same_login(&self, a: &str, b: &str) -> bool {
        let owner = |id: &str| self.accounts.iter().find(|x| x.id == id).map(imap_owner);
        a == b || owner(a).is_some_and(|o| owner(b) == Some(o))
    }

    /// Whether `account`'s mailbox `name` was asked to be deleted, and the server hasn't answered.
    pub(crate) fn mailbox_deleting(&self, account: &str, name: &str) -> bool {
        self.deleting_mailboxes
            .contains_key(&(account.to_owned(), name.to_owned()))
    }

    /// Why mail isn't moved to `account`'s mailbox `to`, if it's being deleted: it'd be deleted
    /// with it.
    fn deleting_destination(&self, account: &str, to: &str) -> Option<String> {
        self.mailbox_deleting(account, to).then(|| {
            let label = self.label(account);
            let shown = self.mailbox_label(account, to);
            format!("{label}: {shown} is being deleted, so nothing's moved there")
        })
    }

    /// Whether the mailbox in the delete dialog was asked to be deleted, and the server hasn't
    /// answered.
    pub(crate) fn deleting_mailbox(&self) -> bool {
        self.delete_mailbox
            .mailbox
            .as_ref()
            .is_some_and(|key| self.deleting_mailboxes.contains_key(key))
    }

    /// Whether `account`'s mailbox `name`, as last listed, may be deleted: one made for no special
    /// mail. The server checks again before it deletes.
    pub(crate) fn can_delete_mailbox(&self, account: &str, name: &str) -> bool {
        self.mailboxes
            .get(account)
            .and_then(|l| l.iter().find(|m| m.name == name))
            .is_some_and(|m| {
                m.role == Role::Other && !has_kept_name(&m.name, m.delimiter.as_deref())
            })
    }

    /// Why `account`'s mailbox `name` can't be deleted now, though it's one the app keeps no mail
    /// in: mail snoozed there is still to come back to the inbox from it.
    fn snoozed_in(&self, account: &str, name: &str) -> Option<String> {
        let Ok(services) = &self.services else {
            return None;
        };
        let shown = self.mailbox_label(account, name);
        match services.store.has_snoozed_in(account, name) {
            Ok(false) => None,
            Ok(true) => Some(format!(
                "{shown} holds snoozed mail that's still to come back to the inbox."
            )),
            Err(e) => Some(format!(
                "Couldn't tell whether {shown} holds snoozed mail: {e}"
            )),
        }
    }

    /// Asks the account's server to delete the mailbox in the dialog, and the mail in it, unless
    /// it was asked already or may no longer be. The dialog stays open, and says it's busy, until
    /// the server answers.
    pub(crate) fn delete_mailbox(&mut self) {
        let Some(key) = self.delete_mailbox.mailbox.clone() else {
            return;
        };
        let Some(uid_validity) = self.delete_mailbox.uid_validity else {
            return;
        };
        if self.delete_mailbox.error.is_some()
            || self.deleting_mailboxes.contains_key(&key)
            || !self.can_delete_mailbox(&key.0, &key.1)
        {
            return;
        }
        // Snoozed there since the dialog opened: waking it would find it gone.
        if let Some(why) = self.snoozed_in(&key.0, &key.1) {
            self.delete_mailbox.error = Some(why);
            return;
        }
        // A move out of it waiting to be sent, by any account on its login, would find it gone,
        // and the message with it.
        if self
            .deferred
            .keys()
            .any(|k| k.1 == key.1 && self.same_login(&k.0, &key.0))
        {
            let shown = self.mailbox_label(&key.0, &key.1);
            self.delete_mailbox.error = Some(format!(
                "A message is still waiting to be moved out of {shown}; try again once it's moved."
            ));
            return;
        }
        let Ok(services) = &self.services else { return };
        let id = self.next_delete;
        match services
            .mailroom
            .delete_mailbox(&key.0, &key.1, uid_validity, id)
        {
            Ok(()) => {
                self.next_delete += 1;
                let label = self.mailbox_label(&key.0, &key.1);
                self.deleting_mailboxes.insert(
                    key,
                    Deleting {
                        id,
                        uid_validity,
                        label,
                    },
                );
                self.delete_mailbox.error = None;
            }
            Err(e) => self.delete_mailbox.error = Some(e.to_string()),
        }
    }

    /// The server deleted the mailbox `name` asked for with [`Mail::delete_mailbox`], or refused
    /// to. Deleted, its dialog closes, a notice says so, the account's inbox shows if the mailbox
    /// did, and the mailbox and its cached mail go once the account's mailboxes are listed again.
    /// Refused, its dialog says why; with it closed, a notice does. An answer no longer waited
    /// for, as when its account was removed meanwhile, is dropped.
    fn mailbox_deleted(&mut self, update: Update) {
        let Update::MailboxDeleted {
            id,
            account,
            name,
            result,
        } = update
        else {
            return;
        };
        let key = (account, name);
        if self.deleting_mailboxes.get(&key).map(|d| d.id) != Some(id) {
            return;
        }
        let Some(Deleting {
            label: shown,
            uid_validity,
            ..
        }) = self.deleting_mailboxes.remove(&key)
        else {
            return;
        };
        // Listed again by whichever refresher watches the account now, as one asked before its
        // interval changed may have stopped.
        if let Ok(services) = &self.services {
            services.mailroom.refresh(&key.0);
        }
        let (account, name) = (key.0.as_str(), key.1.as_str());
        // Another mailbox made under its name since, and synced, stays shown.
        let replaced = self
            .uid_validity(account, name)
            .is_some_and(|v| v != uid_validity);
        if result.is_ok() && !replaced {
            self.leave_gone_mailbox(account, name);
        }
        // Gone, it's no place to autosort to, nor is another made under its name later.
        if result.is_ok() && self.autosort_to.contains(&key) {
            self.set_autosort(&key.0, &key.1, false);
        }
        let open = self.delete_mailbox.mailbox.as_ref() == Some(&key);
        let label = self.label(account).to_owned();
        match (open, result) {
            (true, Err(e)) => self.delete_mailbox.error = Some(e),
            (open, Ok(())) => {
                if open {
                    self.close_delete_mailbox();
                }
                self.notify(format!("{label}: deleted the mailbox {shown}"));
            }
            (false, Err(e)) => {
                self.notify(format!("{label}: deleting the mailbox {shown}: {e}"));
            }
        }
    }

    /// Stops showing `account`'s mailbox `name`, gone from the server: its message closes if one
    /// is open, and the list behind shows the account's inbox if it showed the mailbox. Nothing
    /// else changes, as Settings, the account form or a message from another mailbox open.
    fn leave_gone_mailbox(&mut self, account: &str, name: &str) {
        if self
            .reader
            .as_ref()
            .is_some_and(|r| r.message.account == account && r.message.mailbox == name)
        {
            self.leave_reader();
        }
        if !self.showing.is(account, name) {
            return;
        }
        self.switch_list(Showing::inbox(account));
    }

    /// Syncs one of an account's mailboxes with the server now, rather than waiting for the server
    /// to report a change or the next interval. What it finds shows as it arrives, and the
    /// mailbox shows it's busy until the sync ends.
    pub(crate) fn refresh_mailbox(&mut self, account: &str, mailbox: &str) {
        let Ok(services) = &self.services else { return };
        // Servers may spell the inbox in any case.
        let asked = if mailbox.eq_ignore_ascii_case(INBOX) {
            services.mailroom.refresh_inbox(account)
        } else {
            services.mailroom.refresh(account)
        };
        if let Some(asked) = asked {
            self.refreshing
                .insert((account.to_owned(), mailbox.to_owned()), asked);
        }
    }

    /// Syncs what the list shows with the server now: its mailbox, or every account's inbox or
    /// Sent.
    pub(crate) fn refresh_shown(&mut self) {
        for (account, mailbox) in self.refresh_targets() {
            self.refresh_mailbox(&account, &mailbox);
        }
    }

    /// Syncs every mailbox of every account with the server now, each showing it's busy until
    /// its sync ends.
    pub(crate) fn refresh_all(&mut self) {
        let Ok(services) = &self.services else { return };
        for account in &self.accounts {
            // Both, whatever's listed yet: the inbox's watcher syncs it regardless, and the
            // refresher's listing finds the mailboxes not listed.
            if let Some(inbox) = services.mailroom.refresh_inbox(&account.id) {
                // As INBOX, which `refreshing` matches however the server spells it.
                self.refreshing
                    .insert((account.id.clone(), INBOX.to_owned()), inbox);
            }
            let Some(rest) = services.mailroom.refresh(&account.id) else {
                continue;
            };
            self.refreshing_all.insert(account.id.clone(), rest);
            let listed = self
                .mailboxes
                .get(&account.id)
                .map_or(&[][..], Vec::as_slice);
            for mailbox in listed {
                if !mailbox.name.eq_ignore_ascii_case(INBOX) {
                    self.refreshing
                        .insert((account.id.clone(), mailbox.name.clone()), rest);
                }
            }
        }
    }

    /// Whether one of an account's mailboxes is being refreshed, as asked, waiting on the server.
    pub(crate) fn refreshing(&self, account: &str, mailbox: &str) -> bool {
        // Servers may spell the inbox in any case: the list of every inbox asks for "INBOX".
        let inbox = |name: &str| name.eq_ignore_ascii_case(INBOX);
        self.refreshing
            .keys()
            .any(|(a, m)| a == account && (m == mailbox || (inbox(m) && inbox(mailbox))))
    }

    /// What an account's watchers report: its connection's status, or that a sync of one of its
    /// mailboxes ended, or with none, of every one but its inbox ([`Update::Synced`]). Those
    /// refreshed as asked up to then no longer show they're busy.
    fn watching(&mut self, update: Update) {
        let (account, mailbox, asked) = match update {
            Update::Status { account, status } => {
                let was = self.connected(&account);
                self.statuses.insert(account.clone(), Ok(status));
                // What autosort waited for the connection for may go now.
                if !was && self.connected(&account) {
                    self.sort_mail();
                }
                return;
            }
            Update::Synced {
                account,
                mailbox,
                asked,
            } => (account, mailbox, asked),
            _ => return,
        };
        let inbox = |name: &str| name.eq_ignore_ascii_case(INBOX);
        // Unread mail older than what's cached is looked for as each inbox syncs, for its unread
        // count; the other mailboxes are looked in together once the round syncing them ends.
        if let (Ok(services), Some(synced)) = (&self.services, mailbox.as_deref())
            && inbox(synced)
        {
            // One that isn't watched looks once it syncs.
            let _ignored = services
                .mailroom
                .find_unread(&services.store, &account, synced);
        }
        if mailbox.is_none() {
            // The refresher's round ended.
            self.find_unread_each(&account);
        }
        if mailbox.is_none()
            && self
                .refreshing_all
                .get(&account)
                .is_some_and(|&n| n <= asked)
        {
            self.refreshing_all.remove(&account);
        }
        self.refreshing.retain(|(a, m), &mut n| {
            let answered = mailbox.as_deref().map_or_else(
                || !inbox(m),
                |synced| m == synced || (inbox(m) && inbox(synced)),
            );
            !(*a == account && n <= asked && answered)
        });
    }

    /// Looks in every mailbox of `account` but the inbox for its unread mail, all over one
    /// connection, for their unread counts.
    fn find_unread_each(&self, account: &str) {
        let (Ok(services), Some(mailboxes)) = (&self.services, self.mailboxes.get(account)) else {
            return;
        };
        let others: Vec<String> = mailboxes
            .iter()
            .filter(|m| m.role != Role::Inbox)
            .map(|m| m.name.clone())
            .collect();
        // An account that isn't connected looks after its next round.
        let _ignored = services
            .mailroom
            .find_unread_each(&services.store, account, &others);
    }

    /// Older unread mail was looked for in one of an account's mailboxes ([`Update::Unread`]): it
    /// isn't new, so it's only listed. Failing to look isn't shown, as the account's status says
    /// whether its server answers, and the next sync looks again; nor is what changed nothing,
    /// as every mailbox is looked in after each round. Asked for `again` meanwhile, it looks
    /// again: in the inbox, or in every other mailbox together.
    fn unread_found(&mut self, update: &Update) {
        // Counted now, even when nothing changed: none unread is known to be none. As the store
        // says, since a search the mailbox started over during counts nothing.
        if let (Update::Unread { result: Ok(_), .. }, Ok(services)) = (update, &self.services)
            && let Ok(counted) = services.store.counted_mailboxes()
        {
            self.counted = counted;
        }
        if let Update::Unread {
            account,
            mailbox,
            result: Ok(1..),
            ..
        } = update
        {
            self.reload();
            // Open, though not listed, it's read or not as the store now says.
            self.reader_flags_changed(account, mailbox);
        }
        if let Update::Unread {
            account,
            mailbox,
            again: true,
            ..
        } = update
        {
            self.recount(account, mailbox);
        }
    }

    /// Looks for unread mail again in an account's mailbox: the inbox alone, any other with the
    /// rest, together.
    fn recount(&self, account: &str, mailbox: &str) {
        if mailbox.eq_ignore_ascii_case(INBOX) {
            self.recount_one(account, mailbox);
        } else {
            self.find_unread_each(account);
        }
    }

    /// Looks for unread mail again in an account's mailbox alone: the inbox, or another over the
    /// connection the others are looked in over, after them if they're being looked in.
    fn recount_one(&self, account: &str, mailbox: &str) {
        let Ok(services) = &self.services else { return };
        // An account that isn't connected looks once it syncs.
        let _ignored = if mailbox.eq_ignore_ascii_case(INBOX) {
            services
                .mailroom
                .find_unread(&services.store, account, mailbox)
        } else {
            services
                .mailroom
                .find_unread_each(&services.store, account, &[mailbox.to_owned()])
        };
    }

    /// The accounts and mailboxes the list shows, which [`Self::refresh_shown`] syncs: searching
    /// every inbox, those, whatever's selected.
    pub(crate) fn refresh_targets(&self) -> Vec<(String, String)> {
        match self.listing() {
            Showing::Mailbox { account, name } => vec![(account.clone(), name.clone())],
            Showing::All(role) => self
                .accounts
                .iter()
                .filter_map(|account| {
                    let name = match role {
                        Role::Inbox => INBOX,
                        _ => self.mailbox_with_role(&account.id, *role)?.name.as_str(),
                    };
                    Some((account.id.clone(), name.to_owned()))
                })
                .collect(),
        }
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
        // Z mustn't undo something done before this.
        self.did(None);
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
        let answer = if all { Answer::ReplyAll } else { Answer::Reply };
        self.answer_message(account, mailbox, uid, answer);
    }

    /// Replies to or forwards a message, as [`Mail::reply_to`].
    pub(crate) fn answer_message(
        &mut self,
        account: &str,
        mailbox: &str,
        uid: u32,
        answer: Answer,
    ) {
        let open = self.reader.as_ref().filter(|r| r.is(account, mailbox, uid));
        match open.map(|r| &r.body) {
            Some(Some(Ok(_))) => return self.answer(answer),
            // Still downloading: it replies once that's done.
            Some(None) => {}
            _ => self.open_message(account, mailbox, uid),
        }
        let open = self.reader.as_ref().filter(|r| r.is(account, mailbox, uid));
        match open.map(|r| &r.body) {
            // Downloaded ahead, it opened at once.
            Some(Some(Ok(_))) => self.answer(answer),
            Some(None) => {
                self.pending_reply = Some(((account.to_owned(), mailbox.to_owned(), uid), answer));
            }
            _ => {}
        }
    }

    /// Moves a message to another of its account's mailboxes. It leaves the list at once, and
    /// comes back if the server refuses.
    pub(crate) fn move_message(&mut self, account: &str, mailbox: &str, uid: u32, to: &str) {
        self.keep_in_the_inbox(&(account.to_owned(), mailbox.to_owned(), uid), to);
        self.move_message_under(account, mailbox, uid, to, None, true);
    }

    /// Has autosort leave the message `key` names alone if it's moved to `to`, its account's
    /// inbox, by hand: it's there to be seen, as when it isn't spam after all.
    fn keep_in_the_inbox(&mut self, key: &MessageKey, to: &str) {
        let inbox = self.mailbox_with_role(&key.0, Role::Inbox);
        if inbox.is_none_or(|b| b.name != to) {
            return;
        }
        let Some(id) = self
            .find_message(key)
            .and_then(|m| m.envelope.message_id.clone())
        else {
            return;
        };
        if let Ok(services) = &self.services
            && let Err(e) = services.store.keep_from_autosort(&key.0, &id)
        {
            self.notify(format!("Couldn't keep the message from autosort: {e}"));
        }
    }

    /// Moves a message as [`Mail::move_message`] does, only while its mailbox is cached under
    /// `uid_validity`, if given: its UID names another message under another. Z undoes it if
    /// `undoable`. Returns whether it's on its way.
    fn move_message_under(
        &mut self,
        account: &str,
        mailbox: &str,
        uid: u32,
        to: &str,
        uid_validity: Option<u32>,
        undoable: bool,
    ) -> bool {
        let key = (account.to_owned(), mailbox.to_owned(), uid);
        if self.moving_back(&key) {
            return false;
        }
        if let Some(why) = self.deleting_destination(account, to) {
            // Autosort, which isn't undone, leaves it in the inbox without a word.
            if undoable {
                self.notify(why);
            }
            return false;
        }
        if self.pending(&key) {
            let uid_validity = uid_validity.or_else(|| self.uid_validity(account, mailbox));
            let deferred = (Deferred::Move(to.to_owned()), uid_validity);
            self.deferred.insert(key.clone(), deferred);
            if undoable {
                self.undo_move(&key, to, None);
            }
            self.left_list(key);
            return true;
        }
        let Ok(services) = &self.services else {
            return false;
        };
        if let Err(e) =
            services
                .mailroom
                .move_message(&services.store, account, mailbox, uid, to, uid_validity)
        {
            let label = self.label(account).to_owned();
            self.notify(format!("{label}: couldn't move the message: {e}"));
            return false;
        }
        let key = (account.to_owned(), mailbox.to_owned(), uid);
        if undoable {
            self.undo_move(&key, to, None);
        }
        self.left_list(key);
        true
    }

    /// Moves the focused message to its account's mailbox with `role`: its Archive, Trash or Junk,
    /// or its inbox for spam that isn't.
    pub(crate) fn move_focused_to(&mut self, role: Role) {
        let Some(m) = self.focused_message() else {
            return;
        };
        let (account, mailbox, uid) = (m.account.clone(), m.mailbox.clone(), m.envelope.uid);
        let Some(to) = self
            .mailbox_with_role(&account, role)
            .map(|b| b.name.clone())
        else {
            return;
        };
        if to != mailbox {
            self.move_message(&account, &mailbox, uid, &to);
        }
    }

    /// Learns from the mail cached again, in the background, if mail is sorted: the inbox's tags
    /// change once that's done.
    fn sort_mail(&self) {
        if let Ok(Services {
            sorter: Some(sorter),
            ..
        }) = &self.services
            && self.settings.sort_mail
        {
            sorter.sort();
        }
    }

    /// Sorting said this of the inbox, by what it learned in `generation`: shown unless sorting
    /// was turned off meanwhile, or what it learned was forgotten or replaced, as far as can be
    /// read.
    ///
    /// Only the tags of mailboxes still cached under the same UIDVALIDITY are kept: of one
    /// started over since, their UIDs may name other messages.
    pub(crate) fn sorted(&mut self, generation: u64, sorted: Sorted) {
        let current = self
            .services
            .as_ref()
            .ok()
            .and_then(|s| s.store.sorting_generation().ok());
        if !self.settings.sort_mail || current.is_some_and(|c| c != generation) {
            return;
        }
        let mut same: HashMap<(String, String), bool> = HashMap::new();
        let mut tags = sorted.tags;
        tags.retain(|(account, mailbox, _), _| {
            let mailbox = (account.clone(), mailbox.clone());
            *same
                .entry(mailbox)
                .or_insert_with_key(|(account, mailbox)| {
                    let then = sorted.uid_validity.get(&(account.clone(), mailbox.clone()));
                    self.uid_validity(account, mailbox)
                        .is_none_or(|now| then == Some(&now))
                })
        });
        // Its menu open as this came, a message stays where the user was looking at it, even
        // once the menu closes below.
        let menu = self.message_menu.as_ref().map(|m| m.message.clone());
        // A menu showing a suggestion that changed would act on another item where it was.
        if let Some(menu) = &self.message_menu
            && self
                .tags
                .get(&menu.message)
                .and_then(|t| t.suggested.as_ref())
                != tags.get(&menu.message).and_then(|t| t.suggested.as_ref())
        {
            self.close_message_menu();
        }
        self.tags = tags;
        self.tagged_under = sorted.uid_validity;
        self.autosort(sorted.autosort, menu.as_ref());
    }

    /// What sorting says of a message: where it seems to belong, and whether it seems important.
    /// A suggestion to move it to where it is already, as once moved meanwhile, isn't one.
    pub(crate) fn tags(&self, key: &MessageKey) -> Option<&Tags> {
        self.tags
            .get(key)
            .filter(|t| t.important || t.suggested.as_ref().is_some_and(|to| *to != key.1))
    }

    /// Where sorting suggests a message goes, if anywhere.
    pub(crate) fn suggested(&self, key: &MessageKey) -> Option<&str> {
        self.tags(key)?
            .suggested
            .as_deref()
            .filter(|to| *to != key.1)
    }

    /// Moves a message to `to`, shown as where sorting suggests it goes, while that's still what
    /// it suggests, and only while the message's mailbox is cached as when it was sorted.
    pub(crate) fn move_as_suggested(&mut self, key: &MessageKey, to: &str) {
        if self.suggested(key) != Some(to) {
            return;
        }
        let under = self
            .tagged_under
            .get(&(key.0.clone(), key.1.clone()))
            .copied();
        self.move_message_under(&key.0, &key.1, key.2, to, under, true);
    }

    /// Moves by themselves the inbox messages sorting says go to a mailbox autosorted to, while
    /// that's still what it says: each once. Not one the user is acting on, which waits for a
    /// later pass: open to be read (until it's closed), already moving or snoozing, or with a
    /// read mark or star on its way, which a move would wait for. Z doesn't undo them: they're
    /// not what was last done by hand.
    fn autosort(&mut self, autosort: Vec<Autosort>, menu: Option<&MessageKey>) {
        let open = self.reader.as_ref().map(|r| message_key(&r.message));
        for a in autosort {
            let (account, mailbox, uid) = &a.key;
            let wanted = self.autosort_to.contains(&(account.clone(), a.to.clone()));
            if !wanted || self.suggested(&a.key) != Some(a.to.as_str()) {
                continue;
            }
            // Not under the user's eyes: open, or its menu open.
            if open.as_ref() == Some(&a.key) || menu == Some(&a.key) {
                self.autosort_waits_for = Some(a.key);
                continue;
            }
            // Offline, the move would fail, and autosort tries once: it waits to be connected.
            // Nor one being marked read with the rest of its mailbox: once that's answered.
            if !self.connected(account)
                || self.marking_seen(account, mailbox)
                || self.moving.contains(&a.key)
                || self.deferred.contains_key(&a.key)
                || self.pending(&a.key)
            {
                continue;
            }
            // A few at a time, each on a connection of its own: the rest go once they're
            // answered, which sorts again.
            let under_way = self.autosorting.keys().filter(|k| k.0 == *account);
            if under_way.count() >= AUTOSORT_AT_ONCE {
                continue;
            }
            let Ok(services) = &self.services else { return };
            // Only to a mailbox mail is still filed in, as last listed: as the move reads it.
            let filed_in = services.store.mailboxes(account).is_ok_and(|list| {
                list.iter()
                    .any(|m| m.name == a.to && postden_app::is_filed_in(&m.name, m.role))
            });
            if !filed_in {
                continue;
            }
            match services
                .store
                .note_autosorted(account, &a.message_id, &a.to)
            {
                Ok(true) => {}
                Ok(false) => continue,
                Err(e) => {
                    self.notify(format!("Couldn't autosort mail: {e}"));
                    return;
                }
            }
            let under = self
                .tagged_under
                .get(&(account.clone(), mailbox.clone()))
                .copied();
            self.autosorting.insert(a.key.clone(), a.message_id);
            // Selected, it leaves no selection behind rather than the next row's: a key pressed
            // for it mustn't act on another message.
            let selected = self.cursor.take_if(|c| *c == a.key);
            // Not sent, it stays in the inbox, tagged, with why it wasn't moved said instead.
            if !self.move_message_under(account, mailbox, *uid, &a.to, under, false) {
                self.autosort_answered(&a.key, false);
                self.cursor = self.cursor.take().or(selected);
            }
        }
    }

    /// The server moved a message, or refused to: if autosort was moving it, notes so, and
    /// returns true. Refused, it was never autosorted, and may be later.
    fn autosort_answered(&mut self, key: &MessageKey, moved: bool) -> bool {
        let Some(message_id) = self.autosorting.remove(key) else {
            return false;
        };
        let Ok(services) = &self.services else {
            return true;
        };
        if let Err(e) = services.store.autosort_answered(&key.0, &message_id, moved) {
            self.notify(format!("Couldn't note what autosort did: {e}"));
        }
        true
    }

    /// Whether `account`'s watcher is connected to its server.
    fn connected(&self, account: &str) -> bool {
        matches!(
            self.statuses.get(account),
            Some(Ok(Status::Push | Status::Polling(_)))
        )
    }

    /// Reads which mailboxes inbox mail is autosorted to.
    fn load_autosort(&mut self) {
        let Ok(services) = &self.services else { return };
        match services.store.autosort_mailboxes() {
            Ok(to) => self.autosort_to = to,
            Err(e) => self.notify(format!("Couldn't read where mail is autosorted to: {e}")),
        }
    }

    /// Whether inbox mail is moved to `account`'s `mailbox` by itself once sorting says it goes
    /// there.
    pub(crate) fn autosorts_to(&self, account: &str, mailbox: &str) -> bool {
        self.autosort_to
            .contains(&(account.to_owned(), mailbox.to_owned()))
    }

    /// Has inbox mail sorting says goes to `account`'s `mailbox` moved there by itself, or
    /// stops. Mail already waiting in the inbox that goes there is moved once sorting next says
    /// so.
    pub(crate) fn set_autosort(&mut self, account: &str, mailbox: &str, on: bool) {
        let Ok(services) = &self.services else { return };
        if let Err(e) = services.store.set_autosort(account, mailbox, on) {
            self.notify(format!("Couldn't change where mail is autosorted to: {e}"));
            return;
        }
        let key = (account.to_owned(), mailbox.to_owned());
        if on {
            self.autosort_to.insert(key);
            self.sort_mail();
        } else {
            self.autosort_to.remove(&key);
        }
    }

    /// What Z will undo once the message `from` names has been moved to `to`, or snoozed until
    /// `snoozed`: moving it back, if it has a Message-ID to find it by; otherwise nothing, as
    /// what it would have undone isn't the last thing done any more.
    pub(crate) fn undo_move(&mut self, from: &MessageKey, to: &str, snoozed: Option<i64>) {
        let message_id = self
            .find_message(from)
            .and_then(|m| m.envelope.message_id.clone())
            .filter(|id| postden_app::is_searchable(id));
        self.did(message_id.map(|message_id| Undo::Moved {
            from: from.clone(),
            to: to.to_owned(),
            message_id,
            snoozed,
            done: false,
        }));
    }

    /// The server answered the move of the message `key` named: moved, Z can move it back; not,
    /// there's nothing to undo. Returns whether Z undoes it.
    pub(crate) fn moved_for_undo(&mut self, key: &MessageKey, moved: bool) -> bool {
        let Some(Undo::Moved { from, done, .. }) = &mut self.undo else {
            return false;
        };
        if from != key {
            return false;
        }
        if moved {
            *done = true;
        } else {
            self.undo = None;
        }
        moved
    }

    /// Whether a message moved from `account`'s mailbox `mailbox` to `to` can't be moved back yet,
    /// said in a notice: a change to its copy in `to` still on its way, or a move of it waiting
    /// for one, would be lost once it's moved, whichever message of that mailbox it's to; and
    /// `mailbox` being deleted would take it with it.
    fn restore_waits(&mut self, account: &str, mailbox: &str, to: &str) -> bool {
        let busy = |key: &MessageKey| key.0 == account && key.1 == to;
        if self.unanswered.keys().any(|(key, _)| busy(key)) || self.deferred.keys().any(busy) {
            self.notify("Still waiting for the server about a message there".into());
            return true;
        }
        if let Some(why) = self.deleting_destination(account, mailbox) {
            self.notify(why);
            return true;
        }
        false
    }

    /// Undoes the last thing done to a message, as Z does: moves it back, marks it read or unread
    /// again, or gives back or takes away its star.
    pub(crate) fn undo(&mut self) {
        let Some(undo) = self.undo.clone() else {
            return;
        };
        match undo {
            Undo::Moved { done: false, .. } => {
                self.notify("Still moving the message; undo once it's moved".into());
            }
            Undo::Seen { done: false, .. } | Undo::Flagged { done: false, .. } => {
                self.notify("Still waiting for the server; undo once it's answered".into());
            }
            Undo::Moved {
                from,
                to,
                message_id,
                snoozed,
                done: true,
            } => {
                let (account, mailbox, _) = &from;
                if self.restore_waits(account, mailbox, &to) {
                    return;
                }
                // Back in the inbox by hand, it's there to be seen, not autosorted away again.
                let to_inbox = self
                    .mailbox_with_role(account, Role::Inbox)
                    .is_some_and(|b| b.name == *mailbox);
                if to_inbox
                    && let Ok(services) = &self.services
                    && let Err(e) = services.store.keep_from_autosort(account, &message_id)
                {
                    self.notify(format!("Couldn't keep the message from autosort: {e}"));
                }
                let Ok(services) = &self.services else { return };
                if let Err(e) = services.mailroom.restore(
                    &services.store,
                    account,
                    &to,
                    &message_id,
                    mailbox,
                    snoozed,
                ) {
                    let label = self.label(account).to_owned();
                    self.notify(format!("{label}: couldn't move the message back: {e}"));
                    return;
                }
                self.undoing.extend(self.undo.take());
                self.leaving.push(Restoring {
                    account: account.clone(),
                    mailbox: to,
                    message_id: message_id.clone(),
                });
                let restoring = Restoring {
                    account: account.clone(),
                    mailbox: mailbox.clone(),
                    message_id,
                };
                // Only mail arriving in an inbox is announced.
                if self
                    .mailbox_with_role(account, Role::Inbox)
                    .is_some_and(|b| b.name == *mailbox)
                {
                    self.returning.push(restoring.clone());
                }
                self.restoring = Some(restoring);
                let back = self.mailbox_label(account, mailbox);
                self.notify(format!("Moving the message back to {back}…"));
            }
            Undo::Seen {
                message,
                uid_validity,
                seen,
                ..
            } => {
                if self.still_saving(&message, false) {
                    return;
                }
                self.undo = None;
                if self.still_there(&message, uid_validity) {
                    let (account, mailbox, uid) = (&message.0, &message.1, message.2);
                    self.set_seen(account, mailbox, uid, seen, Some(uid_validity));
                }
            }
            Undo::Flagged {
                message,
                uid_validity,
                flagged,
                ..
            } => {
                if self.still_saving(&message, true) {
                    return;
                }
                self.undo = None;
                if self.still_there(&message, uid_validity) {
                    let (account, mailbox, uid) = (&message.0, &message.1, message.2);
                    self.set_flagged(account, mailbox, uid, flagged, Some(uid_validity));
                }
            }
        }
    }

    /// Whether the mailbox of the message `key` names is still cached under `uid_validity`, so
    /// its UID still names it; if not, a notice says it can't be undone.
    fn still_there(&mut self, key: &MessageKey, uid_validity: u32) -> bool {
        if self.uid_validity(&key.0, &key.1) == Some(uid_validity) {
            return true;
        }
        let name = self.mailbox_label(&key.0, &key.1);
        self.notify(format!(
            "{name} changed on the server, so this can't be undone"
        ));
        false
    }

    /// What Z undoes, as the palette offers it.
    pub(crate) fn undo_title(&self) -> String {
        match &self.undo {
            Some(Undo::Moved {
                snoozed: Some(_), ..
            }) => "Undo snooze".into(),
            Some(Undo::Moved { from, to, .. }) => {
                format!("Undo move to {}", self.mailbox_label(&from.0, to))
            }
            Some(Undo::Seen { seen: true, .. }) => "Undo mark as unread".into(),
            Some(Undo::Seen { .. }) => "Undo mark as read".into(),
            Some(Undo::Flagged { flagged: true, .. }) => "Undo remove star".into(),
            Some(Undo::Flagged { .. }) | None => "Undo star".into(),
        }
    }

    /// The server marked a message read or unread, starred or unstarred it, or moved it back,
    /// or refused to.
    fn message_changed(&mut self, update: Update) {
        match update {
            Update::Seen {
                account,
                mailbox,
                uid,
                seen,
            } => {
                // Marked unread and not cached, as a server search's: it counts once the server
                // says it's there.
                let uncached = |s: &Services| {
                    s.store
                        .seen(&account, &mailbox, uid)
                        .is_ok_and(|c| c.is_none())
                };
                if !seen && self.services.as_ref().is_ok_and(uncached) {
                    self.recount_one(&account, &mailbox);
                }
                self.search.settle(&account, &mailbox, uid, Some(seen));
                self.changed_for_undo(&(account, mailbox, uid), false, Ok(seen));
                self.reload();
            }
            Update::SeenFailed {
                account,
                mailbox,
                uid,
                seen,
                error,
            } => {
                let key = (account, mailbox, uid);
                self.changed_for_undo(&key, false, Err(seen));
                self.seen_failed(&key.0, &key.1, uid, &error);
            }
            Update::FlagsChanged { account, mailbox } => {
                self.reader_flags_changed(&account, &mailbox);
                self.reload();
            }
            Update::Flagged {
                account,
                mailbox,
                uid_validity,
                uid,
                flagged,
                result,
            } => {
                let key = (account, mailbox, uid);
                self.flagged(&key, uid_validity, flagged, result);
            }
            Update::Restored {
                account,
                mailbox,
                message_id,
                uids,
                to,
                result,
            } => {
                let back = Restoring {
                    account,
                    mailbox: to,
                    message_id,
                };
                self.restored(&back, &mailbox, &uids, result);
            }
            _ => {}
        }
    }

    /// The server moved the message `back` names from `mailbox`, where it had `uids`, back where
    /// it was, as asked by [`Mail::undo`], or refused to.
    fn restored(
        &mut self,
        back: &Restoring,
        mailbox: &str,
        uids: &[u32],
        result: Result<bool, String>,
    ) {
        let account = back.account.as_str();
        for &uid in uids {
            self.search.forget_message(account, mailbox, uid);
        }
        self.leaving.retain(|r| {
            !(r.account == back.account && r.mailbox == mailbox && r.message_id == back.message_id)
        });
        // Its copy there is gone: one open, or under the cursor, isn't shown any more.
        let left = |a: &str, m: &str, uid: u32| a == account && m == mailbox && uids.contains(&uid);
        if self.reader.as_ref().is_some_and(|r| {
            left(
                &r.message.account,
                &r.message.mailbox,
                r.message.envelope.uid,
            )
        }) {
            self.leave_reader();
        }
        if self.cursor.as_ref().is_some_and(|c| left(&c.0, &c.1, c.2)) {
            self.cursor = None;
        }
        let undone = self
            .undoing
            .extract_if(.., |u| {
                matches!(u, Undo::Moved { from, message_id, .. }
                    if from.0 == back.account && from.1 == back.mailbox
                        && *message_id == back.message_id)
            })
            .next();
        let to = self.mailbox_label(account, &back.mailbox);
        let from = self.mailbox_label(account, mailbox);
        match result {
            Ok(true) => self.notify(format!("Moved back to {to}")),
            Ok(false) => {
                self.forget_restoring(back);
                self.notify(format!("The message is no longer in {from}"));
            }
            Err(error) => {
                self.forget_restoring(back);
                // Z tries again, unless something else was done since.
                if self.undo.is_none() {
                    self.undo = undone;
                }
                let label = self.label(account).to_owned();
                self.notify(format!(
                    "{label}: couldn't move the message back to {to}: {error}"
                ));
            }
        }
        self.reload();
    }

    /// Forgets a message being moved back, which won't arrive.
    fn forget_restoring(&mut self, back: &Restoring) {
        self.returning.retain(|r| r != back);
        if self.restoring.as_ref() == Some(back) {
            self.restoring = None;
        }
    }

    /// Puts the list's cursor on a message moved back by undoing, once the list shows it.
    fn follow_restored(&mut self, tx: &mut Tx) {
        let Some(r) = &self.restoring else {
            return;
        };
        let found = self
            .shown()
            .find(|m| {
                m.account == r.account
                    && m.mailbox == r.mailbox
                    && m.envelope.message_id.as_deref() == Some(&r.message_id)
            })
            .map(message_key);
        if let Some(key) = found {
            tx.scroll_into_view(row_key(&key));
            if self.reader.is_none() {
                self.cursor = Some(key);
            }
            self.restoring = None;
        }
    }

    /// Snoozes a message until `until`: it leaves the list at once, as when moved, and comes back
    /// to the inbox unread once it's due.
    pub(crate) fn snooze_message(&mut self, message: &StoredMessage, until: Timestamp) {
        let (account, mailbox) = (message.account.as_str(), message.mailbox.as_str());
        let key = message_key(message);
        if self.moving_back(&key) {
            return;
        }
        let to = self
            .mailbox_with_role(account, Role::Snoozed)
            .map_or(postden_mail::imap::SNOOZED_NAME, |b| b.name.as_str())
            .to_owned();
        // Back in the inbox when it wakes, it's there to be seen, not autosorted away.
        if let (Ok(services), Some(id)) = (&self.services, &message.envelope.message_id)
            && let Err(e) = services.store.keep_from_autosort(account, id)
        {
            self.notify(format!("Couldn't keep the message from autosort: {e}"));
        }
        if self.pending(&key) {
            let deferred = Deferred::Snooze(Box::new(message.clone()), until);
            let uid_validity = self.uid_validity(account, mailbox);
            self.deferred.insert(key.clone(), (deferred, uid_validity));
            self.undo_move(&key, &to, Some(until.as_second()));
            self.left_list(key);
            return;
        }
        let Ok(services) = &self.services else { return };
        if let Err(e) = services.mailroom.snooze(
            &services.store,
            account,
            mailbox,
            &message.envelope,
            until.as_second(),
            None,
        ) {
            let label = self.label(account).to_owned();
            self.notify(format!("{label}: couldn't snooze the message: {e}"));
            return;
        }
        self.undo_move(&key, &to, Some(until.as_second()));
        self.left_list(key);
    }

    /// The server moved a message asked for with [`Mail::snooze_message`] to its Snoozed mailbox,
    /// or refused to, which lists it where it was again; or a snoozed message came back.
    fn snoozed(&mut self, update: Update) {
        let Update::Snoozed {
            account,
            mailbox,
            uid,
            until,
            result,
            ..
        } = update
        else {
            // Back in the inbox, it arrives there as new mail; what a search found where it
            // waited is gone. Failing, it's tried again soon.
            if let Update::Woke {
                account,
                mailbox,
                uids,
                result: Ok(()),
            } = update
            {
                for uid in uids {
                    self.search.forget_message(&account, &mailbox, uid);
                }
                self.reload();
            }
            return;
        };
        let (account, mailbox) = (account.as_str(), mailbox.as_str());
        let key = (account.to_owned(), mailbox.to_owned(), uid);
        self.moving.remove(&key);
        let undoable = self.moved_for_undo(&key, result.is_ok());
        match result {
            Ok(current) => {
                if current {
                    self.search.forget_message(account, mailbox, uid);
                }
                let until = Timestamp::from_second(until).unwrap_or_default();
                self.notify(format!(
                    "Snoozed until {}{}",
                    postden_app::format_snooze(until, &Zoned::now()),
                    undo_hint(undoable)
                ));
            }
            Err(error) => {
                let label = self.label(account).to_owned();
                self.notify(format!(
                    "{label}: couldn't move the message to be snoozed: {error}"
                ));
            }
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
    pub(crate) fn mailbox_label(&self, account: &str, name: &str) -> String {
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
    fn inbox_mail(
        &mut self,
        target: Target,
        count: usize,
        first_sync: bool,
        unread: &[Envelope],
        message_ids: &[String],
    ) {
        // Messages moved back by undoing aren't new.
        let mut unread = unread.to_vec();
        let back: Vec<Restoring> = self
            .returning
            .extract_if(.., |r| {
                r.account == target.account
                    && r.mailbox == target.mailbox
                    && message_ids.contains(&r.message_id)
            })
            .collect();
        unread.retain(|e| {
            !back
                .iter()
                .any(|r| e.message_id.as_ref() == Some(&r.message_id))
        });
        let count = count.saturating_sub(back.len());
        self.reload();
        if !back.is_empty() && count == 0 && !first_sync {
            return;
        }
        let label = self.label(&target.account).to_owned();
        self.notify(postden_app::mail_notice(
            &label,
            count,
            first_sync,
            &Zoned::now(),
        ));
        self.announce(&label, target, &unread);
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
        // What the palette offered was for what the window showed, which this replaces.
        self.close_palette();
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
        self.unread
            .iter()
            .filter(|((a, m), _)| account.is_none_or(|id| a == id) && m.eq_ignore_ascii_case(INBOX))
            .map(|(_, n)| n)
            .sum()
    }

    /// How many messages are unread in the mailboxes the list shows.
    pub(crate) fn listed_unread(&self) -> usize {
        self.listed
            .iter()
            .map(|(account, name)| self.mailbox_unread(account, name))
            .sum()
    }

    /// How many messages in an account's mailbox are unread.
    pub(crate) fn mailbox_unread(&self, account: &str, mailbox: &str) -> usize {
        self.unread
            .get(&(account.to_owned(), mailbox.to_owned()))
            .copied()
            .unwrap_or_default()
    }

    /// Shows or hides an account's mailboxes in the sidebar.
    pub(crate) fn toggle_mailboxes(&mut self, account: &str) {
        if !self.collapsed.remove(account) {
            self.collapsed.insert(account.to_owned());
        }
    }

    /// An account's mailboxes as the sidebar lists them: the inbox first, then the rest; and,
    /// while the settings say to group them, apart from those the mailboxes with nothing unread.
    /// A mailbox not counted yet isn't known to have none, and the inbox always stays out of the
    /// group.
    pub(crate) fn sidebar_mailboxes(&self, account: &str) -> (Vec<&Mailbox>, Vec<&Mailbox>) {
        let listed = self.mailboxes.get(account).map_or(&[][..], Vec::as_slice);
        let (inbox, rest): (Vec<_>, Vec<_>) = listed.iter().partition(|m| m.role == Role::Inbox);
        if !self.settings.group_read_mailboxes {
            return (inbox.into_iter().chain(rest).collect(), Vec::new());
        }
        let (read, unread): (Vec<_>, Vec<_>) = rest
            .into_iter()
            .partition(|m| self.grouped(account, &m.name));
        (inbox.into_iter().chain(unread).collect(), read)
    }

    /// Whether an account's mailbox other than the inbox is in its group of mailboxes with
    /// nothing unread: counted, with none. The one shown and the one whose menu is open stay
    /// where they were when picked, so a row doesn't leave from under the pointer as its last
    /// message is read, or arrives; and out of the group while it's closed, to stay in sight.
    fn grouped(&self, account: &str, mailbox: &str) -> bool {
        let key = (account.to_owned(), mailbox.to_owned());
        if self.picked(account, mailbox) {
            return self.read_mailboxes_open(account) && self.held.get(&key) == Some(&true);
        }
        self.counted.contains(&key) && self.mailbox_unread(account, mailbox) == 0
    }

    /// Whether an account's mailbox is listed in its open group of mailboxes with nothing unread,
    /// as the sidebar shows it now.
    fn in_open_group(&self, account: &str, mailbox: &str) -> bool {
        self.settings.group_read_mailboxes
            && self.read_mailboxes_open(account)
            && self.grouped(account, mailbox)
    }

    /// Whether an account's mailbox is the one shown, or the one whose menu is open.
    fn picked(&self, account: &str, mailbox: &str) -> bool {
        self.showing.is(account, mailbox)
            || self
                .mailbox_menu
                .as_ref()
                .is_some_and(|menu| menu.is(account, mailbox))
    }

    /// Keeps a mailbox just picked, shown or with its menu open, in the open group of mailboxes
    /// with nothing unread or out of it, as `grouped` says it was; and forgets those no longer
    /// picked.
    fn hold(&mut self, account: &str, mailbox: &str, grouped: bool) {
        let held = std::mem::take(&mut self.held);
        self.held = held
            .into_iter()
            .filter(|((a, m), _)| self.picked(a, m))
            .collect();
        self.held
            .insert((account.to_owned(), mailbox.to_owned()), grouped);
    }

    /// Whether an account's group of mailboxes with nothing unread is open in the sidebar.
    pub(crate) fn read_mailboxes_open(&self, account: &str) -> bool {
        self.read_open.contains(account)
    }

    /// Opens or closes an account's group of mailboxes with nothing unread in the sidebar.
    pub(crate) fn toggle_read_mailboxes(&mut self, account: &str) {
        if !self.read_open.remove(account) {
            self.read_open.insert(account.to_owned());
        }
    }

    /// Keeps each account's mailboxes with nothing unread in a group of their own in the
    /// sidebar, or lists them with the rest again.
    pub(crate) fn set_group_read_mailboxes(&mut self, on: bool) {
        if self.settings.group_read_mailboxes == on {
            return;
        }
        self.settings.group_read_mailboxes = on;
        self.save_settings();
    }

    /// Lists `showing`. A mailbox other than an inbox is also refreshed from the server, as those
    /// are otherwise only synced every few minutes.
    pub(crate) fn show(&mut self, showing: Showing) {
        self.form.open = false;
        self.settings_open = false;
        self.leave_reader();
        self.confirm_remove = None;
        if let (Ok(services), Showing::Mailbox { account, name }) = (&self.services, &showing)
            && name != INBOX
        {
            services.mailroom.refresh(account);
        }
        if self.showing == showing {
            // A message being moved back no longer takes the cursor once it's listed.
            self.restoring = None;
            // Shown again, as going back from a message.
            self.forget_read();
        } else {
            self.switch_list(showing);
        }
    }

    /// Lists what `showing` says behind whatever's open in front of the list, which stays.
    fn switch_list(&mut self, showing: Showing) {
        // A message being moved back no longer takes the cursor once it's listed.
        self.restoring = None;
        // A search goes on in the mailbox now shown, unless it looks through every inbox.
        if !self.search.everywhere {
            self.search.forget_server();
        }
        let grouped = match &showing {
            Showing::Mailbox { account, name } => Some((
                account.clone(),
                name.clone(),
                self.in_open_group(account, name),
            )),
            Showing::All(_) => None,
        };
        self.showing = showing;
        if let Some((account, name, grouped)) = grouped {
            self.hold(&account, &name, grouped);
        }
        self.list_limit = LIST_LIMIT;
        self.reload();
        self.find_unread_listed();
    }

    /// Showing only unread mail, looks for unread mail in each mailbox listed not looked in yet:
    /// an inbox alone, the others together. An inbox looks again as it syncs, the others after
    /// each round syncing them.
    fn find_unread_listed(&self) {
        let Ok(services) = &self.services else { return };
        if !self.settings.unread_only {
            return;
        }
        let mut others: Vec<&str> = Vec::new();
        for (account, name) in &self.listed {
            if services
                .store
                .unread_searched(account, name)
                .unwrap_or(true)
            {
                continue;
            }
            if name.eq_ignore_ascii_case(INBOX) {
                // An account that isn't connected looks once it syncs.
                let _ignored = services
                    .mailroom
                    .find_unread(&services.store, account, name);
            } else if !others.contains(&account.as_str()) {
                others.push(account);
            }
        }
        // The others are looked in together, as after a round.
        for account in others {
            self.find_unread_each(account);
        }
    }

    /// Whether every mailbox [`Mail::showing`] lists was synced once, and searched for unread
    /// mail older than what's cached: each account's with its role, or the one mailbox. Until
    /// then, an empty list of unread mail isn't an inbox at zero.
    pub(crate) fn listing_synced(&self) -> bool {
        let Ok(services) = &self.services else {
            return false;
        };
        let searched = |account: &str, name: &str| {
            services
                .store
                .unread_searched(account, name)
                .unwrap_or(false)
        };
        match &self.showing {
            Showing::Mailbox { account, name } => {
                services
                    .store
                    .mailbox_state(account, name)
                    .is_ok_and(|state| state.is_some())
                    && searched(account, name)
            }
            // An account listed without such a mailbox has none to wait for.
            Showing::All(role) => self.accounts.iter().all(|a| {
                let none = self
                    .mailboxes
                    .get(&a.id)
                    .is_some_and(|list| !list.is_empty() && list.iter().all(|m| m.role != *role));
                none || services
                    .store
                    .mailbox_for_role(&a.id, *role)
                    .is_ok_and(|found| found.is_some_and(|(name, _)| searched(&a.id, &name)))
            }),
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
    /// Showing only unread mail, that's only more cached: the cache holds the newest of what's
    /// unread, and the unread counts can say more than it lists.
    pub(crate) fn has_older(&self) -> bool {
        if self.search.is_active() || self.settings.unread_only {
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
            if self.messages.len() > listed || self.search.is_active() || self.settings.unread_only
            {
                return;
            }
        }
        if self.search.is_active() || self.settings.unread_only {
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

    /// Opens the add-account form, noting which other mail apps it can set accounts up from.
    pub(crate) fn open_form(&mut self) {
        self.close_reader();
        self.settings_open = false;
        self.form = AccountForm {
            open: true,
            imports: Imports {
                apps: import::installed_here().iter().map(|s| s.app()).collect(),
                session: FORM_OPENINGS.fetch_add(1, Ordering::Relaxed),
                ..Imports::default()
            },
            ..AccountForm::default()
        };
    }

    /// Moves the add-account form to `step`, starting its fields afresh. The accounts other
    /// apps have stay as read; they're read the first time [`Step::Import`] shows.
    pub(crate) fn form_step(&mut self, step: Step) {
        let imports = std::mem::take(&mut self.form.imports);
        self.form = AccountForm {
            open: true,
            step,
            imports,
            ..AccountForm::default()
        };
        self.form.imports.picked = None;
        self.form.email.focused = step == Step::Google || step == Step::Servers;
        if step == Step::Import && self.form.imports.found.is_none() {
            self.read_imports();
        }
    }

    /// Back one screen: from an app's accounts to picking an app; from setting up an account
    /// to the accounts of the app it was picked from, when it was; else to choosing how to
    /// connect.
    pub(crate) fn form_back(&mut self) {
        if self.form.step == Step::Import && self.form.imports.app.is_some() {
            self.form.imports.app = None;
            return;
        }
        let step = if self.form.imports.picked.is_some() {
            Step::Import
        } else {
            Step::Choose
        };
        self.form_step(step);
    }

    /// Shows the other mail apps on this computer, to pick one.
    pub(crate) fn open_imports(&mut self) {
        self.form.imports.app = None;
        self.form_step(Step::Import);
    }

    /// Shows the accounts `app`, one of the other mail apps, has.
    pub(crate) fn pick_import_app(&mut self, app: &'static str) {
        if self.form.imports.apps.contains(&app) {
            self.form.imports.app = Some(app);
        }
    }

    /// Opens System Settings where Postden can be given Full Disk Access. macOS can't be asked
    /// for it any other way.
    pub(crate) fn open_full_disk_access(&mut self) {
        if let Err(e) = import::open_full_disk_access_settings() {
            self.form.error = Some(format!("Couldn't open System Settings: {e}"));
        }
    }

    /// The window is in front again: maybe from System Settings, with Full Disk Access given
    /// to read an app's accounts that needed it, so they're read again.
    pub(crate) fn window_focused(&mut self) {
        let imports = &self.form.imports;
        let waiting = imports
            .found
            .iter()
            .flatten()
            .any(|(_, read)| matches!(read, Err(Unreadable::NeedsFullDiskAccess)));
        if self.form.open && self.form.step == Step::Import && waiting {
            self.reread_imports();
        }
    }

    /// Reads the other apps' accounts again, as after Full Disk Access was given.
    pub(crate) fn reread_imports(&mut self) {
        if self.form.imports.reading {
            return;
        }
        self.form.imports.found = None;
        self.form.error = None;
        self.read_imports();
    }

    /// Reads, in the background, the accounts each mail app found on this computer has, once
    /// for each opening of the form.
    fn read_imports(&mut self) {
        let (Ok(services), Some(handle)) = (&self.services, &self.handle) else {
            return;
        };
        let imports = &mut self.form.imports;
        if imports.reading {
            return;
        }
        imports.reading = true;
        let (session, handle) = (imports.session, handle.clone());
        services.mailroom.handle().spawn_blocking(move || {
            let found: Vec<_> = import::installed_here()
                .iter()
                .map(|source| {
                    let accounts = source.accounts().map_err(Unreadable::from);
                    (source.app(), accounts)
                })
                .collect();
            handle.update(move |s: &mut Self, _| s.imports_read(session, found));
        });
    }

    /// What the other apps have, as read for the form's opening `session`. Not taken when the
    /// form was opened again meanwhile: that opening reads them anew.
    fn imports_read(&mut self, session: u64, found: Vec<AppAccounts>) {
        let imports = &mut self.form.imports;
        if imports.session == session {
            imports.found = Some(found);
            imports.reading = false;
        }
    }

    /// Sets up `address`, one of the accounts another app has: the form shows its screen,
    /// filled in with what that app has, for its password or a Google sign-in. Not when it's
    /// set up already, as saving it would replace that one.
    pub(crate) fn set_up_found(&mut self, address: &str) {
        if is_set_up(&self.accounts, address) {
            return;
        }
        let Some(found) = self.form.imports.find(address).cloned() else {
            return;
        };
        let mut form = AccountForm {
            open: true,
            imports: std::mem::take(&mut self.form.imports),
            ..AccountForm::default()
        };
        let Some(step) = form.fill(&found) else {
            self.form.imports = form.imports;
            return;
        };
        form.step = step;
        form.imports.picked = Some(found.address);
        // The servers are filled in: what's left to type is the password.
        form.password.focused = step == Step::Servers;
        form.name.focused = step == Step::Google;
        self.form = form;
    }

    /// Submits the form's current screen: signs in with Google, or checks the typed-in login.
    pub(crate) fn submit_form(&mut self, _: &mut Tx) {
        match self.form.step {
            Step::Choose | Step::Import => {}
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
            Ok(account) => Account {
                label: self.form.label.clone(),
                ..account
            },
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
            Ok((account, login)) => {
                let label = self.form_label(&account.id);
                self.login_checked(&Account { label, ..account }, &login, Ok(()));
            }
            Err(e) => {
                self.form.checking = false;
                self.form.error = Some(e);
            }
        }
    }

    /// The label to give `address`, signed in to from the form: the one the other app it was
    /// picked from has. Google may have been signed in to with another address than the one
    /// picked, which that label doesn't name.
    fn form_label(&self, address: &str) -> Option<String> {
        match &self.form.imports.picked {
            Some(picked) if !picked.eq_ignore_ascii_case(address) => None,
            _ => self.form.label.clone(),
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
        // Added again on another server or user: what was asked of its old mailboxes isn't of these.
        let before = services.store.account(&id).ok().flatten();
        let moved = before
            .as_ref()
            .is_some_and(|before| imap_owner(before) != imap_owner(account));
        // Its old watch stops first, so nothing it brings is written once saving the account has
        // forgotten its old mailboxes: none is left to be taken for one there, as one to delete.
        if moved {
            services.mailroom.unwatch(&id);
        }
        if let Err(e) = save_account(&services.store, services.secrets.as_ref(), account, login) {
            // Not saved: it's still the account it was, watched again.
            let rewatched = before.filter(|_| moved).map(|before| {
                services
                    .mailroom
                    .watch(&before, &services.store, services.secrets.as_ref())
            });
            self.form.error = Some(match rewatched {
                Some(Err(again)) => format!("{e}; and it couldn't be checked again: {again}"),
                _ => e.to_string(),
            });
            return;
        }
        let watched = services
            .mailroom
            .watch(account, &services.store, services.secrets.as_ref());
        // Saved on another login: what was shown of the old one goes, watched or not.
        if moved {
            self.forget_mail_of(&id);
        }
        if let Err(e) = watched {
            self.form.error = Some(e.to_string());
            self.reload();
            return;
        }
        self.statuses.insert(id, Ok(Status::Connecting));
        // Saved on another server, it autosorts nowhere anymore.
        self.load_autosort();
        self.notify(format!("Added {label}"));
        self.form_added();
        self.reload();
    }

    /// The form once the account it set up was added: back to what the other app it was picked
    /// from has, for the next one, unless the form was left for something else meanwhile; else
    /// closed.
    fn form_added(&mut self) {
        if self.form.open && self.form.imports.picked.is_some() {
            self.form_step(Step::Import);
        } else {
            self.form = AccountForm::default();
        }
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

    /// Shows another part of the settings page. A Remove waiting for its second press is
    /// forgotten. While moving to another computer is under way, the page stays on Accounts,
    /// where it shows, so nothing goes on out of sight.
    pub(crate) fn show_settings_section(&mut self, section: SettingsSection) {
        if section == self.settings_section || !self.can_show_settings_section(section) {
            return;
        }
        self.settings_section = section;
        self.confirm_remove = None;
    }

    /// Whether the settings page can go to `section`: not away from Accounts while moving to
    /// another computer is under way.
    pub(crate) fn can_show_settings_section(&self, section: SettingsSection) -> bool {
        section == SettingsSection::Accounts || self.transfer.step == transfer::Step::Idle
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
        let profile = match collect_profile(services, self.transfer.with_sorting) {
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
        let profile = match collect_profile(services, self.transfer.with_sorting) {
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
            Ok(file) if is_sealed(&file) => {
                self.transfer = Transfer::unlock(name, file);
                // Another section may have been picked while the file was being chosen.
                self.settings_section = SettingsSection::Accounts;
            }
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
        // What sorting learned may have been replaced, and the accounts it learns of changed.
        if imported.sorting == Some(Ok(())) {
            self.tags.clear();
        }
        // An account replaced by one on another server autosorts nowhere anymore.
        self.load_autosort();
        self.sort_mail();
        self.notify(imported_notice(&imported));
    }

    /// Switches to `settings`, as imported, doing what changing each by hand does.
    fn use_settings(&mut self, settings: Settings) {
        self.set_theme(settings.theme);
        self.set_poll_interval(settings.poll_interval);
        self.set_prefetch(settings.prefetch);
        self.set_keep_opened(settings.keep_opened);
        self.set_remember_contacts(settings.remember_contacts);
        self.set_sort_mail(settings.sort_mail);
        self.set_notify_new_mail(settings.notify_new_mail);
        self.set_keep_running(settings.keep_running);
        self.set_check_updates(settings.check_updates);
    }

    /// Shows an account's settings: what it's called, the name its mail goes under, and its
    /// signature. Going back from them shows the settings' Accounts section.
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
        self.settings_section = SettingsSection::Accounts;
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

    /// Keeps messages downloaded once opened, or stops, dropping those kept.
    pub(crate) fn set_keep_opened(&mut self, on: bool) {
        let Ok(services) = &self.services else {
            return;
        };
        if services.mailroom.keep_opened() == on {
            return;
        }
        services.mailroom.set_keep_opened(on);
        self.settings.keep_opened = on;
        self.save_settings();
    }

    /// Remembers who mail is sent to, to suggest them as recipients, or stops. Those remembered
    /// are kept until forgotten.
    pub(crate) fn set_remember_contacts(&mut self, on: bool) {
        if self.settings.remember_contacts != on {
            self.settings.remember_contacts = on;
            self.compose.close_suggestions();
            self.save_settings();
        }
    }

    /// Forgets every contact remembered, and says how many there were.
    pub(crate) fn forget_contacts(&mut self) {
        let Ok(services) = &self.services else {
            return;
        };
        let notice = match services.store.forget_contacts() {
            Ok(0) => "No contacts to forget".to_owned(),
            Ok(1) => "Forgot 1 contact".to_owned(),
            Ok(n) => format!("Forgot {n} contacts"),
            Err(e) => format!("Couldn't forget the contacts: {e}"),
        };
        self.compose.close_suggestions();
        self.notify(notice);
    }

    /// Lists only unread mail, or all of it again.
    pub(crate) fn set_unread_only(&mut self, on: bool) {
        if self.settings.unread_only == on {
            return;
        }
        // While it's still on, so what was read and is still being marked so stays put away.
        self.clear_listed();
        self.settings.unread_only = on;
        self.list_limit = LIST_LIMIT;
        self.save_settings();
        self.reload();
        self.find_unread_listed();
    }

    /// Drops every message downloaded, ahead or once opened, to download them again as needed.
    /// How many arrives as [`Update::Cleared`].
    pub(crate) fn clear_downloaded(&mut self) {
        if let Ok(services) = &self.services {
            services.mailroom.clear_downloaded();
        }
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

    /// Sorts inbox mail, as learned on this computer, or stops and takes the tags away.
    pub(crate) fn set_sort_mail(&mut self, on: bool) {
        if self.settings.sort_mail == on {
            return;
        }
        self.settings.sort_mail = on;
        self.save_settings();
        let (Ok(services), Some(handle)) = (&mut self.services, &self.handle) else {
            return;
        };
        self.tags.clear();
        // What a pass under way learns once it's off isn't saved.
        if !on && let Err(e) = services.store.renew_sorting() {
            self.notify(format!("couldn't stop sorting mail: {e}"));
            return;
        }
        // One sorter at most, kept once started, so two never learn at once.
        if on && services.sorter.is_none() {
            match start_sorter(handle, &services.database) {
                Ok(sorter) => services.sorter = Some(sorter),
                Err(e) => self.notify(e),
            }
        }
        self.sort_mail();
    }

    /// Forgets everything sorting learned, and takes the tags away: it learns again from the
    /// mail cached.
    pub(crate) fn forget_sorting(&mut self) {
        let Ok(services) = &mut self.services else {
            return;
        };
        match services.store.forget_sorting() {
            Ok(()) => {
                self.tags.clear();
                self.sort_mail();
                self.notify("Forgot what sorting learned".to_owned());
            }
            Err(e) => self.notify(format!("Couldn't forget what sorting learned: {e}")),
        }
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

    /// Switches between releases only and nightly builds too, then checks the new track, so
    /// an update found on the old one isn't offered on it.
    pub(crate) fn set_update_track(&mut self, track: Track) {
        if self.settings.update_track == track {
            return;
        }
        self.settings.update_track = track;
        self.save_settings();
        match self.updater.phase {
            // What's downloading or installed already stays.
            Phase::Downloading(_) | Phase::Ready => {}
            // Its answer is dropped for the old track's, and the new one is checked then.
            Phase::Checking { manual } => {
                self.forget_update();
                self.updater.phase = Phase::Checking { manual };
            }
            _ => {
                self.forget_update();
                self.check_for_updates(false);
            }
        }
    }

    /// Brings back the snoozed messages that are due, then checks again after [`WAKE_EVERY`].
    fn wake_snoozed(&mut self) {
        let Some(handle) = self.handle.clone() else {
            return;
        };
        if let Ok(services) = &self.services
            && let Err(e) = services
                .mailroom
                .wake_due(&services.store, Timestamp::now().as_second())
        {
            self.notify(format!("couldn't check snoozed mail: {e}"));
        }
        drop(rune::task::spawn_after(
            "wake-snoozed",
            WAKE_EVERY,
            move || {
                handle.update(|s: &mut Self, _| s.wake_snoozed());
            },
        ));
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
        let (handle, track) = (handle.clone(), self.settings.update_track);
        services.mailroom.handle().spawn(async move {
            let result = postden_update::check(update::current(), track)
                .await
                .map_err(|e| e.to_string());
            handle.update(move |s: &mut Self, _| s.update_checked(track, result));
        });
    }

    fn update_checked(
        &mut self,
        track: Track,
        result: Result<Option<postden_update::Available>, String>,
    ) {
        let manual = matches!(self.updater.phase, Phase::Checking { manual: true });
        if track != self.settings.update_track {
            // The track changed while checking: this answer is for the old one.
            self.forget_update();
            self.check_for_updates(manual);
            return;
        }
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
        } else if manual && !self.updates_shown() {
            // Asked for, its result shows somewhere: up to date, or why it failed.
            let status = self.updater.status();
            self.notify(status);
        }
        // A day after the last check, whichever started it; this replaces any timer running.
        if self.settings.check_updates {
            self.schedule_update_check(UPDATE_CHECK_EVERY);
        }
    }

    /// Whether the settings' updates are on screen, where a check's result shows.
    fn updates_shown(&self) -> bool {
        self.settings_open
            && !self.account_settings.open
            && self.settings_section == SettingsSection::General
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
            .await;
            handle.update(move |s: &mut Self, _| s.update_installed(result));
        });
    }

    fn update_installed(&mut self, result: postden_update::Result<()>) {
        self.updater.phase = match result {
            Ok(()) => Phase::Ready,
            // Replaced by a newer nightly since it was found: look for what's there now.
            Err(postden_update::Error::Gone) => {
                self.forget_update();
                self.check_for_updates(true);
                return;
            }
            Err(error) => {
                // Nightlies were turned off while one downloaded: it's not retried.
                if self.settings.update_track == Track::Stable
                    && self
                        .updater
                        .available
                        .as_ref()
                        .is_some_and(|a| a.latest.version.is_nightly())
                {
                    self.forget_update();
                }
                Phase::InstallFailed(error.to_string())
            }
        };
    }

    /// Stops offering the update found, and closes its announcement.
    fn forget_update(&mut self) {
        self.updater.available = None;
        self.updater.dialog_open = false;
        self.updater.phase = Phase::Idle;
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
        if self.undo.as_ref().is_some_and(|u| u.account() == id) {
            self.undo = None;
        }
        self.undoing.retain(|u| u.account() != id);
        self.unanswered.retain(|(key, _), _| key.0 != id);
        self.read_later.retain(|key, _| key.0 != id);
        self.starring.retain(|key, _| key.0 != id);
        self.unreading.retain(|key| key.0 != id);
        self.refreshing.retain(|key, _| key.0 != id);
        self.refreshing_all.remove(id);
        self.leaving.retain(|r| r.account != id);
        // The store forgot where it autosorts to as well.
        self.autosort_to.retain(|(account, _)| account != id);
        self.autosorting.retain(|key, _| key.0 != id);
        let dropped: Vec<MessageKey> = self
            .deferred
            .extract_if(|key, _| key.0 == id)
            .map(|(key, _)| key)
            .collect();
        // No move of theirs will be answered to list them again.
        for key in dropped {
            self.moving.remove(&key);
        }
        self.returning.retain(|r| r.account != id);
        if self.restoring.as_ref().is_some_and(|r| r.account == id) {
            self.restoring = None;
        }
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
            // What was read in other accounts stays put away; this one's marks are gone above.
            self.leave_reader();
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
        if self.account_menu.as_ref().is_some_and(|m| m.account == id) {
            self.account_menu = None;
        }
        if self.new_mailbox.account == id {
            self.close_new_mailbox();
        }
        if self
            .delete_mailbox
            .mailbox
            .as_ref()
            .is_some_and(|m| m.0 == id)
        {
            self.close_delete_mailbox();
        }
        // Their answers are dropped: they'd be for a mailbox no longer listed.
        self.deleting_mailboxes.retain(|m, _| m.0 != id);
    }
}

/// A message's place in the store: its account, mailbox and UID.
pub(crate) fn message_key(m: &StoredMessage) -> MessageKey {
    (m.account.clone(), m.mailbox.clone(), m.envelope.uid)
}

/// The key of a message's row in the list. Hashed, as a mailbox's name can hold any character.
pub(crate) fn row_key(key: &MessageKey) -> String {
    let mut hasher = std::hash::DefaultHasher::new();
    std::hash::Hash::hash(key, &mut hasher);
    format!("msg-{:016x}", std::hash::Hasher::finish(&hasher))
}

#[cfg(test)]
mod tests {
    use postden_core::account::Security;

    use super::*;
    use postden_store::AutosortState;

    #[test]
    fn a_deleted_mailbox_closes_its_dialog_leaves_its_view_and_says_why_it_was_refused() {
        let me = "me@example.com";
        let deleting = |id, label: &str| Deleting {
            id,
            uid_validity: 1,
            label: label.into(),
        };
        let key = |name: &str| (me.to_owned(), name.to_owned());
        let deleted = |id, name: &str, result: Result<(), String>| Update::MailboxDeleted {
            id,
            account: me.into(),
            name: name.into(),
            result,
        };
        let mut s = Mail::not_started();
        s.show(Showing::Mailbox {
            account: me.into(),
            name: "Work".into(),
        });
        s.open_delete_mailbox(me, "Work");
        s.deleting_mailboxes
            .insert(key("Work"), deleting(1, "Work"));
        assert!(s.deleting_mailbox());
        // Refused, the dialog says why, and only closes: opened again, it's on the mailbox as it
        // is then.
        s.apply(
            deleted(1, "Work", Err("has children".into())),
            &mut Tx::default(),
        );
        assert!(s.delete_mailbox.mailbox.is_some());
        assert!(!s.deleting_mailbox());
        assert_eq!(s.delete_mailbox.error.as_deref(), Some("has children"));
        assert_eq!(s.notice, None);
        assert!(s.showing.is(me, "Work"));
        s.close_delete_mailbox();

        // Closed while busy and opened again, it's still busy, and asks nothing twice.
        s.open_delete_mailbox(me, "Work");
        s.deleting_mailboxes
            .insert(key("Work"), deleting(2, "Work"));
        s.close_delete_mailbox();
        s.open_delete_mailbox(me, "Work");
        assert!(s.deleting_mailbox());
        // An answer to an earlier request isn't this one's.
        s.apply(deleted(1, "Work", Ok(())), &mut Tx::default());
        assert!(s.deleting_mailbox());
        // Settings, and a removal waiting to be confirmed there, stay open.
        s.settings_open = true;
        s.confirm_remove = Some(me.into());
        s.apply(deleted(2, "Work", Ok(())), &mut Tx::default());
        assert_eq!(s.delete_mailbox, DeleteMailbox::default());
        assert_eq!(s.showing, Showing::inbox(me));
        assert!(s.settings_open);
        assert_eq!(s.confirm_remove.as_deref(), Some(me));
        assert_eq!(
            s.notice.as_deref(),
            Some("me@example.com: deleted the mailbox Work")
        );

        // Closed before the server answered, a notice tells too.
        s.deleting_mailboxes.insert(key("Old"), deleting(3, "Old"));
        s.apply(deleted(3, "Old", Ok(())), &mut Tx::default());
        assert_eq!(
            s.notice.as_deref(),
            Some("me@example.com: deleted the mailbox Old")
        );

        // Not waited for, as when its account was removed meanwhile, it's dropped.
        s.notice = None;
        s.apply(deleted(4, "Gone", Ok(())), &mut Tx::default());
        assert_eq!(s.notice, None);
    }

    #[test]
    fn a_mailbox_isnt_deleted_while_a_move_out_of_it_waits_on_any_account_of_its_login() {
        use postden_core::account::{Auth, Server};
        let server = Server {
            host: "imap.example.com".into(),
            port: 993,
            security: Security::Tls,
            username: "me".into(),
        };
        let mut s = Mail::not_started();
        for id in ["a@example.com", "b@example.com"] {
            s.accounts.push(Account {
                id: id.into(),
                display_name: None,
                label: None,
                imap: Server {
                    host: if id.starts_with('a') {
                        "IMAP.example.com".into()
                    } else {
                        server.host.clone()
                    },
                    ..server.clone()
                },
                smtp: server.clone(),
                auth: Auth::Password,
            });
            s.mailboxes.insert(
                id.into(),
                vec![Mailbox {
                    name: "Work".into(),
                    delimiter: None,
                    role: Role::Other,
                }],
            );
        }
        // Waiting for a star to be answered, a move out of Work through a@.
        let key = ("a@example.com".to_owned(), "Work".to_owned(), 1);
        s.deferred
            .insert(key, (Deferred::Move("INBOX".into()), Some(1)));
        // Deleting Work through b@, armed as when synced.
        s.open_delete_mailbox("b@example.com", "Work");
        s.delete_mailbox.uid_validity = Some(1);
        s.delete_mailbox.error = None;
        s.delete_mailbox();
        assert_eq!(
            s.delete_mailbox.error.as_deref(),
            Some("A message is still waiting to be moved out of Work; try again once it's moved.")
        );
    }

    #[test]
    fn mailboxes_with_nothing_unread_are_grouped_once_counted_unless_shown() {
        let me = "me@example.com";
        let mailbox = |name: &str, role| Mailbox {
            name: name.into(),
            delimiter: None,
            role,
        };
        let names = |m: Vec<&Mailbox>| m.into_iter().map(|m| m.name.clone()).collect::<Vec<_>>();
        let listed = |s: &Mail| {
            let (listed, read) = s.sidebar_mailboxes(me);
            (names(listed), names(read))
        };
        let mut s = Mail::not_started();
        s.mailboxes.insert(
            me.into(),
            vec![
                mailbox("Archive", Role::Archive),
                mailbox("INBOX", Role::Inbox),
                mailbox("Lists", Role::Other),
                mailbox("Work", Role::Other),
            ],
        );
        s.unread.insert((me.into(), "Lists".into()), 3);
        let all = || {
            (
                vec![
                    "INBOX".to_owned(),
                    "Archive".into(),
                    "Lists".into(),
                    "Work".into(),
                ],
                vec![],
            )
        };
        assert_eq!(listed(&s), all(), "off: listed as before, the inbox first");

        s.set_group_read_mailboxes(true);
        assert_eq!(listed(&s), all(), "none counted yet: none known to be read");
        // As the store says once each was searched for unread mail.
        for name in ["INBOX", "Archive", "Lists", "Work"] {
            s.counted.insert((me.into(), name.into()));
        }
        assert_eq!(
            listed(&s),
            (
                vec!["INBOX".to_owned(), "Lists".into()],
                vec!["Archive".to_owned(), "Work".into()]
            ),
            "the inbox stays at the top, though it's read"
        );
        let show = |s: &mut Mail, name: &str| {
            s.show(Showing::Mailbox {
                account: me.into(),
                name: name.into(),
            });
        };
        // Shown from the open group, a mailbox stays in it, not under the pointer's next row.
        assert!(!s.read_mailboxes_open(me), "closed until opened");
        s.toggle_read_mailboxes(me);
        assert!(s.read_mailboxes_open(me));
        show(&mut s, "Work");
        assert_eq!(listed(&s).1, ["Archive", "Work"]);
        // Closed, the group leaves the mailbox shown in sight.
        s.toggle_read_mailboxes(me);
        assert_eq!(listed(&s).1, ["Archive"]);
        s.toggle_read_mailboxes(me);

        // Shown while listed, a mailbox stays listed as its last message is read.
        show(&mut s, "Lists");
        s.unread.remove(&(me.into(), "Lists".into()));
        assert_eq!(listed(&s).0, ["INBOX", "Lists"]);
        // Its menu open, it stays too; left, it goes to the group.
        s.open_mailbox_menu(me, "Lists", PointerPress { x: 0.0, y: 0.0 });
        show(&mut s, "Archive");
        assert_eq!(listed(&s).0, ["INBOX", "Lists"], "menu still open");
        s.close_mailbox_menu();
        assert_eq!(listed(&s).1, ["Archive", "Lists", "Work"]);

        // Picked in the list while grouping is off, a mailbox stays there once it's on.
        s.set_group_read_mailboxes(false);
        assert_eq!(listed(&s), all());
        show(&mut s, "Work");
        s.set_group_read_mailboxes(true);
        assert_eq!(
            listed(&s),
            (
                vec!["INBOX".to_owned(), "Work".into()],
                vec!["Archive".to_owned(), "Lists".into()]
            ),
            "the group open, Work isn't in it"
        );

        s.set_group_read_mailboxes(false);
        assert_eq!(listed(&s), all());
    }

    #[test]
    fn nothing_is_moved_to_a_mailbox_being_deleted() {
        let me = "me@example.com";
        let mut s = Mail::not_started();
        s.deleting_mailboxes.insert(
            (me.into(), "Work".into()),
            Deleting {
                id: 0,
                uid_validity: 1,
                label: "Work".into(),
            },
        );
        assert!(s.mailbox_deleting(me, "Work"));
        s.move_message(me, "INBOX", 1, "Work");
        assert_eq!(
            s.notice.as_deref(),
            Some("me@example.com: Work is being deleted, so nothing's moved there")
        );
        assert!(s.deferred.is_empty());
    }

    #[test]
    fn a_mailbox_not_synced_yet_isnt_deleted() {
        let mut s = Mail::not_started();
        s.mailboxes.insert(
            "me@example.com".into(),
            vec![Mailbox {
                name: "Work".into(),
                delimiter: None,
                role: Role::Other,
            }],
        );
        // Nothing's known of its UIDVALIDITY, as with no store here: it can't be confirmed.
        s.open_delete_mailbox("me@example.com", "Work");
        assert_eq!(
            s.delete_mailbox.error.as_deref(),
            Some("Work hasn't synced yet. Try again once it has.")
        );
        s.delete_mailbox();
        assert!(!s.deleting_mailbox());
    }

    #[test]
    fn a_mailbox_no_longer_listed_stops_showing_and_settings_stay_open() {
        let me = "me@example.com";
        let mailbox = |name: &str, role| Mailbox {
            name: name.into(),
            delimiter: None,
            role,
        };
        let mut s = Mail::not_started();
        s.show(Showing::Mailbox {
            account: me.into(),
            name: "Work".into(),
        });
        s.settings_open = true;
        s.mailboxes.insert(
            me.into(),
            vec![mailbox("INBOX", Role::Inbox), mailbox("Notes", Role::Other)],
        );
        // A mailbox to delete, unchanged, stays up; once it's kept for drafts, it doesn't.
        s.open_delete_mailbox(me, "Notes");
        // As synced: no store here to say so.
        s.delete_mailbox.uid_validity = Some(1);
        s.delete_mailbox.error = None;
        assert!(s.can_delete_mailbox(me, "Notes"));
        s.apply(Update::Mailboxes { account: me.into() }, &mut Tx::default());
        assert_eq!(s.showing, Showing::inbox(me));
        assert!(s.settings_open);
        assert!(s.delete_mailbox.mailbox.is_some());

        s.mailboxes.insert(
            me.into(),
            vec![
                mailbox("INBOX", Role::Inbox),
                mailbox("Notes", Role::Drafts),
            ],
        );
        s.apply(Update::Mailboxes { account: me.into() }, &mut Tx::default());
        assert_eq!(s.delete_mailbox, DeleteMailbox::default());
        assert_eq!(
            s.notice.as_deref(),
            Some("me@example.com: Notes changed on the server, so it wasn't deleted")
        );

        // One saying why it can't be deleted stays, for the user to read and close.
        s.notice = None;
        s.open_delete_mailbox(me, "Notes");
        s.delete_mailbox.error = Some("the server keeps it for special mail".into());
        s.apply(Update::Mailboxes { account: me.into() }, &mut Tx::default());
        assert!(s.delete_mailbox.mailbox.is_some());
        assert_eq!(s.notice, None);
    }

    #[test]
    fn a_new_mailbox_closes_its_dialog_once_made_and_says_why_it_was_refused() {
        let created = |name: &str, result: Result<(), String>| Update::MailboxCreated {
            account: "me@example.com".into(),
            name: name.into(),
            result,
        };
        let mut s = Mail::not_started();
        s.open_new_mailbox("me@example.com");
        s.new_mailbox.creating = Some("Work".into());
        s.apply(created("Work", Err("exists".into())), &mut Tx::default());
        assert!(s.new_mailbox.open);
        assert_eq!(s.new_mailbox.creating, None);
        assert_eq!(s.new_mailbox.error.as_deref(), Some("exists"));

        s.new_mailbox.creating = Some("Work 2".into());
        s.apply(created("Work 2", Ok(())), &mut Tx::default());
        assert!(!s.new_mailbox.open);
        assert_eq!(s.notice, None);

        // Closed before the server answered, a notice tells.
        s.apply(created("Later", Ok(())), &mut Tx::default());
        assert_eq!(
            s.notice.as_deref(),
            Some("me@example.com: created the mailbox Later")
        );
    }

    #[test]
    fn a_refreshed_mailbox_stays_busy_until_a_sync_answering_its_refresh_ends() {
        let synced = |mailbox: Option<&str>, asked| Update::Synced {
            account: "me@example.com".into(),
            mailbox: mailbox.map(Into::into),
            asked,
        };
        let mut s = Mail::not_started();
        for (mailbox, asked) in [("Inbox", 3), ("Archive", 4), ("Sent", 4)] {
            s.refreshing
                .insert(("me@example.com".into(), mailbox.into()), asked);
        }
        s.refreshing
            .insert(("you@example.com".into(), "Archive".into()), 4);

        // A sync started before the refresh doesn't answer it.
        s.apply(synced(Some("INBOX"), 2), &mut Tx::default());
        assert!(s.refreshing("me@example.com", "Inbox"));
        // Every inbox is refreshed as INBOX, however the server lists it.
        assert!(s.refreshing("me@example.com", "INBOX"));
        s.apply(synced(Some("INBOX"), 3), &mut Tx::default());
        assert!(!s.refreshing("me@example.com", "Inbox"));

        s.apply(synced(Some("Archive"), 5), &mut Tx::default());
        assert!(!s.refreshing("me@example.com", "Archive"));
        assert!(s.refreshing("me@example.com", "Sent"));
        assert!(
            s.refreshing("you@example.com", "Archive"),
            "another account's"
        );

        // The refresher's round ended, if it failed before reaching Sent too.
        s.apply(synced(None, 4), &mut Tx::default());
        assert!(!s.refreshing("me@example.com", "Sent"));
        assert!(s.refreshing("you@example.com", "Archive"));
    }

    #[test]
    fn mailboxes_a_refresh_of_every_mailbox_finds_stay_busy_until_its_round_ends() {
        let me = "me@example.com";
        let mut s = Mail::not_started();
        // Only the inbox was listed before: the refresh's listing finds the rest.
        s.refreshing_all.insert(me.into(), 7);
        s.mailboxes.insert(
            me.into(),
            ["INBOX", "Archive", "Travel"]
                .map(|name| Mailbox {
                    name: name.into(),
                    delimiter: None,
                    role: if name == "INBOX" {
                        Role::Inbox
                    } else {
                        Role::Other
                    },
                })
                .into(),
        );
        let apply = |s: &mut Mail, update| s.apply(update, &mut Tx::default());
        apply(&mut s, Update::Mailboxes { account: me.into() });
        assert!(s.refreshing(me, "Archive"));
        assert!(s.refreshing(me, "Travel"));
        assert!(!s.refreshing(me, "INBOX"), "its watcher's to answer");

        let synced = |mailbox: Option<&str>| Update::Synced {
            account: me.into(),
            mailbox: mailbox.map(Into::into),
            asked: 7,
        };
        apply(&mut s, synced(Some("Archive")));
        assert!(!s.refreshing(me, "Archive"));
        apply(&mut s, synced(None));
        assert!(!s.refreshing(me, "Travel"));
        // The next round's listing is no longer the refresh's.
        apply(&mut s, Update::Mailboxes { account: me.into() });
        assert!(!s.refreshing(me, "Archive"));
    }

    /// A form open on the accounts Spark has: one with servers, a Gmail one and a Microsoft one.
    fn importing() -> Mail {
        use postden_app::import::{Found, Setup, Unsupported};
        use postden_core::account::Server;

        let found = |address: &str, setup| Found {
            address: address.into(),
            display_name: Some("Ada".into()),
            label: Some("Work".into()),
            setup,
        };
        let server = |host: &str, port| Server {
            host: host.into(),
            port,
            security: Security::StartTls,
            username: "ada@example.com".into(),
        };
        let mut s = Mail::not_started();
        s.open_form();
        s.form.imports.apps = vec!["Spark"];
        s.open_imports();
        s.pick_import_app("Spark");
        s.form.imports.found = Some(vec![(
            "Spark",
            Ok(vec![
                found(
                    "ada@example.com",
                    Setup::Password {
                        imap: server("imap.example.com", 143),
                        smtp: server("smtp.example.com", 587),
                    },
                ),
                found("ada@gmail.com", Setup::Google),
                found(
                    "ada@outlook.com",
                    Setup::Unsupported(Unsupported::Microsoft),
                ),
            ]),
        )]);
        s
    }

    #[test]
    fn an_account_from_another_app_fills_in_its_servers_and_back_returns_to_the_list() {
        let mut s = importing();
        s.set_up_found("ada@example.com");
        let f = &s.form;
        assert_eq!(f.step, Step::Servers);
        assert_eq!(f.email.text, "ada@example.com");
        assert_eq!(f.name.text, "Ada");
        assert_eq!(f.label.as_deref(), Some("Work"));
        assert_eq!(
            (f.imap_host.text.as_str(), f.imap_port.text.as_str()),
            ("imap.example.com", "143")
        );
        assert_eq!(
            (f.smtp_host.text.as_str(), f.smtp_port.text.as_str()),
            ("smtp.example.com", "587")
        );
        assert_eq!(f.smtp_security, Security::StartTls);
        assert_eq!(f.username.text, "", "the user name is the address");
        assert!(f.password.focused, "the password is what's left to type");

        s.form_back();
        assert_eq!(s.form.step, Step::Import);
        assert_eq!(s.form.imports.picked, None);
        assert!(
            s.form.imports.find("ada@gmail.com").is_some(),
            "the list stays as read"
        );
        assert_eq!(s.form.email.text, "", "the fields start afresh");
    }

    #[test]
    fn a_gmail_account_from_another_app_signs_in_with_google_and_others_cant_be_set_up() {
        let mut s = importing();
        s.set_up_found("ada@outlook.com");
        assert_eq!(
            s.form.step,
            Step::Import,
            "a Microsoft account isn't set up"
        );
        assert!(
            s.form.imports.find("ada@example.com").is_some(),
            "the list stays"
        );

        s.set_up_found("ada@gmail.com");
        assert_eq!(s.form.step, Step::Google);
        assert_eq!(s.form.email.text, "ada@gmail.com", "suggested to Google");
        assert_eq!(s.form.imports.picked.as_deref(), Some("ada@gmail.com"));
    }

    #[test]
    fn an_account_added_from_another_app_returns_to_the_list_only_if_the_form_is_still_open() {
        let mut s = importing();
        s.set_up_found("ada@example.com");
        s.form_added();
        assert!(s.form.open);
        assert_eq!(s.form.step, Step::Import, "on to the next one");
        assert!(
            s.form.imports.find("ada@gmail.com").is_some(),
            "the list stays as read"
        );

        s.set_up_found("ada@example.com");
        s.show(Showing::inbox("ada@example.com"));
        s.form_added();
        assert!(!s.form.open, "the inbox shown meanwhile stays");
    }

    #[test]
    fn a_google_sign_in_to_another_address_doesnt_take_the_picked_accounts_label() {
        let mut s = importing();
        s.set_up_found("ada@gmail.com");
        assert_eq!(s.form_label("Ada@Gmail.com").as_deref(), Some("Work"));
        assert_eq!(s.form_label("bob@gmail.com"), None);

        s.form_step(Step::Google);
        s.form.label = None;
        assert_eq!(
            s.form_label("bob@gmail.com"),
            None,
            "nothing picked, nothing to give"
        );
    }

    #[test]
    fn what_was_read_for_an_earlier_opening_of_the_form_isnt_taken() {
        let mut s = Mail::not_started();
        s.open_form();
        let earlier = s.form.imports.session;
        s.open_form();
        s.form.imports.reading = true;
        s.imports_read(
            earlier,
            vec![("Spark", Err(Unreadable::Failed("stale".into())))],
        );
        assert!(
            s.form.imports.found.is_none(),
            "the earlier read is dropped"
        );
        assert!(s.form.imports.reading, "this opening's read still counts");

        let now = s.form.imports.session;
        s.imports_read(now, vec![("Spark", Ok(vec![]))]);
        assert!(s.form.imports.found.is_some());
        assert!(!s.form.imports.reading);
    }

    #[test]
    fn an_account_set_up_already_isnt_set_up_again_from_another_app() {
        let mut s = importing();
        let mut set_up = postden_app::NewAccount {
            email: "Ada@Example.com".into(),
            imap_host: "imap.example.com".into(),
            smtp_host: "smtp.example.com".into(),
            ..postden_app::NewAccount::default()
        }
        .build()
        .unwrap();
        set_up.label = Some("Mine".into());
        s.accounts = vec![set_up];
        s.set_up_found("ada@example.com");
        assert_eq!(s.form.step, Step::Import, "whatever the address's case");
        assert_eq!(s.form.imports.picked, None);

        s.set_up_found("ada@gmail.com");
        assert_eq!(s.form.step, Step::Google, "the others still are");
    }

    #[test]
    fn back_goes_from_an_apps_accounts_to_the_apps_and_then_to_choosing() {
        let mut s = importing();
        s.set_up_found("ada@example.com");
        s.form_back();
        assert_eq!(
            (s.form.step, s.form.imports.app),
            (Step::Import, Some("Spark")),
            "back to the accounts of the app it was picked from"
        );

        s.form_back();
        assert_eq!((s.form.step, s.form.imports.app), (Step::Import, None));
        s.pick_import_app("Thunderbird");
        assert_eq!(
            s.form.imports.app, None,
            "an app that isn't installed isn't picked"
        );

        s.form_back();
        assert_eq!(s.form.step, Step::Choose);
        s.form.imports.app = Some("Spark");
        s.open_imports();
        assert_eq!(s.form.imports.app, None, "the apps are picked from afresh");
    }

    #[test]
    fn an_account_is_set_up_from_the_app_it_was_picked_in() {
        use postden_app::import::{Found, Setup, Unsupported};

        let mut s = importing();
        let unsupported = Found {
            address: "ada@example.com".into(),
            display_name: None,
            label: None,
            setup: Setup::Unsupported(Unsupported::Pop),
        };
        s.form.imports.apps.push("Apple Mail");
        s.form
            .imports
            .found
            .as_mut()
            .unwrap()
            .push(("Apple Mail", Ok(vec![unsupported])));
        s.pick_import_app("Apple Mail");
        s.set_up_found("ada@example.com");
        assert_eq!(s.form.step, Step::Import, "Apple Mail's can't be set up");

        s.form_back();
        s.pick_import_app("Spark");
        s.set_up_found("ada@example.com");
        assert_eq!(s.form.step, Step::Servers, "Spark's can");
    }

    #[test]
    fn coming_back_to_the_window_rereads_only_what_needed_full_disk_access() {
        let mut s = importing();
        s.window_focused();
        assert!(s.form.imports.found.is_some(), "what was read stays");

        s.form.imports.found = Some(vec![("Apple Mail", Err(Unreadable::NeedsFullDiskAccess))]);
        s.form.imports.app = Some("Apple Mail");
        s.form.step = Step::Servers;
        s.window_focused();
        assert!(
            s.form.imports.found.is_some(),
            "not while elsewhere in the form"
        );

        s.form.step = Step::Import;
        s.window_focused();
        assert!(s.form.imports.found.is_none(), "read again");
        assert_eq!(s.form.imports.app, Some("Apple Mail"), "on the same app");
    }

    #[test]
    fn back_from_an_account_typed_in_returns_to_choosing() {
        let mut s = importing();
        s.form_step(Step::Servers);
        s.form_back();
        assert_eq!(s.form.step, Step::Choose);
    }

    #[test]
    fn switching_settings_sections_forgets_a_pending_remove() {
        let mut s = Mail::not_started();
        s.open_settings();
        assert_eq!(s.settings_section, SettingsSection::General);
        s.show_settings_section(SettingsSection::Accounts);
        s.confirm_remove = Some("me@example.com".into());
        s.show_settings_section(SettingsSection::Advanced);
        assert_eq!(s.settings_section, SettingsSection::Advanced);
        assert_eq!(s.confirm_remove, None);
    }

    #[test]
    fn picking_the_section_shown_keeps_a_pending_remove() {
        let mut s = Mail::not_started();
        s.show_settings_section(SettingsSection::Accounts);
        s.confirm_remove = Some("me@example.com".into());
        s.show_settings_section(SettingsSection::Accounts);
        assert_eq!(s.confirm_remove.as_deref(), Some("me@example.com"));
    }

    #[test]
    fn moving_to_another_computer_keeps_the_accounts_section_shown() {
        let mut s = Mail::not_started();
        s.open_settings();
        s.show_settings_section(SettingsSection::Accounts);
        s.start_export();
        s.show_settings_section(SettingsSection::General);
        assert_eq!(s.settings_section, SettingsSection::Accounts);
        s.cancel_transfer();
        s.show_settings_section(SettingsSection::General);
        assert_eq!(s.settings_section, SettingsSection::General);
    }

    #[test]
    fn a_profile_picked_shows_the_accounts_section() {
        let path =
            std::env::temp_dir().join(format!("postden-test-{}.postden", std::process::id()));
        std::fs::write(&path, b"age-encryption.org/v1\n").unwrap();
        let mut s = Mail::not_started();
        s.open_settings();
        s.show_settings_section(SettingsSection::Advanced);
        s.read_profile(&path);
        std::fs::remove_file(&path).unwrap();
        assert_eq!(s.transfer.step, transfer::Step::Unlock);
        assert_eq!(s.settings_section, SettingsSection::Accounts);
    }

    #[test]
    fn settings_reopen_on_the_section_left() {
        let mut s = Mail::not_started();
        s.open_settings();
        s.show_settings_section(SettingsSection::Advanced);
        s.close_settings();
        s.open_settings();
        assert_eq!(s.settings_section, SettingsSection::Advanced);
    }

    #[test]
    fn an_accounts_settings_return_to_the_accounts_section() {
        use postden_core::account::{Auth, Security, Server};
        let server = Server {
            host: "localhost".into(),
            port: 1,
            security: Security::None,
            username: "me".into(),
        };
        let mut s = Mail::not_started();
        s.accounts.push(Account {
            id: "me@example.com".into(),
            display_name: None,
            label: None,
            imap: server.clone(),
            smtp: server,
            auth: Auth::Password,
        });
        s.open_account_settings("me@example.com");
        s.close_account_settings();
        assert!(s.settings_open);
        assert_eq!(s.settings_section, SettingsSection::Accounts);
    }

    #[test]
    fn a_check_for_updates_asked_where_its_result_cant_show_says_it_in_a_notice() {
        let mut s = Mail::not_started();
        s.updater.phase = Phase::Checking { manual: true };
        s.update_checked(Track::Stable, Ok(None));
        assert!(
            s.notice
                .as_deref()
                .is_some_and(|n| n.starts_with("Postden is up to date")),
            "got {:?}",
            s.notice
        );
        // With the updates on screen, they say it themselves.
        s.dismiss_notice();
        s.open_settings();
        s.updater.phase = Phase::Checking { manual: true };
        s.update_checked(Track::Stable, Ok(None));
        assert_eq!(s.notice, None);
    }

    #[test]
    fn a_check_answered_for_the_track_switched_away_from_is_dropped() {
        let mut s = Mail::not_started();
        s.settings.update_track = Track::Nightly;
        s.updater.phase = Phase::Checking { manual: true };
        s.update_checked(Track::Stable, Ok(None));
        assert_eq!(
            s.updater.phase,
            Phase::Idle,
            "not up to date on the new track"
        );
        assert_eq!(s.notice, None);
    }

    #[test]
    fn switching_tracks_mid_check_stops_offering_the_old_tracks_update() {
        let mut s = Mail::not_started();
        s.settings.update_track = Track::Nightly;
        s.updater.phase = Phase::Checking { manual: false };
        s.updater.available = Some(postden_update::Available {
            current: "0.7.0".parse().unwrap(),
            latest: postden_update::Release {
                version: "0.7.1-nightly.1".parse().unwrap(),
                notes: String::new(),
                page: String::new(),
                assets: Vec::new(),
            },
            changes: Vec::new(),
        });
        s.set_update_track(Track::Stable);
        assert_eq!(s.updater.available, None);
        assert_eq!(s.updater.phase, Phase::Checking { manual: false });
    }

    #[test]
    fn an_update_gone_from_github_is_no_longer_offered() {
        let mut s = Mail::not_started();
        s.updater.phase = Phase::Downloading(None);
        s.updater.available = Some(postden_update::Available {
            current: "0.7.0".parse().unwrap(),
            latest: postden_update::Release {
                version: "0.7.1-nightly.1".parse().unwrap(),
                notes: String::new(),
                page: String::new(),
                assets: Vec::new(),
            },
            changes: Vec::new(),
        });
        s.updater.dialog_open = true;
        s.update_installed(Err(postden_update::Error::Gone));
        assert_eq!(s.updater.available, None, "not offered to retry");
        assert_eq!(s.updater.phase, Phase::Idle);
        assert!(!s.updater.dialog_open, "nor announced");
    }

    #[test]
    fn a_nightly_that_failed_to_install_after_nightlies_were_turned_off_isnt_retried() {
        let nightly = || postden_update::Available {
            current: "0.7.0".parse().unwrap(),
            latest: postden_update::Release {
                version: "0.7.1-nightly.1".parse().unwrap(),
                notes: String::new(),
                page: String::new(),
                assets: Vec::new(),
            },
            changes: Vec::new(),
        };
        for (track, kept) in [(Track::Nightly, true), (Track::Stable, false)] {
            let mut s = Mail::not_started();
            s.settings.update_track = track;
            s.updater.phase = Phase::Downloading(None);
            s.updater.available = Some(nightly());
            s.update_installed(Err(postden_update::Error::ChecksumMismatch));
            assert!(
                matches!(s.updater.phase, Phase::InstallFailed(_)),
                "{track:?}: says why"
            );
            assert_eq!(s.updater.available.is_some(), kept, "{track:?}");
        }
    }

    #[test]
    fn the_update_channel_list_closes_for_the_palette_and_commands() {
        let mut s = Mail::not_started();
        s.updater.channel_open = true;
        s.open_palette(PaletteMode::Commands);
        assert!(!s.updater.channel_open, "the palette");
        s.updater.channel_open = true;
        s.close_palette();
        Command::Compose.run(&mut s, &mut Tx::default());
        assert!(!s.updater.channel_open, "a shortcut's command");
    }

    fn ada(uid: u32) -> MessageKey {
        ("ada@example.com".into(), "INBOX".into(), uid)
    }

    #[test]
    fn a_star_is_undone_only_once_the_server_starred_it_and_its_uid_still_names_it() {
        let mut s = Mail::not_started();
        s.undo = Some(Undo::Flagged {
            message: ada(1),
            uid_validity: 7,
            flagged: false,
            done: false,
        });
        s.undo();
        assert!(matches!(s.undo, Some(Undo::Flagged { done: false, .. })));
        assert!(
            s.notice
                .as_deref()
                .is_some_and(|n| n.contains("Still waiting"))
        );
        // Being marked read, another message starred, or one of two stars answered, isn't it.
        s.asked(&ada(1), true);
        s.asked(&ada(1), true);
        s.changed_for_undo(&ada(1), false, Ok(true));
        s.changed_for_undo(&ada(2), true, Ok(true));
        s.changed_for_undo(&ada(1), true, Ok(false));
        assert!(matches!(s.undo, Some(Undo::Flagged { done: false, .. })));
        s.changed_for_undo(&ada(1), true, Ok(true));
        assert!(matches!(s.undo, Some(Undo::Flagged { done: true, .. })));
        // The mailbox isn't cached under UIDVALIDITY 7 any more: UID 1 may be another message.
        s.undo();
        assert_eq!(s.undo, None);
        assert!(
            s.notice
                .as_deref()
                .is_some_and(|n| n.contains("can't be undone"))
        );
    }

    #[test]
    fn a_star_pressed_again_and_again_is_undone_once_every_one_is_answered() {
        let mut s = Mail::not_started();
        // S three times: star, unstar, star; Z is to unstar.
        for _ in 0..3 {
            s.asked(&ada(1), true);
        }
        s.undo = Some(Undo::Flagged {
            message: ada(1),
            uid_validity: 7,
            flagged: false,
            done: false,
        });
        s.changed_for_undo(&ada(1), true, Ok(true));
        s.changed_for_undo(&ada(1), true, Ok(false));
        assert!(
            matches!(s.undo, Some(Undo::Flagged { done: false, .. })),
            "the last star isn't answered yet"
        );
        s.changed_for_undo(&ada(1), true, Ok(true));
        assert!(matches!(s.undo, Some(Undo::Flagged { done: true, .. })));
        assert!(s.unanswered.is_empty());
    }

    #[test]
    fn a_star_waits_for_the_last_one_to_be_answered() {
        let mut s = Mail::not_started();
        s.asked(&ada(1), true);
        s.star(ada(1), false);
        assert_eq!(
            s.unanswered.get(&(ada(1), true)),
            Some(&1),
            "not asked again"
        );
        assert!(
            s.notice
                .as_deref()
                .is_some_and(|n| n.contains("Still waiting"))
        );
    }

    #[test]
    fn a_failed_move_back_isnt_undone_again_once_something_else_was_done() {
        let mut s = Mail::not_started();
        let back = Restoring {
            account: "ada@example.com".into(),
            mailbox: "INBOX".into(),
            message_id: "a@example.com".into(),
        };
        s.undoing = vec![Undo::Moved {
            from: ada(1),
            to: "Archive".into(),
            message_id: "a@example.com".into(),
            snoozed: None,
            done: true,
        }];
        // Something else done, such as marking a mailbox read.
        s.did(None);
        s.restored(&back, "Archive", &[], Err("offline".into()));
        assert_eq!(s.undo, None, "Z mustn't reach past marking all read");
    }

    #[test]
    fn a_move_waits_for_a_change_to_its_message_on_its_way() {
        let mut s = Mail::not_started();
        s.asked(&ada(2), false);
        s.move_message("ada@example.com", "INBOX", 2, "Archive");
        // A read mark asked for under UID 2 would be lost once it's moved: it leaves the list,
        // and goes once that's answered.
        assert!(
            matches!(s.deferred.get(&ada(2)), Some((Deferred::Move(to), _)) if to == "Archive")
        );
        assert!(s.moving.contains(&ada(2)));
        s.answered(&ada(2), false);
        assert!(s.deferred.is_empty(), "sent");
        // Without the services, sending it fails: it's listed where it was again.
        assert!(!s.moving.contains(&ada(2)));
    }

    #[test]
    fn something_else_done_stops_following_a_message_moved_back_but_not_hiding_it() {
        let mut s = Mail::not_started();
        let back = Restoring {
            account: "ada@example.com".into(),
            mailbox: "INBOX".into(),
            message_id: "a@example.com".into(),
        };
        s.restoring = Some(back.clone());
        s.returning = vec![back.clone()];
        s.did(None);
        assert_eq!(s.restoring, None);
        assert_eq!(s.returning, vec![back]);
    }

    #[test]
    fn a_message_being_moved_back_isnt_changed_and_its_copy_closes_once_moved() {
        let mut s = Mail::not_started();
        let archived = StoredMessage {
            account: "ada@example.com".into(),
            mailbox: "Archive".into(),
            envelope: Envelope {
                uid: 3,
                message_id: Some("a@example.com".into()),
                from: Address::default(),
                to: Vec::new(),
                subject: None,
                date: None,
                seen: true,
                flagged: false,
            },
        };
        s.reader = Some(Reader {
            message: archived.clone(),
            body: None,
            markdown: None,
            uid_validity: Some(1),
            seen_before: true,
        });
        s.leaving = vec![Restoring {
            account: "ada@example.com".into(),
            mailbox: "Archive".into(),
            message_id: "a@example.com".into(),
        }];
        let key = message_key(&archived);
        s.star(key, true);
        assert!(
            s.notice
                .as_deref()
                .is_some_and(|n| n.contains("being moved back"))
        );
        let back = Restoring {
            account: "ada@example.com".into(),
            mailbox: "INBOX".into(),
            message_id: "a@example.com".into(),
        };
        s.restored(&back, "Archive", &[3], Ok(true));
        assert!(s.reader.is_none(), "its copy in Archive is gone");
        assert_eq!(s.leaving, vec![]);
    }

    #[test]
    fn a_move_isnt_undone_while_a_change_in_the_mailbox_it_leaves_is_on_its_way() {
        let mut s = Mail::not_started();
        s.undo = Some(Undo::Moved {
            from: ada(1),
            to: "Archive".into(),
            message_id: "a@example.com".into(),
            snoozed: None,
            done: true,
        });
        // Opened there, its read mark is on its way, whatever the window shows now.
        s.asked(&("ada@example.com".into(), "Archive".into(), 9), false);
        s.undo();
        assert!(
            s.undo.is_some(),
            "Z can be pressed again once it's answered"
        );
        assert!(
            s.notice
                .as_deref()
                .is_some_and(|n| n.contains("Still waiting"))
        );
    }

    #[test]
    fn a_u_is_undoable_once_a_download_started_after_it_is_answered() {
        let mut s = Mail::not_started();
        // U, then the message opened again: its download marks it read too.
        s.asked(&ada(1), false);
        s.asked(&ada(1), false);
        s.undo = Some(Undo::Seen {
            message: ada(1),
            uid_validity: 7,
            seen: true,
            done: false,
        });
        s.changed_for_undo(&ada(1), false, Ok(false));
        assert!(matches!(s.undo, Some(Undo::Seen { done: false, .. })));
        s.read_later.insert(ada(1), Some(1));
        s.opened(
            &ada(1),
            1,
            (Err("offline".into()), false),
            &mut Tx::default(),
        );
        assert!(matches!(s.undo, Some(Undo::Seen { done: true, .. })));
        assert!(
            s.read_later.is_empty(),
            "not downloaded, it isn't read either"
        );
    }

    #[test]
    fn a_removed_account_lists_its_waiting_moves_again() {
        let mut s = Mail::not_started();
        let waiting = (Deferred::Move("Archive".into()), Some(1));
        s.deferred.insert(ada(1), waiting);
        s.moving.insert(ada(1));
        s.forget_mail_of("ada@example.com");
        assert!(!s.moving.contains(&ada(1)));
        assert!(s.deferred.is_empty());
    }

    fn open(s: &mut Mail, mailbox: &str, uid: u32, seen: bool) {
        s.reader = Some(Reader {
            message: StoredMessage {
                account: "ada@example.com".into(),
                mailbox: mailbox.into(),
                envelope: Envelope {
                    uid,
                    message_id: None,
                    from: Address::default(),
                    to: Vec::new(),
                    subject: None,
                    date: None,
                    seen,
                    flagged: false,
                },
            },
            body: None,
            markdown: None,
            uid_validity: Some(1),
            seen_before: seen,
        });
    }

    #[test]
    fn a_download_from_before_a_mailbox_started_over_isnt_shown_for_the_message_open_now() {
        let mut s = Mail::not_started();
        open(&mut s, "INBOX", 1, true);
        if let Some(reader) = &mut s.reader {
            reader.uid_validity = Some(2);
        }
        s.pending_reply = Some((ada(1), Answer::Forward));
        s.opened(&ada(1), 1, (Ok(Body::default()), true), &mut Tx::default());
        let reader = s.reader.as_ref().unwrap();
        assert!(reader.body.is_none(), "still waiting for its own download");
        assert!(s.pending_reply.is_some(), "the forward waits for it too");
        s.opened(&ada(1), 2, (Ok(Body::default()), true), &mut Tx::default());
        assert!(s.reader.as_ref().unwrap().body.is_some());
    }

    #[test]
    fn a_read_mark_the_server_refuses_leaves_the_message_open_as_it_was() {
        for seen in [true, false] {
            let mut s = Mail::not_started();
            open(&mut s, "INBOX", 1, seen);
            // Shown read as it opened, as opening it does, and not in the list: a server
            // search's.
            s.reader.as_mut().unwrap().message.envelope.seen = true;
            s.opened(&ada(1), 1, (Ok(Body::default()), false), &mut Tx::default());
            let reader = s.reader.as_ref().unwrap();
            assert!(reader.body.is_some(), "it still opens");
            assert_eq!(reader.message.envelope.seen, seen, "read: {seen}");
        }
    }

    #[test]
    fn a_star_refused_after_its_mailbox_started_over_leaves_the_message_found_now_be() {
        let mut s = Mail::not_started();
        // Found by searching the server after INBOX started over: UID 1 is another message,
        // starred, and isn't cached (the store has no UIDVALIDITY to match the old star's).
        let found = StoredMessage {
            account: "ada@example.com".into(),
            mailbox: "INBOX".into(),
            envelope: Envelope {
                uid: 1,
                message_id: None,
                from: Address::default(),
                to: Vec::new(),
                subject: None,
                date: None,
                seen: true,
                flagged: true,
            },
        };
        s.search.found.push(found);
        s.asked(&ada(1), true);
        s.flagged(&ada(1), 1, true, Err("INBOX changed on the server".into()));
        assert!(
            s.search.found[0].envelope.flagged,
            "not the message starred"
        );
    }

    #[test]
    fn a_mailbox_starting_over_closes_its_open_message_only() {
        let mut s = Mail::not_started();
        let started_over = |mailbox: &str| Update::Mail {
            account: "ada@example.com".into(),
            mailbox: mailbox.into(),
            role: Role::Other,
            count: 0,
            first_sync: true,
            uid_validity: 2,
            unread: Vec::new(),
            message_ids: Vec::new(),
        };
        open(&mut s, "Work", 5, true);
        s.pending_reply = Some((("ada@example.com".into(), "Work".into(), 5), Answer::Reply));
        let work = ("ada@example.com".to_owned(), "Work".to_owned(), 5);
        s.starring.insert(work.clone(), true);
        s.apply(started_over("Receipts"), &mut Tx::default());
        assert!(s.reader.is_some(), "another mailbox started over");
        assert!(s.starring.contains_key(&work));
        s.apply(started_over("Work"), &mut Tx::default());
        assert!(s.reader.is_none(), "UID 5 may name another message now");
        assert!(s.pending_reply.is_none());
        assert!(
            s.starring.is_empty(),
            "a star on its way isn't shown on the message UID 5 names now"
        );
    }

    #[test]
    fn a_message_opened_while_u_is_on_its_way_is_marked_read_once_nothing_is_awaited() {
        let mut s = Mail::not_started();
        // Opened, and closed again, while U and a star were on their way.
        s.asked(&ada(1), false);
        s.asked(&ada(1), true);
        s.read_later.insert(ada(1), Some(1));
        s.answered(&ada(1), false);
        assert!(
            s.read_later.contains_key(&ada(1)),
            "the star is still awaited"
        );
        s.answered(&ada(1), true);
        assert!(s.read_later.is_empty(), "asked for then");
    }

    #[test]
    fn a_read_mark_asked_anew_replaces_one_waiting_to_follow() {
        let mut s = Mail::not_started();
        s.read_later.insert(ada(1), Some(1));
        s.mark_seen(ada(1), false);
        assert!(s.read_later.is_empty(), "U again is the last word");
    }

    #[test]
    fn a_message_reopened_while_its_own_read_mark_is_on_its_way_isnt_marked_again() {
        let mut s = Mail::not_started();
        s.asked(&ada(1), false);
        s.messages.push(StoredMessage {
            account: "ada@example.com".into(),
            mailbox: "INBOX".into(),
            envelope: Envelope {
                uid: 1,
                message_id: None,
                from: Address::default(),
                to: Vec::new(),
                subject: None,
                date: None,
                seen: true,
                flagged: false,
            },
        });
        s.open_message("ada@example.com", "INBOX", 1);
        assert!(s.read_later.is_empty(), "it's being marked read already");
        s.unreading.insert(ada(1));
        s.open_message("ada@example.com", "INBOX", 1);
        assert!(
            s.read_later.contains_key(&ada(1)),
            "after U, it's marked read again"
        );
    }

    #[test]
    fn a_refused_change_leaves_nothing_to_undo() {
        let mut s = Mail::not_started();
        // U marked it unread; Z marks it read again.
        s.undo = Some(Undo::Seen {
            message: ada(1),
            uid_validity: 7,
            seen: true,
            done: false,
        });
        s.changed_for_undo(&ada(1), true, Err(true));
        assert!(s.undo.is_some(), "a star refused isn't the read mark");
        s.asked(&ada(1), false);
        s.asked(&ada(1), false);
        // A read mark refused the other way isn't the one undone, so the undo stays.
        s.changed_for_undo(&ada(1), false, Err(true));
        assert!(matches!(s.undo, Some(Undo::Seen { done: false, .. })));
        s.changed_for_undo(&ada(1), false, Err(false));
        assert_eq!(s.undo, None, "the U itself refused");
    }

    #[test]
    fn a_move_back_that_failed_can_be_undone_again_and_only_its_own() {
        let mut s = Mail::not_started();
        let moved = |uid, id: &str| Undo::Moved {
            from: ada(uid),
            to: "Archive".into(),
            message_id: id.into(),
            snoozed: None,
            done: true,
        };
        let back = |id: &str| Restoring {
            account: "ada@example.com".into(),
            mailbox: "INBOX".into(),
            message_id: id.into(),
        };
        // A and B are both being moved back.
        s.undoing = vec![moved(1, "a@example.com"), moved(2, "b@example.com")];
        s.returning = vec![back("a@example.com"), back("b@example.com")];
        s.restored(
            &back("a@example.com"),
            "Archive",
            &[],
            Err("offline".into()),
        );
        assert_eq!(s.undo, Some(moved(1, "a@example.com")), "A, not B");
        assert_eq!(s.undoing, vec![moved(2, "b@example.com")]);
        assert_eq!(s.returning, vec![back("b@example.com")]);
        // B back, its own move is done with; Z still undoes A's.
        s.restored(&back("b@example.com"), "Archive", &[5], Ok(true));
        assert_eq!(s.undoing, vec![]);
        assert_eq!(s.undo, Some(moved(1, "a@example.com")));
        // Something else done meanwhile is what Z undoes then.
        s.undoing = vec![moved(1, "a@example.com")];
        s.undo = Some(Undo::Seen {
            message: ada(2),
            uid_validity: 1,
            seen: true,
            done: true,
        });
        s.restored(
            &back("a@example.com"),
            "Archive",
            &[],
            Err("offline".into()),
        );
        assert!(matches!(s.undo, Some(Undo::Seen { .. })));
    }

    #[derive(Debug)]
    struct NoSecrets;

    impl SecretStore for NoSecrets {
        fn get(
            &self,
            _: &postden_core::secrets::SecretKey,
        ) -> Result<Option<SecretString>, postden_core::secrets::SecretError> {
            Ok(None)
        }
        fn set(
            &self,
            _: &postden_core::secrets::SecretKey,
            _: &SecretString,
        ) -> Result<(), postden_core::secrets::SecretError> {
            Err(postden_core::secrets::SecretError::ReadOnly)
        }
        fn delete(
            &self,
            _: &postden_core::secrets::SecretKey,
        ) -> Result<(), postden_core::secrets::SecretError> {
            Ok(())
        }
    }

    /// An envelope with subject "hello", dated by its UID: higher is newer.
    fn hello(uid: u32, seen: bool) -> Envelope {
        Envelope {
            uid,
            message_id: None,
            from: Address::default(),
            to: Vec::new(),
            subject: Some("hello".into()),
            date: Some(i64::from(uid)),
            seen,
            flagged: false,
        }
    }

    /// A test's own directory, removed once the test ends, passed or not.
    struct Scratch(PathBuf);

    impl Drop for Scratch {
        fn drop(&mut self) {
            let _ignored = std::fs::remove_dir_all(&self.0);
        }
    }

    /// A store's directory, and the window on that store, its list on Ada's inbox, synced with
    /// `inbox`. Bound in this order, the window closes the store before its directory goes.
    fn synced(name: &str, inbox: &[Envelope]) -> (Scratch, Mail) {
        use postden_core::account::{Auth, Server};
        let dir = std::env::temp_dir().join(format!("postden-{name}-{}", std::process::id()));
        let _ignored = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let database = dir.join("db");
        let mut store = Store::open(&database).unwrap();
        let server = Server {
            host: "imap.example.com".into(),
            port: 993,
            security: Security::Tls,
            username: "ada".into(),
        };
        store
            .save_account(&Account {
                id: "ada@example.com".into(),
                display_name: None,
                label: None,
                imap: server.clone(),
                smtp: server,
                auth: Auth::Password,
            })
            .unwrap();
        store
            .save_messages("ada@example.com", INBOX, Role::Inbox, 1, inbox)
            .unwrap();
        let mailroom =
            Mailroom::start(&database, WatchConfig::default(), 0, false, |_| {}).unwrap();
        let mut s = Mail::not_started();
        s.services = Ok(Services {
            store,
            secrets: Box::new(NoSecrets),
            read_only: true,
            mailroom,
            database,
            sorter: None,
        });
        s.showing = Showing::inbox("ada@example.com");
        s.reload();
        (Scratch(dir), s)
    }

    fn uids(s: &Mail) -> Vec<u32> {
        s.shown().map(|m| m.envelope.uid).collect()
    }

    fn read(s: &Mail, uid: u32) {
        let services = s.services.as_ref().unwrap();
        services
            .store
            .set_seen("ada@example.com", INBOX, uid, true)
            .unwrap();
    }

    /// The server was searched for unread mail in Ada's inbox below `below`, and had `unseen`,
    /// of which `fetched` weren't cached.
    fn searched_unread(s: &mut Mail, below: u32, unseen: &[u32], fetched: &[Envelope]) {
        let services = s.services.as_mut().unwrap();
        services
            .store
            .save_unread(
                "ada@example.com",
                INBOX,
                (1, 0),
                (below, below),
                unseen,
                fetched,
            )
            .unwrap();
        s.reload();
    }

    #[test]
    fn a_search_the_mailbox_started_over_during_doesnt_count_it() {
        let (_dir, mut s) = synced("counted-stale", &[]);
        let me = "ada@example.com";
        let key = (me.to_owned(), "Lists".to_owned());
        let searched = || Update::Unread {
            account: me.into(),
            mailbox: "Lists".into(),
            result: Ok(0),
            again: false,
        };
        let services = s.services.as_mut().unwrap();
        services
            .store
            .save_messages(me, "Lists", Role::Other, 2, &[])
            .unwrap();
        // Begun under UIDVALIDITY 1, the search's answer is dropped, as the mailroom reports.
        assert_eq!(
            services
                .store
                .save_unread(me, "Lists", (1, 0), (0, 4), &[], &[])
                .unwrap(),
            None
        );
        s.apply(searched(), &mut Tx::default());
        assert!(!s.counted.contains(&key), "not counted under UIDVALIDITY 2");

        let services = s.services.as_mut().unwrap();
        services
            .store
            .save_unread(me, "Lists", (2, 0), (0, 4), &[], &[])
            .unwrap();
        s.apply(searched(), &mut Tx::default());
        assert!(s.counted.contains(&key), "none unread, and that's known");
    }

    #[test]
    fn each_mailbox_counts_its_own_unread_mail() {
        let (_dir, mut s) = synced("unread-each", &[hello(1, false), hello(2, true)]);
        let services = s.services.as_mut().unwrap();
        services
            .store
            .save_messages(
                "ada@example.com",
                "Lists",
                Role::Other,
                2,
                &[hello(1, false), hello(2, false), hello(3, true)],
            )
            .unwrap();
        s.reload();
        assert_eq!(s.mailbox_unread("ada@example.com", "Lists"), 2);
        assert_eq!(s.mailbox_unread("ada@example.com", "Archive"), 0);
        assert_eq!(
            s.unread(Some("ada@example.com")),
            1,
            "only the inbox counts for the account and the app's icon"
        );
        assert_eq!(s.unread(None), 1);
    }

    #[test]
    fn a_refused_read_mark_leaves_the_message_open_as_the_store_says_if_newer() {
        // Read since it opened unread, as elsewhere, and not listed: opened from a notice while
        // the list shows sent mail.
        let (_dir, mut s) = synced("refused-newer", &[hello(1, true)]);
        s.showing = Showing::All(Role::Sent);
        s.reload();
        assert_eq!(uids(&s), Vec::<u32>::new());
        open(&mut s, INBOX, 1, false);
        s.reader.as_mut().unwrap().message.envelope.seen = true;
        s.opened(&ada(1), 1, (Ok(Body::default()), false), &mut Tx::default());
        assert!(
            s.reader.as_ref().unwrap().message.envelope.seen,
            "the store's read, not the unread it opened as"
        );
    }

    #[test]
    fn unread_mail_counted_but_not_cached_isnt_an_inbox_at_zero() {
        let (_dir, mut s) = synced("unread-uncached", &[hello(10, true), hello(11, true)]);
        s.set_unread_only(true);
        // Searched: 3 is unread and older than what's cached, but wasn't fetched.
        searched_unread(&mut s, 10, &[3], &[]);
        assert_eq!(uids(&s), Vec::<u32>::new());
        assert!(s.listing_synced());
        assert_eq!(s.listed_unread(), 1, "so the list doesn't say all is read");
        searched_unread(&mut s, 10, &[], &[]);
        assert_eq!(s.listed_unread(), 0);
    }

    #[test]
    fn unread_mail_older_than_whats_cached_isnt_an_inbox_at_zero() {
        let (_dir, mut s) = synced("unread-older", &[hello(10, true), hello(11, true)]);
        s.set_unread_only(true);
        assert_eq!(uids(&s), Vec::<u32>::new());
        assert!(
            !s.listing_synced(),
            "the server wasn't searched for older unread mail yet"
        );
        searched_unread(&mut s, 10, &[3], &[hello(3, false)]);
        assert_eq!(uids(&s), [3]);
        assert_eq!(s.unread(Some("ada@example.com")), 1, "counted too");
        // Read elsewhere, it's gone at the next search: now the inbox is at zero.
        searched_unread(&mut s, 10, &[], &[]);
        assert_eq!(uids(&s), Vec::<u32>::new());
        assert!(s.listing_synced());
    }

    #[test]
    fn a_message_read_in_a_list_of_unread_mail_stays_until_back_from_it() {
        let (_dir, mut s) = synced(
            "unread-back",
            &[hello(1, false), hello(2, true), hello(3, false)],
        );
        assert_eq!(uids(&s), [3, 2, 1]);
        s.set_unread_only(true);
        assert_eq!(uids(&s), [3, 1]);
        // Read open, it stays listed while open, and so does one read another way meanwhile.
        s.cursor = Some(ada(3));
        open(&mut s, INBOX, 3, true);
        read(&s, 3);
        read(&s, 1);
        s.reload();
        assert_eq!(uids(&s), [3, 1], "read where they're listed");
        s.close_reader();
        assert_eq!(uids(&s), Vec::<u32>::new());
        assert_eq!(s.cursor, None, "nothing left to be on");
        assert!(!s.listing_synced(), "unless older mail is unread");
        searched_unread(&mut s, 1, &[], &[]);
        assert!(s.listing_synced(), "an inbox at zero");
    }

    #[test]
    fn a_cursor_on_a_read_message_moves_to_the_unread_one_after_it() {
        let (_dir, mut s) = synced(
            "unread-cursor",
            &[hello(1, false), hello(2, false), hello(3, false)],
        );
        s.set_unread_only(true);
        s.cursor = Some(ada(2));
        open(&mut s, INBOX, 2, true);
        read(&s, 2);
        s.reload();
        s.close_reader();
        assert_eq!(uids(&s), [3, 1]);
        assert_eq!(s.cursor, Some(ada(1)), "the row after it");
    }

    #[test]
    fn a_mailbox_marked_read_leaves_a_list_of_unread_mail_at_once_but_not_what_came_after() {
        let (_dir, mut s) = synced("unread-all", &[hello(1, false), hello(5, false)]);
        s.set_unread_only(true);
        open(&mut s, INBOX, 5, true);
        read(&s, 1);
        read(&s, 5);
        s.reload();
        assert_eq!(uids(&s), [5, 1], "still listed");
        // Marked below 5: 5 arrived after, and was read on its own.
        s.all_seen("ada@example.com", INBOX, Ok(Some(5)));
        assert_eq!(uids(&s), [5]);
        // Another mailbox shown and back, only unread mail is listed again.
        s.show(Showing::All(Role::Inbox));
        assert_eq!(uids(&s), Vec::<u32>::new());
    }

    #[test]
    fn read_matches_on_the_server_dont_take_the_place_of_unread_ones() {
        let (_dir, mut s) = synced("unread-search", &[hello(1, false), hello(2, true)]);
        s.set_unread_only(true);
        s.search.query = TextEdit::new("hello");
        s.search.found = vec![StoredMessage {
            account: "ada@example.com".into(),
            mailbox: INBOX.into(),
            envelope: hello(9, true),
        }];
        s.list_limit = 1;
        s.reload();
        assert_eq!(uids(&s), [1]);
        assert!(s.has_older(), "a full page");
    }

    /// Opens `uid` in Ada's inbox as the window does, shown read before the server agrees, and,
    /// `asked`, asking the server to mark it so.
    fn open_listed(s: &mut Mail, uid: u32, asked: bool) {
        s.cursor = Some(ada(uid));
        open(s, INBOX, uid, true);
        if asked {
            s.asked(&ada(uid), false);
        }
        if let Some(m) = s.messages.iter_mut().find(|m| m.envelope.uid == uid) {
            m.envelope.seen = true;
        }
    }

    #[test]
    fn a_message_gone_back_from_before_the_server_marked_it_read_leaves_at_once() {
        let (_dir, mut s) = synced("unread-early", &[hello(1, false), hello(3, false)]);
        s.set_unread_only(true);
        open_listed(&mut s, 3, true);
        // Other mail arriving meanwhile rereads the list, which the store says is unread.
        s.reload();
        s.close_reader();
        assert_eq!(uids(&s), [1], "the store still says it's unread");
        assert_eq!(s.cursor, Some(ada(1)), "the row after it");
        // Marked read by the server, it stays out.
        read(&s, 3);
        s.reload();
        assert_eq!(uids(&s), [1]);
        assert!(s.put_away.is_empty(), "nothing left to wait for");
    }

    #[test]
    fn a_message_the_server_wouldnt_mark_read_comes_back() {
        let (_dir, mut s) = synced("unread-refused", &[hello(1, false), hello(3, false)]);
        s.set_unread_only(true);
        open_listed(&mut s, 3, true);
        s.close_reader();
        assert_eq!(uids(&s), [1]);
        s.message_changed(Update::SeenFailed {
            account: "ada@example.com".into(),
            mailbox: INBOX.into(),
            uid: 3,
            seen: true,
            error: "refused".into(),
        });
        assert_eq!(uids(&s), [3, 1]);
    }

    #[test]
    fn a_message_put_away_doesnt_take_the_place_of_an_unread_one() {
        let (_dir, mut s) = synced("unread-page", &[hello(1, false), hello(3, false)]);
        s.set_unread_only(true);
        s.list_limit = 1;
        s.reload();
        assert_eq!(uids(&s), [3]);
        open_listed(&mut s, 3, true);
        s.close_reader();
        assert_eq!(uids(&s), [1]);
        assert!(s.has_older(), "a full page");
    }

    #[test]
    fn a_message_being_moved_doesnt_take_the_place_of_an_unread_one() {
        let (_dir, mut s) = synced("unread-moving", &[hello(1, false), hello(3, false)]);
        s.set_unread_only(true);
        s.list_limit = 1;
        s.moving.insert(ada(3));
        s.reload();
        assert_eq!(uids(&s), [1]);
        assert!(s.has_older(), "a full page");
    }

    #[test]
    fn a_message_opened_without_asking_the_server_stays_unread_and_listed() {
        let (_dir, mut s) = synced("unread-offline", &[hello(1, false), hello(3, false)]);
        s.set_unread_only(true);
        open_listed(&mut s, 3, false);
        s.close_reader();
        assert_eq!(uids(&s), [3, 1]);
    }

    #[test]
    fn a_mailbox_starting_over_forgets_what_was_put_away_there() {
        let (_dir, mut s) = synced("unread-over", &[hello(1, false), hello(3, false)]);
        s.set_unread_only(true);
        open_listed(&mut s, 3, true);
        s.close_reader();
        assert_eq!(uids(&s), [1]);
        // UID 3 names another message now, unread.
        s.started_over("ada@example.com", INBOX);
        s.reload();
        assert_eq!(uids(&s), [3, 1]);
        // Its read mark from before still on its way, going back again leaves the new one.
        s.show(Showing::inbox("ada@example.com"));
        assert_eq!(uids(&s), [3, 1]);
    }

    #[test]
    fn a_message_reopened_after_its_mailbox_started_over_isnt_put_away_for_an_older_mark() {
        let (_dir, mut s) = synced("unread-reopen", &[hello(1, false), hello(3, false)]);
        s.set_unread_only(true);
        open_listed(&mut s, 3, true);
        s.close_reader();
        // UID 3 names another message now, unread, listed again.
        s.started_over("ada@example.com", INBOX);
        s.reload();
        assert_eq!(uids(&s), [3, 1]);
        if let Ok(services) = &s.services {
            let body = Body::default();
            services
                .store
                .save_body("ada@example.com", INBOX, 3, &body, false)
                .unwrap();
        }
        // Downloaded already, it opens without a read mark of its own: the older one is still
        // on its way.
        s.open_message("ada@example.com", INBOX, 3);
        s.close_reader();
        assert_eq!(uids(&s), [3, 1]);
    }

    #[test]
    fn a_message_downloaded_anew_after_its_mailbox_started_over_is_put_away_for_its_own_mark() {
        let (_dir, mut s) = synced("unread-download", &[hello(1, false), hello(3, false)]);
        s.set_unread_only(true);
        open_listed(&mut s, 3, true);
        s.close_reader();
        s.started_over("ada@example.com", INBOX);
        s.reload();
        // Not downloaded yet, opening asks for it, which marks it read as this message. The
        // server can't be reached here, so this asks as opening does.
        s.cursor = Some(ada(3));
        open(&mut s, INBOX, 3, true);
        s.asked(&ada(3), false);
        s.close_reader();
        assert_eq!(uids(&s), [1]);
    }

    #[test]
    fn a_message_read_leaves_the_list_however_it_was_left_for() {
        let (_dir, mut s) = synced("unread-search-key", &[hello(1, false), hello(3, false)]);
        s.set_unread_only(true);
        open_listed(&mut s, 3, true);
        read(&s, 3);
        s.reload();
        assert_eq!(uids(&s), [3, 1], "still open");
        // Left for the search field, not Back.
        s.focus_search();
        assert_eq!(uids(&s), [1]);
        // Read elsewhere while the list shows, it leaves too.
        read(&s, 1);
        s.reload();
        assert_eq!(uids(&s), Vec::<u32>::new());
    }

    #[test]
    fn j_and_k_step_back_to_a_message_read_while_another_is_open() {
        let (_dir, mut s) = synced("unread-jk", &[hello(1, false), hello(3, false)]);
        s.set_unread_only(true);
        open_listed(&mut s, 3, true);
        read(&s, 3);
        s.reload();
        s.move_cursor(1, &mut Tx::default());
        read(&s, 1);
        s.reload();
        assert_eq!(uids(&s), [3, 1], "both read, one open");
        s.move_cursor(-1, &mut Tx::default());
        assert!(
            s.reader
                .as_ref()
                .is_some_and(|r| r.is("ada@example.com", INBOX, 3))
        );
    }

    #[test]
    fn a_message_put_away_comes_back_once_its_read_mark_is_answered_and_it_isnt_read() {
        let (_dir, mut s) = synced("unread-answered", &[hello(1, false), hello(3, false)]);
        s.set_unread_only(true);
        open_listed(&mut s, 3, true);
        s.close_reader();
        assert_eq!(uids(&s), [1]);
        // Answered, but marked unread again elsewhere before the store showed it read.
        s.answered(&ada(3), false);
        s.reload();
        assert_eq!(uids(&s), [3, 1]);
    }

    #[test]
    fn a_message_opened_from_outside_the_list_is_put_away_when_gone_back_from() {
        let (_dir, mut s) = synced("unread-notified", &[hello(1, false), hello(3, false)]);
        s.set_unread_only(true);
        s.show(Showing::Mailbox {
            account: "ada@example.com".into(),
            name: "Archive".into(),
        });
        // Opened from a notification: not in the list shown.
        open(&mut s, INBOX, 3, true);
        s.asked(&ada(3), false);
        s.show(Showing::inbox("ada@example.com"));
        assert_eq!(uids(&s), [1]);
    }

    #[test]
    fn messages_read_one_after_another_stay_out_however_the_list_is_left_for() {
        let leave: [fn(&mut Mail); 3] = [
            |s| s.show(Showing::All(Role::Inbox)),
            |s| s.left_list(ada(1)),
            |s| {
                s.open_form();
                s.form.open = false;
            },
        ];
        for (n, leave) in leave.into_iter().enumerate() {
            let inbox = [hello(1, false), hello(2, false), hello(3, false)];
            let (_dir, mut s) = synced(&format!("unread-leave-{n}"), &inbox);
            s.set_unread_only(true);
            // 3 read, then J to 1... as opening does, both still being marked read.
            open_listed(&mut s, 3, true);
            s.reload();
            s.cursor = Some(ada(1));
            open(&mut s, INBOX, 1, true);
            s.asked(&ada(1), false);
            leave(&mut s);
            s.show(Showing::inbox("ada@example.com"));
            assert_eq!(uids(&s), [2], "left by way {n}");
        }
    }

    #[test]
    fn an_older_download_failing_doesnt_bring_back_one_marked_anew() {
        let (_dir, mut s) = synced("unread-old-download", &[hello(1, false), hello(3, false)]);
        s.set_unread_only(true);
        open_listed(&mut s, 3, true);
        s.close_reader();
        s.started_over("ada@example.com", INBOX);
        s.reload();
        s.cursor = Some(ada(3));
        open(&mut s, INBOX, 3, true);
        s.asked(&ada(3), false);
        s.close_reader();
        assert_eq!(uids(&s), [1]);
        // The older download fails; the new read mark is still on its way.
        s.opened(&ada(3), 1, (Err("gone".into()), false), &mut Tx::default());
        assert_eq!(uids(&s), [1]);
    }

    #[test]
    fn a_message_read_and_pushed_past_the_lists_end_stays_out() {
        let (_dir, mut s) = synced(
            "unread-pushed",
            &[hello(1, false), hello(2, false), hello(3, false)],
        );
        s.set_unread_only(true);
        s.list_limit = 2;
        s.reload();
        open_listed(&mut s, 2, true);
        s.reload();
        s.cursor = Some(ada(3));
        open(&mut s, INBOX, 3, true);
        s.asked(&ada(3), false);
        // New mail pushes 2 past the end while 3 is open.
        if let Ok(services) = &mut s.services {
            services
                .store
                .save_messages("ada@example.com", INBOX, Role::Inbox, 1, &[hello(9, false)])
                .unwrap();
        }
        s.reload();
        s.close_reader();
        s.list_limit = 10;
        s.reload();
        assert_eq!(uids(&s), [9, 1]);
    }

    #[test]
    fn another_mailbox_starting_over_keeps_what_was_read_elsewhere_put_away() {
        let (_dir, mut s) = synced("unread-elsewhere", &[hello(1, false), hello(3, false)]);
        s.set_unread_only(true);
        open_listed(&mut s, 3, true);
        s.reload();
        // Another mailbox's message open, which starts over.
        open(&mut s, "Archive", 7, true);
        s.started_over("ada@example.com", "Archive");
        s.reload();
        assert_eq!(uids(&s), [1]);
    }

    #[test]
    fn removing_the_account_of_the_message_open_keeps_what_was_read_elsewhere_put_away() {
        let (_dir, mut s) = synced("unread-removed", &[hello(1, false), hello(3, false)]);
        s.set_unread_only(true);
        open_listed(&mut s, 3, true);
        s.reload();
        // Another account's message open, whose account is removed.
        open(&mut s, INBOX, 7, true);
        if let Some(reader) = &mut s.reader {
            reader.message.account = "bob@example.com".into();
        }
        s.forget_mail_of("bob@example.com");
        s.reload();
        assert_eq!(uids(&s), [1]);
    }

    #[test]
    fn searching_anew_while_reading_keeps_what_was_read_put_away() {
        let (_dir, mut s) = synced(
            "unread-searched",
            &[hello(1, false), hello(2, false), hello(3, false)],
        );
        s.set_unread_only(true);
        s.list_limit = 2;
        open_listed(&mut s, 2, true);
        s.reload();
        s.cursor = Some(ada(3));
        open(&mut s, INBOX, 3, true);
        s.asked(&ada(3), false);
        // The search changes with a message still open, to one 2 doesn't match, then is cleared,
        // as a clicked notification does.
        s.search.query = TextEdit::new("nothing like it");
        s.search_changed();
        s.close_reader();
        s.search.query = TextEdit::new("");
        s.search_changed();
        s.list_limit = 10;
        s.reload();
        assert_eq!(uids(&s), [1]);
    }

    #[test]
    fn showing_all_mail_and_only_unread_again_keeps_what_was_read_put_away() {
        let inbox = [hello(1, false), hello(2, false), hello(3, false)];
        let (_dir, mut s) = synced("unread-toggled", &inbox);
        s.set_unread_only(true);
        open_listed(&mut s, 2, true);
        s.reload();
        s.cursor = Some(ada(3));
        open(&mut s, INBOX, 3, true);
        s.asked(&ada(3), false);
        s.set_unread_only(false);
        s.close_reader();
        s.set_unread_only(true);
        assert_eq!(uids(&s), [1]);
    }

    #[test]
    fn the_message_open_stays_listed_when_the_list_starts_again() {
        let (_dir, mut s) = synced("unread-still-open", &[hello(1, false), hello(3, false)]);
        s.set_unread_only(true);
        open_listed(&mut s, 3, true);
        s.reload();
        s.set_unread_only(false);
        s.set_unread_only(true);
        assert_eq!(
            uids(&s),
            [3, 1],
            "after showing all mail and only unread again"
        );
        s.search.query = TextEdit::new("hello");
        s.search_changed();
        assert_eq!(uids(&s), [3, 1], "after searching");
        // Left, it's put away as any other.
        s.close_reader();
        assert_eq!(uids(&s), [1]);
    }

    #[test]
    fn the_message_open_past_the_lists_end_stays_listed_when_it_starts_again() {
        let inbox = [hello(1, false), hello(2, false), hello(3, false)];
        let (_dir, mut s) = synced("unread-open-past", &inbox);
        s.set_unread_only(true);
        open_listed(&mut s, 1, true);
        s.reload();
        // The list starts again shorter than where the open message is.
        s.set_unread_only(false);
        s.set_unread_only(true);
        s.list_limit = 2;
        s.reload();
        assert_eq!(uids(&s), [3, 2, 1]);
        s.move_cursor(-1, &mut Tx::default());
        assert!(
            s.reader
                .as_ref()
                .is_some_and(|r| r.is("ada@example.com", INBOX, 2))
        );
    }

    #[test]
    fn the_search_result_open_past_the_lists_end_stays_listed_when_it_starts_again() {
        let inbox = [hello(1, false), hello(2, false), hello(3, false)];
        let (_dir, mut s) = synced("unread-search-past", &inbox);
        s.set_unread_only(true);
        s.search.query = TextEdit::new("hello");
        s.search_changed();
        open_listed(&mut s, 1, true);
        s.reload();
        s.set_unread_only(false);
        s.set_unread_only(true);
        s.list_limit = 2;
        s.reload();
        assert_eq!(uids(&s), [3, 2, 1]);
    }

    #[test]
    fn the_message_open_past_the_lists_end_stays_listed_when_all_mail_shows_again() {
        let inbox = [hello(1, false), hello(2, true), hello(3, true)];
        let (_dir, mut s) = synced("unread-all-again", &inbox);
        s.set_unread_only(true);
        open_listed(&mut s, 1, true);
        s.reload();
        s.list_limit = 2;
        s.set_unread_only(false);
        s.list_limit = 2;
        s.reload();
        assert_eq!(uids(&s), [3, 2, 1]);
        s.move_cursor(-1, &mut Tx::default());
        assert!(
            s.reader
                .as_ref()
                .is_some_and(|r| r.is("ada@example.com", INBOX, 2))
        );
    }

    #[test]
    fn the_message_open_from_elsewhere_isnt_listed_where_it_isnt() {
        for unread_only in [false, true] {
            let name = format!("unread-elsewhere-open-{unread_only}");
            let (_dir, mut s) = synced(&name, &[hello(1, false), hello(3, false)]);
            // Another account's inbox, with a message of its own.
            if let Ok(services) = &mut s.services {
                let mut bob = services.store.account("ada@example.com").unwrap().unwrap();
                bob.id = "bob@example.com".into();
                services.store.save_account(&bob).unwrap();
                services
                    .store
                    .save_messages("bob@example.com", INBOX, Role::Inbox, 1, &[hello(7, false)])
                    .unwrap();
            }
            s.set_unread_only(unread_only);
            // Found searching every inbox, Bob's open.
            s.search.query = TextEdit::new("hello");
            s.search_everywhere(true);
            s.search_changed();
            assert_eq!(uids(&s), [7, 3, 1]);
            s.cursor = Some(("bob@example.com".into(), INBOX.into(), 7));
            open(&mut s, INBOX, 7, true);
            if let Some(reader) = &mut s.reader {
                reader.message.account = "bob@example.com".into();
            }
            s.reload();
            // The search cleared, as a clicked notification does, with Bob's still open.
            s.clear_search();
            s.search_everywhere(false);
            assert_eq!(uids(&s), [3, 1], "unread only: {unread_only}");
        }
    }

    #[test]
    fn an_older_read_mark_refused_doesnt_bring_back_one_marked_anew() {
        let (_dir, mut s) = synced("unread-old-refused", &[hello(1, false), hello(3, false)]);
        s.set_unread_only(true);
        open_listed(&mut s, 3, true);
        s.close_reader();
        s.started_over("ada@example.com", INBOX);
        s.reload();
        // UID 3 names another message now, opened and marked read as it.
        s.cursor = Some(ada(3));
        open(&mut s, INBOX, 3, true);
        s.asked(&ada(3), false);
        s.close_reader();
        assert_eq!(uids(&s), [1]);
        // The older mark is refused; the new one is still on its way.
        s.message_changed(Update::SeenFailed {
            account: "ada@example.com".into(),
            mailbox: INBOX.into(),
            uid: 3,
            seen: true,
            error: "refused".into(),
        });
        assert_eq!(uids(&s), [1]);
    }

    #[test]
    fn showing_the_mailbox_shown_again_is_going_back_from_a_message() {
        let (_dir, mut s) = synced("unread-again", &[hello(1, false), hello(3, false)]);
        s.set_unread_only(true);
        open_listed(&mut s, 3, true);
        read(&s, 3);
        s.reload();
        assert_eq!(uids(&s), [3, 1], "still open");
        s.show(Showing::inbox("ada@example.com"));
        assert_eq!(uids(&s), [1]);
    }

    #[test]
    fn an_account_without_a_sent_mailbox_has_none_to_wait_for() {
        let (_dir, mut s) = synced("unread-sent", &[]);
        s.set_unread_only(true);
        s.show(Showing::All(Role::Sent));
        assert!(!s.listing_synced(), "its mailboxes aren't listed yet");
        let inbox = Mailbox {
            name: INBOX.into(),
            delimiter: None,
            role: Role::Inbox,
        };
        if let Ok(services) = &mut s.services {
            services
                .store
                .save_mailbox_list("ada@example.com", &[inbox])
                .unwrap();
        }
        s.reload();
        assert!(s.listing_synced(), "listed, without one");
    }

    /// What sorting says of the first of `synced`'s inbox: it goes to Receipts, by itself if
    /// that's autosorted to.
    fn sorted_to_receipts() -> Sorted {
        let key: MessageKey = ("ada@example.com".into(), INBOX.into(), 1);
        Sorted {
            tags: [(
                key.clone(),
                Tags {
                    important: false,
                    suggested: Some("Receipts".into()),
                },
            )]
            .into(),
            uid_validity: [(("ada@example.com".into(), INBOX.into()), 1)].into(),
            autosort: vec![Autosort {
                key,
                message_id: "1@example.com".into(),
                to: "Receipts".into(),
            }],
        }
    }

    fn autosorted_noted(s: &Mail, id: &str) -> Option<AutosortState> {
        let store = &s.services.as_ref().unwrap().store;
        let noted = store.autosorted().unwrap();
        noted
            .get(&("ada@example.com".into(), id.into()))
            .map(|a| a.state)
    }

    /// `synced`'s account, its inbox and Receipts listed, Receipts with `role`, autosorted to.
    fn autosorting_to_receipts(s: &mut Mail, role: Role) {
        let account = "ada@example.com";
        let mailbox = |name: &str, role| Mailbox {
            name: name.into(),
            delimiter: None,
            role,
        };
        let listed = [mailbox(INBOX, Role::Inbox), mailbox("Receipts", role)];
        let store = &mut s.services.as_mut().unwrap().store;
        store.save_mailbox_list(account, &listed).unwrap();
        s.statuses.insert(account.into(), Ok(Status::Push));
        s.set_autosort(account, "Receipts", true);
        assert!(s.autosorts_to(account, "Receipts"));
    }

    #[test]
    fn mail_is_autosorted_only_to_a_mailbox_chosen_and_tried_once() {
        let (_dir, mut s) = synced("autosort", &[hello(1, false), hello(2, false)]);
        let account = "ada@example.com";
        s.sorted(0, sorted_to_receipts());
        assert_eq!(autosorted_noted(&s, "1@example.com"), None, "not chosen");
        autosorting_to_receipts(&mut s, Role::Other);
        // Without a server to send it to, the move fails at once, and why is said.
        s.sorted(0, sorted_to_receipts());
        assert_eq!(
            autosorted_noted(&s, "1@example.com"),
            Some(AutosortState::Refused)
        );
        assert!(
            s.notice
                .as_deref()
                .is_some_and(|n| n.contains("couldn't move")),
            "{:?}",
            s.notice
        );
        s.notice = None;
        s.sorted(0, sorted_to_receipts());
        assert_eq!(s.notice, None, "not tried again");
        s.set_autosort(account, "Receipts", false);
        assert!(!s.autosorts_to(account, "Receipts"));
    }

    #[test]
    fn mail_waits_to_be_autosorted_until_its_account_is_connected() {
        let (_dir, mut s) = synced("autosort-offline", &[hello(1, false), hello(2, false)]);
        autosorting_to_receipts(&mut s, Role::Other);
        s.statuses.insert(
            "ada@example.com".into(),
            Ok(Status::Failed {
                error: "offline".into(),
                retry_in: Duration::from_secs(5),
            }),
        );
        s.sorted(0, sorted_to_receipts());
        assert_eq!(autosorted_noted(&s, "1@example.com"), None, "tried later");
    }

    #[test]
    fn mail_moved_to_the_inbox_by_hand_isnt_autosorted() {
        let mut junk = hello(1, false);
        junk.message_id = Some("1@example.com".into());
        let (_dir, mut s) = synced("autosort-not-spam", &[junk, hello(2, false)]);
        autosorting_to_receipts(&mut s, Role::Other);
        s.mailboxes.insert(
            "ada@example.com".into(),
            vec![Mailbox {
                name: INBOX.into(),
                delimiter: None,
                role: Role::Inbox,
            }],
        );
        // As Not spam does, here from the inbox itself, where the test's mail is.
        s.move_message("ada@example.com", INBOX, 1, INBOX);
        assert_eq!(
            autosorted_noted(&s, "1@example.com"),
            Some(AutosortState::Kept)
        );
    }

    #[test]
    fn mail_moved_back_to_the_inbox_with_z_isnt_autosorted() {
        let (_dir, mut s) = synced("autosort-undo", &[hello(1, false), hello(2, false)]);
        autosorting_to_receipts(&mut s, Role::Other);
        s.mailboxes.insert(
            "ada@example.com".into(),
            vec![Mailbox {
                name: INBOX.into(),
                delimiter: None,
                role: Role::Inbox,
            }],
        );
        // Filed in Receipts by hand, then Z.
        s.undo = Some(Undo::Moved {
            from: ("ada@example.com".into(), INBOX.into(), 1),
            to: "Receipts".into(),
            message_id: "1@example.com".into(),
            snoozed: None,
            done: true,
        });
        s.undo();
        assert_eq!(
            autosorted_noted(&s, "1@example.com"),
            Some(AutosortState::Kept)
        );
    }

    #[test]
    fn mail_whose_menu_is_open_isnt_autosorted() {
        let (_dir, mut s) = synced("autosort-menu", &[hello(1, false), hello(2, false)]);
        autosorting_to_receipts(&mut s, Role::Other);
        // Its suggestion first comes while the menu is open: the menu closes, as its items
        // would move, but the message stays.
        s.open_message_menu("ada@example.com", INBOX, 1, PointerPress { x: 0.0, y: 0.0 });
        s.sorted(0, sorted_to_receipts());
        assert!(s.message_menu.is_none());
        assert_eq!(autosorted_noted(&s, "1@example.com"), None);
        // The same suggestion, the menu open again: it stays open, and so does the message.
        s.open_message_menu("ada@example.com", INBOX, 1, PointerPress { x: 0.0, y: 0.0 });
        s.sorted(0, sorted_to_receipts());
        assert!(s.message_menu.is_some(), "left open");
        assert_eq!(autosorted_noted(&s, "1@example.com"), None);
    }

    #[test]
    fn mail_being_marked_read_isnt_autosorted_before() {
        let (_dir, mut s) = synced("autosort-all-read", &[hello(1, false), hello(2, false)]);
        autosorting_to_receipts(&mut s, Role::Other);
        s.marking_seen
            .insert(("ada@example.com".into(), INBOX.into()));
        s.sorted(0, sorted_to_receipts());
        assert_eq!(autosorted_noted(&s, "1@example.com"), None);
    }

    #[test]
    fn a_selected_message_autosort_couldnt_move_stays_selected() {
        let (_dir, mut s) = synced("autosort-cursor", &[hello(1, false), hello(2, false)]);
        let key: MessageKey = ("ada@example.com".into(), INBOX.into(), 1);
        autosorting_to_receipts(&mut s, Role::Other);
        s.cursor = Some(key.clone());
        // Without a server to send it to, the move fails at once.
        s.sorted(0, sorted_to_receipts());
        assert_eq!(s.cursor, Some(key));
    }

    #[test]
    fn mail_isnt_autosorted_to_a_mailbox_mail_isnt_filed_in_anymore() {
        let (_dir, mut s) = synced("autosort-trash", &[hello(1, false), hello(2, false)]);
        // The server says Receipts is its Trash now.
        autosorting_to_receipts(&mut s, Role::Trash);
        s.sorted(0, sorted_to_receipts());
        assert_eq!(autosorted_noted(&s, "1@example.com"), None);
    }

    #[test]
    fn autosort_moves_a_few_messages_at_once() {
        let (_dir, mut s) = synced("autosort-few", &[hello(1, false), hello(2, false)]);
        let account = "ada@example.com";
        autosorting_to_receipts(&mut s, Role::Other);
        for uid in 10..13 {
            let key = (account.into(), INBOX.into(), uid);
            s.autosorting.insert(key, format!("{uid}@example.com"));
        }
        s.sorted(0, sorted_to_receipts());
        assert_eq!(
            autosorted_noted(&s, "1@example.com"),
            None,
            "once one of those is answered"
        );
    }

    #[test]
    fn mail_the_user_is_acting_on_isnt_autosorted() {
        let (_dir, mut s) = synced("autosort-busy", &[hello(1, false), hello(2, false)]);
        let account = "ada@example.com";
        let key: MessageKey = (account.into(), INBOX.into(), 1);
        autosorting_to_receipts(&mut s, Role::Other);
        // Snoozed while its read mark is on its way: the snooze waits for it.
        s.asked(&key, false);
        let message = s.find_message(&key).cloned().unwrap();
        s.snooze_message(&message, Timestamp::now() + Duration::from_hours(3));
        assert!(s.deferred.contains_key(&key));
        let undo = s.undo.clone();
        s.sorted(0, sorted_to_receipts());
        assert!(
            matches!(s.deferred.get(&key), Some((Deferred::Snooze(..), _))),
            "the snooze stands"
        );
        assert_eq!(s.undo, undo, "and Z still undoes what it did");
        assert_eq!(autosorted_noted(&s, "1@example.com"), None);
    }

    #[test]
    fn mail_snoozed_isnt_autosorted_once_it_wakes() {
        let mut snoozed = hello(1, false);
        snoozed.message_id = Some("1@example.com".into());
        let (_dir, mut s) = synced("autosort-snoozed", &[snoozed, hello(2, false)]);
        let key: MessageKey = ("ada@example.com".into(), INBOX.into(), 1);
        autosorting_to_receipts(&mut s, Role::Other);
        let message = s.find_message(&key).cloned().unwrap();
        s.snooze_message(&message, Timestamp::now() + Duration::from_hours(3));
        assert_eq!(
            autosorted_noted(&s, "1@example.com"),
            Some(AutosortState::Kept)
        );
    }

    #[test]
    fn what_the_server_answers_an_autosort_is_noted() {
        let (_dir, mut s) = synced("autosort-answer", &[hello(1, false), hello(2, false)]);
        let account = "ada@example.com";
        let attempt = |s: &mut Mail, uid: u32| {
            let id = format!("{uid}@example.com");
            let store = &s.services.as_ref().unwrap().store;
            assert!(store.note_autosorted(account, &id, "Receipts").unwrap());
            s.autosorting
                .insert((account.into(), INBOX.into(), uid), id);
        };
        attempt(&mut s, 1);
        s.moved(account, INBOX, 1, "Receipts", Err("no".into()));
        assert_eq!(
            autosorted_noted(&s, "1@example.com"),
            Some(AutosortState::Refused)
        );
        attempt(&mut s, 2);
        s.moved(account, INBOX, 2, "Receipts", Ok(true));
        assert_eq!(
            autosorted_noted(&s, "2@example.com"),
            Some(AutosortState::Moved)
        );
        assert_eq!(
            s.notice.as_deref(),
            Some("Autosorted a new message to Receipts")
        );
        assert_eq!(s.undo, None, "Z undoes what was done by hand");
    }

    #[test]
    fn tags_of_a_mailbox_started_over_since_arent_shown() {
        let (_dir, mut s) = synced("sorted-validity", &[hello(1, false)]);
        let key: MessageKey = ("ada@example.com".into(), INBOX.into(), 1);
        let sorted_under = |uid_validity| Sorted {
            tags: [(
                key.clone(),
                Tags {
                    important: true,
                    suggested: None,
                },
            )]
            .into(),
            uid_validity: [(("ada@example.com".into(), INBOX.into()), uid_validity)].into(),
            autosort: Vec::new(),
        };
        s.sorted(0, sorted_under(7));
        assert!(s.tags(&key).is_none(), "its UID named another message then");
        s.sorted(0, sorted_under(1));
        assert!(s.tags(&key).is_some_and(|t| t.important));
        s.sorted(1, sorted_under(1));
        assert!(
            s.tags(&key).is_some(),
            "of another generation, not shown instead"
        );
    }

    #[test]
    fn a_mailbox_never_synced_isnt_an_inbox_at_zero() {
        let (_dir, mut s) = synced("unread-unsynced", &[]);
        s.set_unread_only(true);
        assert!(s.listing_synced(), "synced, if empty");
        s.show(Showing::Mailbox {
            account: "ada@example.com".into(),
            name: "Archive".into(),
        });
        assert!(!s.listing_synced());
    }

    #[test]
    fn a_message_moved_back_isnt_announced_as_new_mail() {
        let mut s = Mail::not_started();
        let back = |id: &str| Restoring {
            account: "ada@example.com".into(),
            mailbox: "INBOX".into(),
            message_id: id.into(),
        };
        s.returning = vec![back("m@example.com")];
        let target = || Target {
            account: "ada@example.com".into(),
            mailbox: "INBOX".into(),
            uid_validity: 1,
            uid: None,
        };
        // New mail saved before it isn't taken for it.
        s.inbox_mail(target(), 1, false, &[], &["n@example.com".into()]);
        assert!(
            s.notice
                .as_deref()
                .is_some_and(|n| n.contains("1 new message"))
        );
        assert_eq!(s.returning, vec![back("m@example.com")]);
        s.dismiss_notice();
        s.inbox_mail(target(), 1, false, &[], &["m@example.com".into()]);
        assert_eq!(s.notice, None, "it isn't new");
        assert_eq!(s.returning, vec![]);
        // Arriving again later, as when moved back by hand, it's new.
        s.inbox_mail(target(), 1, false, &[], &["m@example.com".into()]);
        assert!(s.notice.is_some());
    }
}
