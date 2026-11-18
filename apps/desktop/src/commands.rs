//! Everything the app can be asked to do from the keyboard, in one place: the command palette
//! (⌘K) lists what [`available`] gives, and the window's key handler runs the command whose
//! shortcut was pressed. A command is offered, and its shortcuts work, only where it applies.

use std::hash::{DefaultHasher, Hash, Hasher};

use jiff::{Timestamp, Zoned};
use postden_core::message::Role;
use postden_store::StoredMessage;
use rune::{Key, KeyEvent, TextEdit, Tx};
use rune_kit::shell;

use crate::search::MessageKey;
use crate::state::{Answer, Mail, SettingsSection, Showing};

/// A key, or chord, that runs a command.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Shortcut {
    /// ⌘ (Ctrl elsewhere) and a letter, with ⇧ or without.
    Command {
        key: char,
        shift: bool,
    },
    /// A key on its own, when no field takes what's typed: a letter, or a symbol such as `/`.
    Key(char),
    /// G, then a key on its own: "go to".
    Go(char),
    Escape,
    Enter,
    Up,
    Down,
    /// ⇧⌘B, and ⌃⌘S on macOS: the shell's own.
    Sidebar,
}

/// Whether `ev` is `c` typed on its own: no ⌘ or ⌃, and no ⇧ or ⌥ for a letter (a symbol may
/// need them, as `#` does: ⇧3 on a US layout, ⌥3 on a British Mac).
fn typed(ev: &KeyEvent, c: char) -> bool {
    let m = ev.mods;
    if m.cmd || m.ctrl || m.logo || ((m.shift || m.alt) && c.is_alphabetic()) {
        return false;
    }
    // Caps Lock types a capital without ⇧.
    let mut chars = match &ev.key {
        Key::Text(t) => t.chars(),
        _ => return false,
    };
    chars.next().is_some_and(|t| t.eq_ignore_ascii_case(&c)) && chars.next().is_none()
}

/// Whether `ev` is `key` with no modifier.
fn bare(ev: &KeyEvent, key: &Key) -> bool {
    let m = ev.mods;
    ev.key == *key && !(m.shift || m.cmd || m.ctrl || m.alt || m.logo)
}

impl Shortcut {
    const fn cmd(key: char) -> Self {
        Self::Command { key, shift: false }
    }

    const fn shift_cmd(key: char) -> Self {
        Self::Command { key, shift: true }
    }

    /// Whether `ev` presses it, `go` when G was pressed just before.
    pub(crate) fn matches(self, ev: &KeyEvent, go: bool) -> bool {
        match self {
            Self::Command { key, shift } => ev.is_command(key) && ev.mods.shift == shift,
            Self::Key(c) => !go && typed(ev, c),
            Self::Go(c) => go && typed(ev, c),
            // After G, Escape takes it back.
            Self::Escape => !go && ev.key == Key::Escape,
            Self::Enter => !go && bare(ev, &Key::Enter),
            Self::Up => !go && bare(ev, &Key::Up),
            Self::Down => !go && bare(ev, &Key::Down),
            Self::Sidebar => shell::is_sidebar_toggle(ev),
        }
    }

    /// The keys to press, one keycap each.
    pub(crate) fn keycaps(self) -> Vec<String> {
        let key = |c: char| c.to_uppercase().to_string();
        match self {
            Self::Command { key: c, shift } => {
                vec![format!("{}⌘{}", if shift { "⇧" } else { "" }, key(c))]
            }
            Self::Key(c) => vec![key(c)],
            Self::Go(c) => vec!["G".into(), key(c)],
            Self::Escape => vec!["Esc".into()],
            Self::Enter => vec!["↩".into()],
            Self::Up => vec!["↑".into()],
            Self::Down => vec!["↓".into()],
            Self::Sidebar => vec!["⇧⌘B".into()],
        }
    }

    /// How menus and screen readers give it.
    pub(crate) fn label(self) -> String {
        self.keycaps().join(" ")
    }
}

/// One thing the app can do.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub(crate) enum Command {
    Compose,
    Search,
    Reply,
    ReplyAll,
    Forward,
    ToggleSeen,
    /// Stars the focused message, or takes its star away.
    ToggleStar,
    /// Moves the focused message to its account's Archive.
    Archive,
    /// Deletes the focused message: moves it to its account's Trash.
    Trash,
    /// Moves the focused message to its account's Junk mailbox.
    Spam,
    /// Moves the focused message out of its account's Junk mailbox, to its inbox.
    NotSpam,
    /// Undoes the last thing done to a message.
    Undo,
    ExportMessage,
    CloseReader,
    /// Moves the list's cursor down, or opens the next message while one is open.
    NextMessage,
    /// Moves the list's cursor up, or opens the previous message while one is open.
    PreviousMessage,
    /// Opens the message under the list's cursor.
    OpenMessage,
    /// Lists when the focused message can be snoozed until.
    PickSnooze,
    /// Lists where the focused message can be moved to.
    PickMove,
    /// Lists every mailbox to go to.
    PickMailbox,
    SnoozeUntil {
        message: MessageKey,
        label: &'static str,
        until: Timestamp,
    },
    MoveTo {
        message: MessageKey,
        mailbox: String,
    },
    Show(Showing),
    /// Syncs what's shown with the server now: one mailbox, or every account's inbox or Sent.
    Refresh,
    /// Marks every message in the mailbox shown read.
    MarkAllSeen {
        account: String,
        mailbox: String,
    },
    AccountSettings(String),
    AddAccount,
    Settings,
    ToggleSidebar,
    CheckForUpdates,
}

/// The account the window is about: the one listed (or searched), else the focused message's,
/// else the only one.
fn context_account(s: &Mail) -> Option<&str> {
    let only = match s.accounts.as_slice() {
        [only] => Some(only.id.as_str()),
        _ => None,
    };
    s.listing()
        .account()
        .or_else(|| s.focused_message().map(|m| m.account.as_str()))
        .or(only)
}

impl Command {
    /// What runs it, the key on its own first.
    pub(crate) fn shortcuts(&self, s: &Mail) -> Vec<Shortcut> {
        use Shortcut::{Down, Enter, Escape, Go, Key, Sidebar, Up};
        match self {
            Self::Compose => vec![Key('c'), Shortcut::cmd('n')],
            Self::Search => vec![Key('/'), Shortcut::cmd('f')],
            Self::Reply => vec![Key('r'), Shortcut::cmd('r')],
            Self::ReplyAll => vec![Shortcut::shift_cmd('r')],
            Self::Forward => vec![Key('f'), Shortcut::shift_cmd('f')],
            Self::ToggleSeen => vec![Key('u'), Shortcut::shift_cmd('u')],
            Self::ToggleStar => vec![Key('s'), Shortcut::shift_cmd('l')],
            Self::Archive => vec![Key('e')],
            Self::Trash => vec![Key('#')],
            // Only one of them applies at a time: in Junk, the same keys take a message out.
            Self::Spam | Self::NotSpam => vec![Key('!'), Shortcut::shift_cmd('j')],
            Self::Undo => vec![Key('z')],
            Self::CloseReader => vec![Escape],
            // ↑ and ↓ move the cursor in the list; an open message steps with J and K alone.
            Self::NextMessage if s.reader.is_none() => vec![Key('j'), Down],
            Self::NextMessage => vec![Key('j')],
            Self::PreviousMessage if s.reader.is_none() => vec![Key('k'), Up],
            Self::PreviousMessage => vec![Key('k')],
            Self::OpenMessage => vec![Enter],
            Self::PickSnooze => vec![Key('h')],
            Self::PickMove => vec![Key('v')],
            Self::PickMailbox => vec![Go('l')],
            Self::Show(Showing::All(Role::Sent)) => vec![Go('t')],
            Self::Show(Showing::All(_)) => vec![Go('i')],
            Self::Show(Showing::Mailbox { account, name })
                if Some(account.as_str()) == context_account(s) =>
            {
                let role = s
                    .mailboxes
                    .get(account)
                    .and_then(|list| list.iter().find(|m| m.name == *name))
                    .map(|m| m.role);
                match role {
                    Some(Role::Archive) => vec![Go('e')],
                    Some(Role::Snoozed) => vec![Go('h')],
                    Some(Role::Trash) => vec![Go('#')],
                    Some(Role::Junk) => vec![Go('!')],
                    _ => Vec::new(),
                }
            }
            Self::Refresh => vec![Shortcut::shift_cmd('n')],
            Self::Settings => vec![Shortcut::cmd(',')],
            Self::ToggleSidebar => vec![Sidebar],
            _ => Vec::new(),
        }
    }

    /// Its row's key in the palette: the same for as long as it's offered, so a screen reader
    /// hears the highlight move to another command whatever row that is. Hashed, as a mailbox's
    /// name can hold any character.
    pub(crate) fn key(&self) -> String {
        let mut hasher = DefaultHasher::new();
        self.hash(&mut hasher);
        format!("palette-{:016x}", hasher.finish())
    }

    /// Its ⌘ shortcut as menus show it, or nothing.
    pub(crate) fn hint(&self, s: &Mail) -> String {
        self.shortcuts(s)
            .into_iter()
            .find(|k| matches!(k, Shortcut::Command { .. }))
            .map(Shortcut::label)
            .unwrap_or_default()
    }

    /// What the palette calls it.
    pub(crate) fn title(&self, s: &Mail) -> String {
        match self {
            Self::Compose => "New message".into(),
            Self::Search => "Search mail".into(),
            Self::Reply => "Reply".into(),
            Self::ReplyAll => "Reply all".into(),
            Self::Forward => "Forward".into(),
            Self::ToggleSeen => match s.focused_message() {
                Some(m) if m.envelope.seen => "Mark as unread".into(),
                _ => "Mark as read".into(),
            },
            Self::ToggleStar => match s.focused_message() {
                Some(m) if m.envelope.flagged => "Remove star".into(),
                _ => "Star".into(),
            },
            Self::Archive => "Archive".into(),
            Self::Trash => "Delete".into(),
            Self::Spam => "Mark as spam".into(),
            Self::NotSpam => "Not spam".into(),
            Self::Undo => s.undo_title(),
            Self::ExportMessage => "Export as .eml…".into(),
            Self::CloseReader => "Back to the list".into(),
            Self::NextMessage => "Next message".into(),
            Self::PreviousMessage => "Previous message".into(),
            Self::OpenMessage => "Open message".into(),
            Self::PickSnooze => "Snooze…".into(),
            Self::PickMove => "Move to…".into(),
            Self::PickMailbox => "Go to mailbox…".into(),
            Self::SnoozeUntil { label, .. } => (*label).into(),
            Self::MoveTo { message, mailbox } => s.mailbox_label(&message.0, mailbox),
            Self::Show(Showing::All(Role::Sent)) => "Go to Sent".into(),
            Self::Show(Showing::All(_)) => "Go to All inboxes".into(),
            Self::Show(Showing::Mailbox { account, name }) => {
                format!("Go to {}", s.mailbox_label(account, name))
            }
            Self::Refresh => "Refresh".into(),
            Self::MarkAllSeen { .. } => "Mark all as read".into(),
            Self::AccountSettings(_) => "Account settings".into(),
            Self::AddAccount => "Add account".into(),
            Self::Settings => "Settings".into(),
            Self::ToggleSidebar => "Show or hide the sidebar".into(),
            Self::CheckForUpdates => "Check for updates".into(),
        }
    }

    /// What it applies to, where that isn't plain from the title: an account or a mailbox, or
    /// when a snooze ends.
    pub(crate) fn detail(&self, s: &Mail) -> Option<String> {
        match self {
            Self::Show(Showing::Mailbox { account, .. }) | Self::AccountSettings(account) => {
                Some(s.label(account).to_owned())
            }
            Self::MarkAllSeen { account, mailbox } => Some(format!(
                "{} · {}",
                s.mailbox_label(account, mailbox),
                s.label(account)
            )),
            Self::SnoozeUntil { until, .. } => {
                Some(postden_app::format_snooze(*until, &Zoned::now()))
            }
            _ => None,
        }
    }

    pub(crate) fn run(self, s: &mut Mail, tx: &mut Tx) {
        match self {
            Self::Compose => s.compose(),
            Self::Search => s.focus_search(),
            Self::Reply => s.answer_focused(Answer::Reply),
            Self::ReplyAll => s.answer_focused(Answer::ReplyAll),
            Self::Forward => s.answer_focused(Answer::Forward),
            Self::ToggleSeen => s.toggle_seen(),
            Self::ToggleStar => s.toggle_star(),
            Self::Archive => s.move_focused_to(Role::Archive),
            Self::Trash => s.move_focused_to(Role::Trash),
            Self::Spam => s.move_focused_to(Role::Junk),
            Self::NotSpam => s.move_focused_to(Role::Inbox),
            Self::Undo => s.undo(),
            Self::ExportMessage => {
                if let Some(message) = s.focused_message() {
                    Mail::export_message(tx, message);
                }
            }
            Self::CloseReader => {
                s.close_reader();
                // With the list as it comes back, not a frame after (as its `on_visible` would).
                s.reveal_cursor(tx);
            }
            // Held down with a message open, it doesn't open (and mark read) every one it passes.
            Self::NextMessage | Self::PreviousMessage if s.reader.is_some() && tx.key_repeat() => {}
            Self::NextMessage => s.move_cursor(1, tx),
            Self::PreviousMessage => s.move_cursor(-1, tx),
            Self::OpenMessage => s.open_cursor(),
            Self::PickSnooze | Self::PickMove => {
                if let Some(m) = s.focused_message() {
                    let message = (m.account.clone(), m.mailbox.clone(), m.envelope.uid);
                    s.open_palette(if self == Self::PickSnooze {
                        let times = postden_app::snooze_times(&Zoned::now());
                        PaletteMode::Snooze { message, times }
                    } else {
                        PaletteMode::Move(message)
                    });
                }
            }
            Self::PickMailbox => s.open_palette(PaletteMode::Mailboxes),
            Self::SnoozeUntil { message, until, .. } => s.snooze_key(&message, until),
            Self::MoveTo { message, mailbox } => {
                s.move_message(&message.0, &message.1, message.2, &mailbox);
            }
            Self::Show(showing) => s.show(showing),
            Self::Refresh => s.refresh_shown(),
            Self::MarkAllSeen { account, mailbox } => s.mark_all_seen(&account, &mailbox),
            Self::AccountSettings(account) => s.open_account_settings(&account),
            Self::AddAccount => s.open_form(),
            Self::Settings => s.open_settings(),
            Self::ToggleSidebar => s.sidebar.toggle(),
            Self::CheckForUpdates => {
                // Its progress, and its result when there's nothing new or it fails, show in
                // the settings' updates, which the palette would otherwise leave unseen; where
                // they can't be shown, as while moving to another computer, a notice says it.
                s.open_settings();
                s.show_settings_section(SettingsSection::General);
                s.check_for_updates(true);
            }
        }
    }
}

/// Whether `m` can be snoozed: not once it waits in the Snoozed mailbox already.
fn snoozable(s: &Mail, m: &StoredMessage) -> bool {
    s.mailbox_with_role(&m.account, Role::Snoozed)
        .is_none_or(|b| b.name != m.mailbox)
}

/// Whether `m` can be moved to its account's mailbox with `role`: it has one, and `m` isn't there.
fn can_move_to(s: &Mail, m: &StoredMessage, role: Role) -> bool {
    s.mailbox_with_role(&m.account, role)
        .is_some_and(|b| b.name != m.mailbox)
}

/// Whether `m` is in its account's mailbox with `role`.
fn in_role(s: &Mail, m: &StoredMessage, role: Role) -> bool {
    s.mailbox_with_role(&m.account, role)
        .is_some_and(|b| b.name == m.mailbox)
}

/// Where `message` can be moved to: its account's other mailboxes, the inbox first.
fn destinations(s: &Mail, message: &MessageKey) -> Vec<Command> {
    let listed = s.mailboxes.get(&message.0).map_or(&[][..], Vec::as_slice);
    let (inbox, rest): (Vec<_>, Vec<_>) = listed.iter().partition(|m| m.role == Role::Inbox);
    inbox
        .into_iter()
        .chain(rest)
        .filter(|b| b.name != message.1)
        .map(|b| Command::MoveTo {
            message: message.clone(),
            mailbox: b.name.clone(),
        })
        .collect()
}

/// The commands that apply to what the window shows, in the palette's order.
pub(crate) fn available(s: &Mail) -> Vec<Command> {
    // The list, or a message from it, is shown: not a form, a message being written or the
    // settings.
    let browsing = !s.form.open && !s.compose.open && !s.settings_open && !s.updater.dialog_open;
    let mut commands = Vec::new();
    let mut add = |on: bool, command: Command| {
        if on {
            commands.push(command);
        }
    };
    let has_accounts = !s.accounts.is_empty();
    add(browsing && has_accounts, Command::Compose);
    if let Some(m) = s.focused_message().filter(|_| browsing) {
        // An open message is replied to once it's downloaded; one in the list opens first.
        let replies = s
            .reader
            .as_ref()
            .is_none_or(|r| matches!(r.body, Some(Ok(_))));
        let key = (m.account.clone(), m.mailbox.clone(), m.envelope.uid);
        add(replies, Command::Reply);
        add(replies, Command::ReplyAll);
        add(replies, Command::Forward);
        add(can_move_to(s, m, Role::Archive), Command::Archive);
        add(true, Command::ToggleSeen);
        add(true, Command::ToggleStar);
        add(snoozable(s, m), Command::PickSnooze);
        add(!destinations(s, &key).is_empty(), Command::PickMove);
        add(can_move_to(s, m, Role::Trash), Command::Trash);
        add(can_move_to(s, m, Role::Junk), Command::Spam);
        add(
            in_role(s, m, Role::Junk) && can_move_to(s, m, Role::Inbox),
            Command::NotSpam,
        );
        add(true, Command::ExportMessage);
    }
    add(browsing && s.undo.is_some(), Command::Undo);
    add(browsing && s.reader.is_some(), Command::CloseReader);
    let listed = s.shown().next().is_some();
    add(
        browsing && (s.reader.is_some() || listed),
        Command::NextMessage,
    );
    add(
        browsing && (s.reader.is_some() || listed),
        Command::PreviousMessage,
    );
    add(
        browsing && s.reader.is_none() && s.focused_message().is_some(),
        Command::OpenMessage,
    );
    add(browsing && has_accounts, Command::Search);
    add(browsing && has_accounts, Command::Refresh);
    if let Showing::Mailbox { account, name } = &s.showing
        && browsing
        && !s.marking_seen(account, name)
    {
        add(
            true,
            Command::MarkAllSeen {
                account: account.clone(),
                mailbox: name.clone(),
            },
        );
    }
    if browsing && has_accounts {
        add(true, Command::PickMailbox);
        add(true, Command::Show(Showing::All(Role::Inbox)));
        add(true, Command::Show(Showing::All(Role::Sent)));
        for account in &s.accounts {
            let listed = s.mailboxes.get(&account.id).map_or(&[][..], Vec::as_slice);
            // The inbox first, as the sidebar lists them.
            let (inbox, rest): (Vec<_>, Vec<_>) =
                listed.iter().partition(|m| m.role == Role::Inbox);
            for mailbox in inbox.into_iter().chain(rest) {
                add(
                    true,
                    Command::Show(Showing::Mailbox {
                        account: account.id.clone(),
                        name: mailbox.name.clone(),
                    }),
                );
            }
        }
    }
    if let Some(account) = s.showing.account().filter(|_| browsing) {
        add(true, Command::AccountSettings(account.to_owned()));
    }
    add(!s.compose.open && !s.form.open, Command::AddAccount);
    add(true, Command::Settings);
    add(true, Command::ToggleSidebar);
    add(s.updater.can_check(), Command::CheckForUpdates);
    commands
}

/// Whether `query` picks out a command called `title`, for `detail`: each of its words starts a
/// word of either, ignoring case. "go sent" finds "Go to Sent", "mark read" "Mark all as read".
pub(crate) fn matches(query: &str, title: &str, detail: Option<&str>) -> bool {
    let words: Vec<String> = format!("{title} {}", detail.unwrap_or_default())
        .split(|c: char| !c.is_alphanumeric())
        .filter(|w| !w.is_empty())
        .map(str::to_lowercase)
        .collect();
    query
        .split(|c: char| !c.is_alphanumeric())
        .filter(|q| !q.is_empty())
        .map(str::to_lowercase)
        .all(|q| words.iter().any(|w| w.starts_with(&q)))
}

/// What the palette lists.
#[derive(Debug, Clone, Default, PartialEq)]
pub(crate) enum PaletteMode {
    /// Every command that applies.
    #[default]
    Commands,
    /// When a message can be snoozed until, as of when the list opened.
    Snooze {
        message: MessageKey,
        times: Vec<(&'static str, Timestamp)>,
    },
    /// Where a message can be moved to.
    Move(MessageKey),
    /// Every mailbox to go to.
    Mailboxes,
}

/// The command palette: a search over [`available`] commands, or over one command's choices.
#[derive(Default)]
pub(crate) struct Palette {
    pub(crate) open: bool,
    pub(crate) mode: PaletteMode,
    pub(crate) query: TextEdit,
    /// The highlighted command, which Return runs; the first row when it's `None` or no longer
    /// shown. A command, not a row, so rows that come and go (Reply, once a message downloads)
    /// don't move the highlight onto another.
    pub(crate) active: Option<Command>,
}

/// One row of the palette.
pub(crate) struct Entry {
    pub(crate) command: Command,
    pub(crate) title: String,
    pub(crate) detail: Option<String>,
    pub(crate) shortcuts: Vec<Shortcut>,
}

impl Palette {
    /// The commands that match what's typed, in [`available`]'s order, or the choices of the
    /// command it was opened for, while its message is still there.
    pub(crate) fn shown(&self, s: &Mail) -> Vec<Entry> {
        let commands = match &self.mode {
            PaletteMode::Commands => available(s),
            PaletteMode::Mailboxes => available(s)
                .into_iter()
                .filter(|c| matches!(c, Command::Show(_)))
                .collect(),
            PaletteMode::Snooze { message, times } => match s.find_message(message) {
                Some(m) if snoozable(s, m) => times
                    .iter()
                    .map(|&(label, until)| Command::SnoozeUntil {
                        message: message.clone(),
                        label,
                        until,
                    })
                    .collect(),
                _ => Vec::new(),
            },
            PaletteMode::Move(message) if s.find_message(message).is_some() => {
                destinations(s, message)
            }
            PaletteMode::Move(_) => Vec::new(),
        };
        commands
            .into_iter()
            .map(|command| Entry {
                title: command.title(s),
                detail: command.detail(s),
                shortcuts: command.shortcuts(s),
                command,
            })
            .filter(|e| matches(&self.query.text, &e.title, e.detail.as_deref()))
            .collect()
    }

    /// What the field asks for.
    pub(crate) const fn placeholder(&self) -> &'static str {
        match self.mode {
            PaletteMode::Commands => "Type a command",
            PaletteMode::Snooze { .. } => "Snooze until…",
            PaletteMode::Move(_) => "Move to…",
            PaletteMode::Mailboxes => "Go to…",
        }
    }

    /// The highlighted row of `shown`.
    pub(crate) fn active_row(&self, shown: &[Entry]) -> usize {
        self.active
            .as_ref()
            .and_then(|a| shown.iter().position(|e| &e.command == a))
            .unwrap_or(0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::state::Undo;
    use postden_core::account::{Account, Auth, Security, Server};

    fn account(id: &str) -> Account {
        let server = Server {
            host: "localhost".into(),
            port: 1,
            security: Security::None,
            username: "me".into(),
        };
        Account {
            id: id.into(),
            display_name: None,
            label: None,
            imap: server.clone(),
            smtp: server,
            auth: Auth::Password,
        }
    }

    #[test]
    fn words_match_the_start_of_words_in_any_order() {
        assert!(matches("", "New message", None));
        assert!(matches("new", "New message", None));
        assert!(matches("MESS", "New message", None));
        assert!(matches("go sent", "Go to Sent", None));
        assert!(matches("sent go", "Go to Sent", None));
        assert!(matches("mark read", "Mark all as read", None));
        assert!(matches("work", "Go to Inbox", Some("work@example.com")));
        assert!(!matches("essage", "New message", None));
        assert!(!matches("reply x", "Reply all", None));
        assert!(matches(".eml", "Export as .eml…", None));
        assert!(matches(
            "ada@example.com",
            "Go to Inbox",
            Some("ada@example.com")
        ));
    }

    #[test]
    fn shortcuts_read_as_in_the_menus() {
        assert_eq!(Shortcut::cmd('n').label(), "⌘N");
        assert_eq!(Shortcut::shift_cmd('r').label(), "⇧⌘R");
        assert_eq!(Shortcut::cmd(',').label(), "⌘,");
    }

    #[test]
    fn without_accounts_only_app_wide_commands_are_offered() {
        let s = Mail::not_started();
        let titles: Vec<String> = available(&s).iter().map(|c| c.title(&s)).collect();
        assert_eq!(
            titles,
            [
                "Add account",
                "Settings",
                "Show or hide the sidebar",
                "Check for updates"
            ]
        );
    }

    #[test]
    fn the_settings_offer_neither_writing_nor_searching() {
        let mut s = Mail::not_started();
        s.open_settings();
        let commands = available(&s);
        assert!(!commands.contains(&Command::Compose));
        assert!(!commands.contains(&Command::Search));
        assert!(commands.contains(&Command::Settings));
    }

    #[test]
    fn the_palette_opens_empty_and_closes_again() {
        let mut s = Mail::not_started();
        s.toggle_palette();
        s.palette.query = TextEdit::new("upd");
        s.palette.active = Some(Command::Settings);
        s.toggle_palette();
        assert!(!s.palette.open);
        s.toggle_palette();
        assert!(s.palette.open && s.palette.query.focused);
        assert_eq!(s.palette.query.text, "");
        assert_eq!(s.palette.active, None);
    }

    #[test]
    fn the_highlight_wraps_around_what_matches() {
        let mut s = Mail::not_started();
        s.toggle_palette();
        assert_eq!(s.move_palette(-1), 3);
        assert_eq!(s.palette.active, Some(Command::CheckForUpdates));
        assert_eq!(s.move_palette(1), 0);
        s.palette.query = TextEdit::new("s");
        s.palette_changed();
        // "Settings", "Show or hide the sidebar".
        s.move_palette(1);
        assert_eq!(s.move_palette(1), 0);
    }

    #[test]
    fn a_clicked_notification_closes_the_palette() {
        let mut s = Mail::not_started();
        s.toggle_palette();
        s.notification_clicked(&crate::notifications::Target {
            account: "ada@example.com".into(),
            mailbox: "INBOX".into(),
            uid_validity: 1,
            uid: Some(1),
        });
        assert!(!s.palette.open);
    }

    #[test]
    fn the_highlight_stays_on_its_command_as_rows_come_and_go() {
        let mut s = Mail::not_started();
        s.toggle_palette();
        s.move_palette(2);
        assert_eq!(s.palette.active, Some(Command::ToggleSidebar));
        // The add-account form opening drops "Add account", the first row.
        s.form.open = true;
        let shown = s.palette.shown(&s);
        let row = s.palette.active_row(&shown);
        assert_eq!(row, 1);
        assert_eq!(shown[row].command, Command::ToggleSidebar);
    }

    #[test]
    fn refreshing_is_offered_on_the_list_with_its_shortcut() {
        let mut s = Mail::not_started();
        assert!(!available(&s).contains(&Command::Refresh));
        s.accounts.push(account("me@example.com"));
        assert!(available(&s).contains(&Command::Refresh));
        assert_eq!(Command::Refresh.hint(&s), "⇧⌘N");
        s.open_settings();
        assert!(!available(&s).contains(&Command::Refresh));
    }

    #[test]
    fn refreshing_a_search_of_every_inbox_syncs_the_inboxes() {
        let mut s = Mail::not_started();
        s.accounts.push(account("me@example.com"));
        s.showing = Showing::Mailbox {
            account: "me@example.com".into(),
            name: "Sent".into(),
        };
        let sent = [("me@example.com".to_owned(), "Sent".to_owned())];
        assert_eq!(s.refresh_targets(), sent);
        s.search.query = TextEdit::new("invoice");
        assert_eq!(s.refresh_targets(), sent);
        s.search.everywhere = true;
        assert_eq!(
            s.refresh_targets(),
            [("me@example.com".to_owned(), "INBOX".to_owned())]
        );
    }

    #[test]
    fn checking_for_updates_from_the_palette_shows_where_the_result_appears() {
        let mut s = Mail::not_started();
        s.settings_section = SettingsSection::Accounts;
        Command::CheckForUpdates.run(&mut s, &mut Tx::default());
        assert!(s.settings_open);
        assert_eq!(s.settings_section, SettingsSection::General);
    }

    fn key(text: &str) -> KeyEvent {
        KeyEvent {
            key: Key::Text(text.into()),
            mods: rune::Mods::default(),
        }
    }

    const ADA: &str = "ada@example.com";

    /// Ada's inbox listed, with three messages, and her Archive, Snoozed and Trash mailboxes.
    fn listing() -> Mail {
        use postden_core::account::{Account, Auth, Security, Server};
        use postden_core::message::{Envelope, Mailbox};
        let server = Server {
            host: "localhost".into(),
            port: 1,
            security: Security::None,
            username: ADA.into(),
        };
        let mut s = Mail::not_started();
        s.accounts.push(Account {
            id: ADA.into(),
            display_name: None,
            label: None,
            imap: server.clone(),
            smtp: server,
            auth: Auth::Password,
        });
        let mailbox = |name: &str, role| Mailbox {
            name: name.into(),
            delimiter: None,
            role,
        };
        s.mailboxes.insert(
            ADA.into(),
            vec![
                mailbox("INBOX", Role::Inbox),
                mailbox("Archive", Role::Archive),
                mailbox("Snoozed", Role::Snoozed),
                mailbox("Trash", Role::Trash),
            ],
        );
        s.showing = Showing::inbox(ADA);
        for uid in 1..=3 {
            s.messages.push(StoredMessage {
                account: ADA.into(),
                mailbox: "INBOX".into(),
                envelope: Envelope {
                    uid,
                    message_id: None,
                    from: postden_core::message::Address::default(),
                    to: Vec::new(),
                    subject: None,
                    date: None,
                    seen: false,
                    flagged: false,
                },
            });
        }
        s
    }

    /// [`listing`], each message with a Message-ID.
    fn with_ids() -> Mail {
        let mut s = listing();
        for m in &mut s.messages {
            m.envelope.message_id = Some(format!("{}@example.com", m.envelope.uid));
        }
        s
    }

    fn at(uid: u32) -> MessageKey {
        (ADA.into(), "INBOX".into(), uid)
    }

    #[test]
    fn keys_on_their_own_take_no_modifier() {
        let mut cmd = key("u");
        cmd.mods.cmd = true;
        let mut shifted = key("U");
        shifted.mods.shift = true;
        let mut hash = key("#");
        hash.mods.shift = true;
        assert!(Shortcut::Key('u').matches(&key("u"), false));
        // Caps Lock.
        assert!(Shortcut::Key('u').matches(&key("U"), false));
        assert!(!Shortcut::Key('u').matches(&cmd, false));
        assert!(!Shortcut::Key('u').matches(&shifted, false));
        assert!(Shortcut::Go('#').matches(&hash, true));
        let mut option_hash = key("#");
        option_hash.mods.alt = true;
        assert!(Shortcut::Go('#').matches(&option_hash, true));
        let mut option_u = key("u");
        option_u.mods.alt = true;
        assert!(!Shortcut::Key('u').matches(&option_u, false));
        assert!(!Shortcut::Go('i').matches(&key("i"), false));
        assert!(!Shortcut::Key('i').matches(&key("i"), true));
        assert_eq!(Shortcut::Go('i').label(), "G I");
    }

    #[test]
    fn j_and_k_move_the_cursor_and_stop_at_the_ends() {
        let mut s = listing();
        let mut tx = Tx::default();
        assert!(s.run_shortcut(&key("j"), &mut tx));
        assert_eq!(s.cursor, Some(at(1)));
        for _ in 0..3 {
            s.run_shortcut(&key("j"), &mut tx);
        }
        assert_eq!(s.cursor, Some(at(3)));
        s.run_shortcut(&key("k"), &mut tx);
        assert_eq!(s.cursor, Some(at(2)));
    }

    #[test]
    fn g_then_a_key_goes_there() {
        let mut s = listing();
        let mut tx = Tx::default();
        assert!(s.run_shortcut(&key("g"), &mut tx));
        assert!(s.run_shortcut(&key("e"), &mut tx));
        assert_eq!(
            s.showing,
            Showing::Mailbox {
                account: ADA.into(),
                name: "Archive".into()
            }
        );
        // A key after G that goes nowhere is taken, and doesn't act alone.
        s.run_shortcut(&key("g"), &mut tx);
        assert!(s.run_shortcut(&key("c"), &mut tx));
        assert!(!s.compose.open);
        assert!(s.run_shortcut(&key("c"), &mut tx));
        assert!(s.compose.open);
    }

    #[test]
    fn h_and_v_offer_choices_for_the_message_under_the_cursor() {
        let mut s = listing();
        let mut tx = Tx::default();
        assert!(!s.run_shortcut(&key("v"), &mut tx));
        s.cursor = Some(at(2));
        s.run_shortcut(&key("v"), &mut tx);
        assert_eq!(
            s.palette.mode,
            PaletteMode::Move((ADA.into(), "INBOX".into(), 2))
        );
        let titles: Vec<String> = s.palette.shown(&s).into_iter().map(|e| e.title).collect();
        assert_eq!(titles, ["Archive", "Snoozed", "Trash"]);
        s.close_palette();
        s.run_shortcut(&key("h"), &mut tx);
        assert!(matches!(s.palette.mode, PaletteMode::Snooze { .. }));
        assert_eq!(s.palette.shown(&s).len(), 3);
    }

    #[test]
    fn the_cursor_moves_on_only_from_a_message_that_left() {
        let mut s = listing();
        s.cursor = Some(at(2));
        // Without the services, the move fails: the message stays, and so does the cursor.
        s.move_message(ADA, "INBOX", 2, "Archive");
        assert_eq!(s.cursor, Some(at(2)));
        s.left_list(at(2));
        assert_eq!(s.cursor, Some(at(3)));
        // The server takes it, and the list no longer shows it.
        s.messages.retain(|m| m.envelope.uid != 2);
        s.left_list(at(3));
        assert_eq!(s.cursor, Some(at(1)));
        // Another message leaving leaves the cursor be.
        s.messages.retain(|m| m.envelope.uid != 3);
        s.left_list(at(2));
        assert_eq!(s.cursor, Some(at(1)));
    }

    #[test]
    fn keys_on_their_own_leave_overlays_alone() {
        let mut s = listing();
        let mut tx = Tx::default();
        // The update announcement over the list.
        s.updater.dialog_open = true;
        assert!(!s.run_shortcut(&key("j"), &mut tx));
        assert!(!s.run_shortcut(&key("c"), &mut tx));
        assert_eq!(s.cursor, None);
        // A menu takes keys on its own, but not ⌘ shortcuts.
        s.updater.dialog_open = false;
        s.actions_open = true;
        assert!(!s.run_shortcut(&key("j"), &mut tx));
        assert!(!s.run_shortcut(&key("g"), &mut tx));
        let mut new = KeyEvent {
            key: Key::Command('n'),
            mods: rune::Mods::default(),
        };
        new.mods.cmd = true;
        assert!(s.run_shortcut(&new, &mut tx));
        assert!(s.compose.open);
        // The menu closes, so it doesn't come back once the message is put away.
        assert!(!s.actions_open);
    }

    #[test]
    fn a_g_whose_next_key_went_elsewhere_is_forgotten() {
        let mut s = listing();
        let mut tx = Tx::default();
        s.run_shortcut(&key("g"), &mut tx);
        // Escape in a menu, handled by the menu.
        s.close_message_menu();
        assert!(s.run_shortcut(&key("c"), &mut tx));
        assert!(s.compose.open);
    }

    #[test]
    fn a_message_is_archived_deleted_or_marked_spam_only_where_it_can_go() {
        let mut s = listing();
        s.cursor = Some(at(1));
        let commands = available(&s);
        for command in [
            Command::Archive,
            Command::Trash,
            Command::ToggleStar,
            Command::Forward,
        ] {
            assert!(commands.contains(&command), "{command:?}");
        }
        assert!(
            !commands.contains(&Command::Spam),
            "Ada has no Junk mailbox"
        );
        s.mailboxes
            .get_mut(ADA)
            .unwrap()
            .push(postden_core::message::Mailbox {
                name: "Spam".into(),
                delimiter: None,
                role: Role::Junk,
            });
        assert!(available(&s).contains(&Command::Spam));
        // Already in the Archive, it isn't archived again.
        for m in &mut s.messages {
            m.mailbox = "Archive".into();
        }
        s.cursor = Some((ADA.into(), "Archive".into(), 1));
        let commands = available(&s);
        assert!(!commands.contains(&Command::Archive));
        assert!(commands.contains(&Command::Trash));
        assert!(!commands.contains(&Command::NotSpam), "it isn't spam");
    }

    #[test]
    fn spam_goes_back_to_the_inbox_with_the_keys_that_sent_it() {
        let mut s = listing();
        s.mailboxes
            .get_mut(ADA)
            .unwrap()
            .push(postden_core::message::Mailbox {
                name: "Spam".into(),
                delimiter: None,
                role: Role::Junk,
            });
        for m in &mut s.messages {
            m.mailbox = "Spam".into();
        }
        s.showing = Showing::Mailbox {
            account: ADA.into(),
            name: "Spam".into(),
        };
        s.cursor = Some((ADA.into(), "Spam".into(), 1));
        let commands = available(&s);
        assert!(commands.contains(&Command::NotSpam));
        assert!(!commands.contains(&Command::Spam), "it's spam already");
        assert_eq!(Command::NotSpam.title(&s), "Not spam");
        assert_eq!(Command::NotSpam.hint(&s), "⇧⌘J");
        let mut ev = key("!");
        ev.mods.shift = true;
        let pressed: Vec<_> = commands
            .iter()
            .filter(|c| c.shortcuts(&s).iter().any(|k| k.matches(&ev, false)))
            .collect();
        assert_eq!(pressed, [&Command::NotSpam]);
        // With no inbox to go back to, there's nowhere to take it.
        s.mailboxes
            .get_mut(ADA)
            .unwrap()
            .retain(|m| m.role != Role::Inbox);
        assert!(!available(&s).contains(&Command::NotSpam));
    }

    #[test]
    fn new_keys_run_their_commands() {
        let s = listing();
        let shortcut = |c: Command, k: &str, shift: bool| {
            let mut ev = key(k);
            ev.mods.shift = shift;
            c.shortcuts(&s).iter().any(|x| x.matches(&ev, false))
        };
        assert!(shortcut(Command::Archive, "e", false));
        assert!(shortcut(Command::Trash, "#", true));
        assert!(shortcut(Command::Spam, "!", true));
        assert!(shortcut(Command::ToggleStar, "s", false));
        assert!(shortcut(Command::Forward, "f", false));
        assert!(shortcut(Command::Undo, "z", false));
        assert!(!shortcut(Command::Undo, "Z", true), "⇧Z isn't Z");
        assert_eq!(Command::Spam.hint(&s), "⇧⌘J");
        assert_eq!(Command::ToggleStar.hint(&s), "⇧⌘L");
        assert_eq!(Command::Forward.hint(&s), "⇧⌘F");
    }

    #[test]
    fn a_move_is_undone_only_once_the_server_moved_it() {
        let mut s = with_ids();
        assert!(!available(&s).contains(&Command::Undo));
        s.undo_move(&at(2), "Archive", None);
        assert!(available(&s).contains(&Command::Undo));
        assert_eq!(Command::Undo.title(&s), "Undo move to Archive");
        // Z before the server answered keeps it, and says to wait.
        s.undo();
        assert!(matches!(s.undo, Some(Undo::Moved { done: false, .. })));
        assert!(
            s.notice
                .as_deref()
                .is_some_and(|n| n.contains("Still moving"))
        );
        // Another message's move answered leaves it be.
        assert!(!s.moved_for_undo(&at(3), true));
        assert!(s.moved_for_undo(&at(2), true));
        assert!(matches!(s.undo, Some(Undo::Moved { done: true, .. })));
        // A move refused leaves nothing to undo.
        s.undo_move(&at(1), "Trash", None);
        assert!(!s.moved_for_undo(&at(1), false));
        assert_eq!(s.undo, None);
    }

    #[test]
    fn a_move_without_a_message_id_cant_be_undone_nor_leave_an_older_undo() {
        let mut s = with_ids();
        s.undo_move(&at(2), "Archive", None);
        s.messages[0].envelope.message_id = None;
        s.undo_move(&at(1), "Archive", None);
        assert_eq!(
            s.undo, None,
            "Z mustn't undo something before the last thing done"
        );
        s.undo_move(&at(3), "Snoozed", Some(100));
        assert_eq!(Command::Undo.title(&s), "Undo snooze");
    }
}
