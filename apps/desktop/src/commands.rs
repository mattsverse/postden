//! Everything the app can be asked to do from the keyboard, in one place: the command palette
//! (⌘K) lists what [`available`] gives, and the window's key handler runs the command whose
//! shortcut was pressed. A command is offered, and its shortcut works, only where it applies.

use std::hash::{DefaultHasher, Hash, Hasher};

use postden_core::message::Role;
use rune::{Key, KeyEvent, TextEdit, Tx};
use rune_kit::shell;

use crate::state::{Mail, SettingsSection, Showing};

/// A key, or chord, that runs a command.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Shortcut {
    /// ⌘ (Ctrl elsewhere) and a letter, with ⇧ or without.
    Command {
        key: char,
        shift: bool,
    },
    Escape,
    /// ⇧⌘B, and ⌃⌘S on macOS: the shell's own.
    Sidebar,
}

impl Shortcut {
    const fn cmd(key: char) -> Self {
        Self::Command { key, shift: false }
    }

    const fn shift_cmd(key: char) -> Self {
        Self::Command { key, shift: true }
    }

    pub(crate) fn matches(self, ev: &KeyEvent) -> bool {
        match self {
            Self::Command { key, shift } => ev.is_command(key) && ev.mods.shift == shift,
            Self::Escape => ev.key == Key::Escape,
            Self::Sidebar => shell::is_sidebar_toggle(ev),
        }
    }

    /// How menus and the palette show it.
    pub(crate) fn label(self) -> String {
        match self {
            Self::Command { key, shift } => {
                let shift = if shift { "⇧" } else { "" };
                let key = if key == ',' {
                    ",".to_owned()
                } else {
                    key.to_uppercase().to_string()
                };
                format!("{shift}⌘{key}")
            }
            Self::Escape => "Esc".to_owned(),
            Self::Sidebar => "⇧⌘B".to_owned(),
        }
    }
}

/// One thing the app can do.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub(crate) enum Command {
    Compose,
    Search,
    Reply,
    ReplyAll,
    ToggleSeen,
    ExportMessage,
    CloseReader,
    Show(Showing),
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

impl Command {
    pub(crate) const fn shortcut(&self) -> Option<Shortcut> {
        Some(match self {
            Self::Compose => Shortcut::cmd('n'),
            Self::Search => Shortcut::cmd('f'),
            Self::Reply => Shortcut::cmd('r'),
            Self::ReplyAll => Shortcut::shift_cmd('r'),
            Self::ToggleSeen => Shortcut::shift_cmd('u'),
            Self::CloseReader => Shortcut::Escape,
            Self::Settings => Shortcut::cmd(','),
            Self::ToggleSidebar => Shortcut::Sidebar,
            Self::ExportMessage
            | Self::Show(_)
            | Self::MarkAllSeen { .. }
            | Self::AccountSettings(_)
            | Self::AddAccount
            | Self::CheckForUpdates => return None,
        })
    }

    /// Its row's key in the palette: the same for as long as it's offered, so a screen reader
    /// hears the highlight move to another command whatever row that is. Hashed, as a mailbox's
    /// name can hold any character.
    pub(crate) fn key(&self) -> String {
        let mut hasher = DefaultHasher::new();
        self.hash(&mut hasher);
        format!("palette-{:016x}", hasher.finish())
    }

    /// Its shortcut as menus show it, or nothing.
    pub(crate) fn hint(&self) -> String {
        self.shortcut().map(Shortcut::label).unwrap_or_default()
    }

    /// What the palette calls it.
    pub(crate) fn title(&self, s: &Mail) -> String {
        match self {
            Self::Compose => "New message".into(),
            Self::Search => "Search mail".into(),
            Self::Reply => "Reply".into(),
            Self::ReplyAll => "Reply all".into(),
            Self::ToggleSeen => match &s.reader {
                Some(r) if r.message.envelope.seen => "Mark as unread".into(),
                _ => "Mark as read".into(),
            },
            Self::ExportMessage => "Export as .eml…".into(),
            Self::CloseReader => "Back to the list".into(),
            Self::Show(Showing::All(Role::Sent)) => "Go to Sent".into(),
            Self::Show(Showing::All(_)) => "Go to All inboxes".into(),
            Self::Show(Showing::Mailbox { account, name }) => {
                format!("Go to {}", s.mailbox_label(account, name))
            }
            Self::MarkAllSeen { .. } => "Mark all as read".into(),
            Self::AccountSettings(_) => "Account settings".into(),
            Self::AddAccount => "Add account".into(),
            Self::Settings => "Settings".into(),
            Self::ToggleSidebar => "Show or hide the sidebar".into(),
            Self::CheckForUpdates => "Check for updates".into(),
        }
    }

    /// What it applies to, where that isn't plain from the title: an account or a mailbox.
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
            _ => None,
        }
    }

    pub(crate) fn run(self, s: &mut Mail, tx: &mut Tx) {
        match self {
            Self::Compose => s.compose(),
            Self::Search => s.focus_search(),
            Self::Reply => s.reply(false),
            Self::ReplyAll => s.reply(true),
            Self::ToggleSeen => s.toggle_seen(),
            Self::ExportMessage => {
                if let Some(reader) = &s.reader {
                    Mail::export_message(tx, &reader.message);
                }
            }
            Self::CloseReader => s.close_reader(),
            Self::Show(showing) => s.show(showing),
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

/// The commands that apply to what the window shows, in the palette's order.
pub(crate) fn available(s: &Mail) -> Vec<Command> {
    // The list, or a message from it, is shown: not a form, a message being written or the
    // settings.
    let browsing = !s.form.open && !s.compose.open && !s.settings_open;
    let mut commands = Vec::new();
    let mut add = |on: bool, command: Command| {
        if on {
            commands.push(command);
        }
    };
    let has_accounts = !s.accounts.is_empty();
    add(browsing && has_accounts, Command::Compose);
    if let Some(reader) = s.reader.as_ref().filter(|_| browsing) {
        let downloaded = matches!(reader.body, Some(Ok(_)));
        add(downloaded, Command::Reply);
        add(downloaded, Command::ReplyAll);
        add(true, Command::ToggleSeen);
        add(true, Command::ExportMessage);
        add(true, Command::CloseReader);
    }
    add(browsing && has_accounts, Command::Search);
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

/// The command palette: a search over [`available`] commands.
#[derive(Default)]
pub(crate) struct Palette {
    pub(crate) open: bool,
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
}

impl Palette {
    /// The commands that match what's typed, in [`available`]'s order.
    pub(crate) fn shown(&self, s: &Mail) -> Vec<Entry> {
        available(s)
            .into_iter()
            .map(|command| Entry {
                title: command.title(s),
                detail: command.detail(s),
                command,
            })
            .filter(|e| matches(&self.query.text, &e.title, e.detail.as_deref()))
            .collect()
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
    fn checking_for_updates_from_the_palette_shows_where_the_result_appears() {
        let mut s = Mail::not_started();
        s.settings_section = SettingsSection::Accounts;
        Command::CheckForUpdates.run(&mut s, &mut Tx::default());
        assert!(s.settings_open);
        assert_eq!(s.settings_section, SettingsSection::General);
    }
}
