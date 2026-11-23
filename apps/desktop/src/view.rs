//! What the window shows: rune-kit's app shell, with accounts in the sidebar and mail, a message
//! being written, the add-account form or the settings in the main pane.

use jiff::ZonedDateTime;
use postden_app::{POLL_INTERVALS, PREFETCH_COUNTS, Theme};
use postden_core::account::{Account, Security};
use postden_core::message::{Address, Mailbox, Role};
use postden_html::compat::{self, Client, Provider};
use postden_mail::imap::Status;
use postden_store::StoredMessage;
use postden_update::Track;
use rune::{
    Align, Block, Color, Cx, El, GrowthSpec, Icon, Mark, ScrollSpec, Side, TextEdit, TextStyle,
    Transition, Tx, col, el, icon, row, text,
};
use rune_kit::shell::{RowStatus, Sidebar, SidebarRow, SplitView, Toolbar};
use rune_kit::{
    Badge, BadgeVariant, Button, ButtonVariant, Checkbox, Combobox, ComboboxOption, Dialog,
    Divider, IconButton, IconButtonVariant, Keybind, Kit, Markdown, MarkdownStyle, Menu, MenuEntry,
    MenuItem, Placement, PopupItem, PopupList, SegmentedControl, Select, Spinner, State, Switch,
    TextArea, TextInput, context_menu, glyphs, root,
};

use crate::account_settings::{AccountSettings, IMAGE_EXTENSIONS, Servers};
use crate::commands::{Command, Entry, Shortcut};
use crate::compose::{ComposeForm, Mode, Preview, Recipients};
use crate::form::{AccountForm, SECURITY, Step, Unreadable, is_set_up, security_index};
use crate::rich::RichEditor;
use crate::search::{SERVER_PAGE, ServerSearch};
use crate::state::{
    AccountMenu, Answer, Asking, INBOX as INBOX_NAME, ListRow, Mail, MailboxMenu, MessageMenu,
    NewMailbox, NoticeAction, Reader, SettingsSection, Showing,
};
use crate::transfer::{self as moving, Transfer};
use crate::update::{self, Phase};
use postden_app::import::{Found, Setup};

static INBOX: Icon = Icon {
    name: "postden-inbox",
    view: 16.0,
    body: r#"<path d="M2.75 9.25L4.5 3.75H11.5L13.25 9.25V12.25H2.75Z" stroke="currentColor" stroke-width="1.2" stroke-linejoin="round" fill="none"/><path d="M2.75 9.25H5.75L6.5 10.5H9.5L10.25 9.25H13.25" stroke="currentColor" stroke-width="1.2" stroke-linejoin="round" fill="none"/>"#,
};
/// The open message's subject.
const SUBJECT: TextStyle = TextStyle::new(20.0, 600.0).lh(26.0);

static BACK: Icon = Icon {
    name: "postden-back",
    view: 16.0,
    body: r#"<path d="M10 3.5L5.5 8L10 12.5" stroke="currentColor" stroke-width="1.3" stroke-linecap="round" stroke-linejoin="round" fill="none"/>"#,
};
static SENT: Icon = Icon {
    name: "postden-sent",
    view: 16.0,
    body: r#"<path d="M2.5 7.75L13.5 2.75L9.75 13.25L7.5 8.5Z" stroke="currentColor" stroke-width="1.2" stroke-linejoin="round" fill="none"/><path d="M7.5 8.5L13.5 2.75" stroke="currentColor" stroke-width="1.2" stroke-linecap="round"/>"#,
};
static MORE: Icon = Icon {
    name: "postden-more",
    view: 16.0,
    body: r#"<circle cx="3.5" cy="8" r="1.1" fill="currentColor"/><circle cx="8" cy="8" r="1.1" fill="currentColor"/><circle cx="12.5" cy="8" r="1.1" fill="currentColor"/>"#,
};
/// An envelope with an unread dot: the list shows only unread mail.
static UNREAD: Icon = Icon {
    name: "postden-unread",
    view: 16.0,
    body: r#"<path d="M11.5 12.5H3A1 1 0 0 1 2 11.5V5.25A1 1 0 0 1 3 4.25H8.5" stroke="currentColor" stroke-width="1.2" stroke-linecap="round" fill="none"/><path d="M13.25 8.75V11.5A1 1 0 0 1 12.25 12.5" stroke="currentColor" stroke-width="1.2" stroke-linecap="round" fill="none"/><path d="M2.5 5L7.25 8.5L9.5 6.85" stroke="currentColor" stroke-width="1.2" stroke-linecap="round" stroke-linejoin="round" fill="none"/><circle cx="12.5" cy="4.25" r="2" fill="currentColor"/>"#,
};
static FOLDER: Icon = Icon {
    name: "postden-folder",
    view: 16.0,
    body: r#"<path d="M2.5 4.25C2.5 3.84 2.84 3.5 3.25 3.5H6.25L7.5 5H12.75C13.16 5 13.5 5.34 13.5 5.75V11.75C13.5 12.16 13.16 12.5 12.75 12.5H3.25C2.84 12.5 2.5 12.16 2.5 11.75Z" stroke="currentColor" stroke-width="1.2" stroke-linejoin="round" fill="none"/>"#,
};
/// The Junk mailbox's: a stop sign.
static JUNK: Icon = Icon {
    name: "postden-junk",
    view: 16.0,
    body: r#"<path d="M5.6 2.25H10.4L13.75 5.6V10.4L10.4 13.75H5.6L2.25 10.4V5.6Z" stroke="currentColor" stroke-width="1.2" stroke-linejoin="round" fill="none"/><path d="M8 5V8.5" stroke="currentColor" stroke-width="1.3" stroke-linecap="round"/><circle cx="8" cy="10.75" r="0.8" fill="currentColor"/>"#,
};
static GEAR: Icon = Icon {
    name: "postden-gear",
    view: 16.0,
    body: r#"<circle cx="8" cy="8" r="2" stroke="currentColor" stroke-width="1.2" fill="none"/><path d="M8 1.75V3.5M8 12.5V14.25M1.75 8H3.5M12.5 8H14.25M3.58 3.58L4.82 4.82M11.18 11.18L12.42 12.42M3.58 12.42L4.82 11.18M11.18 4.82L12.42 3.58" stroke="currentColor" stroke-width="1.2" stroke-linecap="round"/><circle cx="8" cy="8" r="4.5" stroke="currentColor" stroke-width="1.2" fill="none"/>"#,
};
/// Stands for an image in a signature's text.
static PICTURE: Icon = Icon {
    name: "postden-picture",
    view: 16.0,
    body: r#"<rect x="2.5" y="3" width="11" height="10" rx="1.5" stroke="currentColor" stroke-width="1.2" fill="none"/><circle cx="6" cy="6.5" r="1.1" fill="currentColor"/><path d="M3 11.5L6.5 8.5L9 10.5L10.75 9L13 11" stroke="currentColor" stroke-width="1.2" stroke-linejoin="round" fill="none"/>"#,
};
/// A starred message.
static STAR: Icon = Icon {
    name: "postden-star",
    view: 16.0,
    body: r#"<path d="M8 2.25L9.69 5.83L13.5 6.31L10.7 8.97L11.4 12.75L8 10.9L4.6 12.75L5.3 8.97L2.5 6.31L6.31 5.83Z" fill="currentColor" stroke="currentColor" stroke-width="1" stroke-linejoin="round"/>"#,
};
/// A message that isn't starred: the star's outline.
static STAR_OUTLINE: Icon = Icon {
    name: "postden-star-outline",
    view: 16.0,
    body: r#"<path d="M8 2.25L9.69 5.83L13.5 6.31L10.7 8.97L11.4 12.75L8 10.9L4.6 12.75L5.3 8.97L2.5 6.31L6.31 5.83Z" fill="none" stroke="currentColor" stroke-width="1.2" stroke-linejoin="round"/>"#,
};
/// Archiving: a box with its lid.
static ARCHIVE: Icon = Icon {
    name: "postden-archive",
    view: 16.0,
    body: r#"<rect x="2.25" y="3" width="11.5" height="3" rx="0.75" stroke="currentColor" stroke-width="1.2" fill="none"/><path d="M3.25 6V12.25C3.25 12.66 3.59 13 4 13H12C12.41 13 12.75 12.66 12.75 12.25V6" stroke="currentColor" stroke-width="1.2" fill="none"/><path d="M6.5 8.75H9.5" stroke="currentColor" stroke-width="1.2" stroke-linecap="round"/>"#,
};
/// Deleting: a bin.
static TRASH: Icon = Icon {
    name: "postden-trash",
    view: 16.0,
    body: r#"<path d="M2.75 4.25H13.25" stroke="currentColor" stroke-width="1.2" stroke-linecap="round"/><path d="M6.25 4.25V2.75H9.75V4.25" stroke="currentColor" stroke-width="1.2" stroke-linejoin="round" fill="none"/><path d="M4 4.25L4.6 12.6C4.63 12.95 4.92 13.25 5.28 13.25H10.72C11.08 13.25 11.37 12.95 11.4 12.6L12 4.25" stroke="currentColor" stroke-width="1.2" stroke-linejoin="round" fill="none"/>"#,
};
static PLUS: Icon = Icon {
    name: "postden-plus",
    view: 16.0,
    body: r#"<path d="M8 3.5V12.5M3.5 8H12.5" stroke="currentColor" stroke-width="1.2" stroke-linecap="round"/>"#,
};

pub(crate) fn view(s: &Mail, cx: &mut Cx<Mail>) -> El {
    let k = Kit::neutral(s.is_dark(cx.system_dark()));
    if let Err(error) = &s.services {
        return unavailable(error, &k);
    }
    let mut layer = Vec::new();
    let main = if s.settings_open && s.account_settings.open {
        account_page(s, &s.account_settings, &k, &mut layer)
    } else if s.settings_open {
        settings(s, &k, &mut layer)
    } else if s.form.open {
        form(&s.form, &s.accounts, &k)
    } else if s.compose.open {
        compose(s, &s.compose, &k, &mut layer)
    } else if let Some(reader) = &s.reader {
        message(reader, &k)
    } else {
        inbox(s, &k, &mut layer)
    };
    let mut toolbar = Toolbar::new().title(title(s));
    let mut trailing = row().gap(12.0).center_y();
    // The notice takes a bar of its own under what's shown, rather than covering it.
    let main = with_notice(s, &k, main);
    let browsing = !s.form.open && !s.compose.open && !s.settings_open;
    if s.settings_open && s.account_settings.open {
        toolbar = toolbar.leading(
            Button::new("Settings")
                .icon(&BACK)
                .variant(ButtonVariant::Ghost)
                .help("Back to the settings")
                .on_press(Mail::close_account_settings)
                .build(&k)
                .key("back-to-settings"),
        );
    }
    if let (Some(reader), true) = (&s.reader, browsing) {
        // The list it goes back to, by name: the title gives the reader's buttons its room.
        toolbar = toolbar.leading(
            Button::new(list_title(s))
                .icon(&BACK)
                .variant(ButtonVariant::Ghost)
                .help("Back to the list (Esc)")
                .on_press_tx(|s: &mut Mail, tx| {
                    s.close_reader();
                    // With the list as it comes back, not a frame after.
                    s.reveal_cursor(tx);
                    tx.instant();
                })
                .build(&k)
                .key("back")
                .transition(Transition::none()),
        );
        trailing = trailing.child(reader_actions(s, reader, &k));
        // In its place from the start, waiting for the message to download, so nothing beside
        // it moves as it does.
        trailing = trailing.child(
            Button::new("Reply")
                .variant(ButtonVariant::Ghost)
                .shortcut(Command::Reply.hint(s))
                .disabled(!matches!(reader.body, Some(Ok(_))))
                .on_press(|s: &mut Mail| s.reply(false))
                .build(&k)
                .key("reply")
                .transition(Transition::none()),
        );
    }
    // A message just arrived is told of here, always, to open at once.
    if browsing {
        trailing = trailing.children(toolbar_status(s, &k));
    }
    if browsing && !s.accounts.is_empty() {
        trailing = trailing.child(actions(s, &k, &mut layer));
    }
    let toolbar = toolbar.trailing(trailing);
    update_dialog(s, &k, &mut layer);
    new_mailbox_dialog(s, &k, &mut layer);
    delete_mailbox_dialog(s, &k, &mut layer);
    autosort_dialog(s, &k, &mut layer);
    palette(s, &k, &mut layer);
    let window = SplitView::new(main)
        .sidebar(
            &s.sidebar,
            |s: &mut Mail| &mut s.sidebar,
            sidebar(s, &k, &mut layer),
        )
        .toolbar(toolbar)
        .titlebar_inset(cx.titlebar_inset())
        .frame_inset(8.0)
        .toolbar_in_panel()
        .sidebar_padding(18.0, 10.0)
        .reduced_motion(cx.is_reduced_motion())
        .build(&k);
    root(window, layer).on_window_focus(|s: &mut Mail, focused, _| {
        if focused {
            s.window_focused();
        }
    })
}

/// What's shown, with the notice's bar under it while there is one: always the same frame, so
/// the bar coming and going never rebuilds (and fades) what's above it.
fn with_notice(s: &Mail, k: &Kit, main: El) -> El {
    col()
        .key("main-with-notice")
        .transition(Transition::none())
        .w_full()
        .grow(1.0)
        .min_h(0.0)
        .child(
            col()
                .key("main-content")
                .transition(Transition::none())
                .w_full()
                .grow(1.0)
                .min_h(0.0)
                .child(main),
        )
        .children(notice(s, k))
}

/// The notice showing, if any, as a bar along the bottom of the main pane, clear of the rows
/// above it: what happened, a second line, its buttons, and a close button.
fn notice(s: &Mail, k: &Kit) -> Option<El> {
    let notice = s.notice.as_ref()?;
    let mut words = row().grow(1.0).min_w(0.0).gap(8.0).center_y().child(
        text(notice.clone())
            .font(k.ty.ui_medium)
            .color(k.c.text_base)
            .key("notice-title"),
    );
    if let Some(detail) = &s.notice_detail {
        words = words.child(
            text(detail.clone())
                .font(k.ty.ui)
                .color(k.c.text_muted)
                .truncate()
                .shrink()
                .min_w(0.0)
                .key("notice-detail"),
        );
    }
    let mut bar = row()
        .key(format!("notice-{}", s.notice_id()))
        .role("status")
        .label(notice.clone())
        .description(s.notice_detail.clone().unwrap_or_default())
        .w_full()
        .h(44.0)
        .px(20.0)
        .gap(10.0)
        .center_y()
        .fill(k.c.layer_01)
        .border_sides([1.0, 0.0, 0.0, 0.0], k.c.border_weak)
        .transition(Transition::none())
        .child(words);
    for (i, action) in s.notice_actions.iter().enumerate() {
        let label = match action {
            // Z types into the search field rather than undoing.
            NoticeAction::Undo if s.search.query.focused => "Undo".to_owned(),
            NoticeAction::Undo => "Undo (Z)".to_owned(),
            NoticeAction::MoveBack(moved) if moved.len() > 1 => "Move them back".to_owned(),
            NoticeAction::MoveBack(_) => "Move it back".to_owned(),
            // Beside moving them back, opening where they went is to review them.
            NoticeAction::Show(Showing::Mailbox { account, name }) => {
                match s.notice_actions.iter().find_map(|a| match a {
                    NoticeAction::MoveBack(moved) => Some(moved.len()),
                    _ => None,
                }) {
                    Some(1) => "Review the moved message".to_owned(),
                    Some(n) => format!("Review {n} moved messages"),
                    None => format!("Open {}", s.mailbox_label(account, name)),
                }
            }
            NoticeAction::Show(_) => "Show".to_owned(),
        };
        bar = bar.child(
            Button::new(label)
                .variant(if i == 0 {
                    ButtonVariant::Neutral
                } else {
                    ButtonVariant::Ghost
                })
                .on_press_tx(move |s: &mut Mail, tx| s.notice_pressed(i, tx))
                .build(k)
                .key(format!("notice-action-{i}"))
                .transition(Transition::none()),
        );
    }
    Some(
        bar.child(
            IconButton::new(k.glyph.close)
                .variant(IconButtonVariant::GhostMuted)
                .label("Close")
                .help("Close")
                .on_press(Mail::dismiss_notice)
                .build(k)
                .key("notice-close"),
        ),
    )
}

/// The open message's everyday actions, each a button with its shortcut in its tooltip: where
/// sorting says it goes, its star, Archive and Delete.
fn reader_actions(s: &Mail, reader: &Reader, k: &Kit) -> El {
    let commands = crate::commands::available(s);
    let mut group = row()
        .gap(2.0)
        .center_y()
        .key("reader-actions")
        .transition(Transition::none());
    let named = |command: &Command| {
        let keys = command.shortcuts(s);
        keys.first().map_or_else(
            || command.title(s),
            |key| format!("{} ({})", command.title(s), key.label()),
        )
    };
    if let Some(suggested @ Command::MoveAsSuggested(..)) = commands
        .iter()
        .find(|c| matches!(c, Command::MoveAsSuggested(..)))
    {
        let run = suggested.clone();
        let title = suggested.title(s);
        // As the list shows it: a suggestion, in its accent chip, with M; said to be one.
        group = group
            .child(text("Suggested").font(k.ty.small).color(k.c.text_muted))
            .child(
                row()
                    .key("reader-move-suggested")
                    .role("button")
                    .label(format!("{title} (M)"))
                    .help(format!(
                        "Sorting on this computer suggests this: press it, or M, to {}",
                        title.to_lowercase()
                    ))
                    .radius(3.0)
                    .hover(k.c.hover)
                    .min_h(30.0)
                    .center_y()
                    .on_press_tx(move |s: &mut Mail, tx| run.clone().run(s, tx))
                    .child(suggestion_badge(title, true, true, k).a11y_hidden()),
            );
        group = group.child(el().w(8.0));
    }
    let starred = reader.message.envelope.flagged;
    let mut buttons = vec![(
        Command::ToggleStar,
        if starred { &STAR } else { &STAR_OUTLINE },
        "reader-star",
    )];
    if commands.contains(&Command::Archive) {
        buttons.push((Command::Archive, &ARCHIVE, "reader-archive"));
    }
    if commands.contains(&Command::Trash) {
        buttons.push((Command::Trash, &TRASH, "reader-trash"));
    }
    for (command, icon, key) in buttons {
        let label = named(&command);
        let accent = command == Command::ToggleStar && starred;
        group = group.child(
            IconButton::new(icon)
                .variant(if accent {
                    IconButtonVariant::Neutral
                } else {
                    IconButtonVariant::Ghost
                })
                .label(label.clone())
                .help(label)
                .on_press_tx(move |s: &mut Mail, tx| {
                    command.clone().run(s, tx);
                    // Back to the list at once, as Escape does.
                    tx.instant();
                })
                .build(k)
                .key(key),
        );
    }
    group
}

/// Has what was added to `layer` since `before` (a dialog, a menu) come and go in one frame,
/// never fading its text through what's under it.
fn instant_since(layer: &mut Vec<El>, before: usize) {
    let added = layer.split_off(before);
    layer.extend(added.into_iter().map(|e| e.transition(Transition::none())));
}

/// The ⋯ menu's actions on the message under the list's cursor, with their keys; its Undo, named
/// for what Z undoes while there's something; and the palette with every command, so none
/// depends on knowing a shortcut.
fn undo_and_commands(s: &Mail, mut menu: Menu) -> Menu {
    // The mailbox shown can be autosorted to from here too, as from its menu in the sidebar.
    if let (Showing::Mailbox { account, name }, None) = (&s.showing, &s.reader) {
        let on = s.autosorts_to(account, name);
        let offered = on
            || s.autosort_candidates()
                .iter()
                .any(|(a, n, _)| a == account && n == name);
        if offered {
            let (account, name) = (account.clone(), name.clone());
            menu = menu.item(
                MenuItem::new(if on {
                    "Turn off autosort here"
                } else {
                    "Autosort mail here…"
                })
                .on_select_tx(move |s: &mut Mail, tx| {
                    s.actions_open = false;
                    s.toggle_autosort(&account, &name);
                    tx.instant();
                }),
            );
        }
    }
    // From the inbox too, where its suggestions show: which mailbox, picked in the settings.
    if !matches!(s.showing, Showing::Mailbox { .. })
        && s.reader.is_none()
        && s.settings.sort_mail
        && !s.autosort_candidates().is_empty()
    {
        menu = menu.item(
            MenuItem::new("Set up autosort…").on_select_tx(|s: &mut Mail, tx| {
                s.actions_open = false;
                s.open_settings();
                // Where the picker is, unless moving to another computer keeps the page put, or
                // an account's changes, not yet saved, show instead.
                if !s.account_settings.open {
                    s.show_settings_section(SettingsSection::General);
                }
                if s.settings_section == SettingsSection::General && !s.account_settings.open {
                    s.toggle_autosort_pick();
                }
                tx.instant();
            }),
        );
    }
    if s.reader.is_none() && s.focused_message().is_some() {
        let commands = crate::commands::available(s);
        for (command, label, key) in [
            (Command::Archive, "Archive", "E"),
            (Command::Trash, "Move to Trash", "#"),
        ] {
            if commands.contains(&command) {
                let target = s
                    .focused_message()
                    .map(|m| {
                        let subject = m.envelope.subject.clone().unwrap_or_default();
                        format!("{} · {subject}", m.envelope.from.label())
                    })
                    .unwrap_or_default();
                menu = menu.item(
                    MenuItem::new(label)
                        .description(target)
                        .shortcut(key)
                        .on_select_tx(move |s: &mut Mail, tx| {
                            s.actions_open = false;
                            command.clone().run(s, tx);
                        }),
                );
            }
        }
    }
    // What Z undoes stays at hand once its notice is gone, set apart from what acts on the
    // selected message, which it may not be.
    if s.undo.is_some() {
        menu = menu.separator().item(
            MenuItem::new(s.undo_title())
                .description(s.undo_subject.clone().unwrap_or_default())
                .shortcut("Z")
                .on_select(|s: &mut Mail| {
                    s.actions_open = false;
                    s.undo();
                }),
        );
    }
    menu = menu.item(
        MenuItem::new("All commands…")
            .shortcut("⌘K")
            .on_select(|s: &mut Mail| {
                s.actions_open = false;
                s.toggle_palette();
            }),
    );
    menu
}

/// The ⋯ menu of everything but Reply, which keeps a button of its own.
fn actions(s: &Mail, k: &Kit, layer: &mut Vec<El>) -> El {
    let close = |s: &mut Mail| {
        s.actions_open = false;
        s.forget_go();
    };
    let mut menu = Menu::new("actions")
        .trigger(
            IconButton::new(&MORE)
                .variant(IconButtonVariant::Ghost)
                .label("More actions")
                .expanded(s.actions_open)
                .on_press(|s: &mut Mail| {
                    s.actions_open = !s.actions_open;
                    // Its items close it without `on_close`: a G pressed before goes nowhere.
                    s.forget_go();
                })
                .build(k)
                .key("more-actions"),
        )
        .placement(Placement::BottomEnd)
        // Wide enough to name what its actions act on.
        .min_width(300.0)
        .open(s.actions_open)
        .on_close(close)
        .item(
            MenuItem::new("New message")
                .shortcut(Command::Compose.hint(s))
                .on_select(|s: &mut Mail| {
                    s.actions_open = false;
                    s.compose();
                }),
        );
    menu = undo_and_commands(s, menu);
    if let Some(account) = s.showing.account() {
        let id = account.to_owned();
        menu = menu.item(
            MenuItem::new("Account settings").on_select(move |s: &mut Mail| {
                s.actions_open = false;
                s.open_account_settings(&id);
            }),
        );
    }
    if let Some(reader) = &s.reader {
        let downloaded = matches!(reader.body, Some(Ok(_)));
        let seen = if reader.message.envelope.seen {
            "Mark as unread"
        } else {
            "Mark as read"
        };
        menu = menu
            .separator()
            .item(
                MenuItem::new("Reply all")
                    .shortcut(Command::ReplyAll.hint(s))
                    .disabled(!downloaded)
                    .on_select(|s: &mut Mail| {
                        s.actions_open = false;
                        s.reply(true);
                    }),
            )
            .item(
                MenuItem::new("Forward")
                    .shortcut(Command::Forward.hint(s))
                    .disabled(!downloaded)
                    .on_select(|s: &mut Mail| {
                        s.actions_open = false;
                        s.answer(Answer::Forward);
                    }),
            )
            .item(
                MenuItem::new(seen)
                    .shortcut(Command::ToggleSeen.hint(s))
                    .on_select(|s: &mut Mail| {
                        s.actions_open = false;
                        s.toggle_seen();
                    }),
            )
            .item(
                MenuItem::new(if reader.message.envelope.flagged {
                    "Remove star"
                } else {
                    "Star"
                })
                .shortcut(Command::ToggleStar.hint(s))
                .on_select(|s: &mut Mail| {
                    s.actions_open = false;
                    s.toggle_star();
                }),
            )
            .item(
                MenuItem::new("Export as .eml…").on_select_tx(|s: &mut Mail, tx| {
                    s.actions_open = false;
                    if let Some(reader) = &s.reader {
                        Mail::export_message(tx, &reader.message);
                    }
                }),
            );
    }
    let before = layer.len();
    let built = menu.build(k, layer);
    instant_since(layer, before);
    built
}

fn title(s: &Mail) -> String {
    if s.settings_open && s.account_settings.open {
        return format!("{} · Settings", s.label(&s.account_settings.account));
    }
    if s.settings_open {
        return "Settings".into();
    }
    if s.form.open {
        return match s.form.step {
            Step::Choose => "Add an account",
            Step::Google => "Sign in with Google",
            Step::Servers => "Other mail (IMAP and SMTP)",
            Step::Import => s.form.imports.app.unwrap_or("From another mail app"),
        }
        .into();
    }
    if s.compose.open {
        return if s.compose.is_reply() {
            "Reply"
        } else if s.compose.is_forward() {
            "Forward"
        } else {
            "New message"
        }
        .into();
    }
    // A message open, its Back button names the list instead.
    if s.reader.is_some() {
        return String::new();
    }
    list_title(s)
}

/// What the list is, by name, searching or not: where a search looks, and whether the server's
/// in it.
fn list_title(s: &Mail) -> String {
    match s.listing() {
        // As soon as the server's results start coming, the title counts them in.
        Showing::Everything
            if matches!(
                s.search.server(),
                ServerSearch::Done | ServerSearch::Running
            ) =>
        {
            "All mail, here and on your server".into()
        }
        Showing::Everything => "All mail on this computer".into(),
        Showing::All(Role::Inbox) if s.search.everywhere => "All inboxes".into(),
        _ => showing_title(s),
    }
}

/// What the toolbar says of what's going on, while the list or a message shows: that autosort
/// files mail to the mailbox shown, and a message just arrived, to open.
fn toolbar_status(s: &Mail, k: &Kit) -> Vec<El> {
    let mut status = Vec::new();
    // A mailbox mail can be autosorted to says whether it is where it's looked at, and turns it
    // on or off from there.
    if let (Showing::Mailbox { account, name }, None, true) =
        (&s.showing, &s.reader, s.settings.sort_mail)
    {
        let on = s.autosorts_to(account, name);
        let offered = on
            || s.autosort_candidates()
                .iter()
                .any(|(a, n, _)| a == account && n == name);
        if offered {
            let (account, name) = (account.clone(), name.clone());
            status.push(
                Button::new(if on {
                    "Autosort on · Turn off"
                } else {
                    "Set up autosort…"
                })
                .variant(if on {
                    ButtonVariant::Outline
                } else {
                    ButtonVariant::Ghost
                })
                .on_press_tx(move |s: &mut Mail, tx| {
                    s.toggle_autosort(&account, &name);
                    tx.instant();
                })
                .build(k)
                .key("autosort-header")
                .transition(Transition::none()),
            );
        }
    }
    if let Some((title, _)) = s.arrival() {
        status.push(
            Button::new(title.to_owned())
                .icon(&INBOX)
                .variant(ButtonVariant::Neutral)
                .on_press(Mail::open_arrival)
                .build(k)
                .key("arrival")
                .transition(Transition::none()),
        );
    }
    status
}

/// What the list shows, by name: "All inboxes", an account's name, or a mailbox and its account's.
fn showing_title(s: &Mail) -> String {
    match &s.showing {
        Showing::All(Role::Sent) => "Sent".into(),
        Showing::All(_) => "All inboxes".into(),
        Showing::Everything => "All mail".into(),
        Showing::Mailbox { account, name } if name == INBOX_NAME => s.label(account).to_owned(),
        Showing::Mailbox { account, name } => {
            let label = s
                .mailboxes
                .get(account)
                .and_then(|list| list.iter().find(|m| m.name == *name))
                .map_or_else(|| name.clone(), Mailbox::label);
            format!("{label} · {}", s.label(account))
        }
    }
}

fn unavailable(error: &str, k: &Kit) -> El {
    col()
        .key("unavailable")
        .w_full()
        .h_full()
        .center()
        .gap(8.0)
        .fill(k.c.bg_base)
        .window_drag()
        .child(
            text("Postden couldn't start")
                .font(k.ty.body_medium)
                .color(k.c.text_base),
        )
        .child(
            text(error.to_owned())
                .font(k.ty.small)
                .color(k.c.text_muted)
                .wrap()
                .max_w(520.0),
        )
}

fn sidebar(s: &Mail, k: &Kit, layer: &mut Vec<El>) -> El {
    let badge = |n: usize| (n > 0).then(|| n.to_string());
    let showing = |role| !s.form.open && !s.settings_open && s.showing == Showing::All(role);
    let mut all = SidebarRow::new("All inboxes")
        .icon(&INBOX)
        .height(32.0)
        .selected(showing(Role::Inbox))
        .on_select(|s: &mut Mail, tx| show_from_sidebar(s, tx, Showing::All(Role::Inbox)));
    if let Some(n) = badge(s.unread(None)) {
        all = all.badge(n);
    }
    let sent = SidebarRow::new("Sent")
        .icon(&SENT)
        .height(32.0)
        .selected(showing(Role::Sent))
        .on_select(|s: &mut Mail, tx| show_from_sidebar(s, tx, Showing::All(Role::Sent)));
    let folders = col()
        .gap(4.0)
        .child(all.build(k).key("all"))
        .child(sent.build(k).key("sent"))
        .child(col().pt(8.0).child(Divider::horizontal().build(k)));
    let mut sidebar = Sidebar::new()
        .label("Accounts")
        .inset(0.0)
        .header_gap(4.0)
        .header_padding(0.0, 12.0)
        .row_gap(4.0)
        .header(folders)
        .footer(
            col()
                .gap(4.0)
                .child(
                    SidebarRow::new("Add account")
                        .icon(&PLUS)
                        .height(32.0)
                        .selected(s.form.open && !s.settings_open)
                        .on_select(|s: &mut Mail, _| s.open_form())
                        .build(k)
                        .key("add-account"),
                )
                .child(
                    SidebarRow::new("Settings")
                        .icon(&GEAR)
                        .height(32.0)
                        .selected(s.settings_open)
                        .on_select(|s: &mut Mail, _| s.open_settings())
                        .build(k)
                        .key("settings"),
                ),
        );
    for account in &s.accounts {
        sidebar = sidebar.child(account_row(s, account, k, layer));
        if !s.collapsed.contains(&account.id) {
            let (listed, read) = s.sidebar_mailboxes(&account.id);
            for mailbox in listed {
                sidebar = sidebar.child(mailbox_row(s, &account.id, mailbox, k, layer));
            }
            if !read.is_empty() {
                sidebar = sidebar.child(read_mailboxes_row(s, &account.id, read.len(), k));
                if s.read_mailboxes_open(&account.id) {
                    for mailbox in read {
                        sidebar = sidebar.child(mailbox_row(s, &account.id, mailbox, k, layer));
                    }
                }
            }
        }
    }
    sidebar.build(k)
}

/// Shows what a sidebar row stands for. A click leaves the keyboard to the list, for J, K and
/// the other keys on their own; the sidebar keeps it only when the keyboard brought it there.
fn show_from_sidebar(s: &mut Mail, tx: &mut Tx, showing: Showing) {
    s.show(showing);
    keep_keys_for_list(tx);
    // The mailbox shows whole at once, never fading through near-black.
    tx.instant();
}

/// After a click, keys go to the window again, not the control clicked: the list's keys on
/// their own keep working. A control the keyboard reached keeps them.
fn keep_keys_for_list(tx: &mut Tx) {
    if !tx.by_keys() {
        tx.blur();
    }
}

/// The row that opens or closes an account's group of `count` mailboxes with nothing unread,
/// listed under it while it's open. A click on it toggles the group. The arrow keys pass over
/// it, as they select each row they reach; its disclosure button, a stop of its own for Tab,
/// toggles the group from the keyboard.
fn read_mailboxes_row(s: &Mail, account: &str, count: usize, k: &Kit) -> El {
    let open = s.read_mailboxes_open(account);
    let (pressed, toggle) = (account.to_owned(), account.to_owned());
    let disclosure = IconButton::new(if open {
        &glyphs::TRIANGLE_DOWN
    } else {
        &glyphs::TRIANGLE_RIGHT
    })
    .variant(IconButtonVariant::GhostMuted)
    .label(if open {
        "Hide read mailboxes"
    } else {
        "Show read mailboxes"
    })
    .expanded(open)
    .on_press_tx(move |s: &mut Mail, tx| {
        s.toggle_read_mailboxes(&toggle);
        keep_keys_for_list(tx);
    })
    .build(k);
    SidebarRow::new("Read")
        .depth(1)
        .detail(count.to_string())
        .trailing(disclosure)
        .on_select(move |s: &mut Mail, tx| {
            s.toggle_read_mailboxes(&pressed);
            keep_keys_for_list(tx);
        })
        .build(k)
        .focusable(false)
        .focus_group()
        .key(format!("read-mailboxes-{account}"))
}

/// One of an account's mailboxes, under the account. Right-clicking it opens its menu.
fn mailbox_row(s: &Mail, account: &str, mailbox: &Mailbox, k: &Kit, layer: &mut Vec<El>) -> El {
    let showing = Showing::Mailbox {
        account: account.to_owned(),
        name: mailbox.name.clone(),
    };
    let selected = !s.form.open && !s.settings_open && s.showing == showing;
    let mut r = SidebarRow::new(mailbox.label())
        .icon(match mailbox.role {
            Role::Inbox => &INBOX,
            Role::Sent => &SENT,
            Role::Junk => &JUNK,
            _ => &FOLDER,
        })
        .depth(1)
        .selected(selected)
        .on_select(move |s: &mut Mail, tx| show_from_sidebar(s, tx, showing.clone()));
    // The inbox's count is the account's, whatever case the server lists it in.
    let unread = match mailbox.role {
        Role::Inbox => s.unread(Some(account)),
        _ => s.mailbox_unread(account, &mailbox.name),
    };
    if unread > 0 {
        r = r.badge(unread.to_string());
    }
    // Busy too while the last message autosort moved here is on its way back.
    let returning = s
        .returned_here
        .iter()
        .any(|(_, m)| m.account == account && m.mailbox == mailbox.name && s.is_moving(m));
    if returning || s.marking_seen(account, &mailbox.name) || s.refreshing(account, &mailbox.name) {
        r = r.status(RowStatus::Busy);
    }
    // What autosort just moved here, still to be reviewed, is said where it's found again.
    let to_review = s
        .batch_in(account, &mailbox.name)
        .map(|(moved, _)| moved.len())
        .filter(|n| *n > 0);
    let auto = (s.settings.sort_mail && s.autosorts_to(account, &mailbox.name)).then(|| {
        text(if returning {
            "Moving…".to_owned()
        } else {
            to_review.map_or_else(|| "auto".to_owned(), |n| format!("{n} to review"))
        })
        .font(k.ty.small)
        .color(k.c.text_accent)
        .label("Autosort on")
        .help("Autosort on: inbox mail that belongs here moves here by itself")
    });
    let key = (account.to_owned(), mailbox.name.clone());
    // Its menu, autosort among it, is a ⋯ away as well as a right-click.
    let menu_button = (s.hovered_mailbox.as_ref() == Some(&key)
        || s.mailbox_menu
            .as_ref()
            .is_some_and(|m| m.is(account, &mailbox.name)))
    .then(|| {
        let (on_account, on_mailbox) = key.clone();
        IconButton::new(&MORE)
            .variant(IconButtonVariant::Ghost)
            .label(format!("{} actions", mailbox.label()))
            .on_press(move |s: &mut Mail| s.open_mailbox_menu(&on_account, &on_mailbox, None))
            .build(k)
            .size(22.0, 22.0)
            .key(format!("mailbox-more-{account}-{}", mailbox.name))
    });
    if auto.is_some() || menu_button.is_some() {
        r = r.trailing(
            row()
                .gap(4.0)
                .center_y()
                .children(auto.into_iter().chain(menu_button)),
        );
    }
    let (menu_account, menu_mailbox) = key.clone();
    let r = r
        .build(k)
        .key(format!("mailbox-{account}-{}", mailbox.name))
        .on_hover_change(move |s: &mut Mail, inside, _| {
            if inside {
                s.hovered_mailbox = Some(key.clone());
            } else if s.hovered_mailbox.as_ref() == Some(&key) {
                s.hovered_mailbox = None;
            }
        })
        .on_secondary_press(move |s: &mut Mail, point, _| {
            s.open_mailbox_menu(&menu_account, &menu_mailbox, Some(point));
        });
    match &s.mailbox_menu {
        Some(menu) if menu.is(account, &mailbox.name) => {
            let marking = s.marking_seen(account, &mailbox.name);
            // Only where sorting can suggest mail goes, while it does.
            let autosort = (s.settings.sort_mail
                && postden_app::is_filed_in(&mailbox.name, mailbox.role))
            .then(|| s.autosorts_to(account, &mailbox.name));
            // The inbox's newest are downloaded ahead as the setting says; others', when chosen.
            let prefetch = (s.settings.prefetch > 0 && mailbox.role != Role::Inbox)
                .then(|| s.prefetches_from(account, &mailbox.name));
            // The inbox, Drafts and the rest the server or the app keeps mail in stay.
            let deletable = s.can_delete_mailbox(account, &mailbox.name);
            let offered = Offered {
                marking,
                autosort,
                prefetch,
                deletable,
            };
            mailbox_menu(menu, offered, r, k, layer)
        }
        _ => r,
    }
}

/// What a mailbox's menu offers, besides refreshing.
#[derive(Clone, Copy)]
struct Offered {
    /// The mailbox is being marked all read, so it can't be marked again.
    marking: bool,
    /// Whether inbox mail is moved there by itself, where that can be turned on or off.
    autosort: Option<bool>,
    /// Whether its newest messages are downloaded ahead, where that can be turned on or off.
    prefetch: Option<bool>,
    deletable: bool,
}

/// A mailbox's menu, open at where it was right-clicked: refreshing it or every mailbox,
/// marking it all read, turning autosorting there and downloading it ahead on or off, where
/// they can be, and deleting it if it's deletable.
fn mailbox_menu(menu: &MailboxMenu, offered: Offered, row: El, k: &Kit, layer: &mut Vec<El>) -> El {
    let Offered {
        marking,
        autosort,
        prefetch,
        deletable,
    } = offered;
    let (account, mailbox) = &menu.mailbox;
    let (a, b) = (account.clone(), mailbox.clone());
    let (refresh_account, refresh_mailbox) = (account.clone(), mailbox.clone());
    let (delete_account, delete_mailbox) = (account.clone(), mailbox.clone());
    // Opened from its ⋯ button, it hangs under the row rather than at a click.
    let mut items = context_menu(format!("mailbox-menu-{account}-{mailbox}"), row, menu.point)
        .open(true)
        .placement(Placement::BottomEnd)
        .min_width(220.0)
        .on_close(Mail::close_mailbox_menu)
        .item(MenuItem::new("Refresh").on_select(move |s: &mut Mail| {
            s.close_mailbox_menu();
            s.refresh_mailbox(&refresh_account, &refresh_mailbox);
        }))
        .item(
            MenuItem::new("Refresh all mailboxes").on_select(|s: &mut Mail| {
                s.close_mailbox_menu();
                s.refresh_all();
            }),
        )
        .item(
            MenuItem::new(if marking {
                "Marking all as read…"
            } else {
                "Mark all as read"
            })
            .disabled(marking)
            .on_select(move |s: &mut Mail| {
                s.close_mailbox_menu();
                s.mark_all_seen(&a, &b);
            }),
        );
    if let Some(on) = autosort {
        let (account, mailbox) = (account.clone(), mailbox.clone());
        items = items.item(
            MenuItem::new("Autosort mail here")
                .description(if on {
                    "On: files mail that belongs here"
                } else {
                    "Files mail that belongs here…"
                })
                .checked(on)
                // Its dialog opens whole, at once, never fading its text over the list.
                .on_select_tx(move |s: &mut Mail, tx| {
                    s.close_mailbox_menu();
                    s.toggle_autosort(&account, &mailbox);
                    tx.instant();
                }),
        );
    }
    if let Some(on) = prefetch {
        let (account, mailbox) = (account.clone(), mailbox.clone());
        items = items.item(
            MenuItem::new("Download ahead here")
                .description("Its newest open without waiting")
                .checked(on)
                .on_select(move |s: &mut Mail| {
                    s.close_mailbox_menu();
                    s.set_prefetch_mailbox(&account, &mailbox, !on);
                }),
        );
    }
    if deletable {
        items = items.item(
            MenuItem::new("Delete mailbox…").on_select(move |s: &mut Mail| {
                s.open_delete_mailbox(&delete_account, &delete_mailbox);
            }),
        );
    }
    let before = layer.len();
    let built = items.build(k, layer).w_full();
    instant_since(layer, before);
    built
}

/// An account, standing for its inbox, over its mailboxes. Right-clicking it opens its menu.
fn account_row(s: &Mail, account: &Account, k: &Kit, layer: &mut Vec<El>) -> El {
    let status = s.statuses.get(&account.id);
    let confirming = s.confirm_remove.as_deref() == Some(account.id.as_str());
    let (subtitle, row_status) = if confirming {
        ("Press × again to remove".to_owned(), None)
    } else {
        match status {
            None | Some(Ok(Status::Connecting)) => {
                ("Connecting…".to_owned(), Some(RowStatus::Busy))
            }
            Some(Ok(Status::Push)) => (account.id.clone(), None),
            Some(Ok(Status::Polling(every))) => {
                (format!("Checking every {}s", every.as_secs()), None)
            }
            Some(Ok(Status::Failed { error, retry_in })) => (
                format!("{error} · retrying in {}s", retry_in.as_secs()),
                Some(RowStatus::Attention),
            ),
            Some(Err(error)) => (error.clone(), Some(RowStatus::Attention)),
        }
    };
    let select = account.id.clone();
    let remove = account.id.clone();
    let toggle = account.id.clone();
    let expanded = !s.collapsed.contains(&account.id);
    let inbox_listed = expanded && s.mailbox_with_role(&account.id, Role::Inbox).is_some();
    let disclosure = IconButton::new(if expanded {
        &glyphs::TRIANGLE_DOWN
    } else {
        &glyphs::TRIANGLE_RIGHT
    })
    .variant(IconButtonVariant::GhostMuted)
    .label(if expanded {
        "Hide mailboxes"
    } else {
        "Show mailboxes"
    })
    .expanded(expanded)
    .on_press_tx(move |s: &mut Mail, tx| {
        s.toggle_mailboxes(&toggle);
        keep_keys_for_list(tx);
    })
    .build(k);
    let mut r = SidebarRow::new(account.label().to_owned())
        .height(45.0)
        .avatar(avatar(account.label(), k))
        .subtitle(subtitle)
        // Its mailboxes listed, the inbox's own row shows it's selected.
        .selected(
            !inbox_listed
                && !s.form.open
                && !s.settings_open
                && s.showing.is(&account.id, INBOX_NAME),
        )
        .trailing(disclosure)
        // The account itself stands for its inbox; its mailboxes, the inbox first, are the rows below.
        .on_select(move |s: &mut Mail, tx| show_from_sidebar(s, tx, Showing::inbox(&select)))
        .on_close(move |s: &mut Mail, _| s.remove(&remove));
    if confirming {
        r = r.subtitle_color(k.c.danger.fg);
    }
    if let Some(row_status) = row_status {
        r = r.status(row_status);
    }
    let unread = s.unread(Some(&account.id));
    if unread > 0 {
        r = r.badge(unread.to_string());
    }
    let menu_account = account.id.clone();
    let r = r
        .build(k)
        .key(format!("account-{}", account.id))
        .on_secondary_press(move |s: &mut Mail, point, _| {
            s.open_account_menu(&menu_account, point);
        });
    match &s.account_menu {
        Some(menu) if menu.account == account.id => account_menu(menu, r, k, layer),
        _ => r,
    }
}

/// An account's menu, open at where it was right-clicked: creating a mailbox on it.
fn account_menu(menu: &AccountMenu, row: El, k: &Kit, layer: &mut Vec<El>) -> El {
    let account = menu.account.clone();
    context_menu(
        format!("account-menu-{}", menu.account),
        row,
        Some(menu.point),
    )
    .min_width(200.0)
    .on_close(Mail::close_account_menu)
    .item(MenuItem::new("New mailbox…").on_select(move |s: &mut Mail| s.open_new_mailbox(&account)))
    .build(k, layer)
    .w_full()
}

/// The dialog naming a mailbox to create on an account: a field, why the server refused the
/// last name, and Create, busy until the server answers.
fn new_mailbox_dialog(s: &Mail, k: &Kit, layer: &mut Vec<El>) {
    let d = &s.new_mailbox;
    if !d.open {
        return;
    }
    let creating = d.creating.is_some();
    let input = TextInput::new(&d.name)
        .label("Mailbox name")
        .placeholder("Name")
        .width(None)
        .disabled(creating)
        .on_edit(|s: &mut Mail| &mut s.new_mailbox.name)
        .on_submit(|s: &mut Mail, _| s.create_mailbox());
    let mut body = col()
        .w_full()
        .gap(12.0)
        .padding(16.0)
        .pt(0.0)
        .child(input.build(k).key("new-mailbox-name"));
    if let Some(error) = &d.error {
        body = body.child(
            text(error.clone())
                .key("new-mailbox-error")
                .font(k.ty.small)
                .color(k.c.danger.fg)
                .wrap()
                .selectable(true),
        );
    }
    body = body.child(new_mailbox_actions(d, k));
    Dialog::new("new-mailbox", "New mailbox")
        .description(format!("Create a mailbox on {}.", s.label(&d.account)))
        .width(400.0)
        .fit()
        .on_close(Mail::close_new_mailbox)
        .child(body)
        .open(true)
        .build(k, layer);
}

/// The dialog confirming a mailbox is to be deleted with the mail in it: a warning, why the server
/// refused, and Delete, busy until the server answers.
/// Asks before autosort is turned on for a mailbox: what it does, and which messages in the
/// inbox it moves straight away.
fn autosort_dialog(s: &Mail, k: &Kit, layer: &mut Vec<El>) {
    const SHOWN: usize = 3;
    let Some(Asking::Autosort((account, mailbox), move_now)) = &s.asking else {
        return;
    };
    let label = s.mailbox_label(account, mailbox);
    let now = s.autosort_preview();
    let mut body = col().w_full().gap(10.0).padding(16.0).pt(0.0).child(
        text(format!(
            "New mail that sorting is sure belongs in {label} skips the inbox and goes \
             there by itself. Turn it off any time from this menu or in Settings → General."
        ))
        .wrap(),
    );
    if now.is_empty() {
        body = body.child(
            text("Nothing in your inbox moves right now.")
                .font(k.ty.small)
                .color(k.c.text_muted),
        );
    } else {
        let n = now.len();
        let s_ = if n == 1 { "" } else { "s" };
        let mut list = col().gap(4.0).child(
            text(format!("{n} message{s_} already in your inbox fit too:"))
                .font(k.ty.ui_medium)
                .key("autosort-count"),
        );
        for subject in now.iter().take(SHOWN) {
            list = list.child(
                text(format!("· {subject}"))
                    .font(k.ty.small)
                    .color(k.c.text_muted)
                    .truncate(),
            );
        }
        if n > SHOWN {
            list = list.child(
                text(format!("and {} more", n - SHOWN))
                    .font(k.ty.small)
                    .color(k.c.text_muted),
            );
        }
        // In a box of its own, so the choice shows as one before it's ticked.
        body = body.child(list).child(
            row()
                .w_full()
                .padding(8.0)
                .radius(6.0)
                .border(1.0, k.c.border_strong)
                .child(
                    // A switch, whose off is as plain to see as its on.
                    labeled_switch(
                        &format!("Also move these {n} now"),
                        Switch::new(*move_now).on_toggle(Mail::toggle_autosort_move_now),
                        "autosort-move-now",
                        k,
                    ),
                ),
        );
    }
    body = body.child(
        row()
            .w_full()
            .gap(8.0)
            .center_y()
            .child(el().grow(1.0))
            .child(
                Button::new("Cancel")
                    .variant(ButtonVariant::Neutral)
                    .on_press_tx(close_at_once)
                    .build(k)
                    .key("autosort-cancel"),
            )
            .child(
                Button::new("Turn on autosort")
                    .variant(ButtonVariant::Submit)
                    .on_press_tx(|s: &mut Mail, tx| {
                        s.submit_autosort();
                        tx.instant();
                    })
                    .build(k)
                    .key("autosort-confirm")
                    .default_button(),
            ),
    );
    instant_dialog(
        "autosort",
        format!("Autosort mail to {label}?"),
        420.0,
        body,
        k,
        layer,
    );
}

/// Closes what asks to be confirmed in one frame, with nothing fading.
fn close_at_once(s: &mut Mail, tx: &mut Tx) {
    s.close_asking();
    tx.instant();
}

/// A modal dialog like the kit's, closing with [`Mail::close_asking`], that's whole the frame
/// it opens and gone the frame it closes: its text never fades over what's under it.
fn instant_dialog(key: &str, title: String, width: f32, body: El, k: &Kit, layer: &mut Vec<El>) {
    let none = Transition::none;
    let dismiss = IconButton::new(k.glyph.close)
        .variant(IconButtonVariant::Ghost)
        .label("Close")
        .on_press_tx(close_at_once)
        .build(k)
        .key("close")
        .size(20.0, 20.0)
        .mt(-2.0)
        .mr(-2.0)
        .transition(none());
    let panel = col()
        .key(format!("{key}-dialog"))
        .role("dialog")
        .label(title.clone())
        .w_full()
        .max_w(width)
        .radius(6.0)
        .fill(k.c.layer_01)
        .child(
            row()
                .key("header")
                .w_full()
                .padding(16.0)
                .items_start()
                .child(
                    col().grow(1.0).child(
                        text(title)
                            .font(k.ty.ui_medium.lh(16.0))
                            .color(k.c.text_base),
                    ),
                )
                .child(dismiss),
        )
        .child(col().key("body").w_full().child(body));
    let panel = k
        .elevate(panel, &k.elevation.overlay)
        .modal_tx(true, close_at_once)
        .modal_click_outside(true)
        .transition(none());
    layer.push(
        el().key(format!("{key}-scrim"))
            .absolute(0.0, 0.0)
            .w_full()
            .h_full()
            .fill(k.c.scrim)
            .a11y_hidden()
            .transition(none()),
    );
    layer.push(
        el().key(format!("{key}-layer"))
            .absolute(0.0, 0.0)
            .w_full()
            .h_full()
            .padding(16.0)
            .center()
            .child(panel)
            .transition(none()),
    );
}

fn delete_mailbox_dialog(s: &Mail, k: &Kit, layer: &mut Vec<El>) {
    let d = &s.delete_mailbox;
    let Some((account, mailbox)) = &d.mailbox else {
        return;
    };
    let mut body = col().w_full().gap(12.0).padding(16.0).pt(0.0).child(
        text("The mail in it is deleted on the server too, for good.")
            .key("delete-mailbox-warning")
            .wrap(),
    );
    if let Some(error) = &d.error {
        body = body.child(
            text(error.clone())
                .key("delete-mailbox-error")
                .font(k.ty.small)
                .color(k.c.danger.fg)
                .wrap()
                .selectable(true),
        );
    }
    body = body.child(delete_mailbox_actions(
        s.deleting_mailbox(),
        d.error.is_none(),
        k,
    ));
    Dialog::new("delete-mailbox", "Delete mailbox")
        .description(format!(
            "Delete {} from {}?",
            s.mailbox_label(account, mailbox),
            s.label(account),
        ))
        .width(400.0)
        .fit()
        .on_close(Mail::close_delete_mailbox)
        .child(body)
        .open(true)
        .build(k, layer);
}

/// Cancel and Delete, the default button Return presses, which waits on the server. While it
/// waits, Cancel is Close: what was asked goes on. Not `armed`, as when it can't be deleted, it's
/// Close alone.
fn delete_mailbox_actions(deleting: bool, armed: bool, k: &Kit) -> El {
    let actions = row()
        .w_full()
        .gap(8.0)
        .center_y()
        .child(el().grow(1.0))
        .child(
            Button::new(if deleting || !armed {
                "Close"
            } else {
                "Cancel"
            })
            .variant(ButtonVariant::Neutral)
            .on_press(Mail::close_delete_mailbox)
            .build(k)
            .key("delete-mailbox-cancel"),
        );
    if !armed {
        return actions;
    }
    let delete = Button::new(if deleting { "Deleting…" } else { "Delete" })
        .variant(if deleting {
            ButtonVariant::Loading
        } else {
            ButtonVariant::Danger
        })
        .on_press(Mail::delete_mailbox)
        .build(k)
        .key("delete-mailbox-confirm");
    actions.child(if deleting {
        delete
    } else {
        delete.default_button()
    })
}

/// Cancel and Create, which needs a name and waits on the server.
fn new_mailbox_actions(d: &NewMailbox, k: &Kit) -> El {
    let creating = d.creating.is_some();
    row()
        .w_full()
        .gap(8.0)
        .center_y()
        .child(el().grow(1.0))
        .child(
            Button::new("Cancel")
                .variant(ButtonVariant::Neutral)
                .on_press(Mail::close_new_mailbox)
                .build(k)
                .key("new-mailbox-cancel"),
        )
        .child(
            Button::new(if creating { "Creating…" } else { "Create" })
                .variant(if creating {
                    ButtonVariant::Loading
                } else {
                    ButtonVariant::Contrast
                })
                .disabled(d.name.text.trim().is_empty())
                .on_press(Mail::create_mailbox)
                .build(k)
                .key("new-mailbox-create"),
        )
}

/// A rounded square with the account's initial.
fn avatar(name: &str, k: &Kit) -> El {
    let initial = name
        .chars()
        .next()
        .map(|c| c.to_uppercase().to_string())
        .unwrap_or_default();
    row()
        .size(16.0, 16.0)
        .radius(4.0)
        .center()
        .fill(k.c.bg_accent)
        .child(text(initial).font(k.ty.small_medium).color(Color::WHITE))
}

/// The message list, under the search bar once there's an account.
fn inbox(s: &Mail, k: &Kit, layer: &mut Vec<El>) -> El {
    let list = message_list(s, k, layer);
    if s.accounts.is_empty() {
        return list;
    }
    col()
        .key("inbox")
        // Swapped with the reader at once, never faded through it.
        .transition(Transition::none())
        .w_full()
        .grow(1.0)
        .min_h(0.0)
        .child(search_bar(s, k))
        .children(batch_banner(s, k))
        .child(list)
}

/// Over a mailbox autosort just moved a batch to, which messages came, and a way to send them
/// back, until that's done or they're kept.
fn batch_banner(s: &Mail, k: &Kit) -> Option<El> {
    let Showing::Mailbox { account, name } = &s.showing else {
        return None;
    };
    // The last one on its way back, the review holds its place until it's there.
    let Some((moved, _)) = s.batch_in(account, name) else {
        return s
            .returned_here
            .iter()
            .any(|(_, m)| m.account == *account && m.mailbox == *name && s.is_moving(m))
            .then(|| closing_banner(k));
    };
    // In the order the list shows them, so each is found where it's named.
    let listed: Vec<String> = s
        .messages
        .iter()
        .filter(|m| s.in_last_batch(m))
        .map(|m| crate::state::subject_of(&m.envelope))
        .collect();
    let subjects = if listed.len() == moved.len() {
        listed
    } else {
        moved.to_vec()
    };
    let n = subjects.len();
    // What's left to look at, rather than what autosort once did.
    let what = if n == 1 {
        "1 message autosort moved here".to_owned()
    } else {
        format!("{n} messages autosort moved here")
    };
    let words = col()
        .grow(1.0)
        .min_w(0.0)
        .gap(2.0)
        .child(
            text(format!("To review: {what}"))
                .font(k.ty.ui_medium)
                .color(k.c.text_base),
        )
        // What moving one back means, before it's done.
        .child(
            text("Moved back, a message stays in Inbox; autosort goes on for new mail.")
                .font(k.ty.small)
                .color(k.c.text_muted)
                .truncate(),
        );
    // Which ones, the rows say themselves, each tagged with a way back.
    Some(
        row()
            // Of how many, so a message leaving redraws it whole, never listing one twice.
            .key(format!("autosort-batch-{n}"))
            .transition(Transition::none())
            .w_full()
            .px(20.0)
            .py(10.0)
            .gap(10.0)
            .center_y()
            .fill(k.c.layer_01)
            .border_sides([0.0, 0.0, 1.0, 0.0], k.c.border_weak)
            .child(words)
            // One left, its own row's Move to Inbox is the way back: not two of them.
            .children((n > 1).then(|| {
                Button::new(match n {
                    1 => "Move it back to Inbox".to_owned(),
                    2 => "Move both to Inbox".to_owned(),
                    _ => format!("Move all {n} to Inbox"),
                })
                .variant(ButtonVariant::Neutral)
                .on_press(Mail::move_back_last_batch)
                .build(k)
                .key("autosort-batch-back")
            }))
            .child(
                // What ending the review does with them, said on its button.
                Button::new(if n == 1 {
                    format!("Keep it in {}", s.mailbox_label(account, name))
                } else {
                    format!("Keep them in {}", s.mailbox_label(account, name))
                })
                .variant(ButtonVariant::Ghost)
                .on_press(Mail::keep_last_batch)
                .build(k)
                .key("autosort-batch-keep"),
            ),
    )
}

/// The review banner while the last message it named is on its way back: its own shape, so
/// nothing under it moves before the end.
fn closing_banner(k: &Kit) -> El {
    row()
        .key("autosort-batch-closing")
        .transition(Transition::none())
        .w_full()
        .px(20.0)
        .py(10.0)
        .center_y()
        .fill(k.c.layer_01)
        .border_sides([0.0, 0.0, 1.0, 0.0], k.c.border_weak)
        .child(
            col()
                .gap(2.0)
                .grow(1.0)
                .child(
                    text("Moving the last message to Inbox…")
                        .font(k.ty.ui_medium)
                        .color(k.c.text_base),
                )
                .child(
                    text("Moved back, a message stays in Inbox; autosort goes on for new mail.")
                        .font(k.ty.small)
                        .color(k.c.text_muted),
                ),
        )
        .child(el().h(32.0))
}

/// The search field; unless the list already shows every inbox, whether to search just what it
/// shows or every inbox; and a button to show only unread mail, or all of it again.
fn search_bar(s: &Mail, k: &Kit) -> El {
    let mut input = TextInput::new(&s.search.query)
        .label("Search mail")
        // Once it has the keys, what it looks through.
        .placeholder(if s.search.is_active() || s.search.query.focused {
            "Subject, sender, recipient or downloaded text"
        } else {
            "Search mail on this computer (⌘F)"
        })
        .leading_icon(&glyphs::SEARCH)
        .width(None)
        .on_edit(|s: &mut Mail| &mut s.search.query)
        .on_change(|s: &mut Mail, _| s.search_changed())
        .on_submit(|s: &mut Mail, tx| s.search_submitted(tx))
        // Escape ends the search and leaves the field, so keys on their own work on the list again;
        // ↓ leaves it for the results, with the cursor on the first.
        .on_key(|s: &mut Mail, ev: &rune::KeyEvent, tx| {
            // Keys typed here don't reach the window: a G pressed before goes nowhere.
            s.forget_go();
            match ev.key {
                rune::Key::Escape => {
                    s.clear_search();
                    tx.instant();
                }
                // Only ↓ alone: ⇧↓ and ⌘↓ still select and move in the field.
                _ if Shortcut::Down.matches(ev, false) && s.cursor_to_first(tx) => {}
                _ => return false,
            }
            tx.blur();
            true
        });
    if !s.search.query.text.is_empty() {
        input = input.clear_button().on_action(|s: &mut Mail, _| {
            s.clear_search();
        });
    }
    // A button, not another segmented control: next to the field, it leaves the field room.
    let unread = Command::ToggleUnreadOnly.title(s);
    let field = row()
        .w_full()
        .gap(12.0)
        .center_y()
        .child(row().grow(1.0).min_w(0.0).child(input.build(k)))
        .child(
            IconButton::new(&UNREAD)
                .variant(if s.settings.unread_only {
                    IconButtonVariant::Neutral
                } else {
                    IconButtonVariant::GhostMuted
                })
                .label(unread.clone())
                .help(unread)
                .on_press_tx(|s: &mut Mail, tx| {
                    s.set_unread_only(!s.settings.unread_only);
                    keep_keys_for_list(tx);
                })
                .build(k)
                .key("unread-only"),
        );
    let bar = col()
        .key("search")
        .w_full()
        .px(20.0)
        .pt(4.0)
        .pb(10.0)
        .gap(8.0)
        .border_sides([0.0, 0.0, 1.0, 0.0], k.c.border_weak)
        .child(field);
    // While searching, or about to, where: what's shown, every inbox, or all mail; and asking
    // the server. On a row of their own, so the field keeps its width for what's typed.
    if s.search.is_active() || s.search.query.focused {
        bar.child(search_controls(s, k))
    } else {
        bar
    }
}

/// Under the search field while searching: where to search, and asking the mail server too.
fn search_controls(s: &Mail, k: &Kit) -> El {
    let here = showing_title(s);
    let inboxes = s.showing == Showing::All(Role::Inbox);
    let mut labels = Vec::new();
    if !inboxes {
        labels.push(here.as_str());
    }
    labels.extend(["All inboxes", "All mail"]);
    let picked = if s.search.all_mail {
        labels.len() - 1
    } else if s.search.everywhere || inboxes {
        labels.len() - 2
    } else {
        0
    };
    let scope = SegmentedControl::new(&labels, Some(picked))
        .label("Search in")
        .fit()
        .on_pick(move |s: &mut Mail, i, _| {
            let i = if inboxes { i + 1 } else { i };
            s.search_within(i == 1, i == 2);
            s.search.query.focused = true;
        })
        .build(k)
        .key("search-in")
        .transition(Transition::none());
    row()
        .key("search-controls")
        .transition(Transition::none())
        .w_full()
        .gap(12.0)
        .center_y()
        .child(scope)
        .child(el().grow(1.0))
        // What asking the server adds, before it's asked.
        .children((s.search.server() == ServerSearch::NotAsked).then(|| {
            text(format!(
                "Looks in {} there, older mail too",
                search_scope(s)
            ))
            .key("server-adds")
            .transition(Transition::none())
            .font(k.ty.small)
            .color(k.c.text_muted)
            .truncate()
            .min_w(0.0)
            .shrink()
        }))
        .child(server_control(s, k))
}

/// Asking the mail server too, for this query only: a button that says it sends it there,
/// then where the results come from, and a way back to only what's on this computer.
fn server_control(s: &Mail, k: &Kit) -> El {
    let note = |words: &str| {
        text(words.to_owned())
            .font(k.ty.small)
            .color(k.c.text_base)
            .key("search-local-note")
            .transition(Transition::none())
    };
    let local_only = || {
        Button::new("Show local results only")
            .variant(ButtonVariant::Ghost)
            .on_press(|s: &mut Mail| {
                s.search_local_only();
                s.search.query.focused = true;
            })
            .build(k)
            .key("search-local-only")
            .transition(Transition::none())
    };
    // Keyed by what it shows, so one state replaces the other whole, in the same frame,
    // rather than the old sliding out from under the new.
    let state = match s.search.server() {
        ServerSearch::NotAsked => "ask",
        ServerSearch::Running => "asking",
        ServerSearch::Done => "done",
        ServerSearch::Failed(_) => "failed",
    };
    let slot = row()
        .key(format!("server-{state}"))
        .transition(Transition::none())
        .gap(8.0)
        .center_y()
        .justify_end();
    match s.search.server() {
        ServerSearch::NotAsked => slot.child(
            // Where on the server it looks, named where the user agrees to send it.
            Button::new("Send this query to your mail server")
                .variant(ButtonVariant::Neutral)
                .disabled(!s.search.is_active())
                // The caret stays in the query, to refine it.
                .on_press(|s: &mut Mail| {
                    s.search_server();
                    s.search.query.focused = true;
                })
                .build(k)
                .key("search-server-switch")
                .transition(Transition::none()),
        ),
        ServerSearch::Running => slot.child(note("Asking your mail server…")),
        // The title says the server's results are in; here, only the way back.
        ServerSearch::Done => slot.child(local_only()),
        ServerSearch::Failed(_) => slot
            .child(note("Couldn't search your mail server"))
            .child(local_only()),
    }
}

/// The end of a search's results: a button to search the server too, a spinner while it does,
/// a word when it held back matches, and a way to try again where it failed.
fn server_search(s: &Mail, k: &Kit) -> El {
    let end = row()
        .key("server-search")
        .transition(Transition::none())
        .w_full()
        .min_h(48.0)
        .px(20.0)
        .gap(8.0)
        .center();
    let note = |words: String| text(words).font(k.ty.small).color(k.c.text_muted);
    let button = |label: &str, key: &str| {
        Button::new(label)
            .variant(ButtonVariant::Ghost)
            .on_press(Mail::search_server)
            .build(k)
            .key(key)
    };
    match s.search.server() {
        ServerSearch::Failed(n) => {
            let mailboxes = if n == 1 {
                "a mailbox".to_owned()
            } else {
                format!("{n} mailboxes")
            };
            end.child(note(format!("Couldn't search {mailboxes} on the server.")))
                .child(button("Try again", "search-server-again"))
        }
        ServerSearch::Done if s.search.more => end.child(note(format!(
            "Showing the newest {SERVER_PAGE} matches on the server from each mailbox. \
             Add words to narrow the search."
        ))),
        ServerSearch::Done => end.child(note(
            "Includes matches on the server, in message text too.".into(),
        )),
        // Asked by the bar's own button, which says what it adds.
        // Asking, it's said once, beside the control that asked.
        ServerSearch::NotAsked | ServerSearch::Running => end,
    }
}

/// What the list says when it has nothing to show.
fn empty_list(s: &Mail, k: &Kit) -> El {
    let searching = s.search.is_active();
    // An inbox got to zero, rather than nothing listed yet.
    if !searching
        && !s.accounts.is_empty()
        && s.settings.unread_only
        && s.listed_unread() == 0
        && s.listing_synced()
    {
        return nothing_unread(s, k);
    }
    let server = s.search.server();
    let scope = search_scope(s);
    let hint = if searching && server == ServerSearch::Running {
        format!("Nothing in {scope} matches yet. Searching the server…")
    } else if searching && server == ServerSearch::Done {
        format!("Nothing in {scope} matches, here or on the server.")
    } else if searching {
        format!(
            "Nothing in {scope} matches in subjects, people or downloaded text on this computer."
        )
    } else {
        empty_hint(s).into()
    };
    let mut empty = col()
        .key("empty")
        .transition(Transition::none())
        .w_full()
        .grow(1.0)
        .center()
        .gap(4.0)
        .child(text(hint).font(k.ty.ui).color(k.c.text_muted));
    // Filed away, mail isn't in an inbox: a search there can look through every mailbox, the
    // query kept. That comes first: it answers at once, unlike the server.
    if searching && !s.search.all_mail {
        empty = empty.child(el().h(6.0)).child(
            Button::new("Search all mail on this computer (↩)")
                .icon(&glyphs::SEARCH)
                .variant(ButtonVariant::Neutral)
                // The caret goes back to the query, to refine it or press Return.
                .on_press(|s: &mut Mail| {
                    s.search_within(false, true);
                    s.search.query.focused = true;
                })
                .build(k)
                .key("search-all-mail"),
        );
    }
    // The server's way to find more stays where it always is, above: said here, not moved.
    if searching && matches!(server, ServerSearch::Failed(_)) {
        empty = empty.child(server_search(s, k));
    }
    empty
}

/// Where a search looks, by name, as the empty list says it.
fn search_scope(s: &Mail) -> String {
    match s.listing() {
        Showing::Everything => "all mail".into(),
        Showing::All(Role::Inbox) => "your inboxes".into(),
        _ => showing_title(s),
    }
}

/// What the list says when it has nothing to show and isn't searching.
fn empty_hint(s: &Mail) -> &'static str {
    if s.accounts.is_empty() {
        "Add an account to get started."
    } else if s.settings.unread_only && s.listed_unread() > 0 {
        // Counted, but not listed: not fetched yet, or read and still being marked so.
        "Some unread mail here isn't shown yet."
    } else if s.showing_sent() {
        "No sent mail yet."
    } else if matches!(&s.showing, Showing::Mailbox { name, .. } if name != INBOX_NAME) {
        "Nothing here yet. Other mailboxes are checked every few minutes."
    } else {
        "No mail yet. New messages appear here as they arrive."
    }
}

/// Sorting's settings: suggestions on or off, where mail is autosorted to, and forgetting what
/// it learned, which asks first.
fn sorting_settings(s: &Mail, k: &Kit, layer: &mut Vec<El>) -> El {
    let note = |words: &str| {
        text(words.to_owned())
            .font(k.ty.small)
            .color(k.c.text_muted)
            .wrap()
    };
    // The reset belongs with the switch, not with any one mailbox's autosort.
    let sorting = col()
        .w_full()
        .gap(10.0)
        .child(labeled_switch(
            "Sort mail on this computer",
            Switch::new(s.settings.sort_mail)
                .on_toggle(|s: &mut Mail| s.set_sort_mail(!s.settings.sort_mail)),
            "sort-mail",
            k,
        ))
        .child(note(
            "Tags inbox mail with where it seems to go (press the tag, or M, to move it) and \
             whether it seems important. Turning this off pauses suggestions and autosort.",
        ))
        .child(forget_sorting(s, k));
    // Autosort hangs off the switch above, as it runs only while that's on.
    let mut autosort = col()
        .w_full()
        .gap(10.0)
        .pl(14.0)
        .border_sides([0.0, 0.0, 0.0, 2.0], k.c.border_strong)
        .child(text("Autosort").font(k.ty.ui_medium).color(k.c.text_base));
    let autosorted = s.autosorted_mailboxes();
    if autosorted.is_empty() {
        autosort = autosort.child(note(
            "Off for every mailbox. Right-click a mailbox and choose Autosort mail here to \
             have mail that belongs there skip the inbox.",
        ));
    }
    for (account, mailbox) in autosorted {
        let label = format!(
            "{} · {}",
            s.mailbox_label(&account, &mailbox),
            s.label(&account)
        );
        let key = format!("autosort-{account}-{mailbox}");
        // Kept while sorting is off, but paused, as nothing's sorted.
        let label = if s.settings.sort_mail {
            label
        } else {
            format!("{label} (paused)")
        };
        autosort = autosort.child(labeled_switch(
            &label,
            Switch::new(true).on_toggle(move |s: &mut Mail| s.toggle_autosort(&account, &mailbox)),
            &key,
            k,
        ));
    }
    if let Some(pick) = autosort_picker(s, k, layer) {
        autosort = autosort.child(pick);
    }
    sorting.child(autosort.mt(4.0))
}

/// A switch with its label in full contrast before it, as what it turns on is what's read
/// first: the kit's own label is faint, a hint's.
fn labeled_switch(label: &str, switch: Switch, key: &str, k: &Kit) -> El {
    row()
        .gap(10.0)
        .center_y()
        .child(text(label.to_owned()).font(k.ty.ui).color(k.c.text_base))
        .child(
            switch
                .label(label)
                .hide_label()
                .build(k)
                .key(key.to_owned()),
        )
}

/// Forgetting what sorting learned, which asks first, saying what's kept.
fn forget_sorting(s: &Mail, k: &Kit) -> El {
    // Asked on the button's own line, so nothing below it moves.
    if s.asking == Some(Asking::ForgetSorting) {
        return row()
            .min_h(52.0)
            .gap(8.0)
            .center_y()
            // The question first, read before the button; what follows, under it.
            .child(
                col()
                    .gap(2.0)
                    .max_w(380.0)
                    .min_w(0.0)
                    .child(
                        text(if s.autosorted_mailboxes().is_empty() {
                            "Clear what sorting learned from your actions?"
                        } else {
                            "Reset learned sorting?"
                        })
                        .font(k.ty.ui_medium)
                        .color(k.c.text_base)
                        .wrap(),
                    )
                    .child(
                        text("Clears where it learned your mail goes and what seems important to you. Autosort continues while it learns again from the mail on this computer.")
                            .font(k.ty.small)
                            .color(k.c.text_muted)
                            .wrap(),
                    ),
            )
            .child(
                Button::new("Reset learned sorting")
                    .variant(ButtonVariant::Danger)
                    .on_press_tx(|s: &mut Mail, tx| {
                        s.forget_sorting();
                        tx.instant();
                    })
                    .build(k)
                    .key("forget-sorting-confirm"),
            )
            .child(
                Button::new("Cancel")
                    .variant(ButtonVariant::Ghost)
                    .on_press_tx(close_at_once)
                    .build(k)
                    .key("forget-sorting-cancel"),
            );
    }
    // As tall as the question that takes its place, so nothing below moves when it does.
    row().min_h(52.0).center_y().child(
        Button::new("Reset learned sorting…")
            .variant(ButtonVariant::Ghost)
            .on_press_tx(|s: &mut Mail, tx| {
                s.ask_forget_sorting();
                // The question takes the button's place at once, never over it.
                tx.instant();
            })
            .build(k)
            .key("forget-sorting"),
    )
}

/// A menu of the mailboxes mail could be autosorted to but isn't, each asking to turn it on.
fn autosort_picker(s: &Mail, k: &Kit, layer: &mut Vec<El>) -> Option<El> {
    let candidates = s.autosort_candidates();
    if candidates.is_empty() {
        return None;
    }
    let several = s.accounts.len() > 1;
    let mut pick = Menu::new("autosort-pick")
        .trigger(
            Button::new("Autosort another mailbox…")
                .icon(&PLUS)
                .variant(ButtonVariant::Ghost)
                .on_press(Mail::toggle_autosort_pick)
                .build(k)
                .key("autosort-add"),
        )
        .placement(Placement::BottomStart)
        .min_width(220.0)
        .open(s.asking == Some(Asking::AutosortPick))
        .on_close(Mail::close_asking);
    for (account, name, label) in candidates {
        let label = if several {
            format!("{label} · {}", s.label(&account))
        } else {
            label
        };
        pick = pick.item(MenuItem::new(label).on_select_tx(move |s: &mut Mail, tx| {
            s.close_asking();
            s.toggle_autosort(&account, &name);
            tx.instant();
        }));
    }
    let before = layer.len();
    let built = pick.build(k, layer);
    instant_since(layer, before);
    Some(row().child(built))
}

/// What a list of only unread mail says once there's none: an inbox got to zero.
fn nothing_unread(s: &Mail, k: &Kit) -> El {
    let inbox = match &s.showing {
        Showing::All(role) => *role == Role::Inbox,
        Showing::Mailbox { name, .. } => name == INBOX_NAME,
        Showing::Everything => false,
    };
    let (title, hint) = if inbox {
        (
            "Congratulations: you got to inbox 0",
            "Nothing unread. New messages appear here as they arrive.",
        )
    } else {
        ("Nothing unread", "Every message here is read.")
    };
    col()
        .key(if inbox {
            "inbox-zero"
        } else {
            "nothing-unread"
        })
        .w_full()
        .grow(1.0)
        .center()
        .gap(6.0)
        .child(text(title).font(k.ty.body_medium).color(k.c.text_base))
        .child(text(hint).font(k.ty.ui).color(k.c.text_muted))
        .child(
            Button::new("Show all mail")
                .variant(ButtonVariant::Ghost)
                .on_press_tx(|s: &mut Mail, tx| {
                    s.set_unread_only(false);
                    keep_keys_for_list(tx);
                })
                .build(k)
                .key("show-all-mail"),
        )
}

fn message_list(s: &Mail, k: &Kit, layer: &mut Vec<El>) -> El {
    if s.shown().next().is_none() {
        return empty_list(s, k);
    }
    let searching = s.search.is_active();
    let now = ZonedDateTime::now();
    // Mail from several accounts says whose it is.
    let multiple_accounts = s.listing().account().is_none() && s.accounts.len() > 1;
    // Searching all mail, each says which mailbox it's in.
    let all_mail = *s.listing() == Showing::Everything;
    let sent = s.showing_sent();
    let mut list = messages_column(searching, &showing_title(s));
    list = list.children(search_coverage(s, k));
    for listed in s.list_rows() {
        let m = match listed {
            ListRow::Message(m) => m,
            ListRow::Returned(m) => {
                list = list.child(returned_row(s, m, k));
                continue;
            }
        };
        let e = &m.envelope;
        let strong = if e.seen { k.ty.ui } else { k.ty.ui_medium };
        let (account, mailbox, uid) = (m.account.clone(), m.mailbox.clone(), e.uid);
        let (menu_account, menu_mailbox) = (account.clone(), mailbox.clone());
        let key = crate::state::message_key(m);
        // Where a search found it in its text, once that's on this computer.
        let second = second_line(s, m, k);
        // The keyboard's cursor, while the list shows.
        let under_cursor = s.reader.is_none() && s.cursor.as_ref() == Some(&key);
        // While the search field has the keys, M types: the hint goes.
        let m_works = under_cursor && !s.search.query.focused;
        // The cursor's row stands apart from one under the pointer: a stronger fill and an
        // accent edge, so a key acts on the row it looks like it will.
        let line = row()
            .key(crate::state::row_key(&key))
            // A row comes and goes at once: a search's rows never fade through the list's.
            .transition(Transition::none())
            .fill(if under_cursor {
                k.c.pressed
            } else {
                Color::TRANSPARENT
            })
            // Opens whole at once, never fading through near-black.
            .on_press_tx(move |s: &mut Mail, tx| {
                s.open_message(&account, &mailbox, uid);
                tx.instant();
            })
            // Reaching another row, the pointer shows its buttons again.
            .on_hover_change(releases_row_actions(key.clone()))
            .on_secondary_press(move |s: &mut Mail, point, _| {
                s.open_message_menu(&menu_account, &menu_mailbox, uid, point);
            })
            .w_full()
            // A match in its text shows a line of it under the subject.
            .h(if second.is_some() { 54.0 } else { 40.0 })
            .pl(3.0)
            .pr(20.0)
            .gap(12.0)
            .center_y()
            .hover(if under_cursor { k.c.pressed } else { k.c.hover })
            .border_sides([0.0, 0.0, 1.0, 0.0], k.c.border_weak)
            .child(
                el().w(3.0)
                    .h_full()
                    .fill(match (under_cursor, s.search.query.focused) {
                        // Muted while the keys go to the search field.
                        (true, true) => k.c.border_strong,
                        (true, false) => k.c.text_accent,
                        _ => Color::TRANSPARENT,
                    }),
            )
            .child(dot(if e.seen {
                Color::TRANSPARENT
            } else {
                k.c.text_accent
            }))
            .child(sender_cell(s, e, sent, strong, k))
            .child(subject(e, strong, s.search.query(), second, k))
            .child(tags(
                s,
                &key,
                s.kept_from_autosort(&m.account, e),
                m_works,
                k,
            ))
            // A slot whether starred or not, so the dates line up.
            .child(if e.flagged {
                icon(&STAR, 14.0).color(k.c.text_accent).label("Starred")
            } else {
                el().w(14.0)
            });
        // On the cursor's row and the one under the pointer, each acting on its own message:
        // the cursor's stay where the keys act while the pointer is elsewhere.
        let hovering = s.reader.is_none() && s.hovered_row.is_some();
        let shows_actions = (under_cursor && !s.search.query.focused)
            || (hovering && s.hovered_row.as_ref() == Some(&key));
        let actions = (shows_actions && s.row_actions_held.as_ref() != Some(&key))
            .then(|| row_actions(s, &key, under_cursor && !s.search.query.focused, k));
        // The row the pointer's buttons act on, not the cursor's, is outlined as their target.
        let pointer_target = actions.is_some() && hovering && !under_cursor;
        let line = row_end(s, m, line, (all_mail, multiple_accounts), &now, actions, k);
        let line = if pointer_target {
            line.outline(1.0, -1.0, k.c.border_base)
        } else {
            line
        };
        list = list.child(match &s.message_menu {
            Some(menu) if menu.is(&m.account, &m.mailbox, e.uid) => {
                message_menu(s, m, menu, line, k, layer)
            }
            _ => line,
        });
    }
    scrolling(s, list, searching, k)
}

/// The list's column, scrolling, ending with what reads older mail, and searching, what asks the
/// server.
fn scrolling(s: &Mail, mut list: El, searching: bool, k: &Kit) -> El {
    if s.has_older() {
        list = list.child(older(s, k));
    }
    if searching {
        list = list.child(server_search(s, k));
    }
    list.scroll(ScrollSpec {
        stick_bottom: false,
    })
    // A row cut off at an edge fades out rather than leaving a sliver of its tags; one the
    // cursor reaches stops clear of the fade.
    .scroll_edge_fade(20.0)
    .scroll_margin(20.0)
    .grow(1.0)
    .min_h(0.0)
}

/// The line under a row's subject, while there's one: where a search found it in its text (or
/// that it did, while that text downloads), and, autosort having just brought it here, a way to
/// send it back alone. On a line of its own, so the subject keeps its width.
fn second_line(s: &Mail, m: &StoredMessage, k: &Kit) -> Option<El> {
    let mut line = Vec::new();
    if let Some(query) = s.search.query()
        && !crate::search::matches(&m.envelope, query)
    {
        line.push(s.excerpt(m, query).map_or_else(
            || {
                text("Found in the message text")
                    .font(k.ty.small)
                    .color(k.c.text_muted)
            },
            |excerpt| {
                text(excerpt.clone())
                    .font(k.ty.small)
                    .color(k.c.text_muted)
                    .spans(found_spans(&excerpt, query, k))
                    .truncate()
                    .min_w(0.0)
                    .label("Matched in the message text")
            },
        ));
    }
    if s.in_last_batch(m) {
        let message = m.clone();
        line.push(text("Autosorted").font(k.ty.small).color(k.c.text_muted));
        line.push(
            Button::new("Move to Inbox")
                .variant(ButtonVariant::Outline)
                // A returned row keeps its place, so this one never slides under the pointer.
                .on_press(move |s: &mut Mail| s.move_back_one(&message))
                .build(k)
                .key(format!("autosorted-move-back-{}", m.envelope.uid)),
        );
    }
    (!line.is_empty()).then(|| row().gap(8.0).center_y().min_w(0.0).children(line))
}

/// What the pointer reaching the row `key` does: shows the cursor row's buttons again, unless
/// it's still on the row they're held back for.
fn releases_row_actions(
    key: crate::search::MessageKey,
) -> impl Fn(&mut Mail, bool, &mut Tx) + 'static {
    move |s: &mut Mail, inside, _| {
        if inside {
            if s.hover_quiet_until
                .is_some_and(|t| std::time::Instant::now() < t)
            {
                return;
            }
            s.hovered_row = Some(key.clone());
            if s.row_actions_held.as_ref() != Some(&key) {
                s.row_actions_held = None;
            }
        } else {
            if s.hovered_row.as_ref() == Some(&key) {
                s.hovered_row = None;
            }
            // Gone from the row they were held back on, the pointer can't click them unseen.
            if s.row_actions_held.as_ref() == Some(&key) {
                s.row_actions_held = None;
            }
        }
    }
}

/// What a search looks through, as a line above what it found: where, which fields, and
/// whether the mail server was asked too.
fn search_coverage(s: &Mail, k: &Kit) -> Option<El> {
    let query = s.search.query()?;
    // Where it looked, the title and the controls above already say: here, how many it found.
    let n = s.shown().count();
    let found = if n == 1 {
        format!("1 message matches “{query}”")
    } else {
        format!("{n} messages match “{query}”")
    };
    // Where it looked, beside what it found.
    let coverage = match s.search.server() {
        ServerSearch::Done => format!("{found} · on this computer and your mail server"),
        ServerSearch::Running => format!("{found} so far"),
        _ => format!("{found} · on this computer: subjects, people and downloaded text"),
    };
    Some(
        row()
            .key("search-coverage")
            .transition(Transition::none())
            .w_full()
            .px(20.0)
            .py(6.0)
            .border_sides([0.0, 0.0, 1.0, 0.0], k.c.border_weak)
            .child(
                text(coverage)
                    .font(k.ty.small)
                    .color(k.c.text_muted)
                    .truncate(),
            ),
    )
}

/// The end of a message's row: its mailbox, searching all mail; its account, listing several;
/// and its date.
fn row_end(
    s: &Mail,
    m: &StoredMessage,
    mut line: El,
    (all_mail, multiple_accounts): (bool, bool),
    now: &ZonedDateTime,
    row_actions: Option<El>,
    k: &Kit,
) -> El {
    if all_mail {
        line = line.child(
            row().w(80.0).justify_end().child(
                text(shortened(&s.mailbox_label(&m.account, &m.mailbox)))
                    .font(k.ty.small)
                    .color(k.c.text_muted)
                    .truncate(),
            ),
        );
    }
    if multiple_accounts {
        line = line.child(
            text(s.label(&m.account).to_owned())
                .font(k.ty.small)
                .color(k.c.text_faint)
                .truncate()
                .w(140.0),
        );
    }
    // Archive and Delete, when shown, in a slot every row keeps, so the date stays put.
    line.child(row_actions.unwrap_or_else(|| el().w(ROW_ACTIONS_WIDTH)))
        .child(
            row().w(64.0).justify_end().child(
                text(postden_app::format_date(m.envelope.date, now))
                    .font(k.ty.small)
                    .color(k.c.text_muted),
            ),
        )
}

/// How wide the slot is that every row keeps for its Archive and Delete buttons, just before
/// its date, so neither the date nor the rest of the row moves as they come and go.
const ROW_ACTIONS_WIDTH: f32 = 48.0;

/// Archive and Delete for the message under the cursor, each as its key does, in the date's
/// slot of its row.
/// With `keyed`, on the cursor's row, their tooltips give the keys that act on it too; on a row
/// the pointer is over, they don't, as the keys act on the cursor's.
fn row_actions(s: &Mail, message: &crate::search::MessageKey, keyed: bool, k: &Kit) -> El {
    // What this row's message can do, whichever the cursor is on (or none, in a mailbox just
    // opened): moved to its account's Archive or Trash, unless it's there already.
    let (account, mailbox, _) = message;
    // Where sorting suggests Archive, its chip, always shown, is the one Archive control.
    let suggested = s.suggested(message);
    let offered = |role: Role| {
        !s.settings_open
            && s.mailbox_with_role(account, role)
                .is_some_and(|b| b.name != *mailbox && suggested != Some(b.name.as_str()))
    };
    let mut group = row()
        .w(ROW_ACTIONS_WIDTH)
        .gap(2.0)
        .justify_end()
        .center_y()
        .key("row-actions")
        .transition(Transition::none());
    for (command, icon, key, label) in [
        (Command::Archive, &ARCHIVE, "row-archive", "Archive (E)"),
        (Command::Trash, &TRASH, "row-trash", "Move to Trash (#)"),
    ] {
        let label = if keyed {
            label
        } else {
            label.split(" (").next().unwrap_or(label)
        };
        let role = if command == Command::Archive {
            Role::Archive
        } else {
            Role::Trash
        };
        if offered(role) {
            group = group.child(
                IconButton::new(icon)
                    .variant(IconButtonVariant::GhostMuted)
                    .label(label)
                    .help(label)
                    .on_press_tx({
                        let message = message.clone();
                        move |s: &mut Mail, tx| {
                            // On this row's message, whichever the cursor is on.
                            s.cursor = Some(message.clone());
                            command.clone().run(s, tx);
                            // The row taking its place, under the same pointer, waits to show its own.
                            s.row_actions_held.clone_from(&s.cursor);
                        }
                    })
                    .build(k)
                    .key(key),
            );
        }
    }
    // The pointer's, apart from the cursor's, as a pill of their own: the keys act elsewhere.
    if keyed {
        group
    } else {
        group.fill(k.c.layer_02).radius(6.0)
    }
}

/// A message's subject in the list, taking the room left, muted once it's read.
/// Who a row's message is from, or, sent, whom it went to; matches of the search marked.
fn sender_cell(
    s: &Mail,
    e: &postden_core::message::Envelope,
    sent: bool,
    strong: TextStyle,
    k: &Kit,
) -> El {
    text(if sent {
        format!("To: {}", postden_app::recipients(&e.to))
    } else {
        e.from.label().to_owned()
    })
    .spans(match (s.search.query(), sent) {
        (Some(q), false) => found_spans(e.from.label(), q, k),
        _ => Vec::new(),
    })
    .font(strong)
    .color(k.c.text_base)
    .truncate()
    // Searching all mail, the mailbox's column takes some of its room.
    .w(if *s.listing() == Showing::Everything {
        112.0
    } else {
        120.0
    })
}

/// The words of `query` found in `text`, marked as the kit's search marks them.
fn found_spans(text: &str, query: &str, k: &Kit) -> Vec<rune::Span> {
    crate::search::found_in(text, query)
        .into_iter()
        .map(|r| rune::Span {
            color: Some(k.c.text_base),
            weight: Some(650.0),
            bg: Some(k.alpha(k.c.text_accent, 0.22)),
            bg_pad: 1.0,
            ..rune::Span::new(r.start, r.end)
        })
        .collect()
}

/// With a `second` line, as [`second_line`] makes, that's under it.
fn subject(
    e: &postden_core::message::Envelope,
    strong: TextStyle,
    query: Option<&str>,
    second: Option<El>,
    k: &Kit,
) -> El {
    let subject = e.subject.clone().unwrap_or_else(|| "(no subject)".into());
    let spans = query.map_or_else(Vec::new, |q| found_spans(&subject, q, k));
    // Cut short, the whole subject shows on hover.
    let line = text(subject.clone())
        .help_when_truncated(subject)
        .spans(spans)
        .font(strong)
        .color(if e.seen {
            k.c.text_muted
        } else {
            k.c.text_base
        })
        .truncate()
        .min_w(0.0);
    let Some(second) = second else {
        return line.grow(1.0);
    };
    col()
        .grow(1.0)
        .min_w(0.0)
        .gap(2.0)
        .child(line)
        .child(second)
}

/// A message just moved back out of the mailbox shown, in the place it had, said to be moved:
/// the rows after it stay where the pointer last found them.
fn returned_row(s: &Mail, m: &StoredMessage, k: &Kit) -> El {
    let e = &m.envelope;
    let back = s.mailbox_with_role(&m.account, Role::Inbox).map_or_else(
        || "Inbox".to_owned(),
        |b| s.mailbox_label(&m.account, &b.name),
    );
    row()
        .key(format!("returned-{}", e.uid))
        .transition(Transition::none())
        .w_full()
        .h(54.0)
        .pl(3.0)
        .pr(20.0)
        .gap(12.0)
        .center_y()
        .border_sides([0.0, 0.0, 1.0, 0.0], k.c.border_weak)
        .opacity(0.7)
        .child(el().w(3.0))
        .child(dot(Color::TRANSPARENT))
        .child(sender_cell(s, e, false, k.ty.ui, k))
        .child(subject(
            e,
            k.ty.ui,
            None,
            Some(
                text(if s.is_moving(m) {
                    format!("Moving to {back}…")
                } else {
                    format!("Moved to {back}")
                })
                // Its status readable, though the message itself is set back.
                .font(k.ty.small_medium)
                .color(k.c.text_base),
            ),
            k,
        ))
}

/// What sorting says of a message in the list: that it seems important, and where it seems to
/// belong, which moves it there when pressed (or with M).
/// Under the cursor, the suggestion spells out what M does.
fn tags(
    s: &Mail,
    key: &crate::search::MessageKey,
    kept_from: Option<&str>,
    focused: bool,
    k: &Kit,
) -> El {
    // Narrow, the folder's name is cut short rather than the date pushed off the row.
    let mut tags = row().gap(6.0).center_y().shrink();
    // Moved back from where autosort put it, it says it stays, not that it could go there;
    // searching, the subject has that room, and the row names its mailbox anyway.
    // Only once it's back: still where autosort put it, it isn't kept anywhere yet.
    if let Some(from) = kept_from
        && key.1 != from
        && s.suggested(key).is_none_or(|to| to == from)
    {
        if s.search.is_active() {
            return tags;
        }
        let label = s.mailbox_label(&key.0, from);
        return tags.child(
            text(format!("Kept in {}", s.mailbox_label(&key.0, &key.1)))
                .font(k.ty.small)
                .color(k.c.text_muted)
                .help(format!("Moved back from {label}: autosort leaves it here")),
        );
    }
    let Some(t) = s.tags(key) else {
        return tags;
    };
    if t.important {
        // Beside a suggestion, the subject needs the room: said by a mark, named on hover.
        let crowded = s.suggested(key).is_some() && (!s.search.is_active() || focused);
        tags = tags.child(if crowded {
            row()
                .size(18.0, 18.0)
                .center()
                .radius(9.0)
                .fill(k.c.layer_02)
                .label("Likely important")
                .help("Likely important: sorting on this computer thinks this matters to you")
                .child(text("!").font(k.ty.small_medium).color(k.c.text_muted))
        } else {
            // Quieter than what the user marked themselves, a star.
            Badge::new("Likely important")
                .variant(BadgeVariant::Subtle)
                .build(k)
                .help("Sorting on this computer thinks this matters to you")
        });
    }
    // Searching, a row's subject and the text it matched need the room: where it could go
    // shows on the row at hand, under the cursor or the pointer.
    let at_hand = focused || s.hovered_row.as_ref() == Some(key);
    if let Some(to) = s
        .suggested(key)
        .filter(|_| at_hand || !s.search.is_active())
    {
        let label = s.mailbox_label(&key.0, to);
        let (pressed, shown) = (key.clone(), to.to_owned());
        // Archive suggested, this is the row's one Archive control, so E works on it too.
        let archives = s
            .mailbox_with_role(&key.0, Role::Archive)
            .is_some_and(|b| b.name == to);
        tags = tags.child(
            row()
                // Of its destination too: a press held while that changes is no press on it. And of
                // whether it's spelled out, so it's swapped whole rather than stretched.
                .key(format!(
                    "suggested-{}-{to}-{focused}",
                    crate::state::row_key(key)
                ))
                .transition(Transition::none())
                .role("button")
                // M moves the message under the cursor, so only its badge says so.
                .label(if focused {
                    format!("Move to {label} (M)")
                } else {
                    format!("Move to {label}")
                })
                .help(if focused && archives {
                    format!("Sorting on this computer suggests {label}: press this, M or E")
                } else if focused {
                    format!(
                        "Sorting on this computer suggests {label}: press this, or M, to move it"
                    )
                } else {
                    format!("Sorting on this computer suggests {label}: press this to move it")
                })
                .radius(3.0)
                .hover(k.c.hover)
                // Easier to hit than it looks: the row's height, the badge centred in it.
                .min_h(30.0)
                .center_y()
                .on_press(move |s: &mut Mail| s.move_as_suggested(&pressed, &shown))
                .shrink()
                .child(
                    suggestion_badge(
                        format!("Move to {}", shortened(&label)),
                        focused,
                        focused || s.hovered_row.as_ref() == Some(key),
                        k,
                    )
                    .a11y_hidden(),
                ),
        );
    }
    tags
}

/// Where sorting suggests a message goes, as a button: a badge in the accent's outline. Under
/// the cursor it shows M, the key that moves it there, in a slot it keeps either way, so the row
/// doesn't reflow as the cursor passes.
fn suggestion_badge(label: String, focused: bool, lit: bool, k: &Kit) -> El {
    let font = TextStyle {
        line_height: 11.0,
        ..k.ty.small_medium
    };
    row()
        .center()
        .h(18.0)
        .pl(7.0)
        .pr(3.0)
        .gap(5.0)
        .radius(5.0)
        .fill(k.c.layer_02)
        // Quiet until it's the one M acts on: the messages come first, not what to do with them.
        // A button at rest, in full text; blue where it's at hand, under the pointer or the
        // cursor; M only where the keys act.
        .border(
            1.0,
            if lit {
                k.c.border_focus
            } else {
                k.c.border_strong
            },
        )
        .shrink()
        // An arrow: something it does, not only something said of the message.
        .child(
            text("→")
                .font(font)
                .color(if lit { k.c.text_accent } else { k.c.text_base }),
        )
        .child(
            text(label)
                .font(font)
                .color(if lit { k.c.text_accent } else { k.c.text_base })
                .truncate(),
        )
        .child(
            row()
                .center()
                .size(12.0, 12.0)
                .radius(6.0)
                .fill(if focused {
                    k.c.bg_accent
                } else {
                    Color::TRANSPARENT
                })
                .child(text("M").font(font).color(if focused {
                    k.c.text_contrast
                } else {
                    Color::TRANSPARENT
                })),
        )
}

/// `label`, cut short with an ellipsis past what a tag in a message's row has room for.
fn shortened(label: &str) -> String {
    const MOST: usize = 24;
    if label.chars().count() <= MOST {
        return label.to_owned();
    }
    let kept: String = label.chars().take(MOST - 1).collect();
    format!("{}…", kept.trim_end())
}

/// The list's column. Back from what covered it (a message, the settings, a message being
/// written), the list comes back at its top: it scrolls to its cursor.
/// The list's column, another while searching and for each list shown, so the results and the
/// list, or one mailbox and the next, swap whole rather than sliding through each other.
fn messages_column(searching: bool, showing: &str) -> El {
    col()
        .key(if searching {
            format!("messages-search-{showing}")
        } else {
            format!("messages-{showing}")
        })
        .transition(Transition::none())
        .w_full()
        .on_visible(|s: &mut Mail, shown, tx| {
            if shown {
                s.reveal_cursor(tx);
            }
        })
}

/// A message's menu, open at where it was right-clicked: marking it read or unread, starring
/// it, replying to or forwarding it, exporting it as an `.eml` file, snoozing it, archiving it,
/// marking it as spam, moving it to another of its account's mailboxes, and deleting it.
fn message_menu(
    s: &Mail,
    m: &StoredMessage,
    menu: &MessageMenu,
    line: El,
    k: &Kit,
    layer: &mut Vec<El>,
) -> El {
    let (account, mailbox, uid) = (m.account.as_str(), m.mailbox.as_str(), m.envelope.uid);
    // What each item does to this message, closing the menu first.
    let act = |f: fn(&mut Mail, &str, &str, u32)| {
        let (account, mailbox) = (account.to_owned(), mailbox.to_owned());
        move |s: &mut Mail| {
            s.close_message_menu();
            f(s, &account, &mailbox, uid);
        }
    };
    let seen = m.envelope.seen;
    let mut items = context_menu(
        format!("message-{account}-{mailbox}-{uid}"),
        line,
        Some(menu.point),
    )
    .min_width(200.0)
    .on_close(Mail::close_message_menu)
    .item(
        MenuItem::new(if seen {
            "Mark as unread"
        } else {
            "Mark as read"
        })
        .on_select(act(if seen {
            |s, a, b, uid| s.mark_seen((a.to_owned(), b.to_owned(), uid), false)
        } else {
            |s, a, b, uid| s.mark_seen((a.to_owned(), b.to_owned(), uid), true)
        })),
    )
    .item(
        MenuItem::new(if m.envelope.flagged {
            "Remove star"
        } else {
            "Star"
        })
        .on_select(act(if m.envelope.flagged {
            |s, a, b, uid| s.star((a.to_owned(), b.to_owned(), uid), false)
        } else {
            |s, a, b, uid| s.star((a.to_owned(), b.to_owned(), uid), true)
        })),
    )
    .separator()
    .item(MenuItem::new("Reply").on_select(act(|s, a, b, uid| s.reply_to(a, b, uid, false))))
    .item(MenuItem::new("Reply all").on_select(act(|s, a, b, uid| s.reply_to(a, b, uid, true))))
    .item(MenuItem::new("Forward").on_select(act(|s, a, b, uid| {
        s.answer_message(a, b, uid, Answer::Forward);
    })))
    .separator()
    .item(MenuItem::new("Export as .eml…").on_select_tx({
        let message = m.clone();
        move |s: &mut Mail, tx| {
            s.close_message_menu();
            Mail::export_message(tx, &message);
        }
    }));
    items = message_menu_moves(s, m, menu, items);
    let before = layer.len();
    let built = items.build(k, layer).w_full();
    instant_since(layer, before);
    built
}

/// The end of a message's menu: snoozing it, and moving it to another of its account's mailboxes.
fn message_menu_moves(s: &Mail, m: &StoredMessage, menu: &MessageMenu, mut items: Menu) -> Menu {
    let (account, mailbox, uid) = (m.account.as_str(), m.mailbox.as_str(), m.envelope.uid);
    let move_to = |to: &str| {
        let (account, mailbox, to) = (account.to_owned(), mailbox.to_owned(), to.to_owned());
        move |s: &mut Mail| {
            s.close_message_menu();
            s.move_message(&account, &mailbox, uid, &to);
        }
    };
    let elsewhere = |role| {
        s.mailbox_with_role(account, role)
            .filter(|b| b.name != mailbox)
    };
    let archive = elsewhere(Role::Archive);
    let junk = elsewhere(Role::Junk);
    // Spam goes back to the inbox.
    let inbox = s
        .mailbox_with_role(account, Role::Junk)
        .filter(|b| b.name == mailbox)
        .and_then(|_| elsewhere(Role::Inbox));
    // A snoozed message waits in the Snoozed mailbox; one already there isn't snoozed again.
    let snoozable = s
        .mailbox_with_role(account, Role::Snoozed)
        .is_none_or(|b| b.name != mailbox);
    let destinations: Vec<MenuEntry> = s
        .mailboxes
        .get(account)
        .map_or(&[][..], Vec::as_slice)
        .iter()
        // One being deleted would take what's moved there with it.
        .filter(|b| b.name != mailbox && !s.mailbox_deleting(account, &b.name))
        .map(|b| MenuEntry::item(MenuItem::new(b.label()).on_select(move_to(&b.name))))
        .collect();
    if archive.is_some()
        || junk.is_some()
        || inbox.is_some()
        || !destinations.is_empty()
        || snoozable
    {
        items = items.separator();
    }
    if snoozable {
        let times = postden_app::snooze_times(&ZonedDateTime::now())
            .into_iter()
            .map(|(label, until)| {
                // The message as listed: one a server search found isn't cached.
                let message = m.clone();
                MenuEntry::item(MenuItem::new(label).on_select(move |s: &mut Mail| {
                    s.close_message_menu();
                    s.snooze_message(&message, until);
                }))
            })
            .collect();
        items = items.item(
            MenuItem::new("Snooze")
                .submenu(times, menu.snooze_open)
                .on_select(Mail::toggle_snooze_menu),
        );
    }
    let key = crate::state::message_key(m);
    if let Some(to) = s.suggested(&key) {
        let label = s.mailbox_label(account, to);
        let shown = to.to_owned();
        let suggested = move |s: &mut Mail| {
            s.close_message_menu();
            s.move_as_suggested(&key, &shown);
        };
        items = items.item(MenuItem::new(format!("Move to {label}")).on_select(suggested));
    }
    // One of a batch autosort just moved can go back alone.
    if s.in_last_batch(m) {
        let message = m.clone();
        items = items.item(
            MenuItem::new("Move back to Inbox").on_select(move |s: &mut Mail| {
                s.close_message_menu();
                s.move_back_one(&message);
            }),
        );
    }
    if let Some(archive) = archive {
        items = items.item(MenuItem::new("Archive").on_select(move_to(&archive.name)));
    }
    if let Some(junk) = junk {
        items = items.item(MenuItem::new("Mark as spam").on_select(move_to(&junk.name)));
    }
    if let Some(inbox) = inbox {
        items = items.item(MenuItem::new("Not spam").on_select(move_to(&inbox.name)));
    }
    if !destinations.is_empty() {
        items = items.item(
            MenuItem::new("Move to")
                .submenu(destinations, menu.move_open)
                .on_select(Mail::toggle_move_menu),
        );
    }
    if let Some(trash) = elsewhere(Role::Trash) {
        items = items.separator().item(
            MenuItem::new("Move to Trash")
                .danger(true)
                .on_select(move_to(&trash.name)),
        );
    }
    items
}

/// The list's end, which loads older mail as it comes on screen, and spins while it's fetched.
///
/// It's keyed by how many messages are listed, so once more arrive it's a new row, which reports
/// coming on screen again if the list is still scrolled to its end.
fn older(s: &Mail, k: &Kit) -> El {
    let end = row()
        .key(format!("older-{}", s.messages.len()))
        .w_full()
        .h(40.0)
        .center()
        .on_visible(|s: &mut Mail, on, _| {
            if on {
                s.load_older();
            }
        });
    if s.fetching_older() {
        end.child(
            Spinner::new()
                .size(16.0)
                .color(k.c.icon_muted)
                .build(k)
                .enter_delay(SPINNER_DELAY),
        )
    } else {
        end
    }
}

/// How long the spinner waits before showing, so a message that downloads quickly never flashes it.
const SPINNER_DELAY: f32 = 0.3;

/// The open message: a spinner while it downloads, then its headers and text, which rise in.
/// A message's subject, star, sender and date, then its To and Cc once its text is here.
fn reader_header(r: &Reader, k: &Kit) -> El {
    let e = &r.message.envelope;
    let subject = e.subject.clone().unwrap_or_else(|| "(no subject)".into());
    let mut header = col()
        .gap(6.0)
        .child({
            let subject = text(subject)
                .key("subject")
                .transition(Transition::none())
                .font(SUBJECT)
                .color(k.c.text_base)
                .wrap()
                .selectable(true)
                .grow(1.0)
                .min_w(0.0);
            let star = e.flagged.then(|| {
                icon(&STAR, 18.0)
                    .color(k.c.text_accent)
                    .label("Starred")
                    .mt(4.0)
            });
            row()
                .gap(8.0)
                .children(std::iter::once(subject).chain(star))
        })
        // The date sits beside who it's from, so the To and Cc lines arriving with the text
        // don't move it.
        .child({
            let date = long_date(e.date).map(|date| {
                text(date)
                    .key("date")
                    .transition(Transition::none())
                    .font(k.ty.small)
                    .color(k.c.text_muted)
            });
            row()
                .gap(12.0)
                .w_full()
                .child(
                    header_line(k, "From", std::slice::from_ref(&e.from))
                        .grow(1.0)
                        .min_w(0.0),
                )
                .children(date)
        });
    if let Some(Ok(body)) = &r.body {
        if !body.to.is_empty() {
            header = header.child(
                header_line(k, "To", &body.to)
                    .key("to")
                    .transition(Transition::none()),
            );
        }
        if !body.cc.is_empty() {
            header = header.child(
                header_line(k, "Cc", &body.cc)
                    .key("cc")
                    .transition(Transition::none()),
            );
        }
    }
    header
}

fn message(r: &Reader, k: &Kit) -> El {
    let header = reader_header(r, k);
    // Who it's from and what it's about show at once; only its text waits to download.
    let Some(body) = &r.body else {
        let waiting = row()
            .key("downloading")
            // Gone the frame the text is there, never over it.
            .transition(Transition::none())
            .w_full()
            .py(24.0)
            .center()
            // Only once it's taken a while: a quick download shows none.
            .children(
                r.waited
                    .then(|| Spinner::new().size(20.0).color(k.c.icon_muted).build(k)),
            );
        return reader_frame(r, header, waiting, k);
    };
    let content = match body {
        Err(error) => col()
            .key("open-failed")
            .gap(10.0)
            .child(
                text(format!("Couldn't download this message: {error}"))
                    .font(k.ty.ui)
                    .color(k.c.danger.fg)
                    .wrap(),
            )
            .child(Button::new("Try again").on_press(Mail::retry_open).build(k)),
        Ok(body) => {
            let mut content = col().gap(16.0);
            if !body.attachments.is_empty() {
                content = content.child(
                    text(format!("Attachments: {}", body.attachments.join(", ")))
                        .key("attachments")
                        .transition(Transition::none())
                        .font(k.ty.small)
                        .color(k.c.text_muted)
                        .wrap(),
                );
            }
            if let Some(markdown) = &r.markdown {
                return reader_frame(
                    r,
                    header,
                    content.child(
                        Markdown::new(markdown.clone())
                            .selectable(true)
                            .on_link_tx(|s: &mut Mail, url: &str, tx: &mut Tx| s.open_link(url, tx))
                            .build(k)
                            .key("body")
                            // In full the frame it's there, not faded up from nothing.
                            .transition(Transition::none()),
                    ),
                    k,
                );
            }
            let shown = if body.text.is_empty() {
                text("This message has no text.").color(k.c.text_muted)
            } else {
                text(body.text.clone()).color(k.c.text_base)
            };
            content.child(
                shown
                    .key("body")
                    .transition(Transition::none())
                    .font(k.ty.body)
                    .wrap()
                    .selectable(true),
            )
        }
    };
    reader_frame(r, header, content, k)
}

/// The reader's scrolling column: the headers, a rule, then the content.
fn reader_frame(r: &Reader, header: El, content: El, k: &Kit) -> El {
    col()
        // One per message, so the next one opens at its top.
        .key(format!(
            "reader-{}",
            crate::state::row_key(&crate::state::message_key(&r.message))
        ))
        // Opaque, and in place at once: the list and it never show through each other.
        .transition(Transition::none())
        .fill(k.c.bg_base)
        .w_full()
        .child(
            col()
                .w_full()
                .max_w(760.0)
                .padding(28.0)
                .gap(20.0)
                .child(header)
                .child(el().w_full().h(1.0).fill(k.c.border_weak))
                .child(content),
        )
        .scroll(ScrollSpec {
            stick_bottom: false,
        })
        .grow(1.0)
        .min_h(0.0)
}

/// The message being written: who it's from and to, its subject and text, and Send.
fn compose(s: &Mail, c: &ComposeForm, k: &Kit, layer: &mut Vec<El>) -> El {
    let mut fields = col()
        .w_full()
        .max_w(760.0)
        .padding(24.0)
        .gap(12.0)
        .child(field(k, "From", from_picker(s, c, k, layer)))
        .child(field(
            k,
            "To",
            recipients_input(
                k,
                c,
                Recipients::To,
                "ada@example.com, Bob <bob@example.com>",
                layer,
            ),
        ))
        .child(
            row()
                .gap(12.0)
                .child(field(
                    k,
                    "Cc",
                    recipients_input(k, c, Recipients::Cc, "", layer),
                ))
                .child(field(
                    k,
                    "Bcc",
                    recipients_input(k, c, Recipients::Bcc, "", layer),
                )),
        )
        .child(field(
            k,
            "Subject",
            compose_input(k, c, "Subject", "", &c.subject, |s| &mut s.compose.subject),
        ))
        .child(editor(
            &c.body,
            Rich {
                disabled: c.sending,
                ..COMPOSE
            },
            k,
        ));
    if let Some(signature) = signature_view(c, k) {
        fields = fields.child(signature);
    }
    if let Some(quote) = quote_view(c, k) {
        fields = fields.child(quote);
    }
    fields = fields.child(compatibility(c, k));
    if let Some(error) = &c.error {
        fields = fields.child(
            text(error.clone())
                .key("compose-error")
                .font(k.ty.small)
                .color(k.c.danger.fg)
                .wrap(),
        );
    }
    col()
        .key("compose")
        .w_full()
        .child(fields.child(compose_actions(c, k)))
        .scroll(ScrollSpec {
            stick_bottom: false,
        })
        .grow(1.0)
        .min_h(0.0)
}

/// Who a message is sent from: when there's more than one account, a picker that lists them
/// all, however many, each by name over its address, and finds one by either as it's typed.
fn from_picker(s: &Mail, c: &ComposeForm, k: &Kit, layer: &mut Vec<El>) -> El {
    if s.accounts.len() > 1 {
        let options: Vec<ComboboxOption> = s.accounts.iter().map(from_option).collect();
        Combobox::new("compose-from", &c.from, &options, |s: &mut Mail| {
            &mut s.compose.from
        })
        .label("From")
        .current(Some(&c.account))
        .search_placeholder("Search accounts")
        .empty_message("No account matches")
        .width(320.0)
        .max_height(280.0)
        .disabled(c.sending)
        .on_choose(|s: &mut Mail, id, _| s.compose_from(id))
        .build(k, layer)
    } else {
        text(s.label(&c.account).to_owned())
            .font(k.ty.ui)
            .color(k.c.text_base)
    }
}

/// An account in the From picker: its name, over its address when the name isn't it.
fn from_option(account: &Account) -> ComboboxOption {
    let mut option = ComboboxOption::new(&account.id, account.label());
    if account.label() != account.id {
        option.detail.clone_from(&account.id);
    }
    option
}

fn compose_actions(c: &ComposeForm, k: &Kit) -> El {
    let (label, variant) = if c.sending {
        ("Sending…", ButtonVariant::Loading)
    } else {
        ("Send", ButtonVariant::Contrast)
    };
    row()
        .gap(8.0)
        .center_y()
        .child(
            Button::new(label)
                .variant(variant)
                .disabled(c.sending)
                .shortcut("⌘↩")
                .on_press(Mail::send)
                .build(k),
        )
        .child(
            Button::new("Discard")
                .variant(ButtonVariant::Ghost)
                .on_press(Mail::discard)
                .build(k),
        )
}

/// A list of recipients, filling its column, with the contacts the one being typed could be
/// listed under it, when contacts are remembered.
fn recipients_input(
    k: &Kit,
    c: &ComposeForm,
    recipients: Recipients,
    placeholder: &str,
    layer: &mut Vec<El>,
) -> El {
    let suggested = c.suggested(recipients);
    let active = (!suggested.is_empty()).then_some(c.suggestions.active);
    let items = suggested.iter().enumerate().map(|(i, contact)| {
        let item = PopupItem::new(i.to_string(), contact.label());
        match (&contact.name, &contact.email) {
            (Some(_), Some(email)) => item.description(email.clone()),
            _ => item,
        }
    });
    PopupList::new(recipients.suggestions_key(), recipients.key())
        .anchored(recipients.key(), Side::Bottom, Align::Start, 4.0)
        .open(active.is_some())
        .width(360.0)
        .label("Contacts")
        .items(items)
        .active(active)
        .on_active(move |s: &mut Mail, i| s.compose.suggestions.active = i)
        .on_choose(move |s: &mut Mail, id, _| {
            if let Ok(i) = id.parse() {
                s.compose.choose_suggestion(recipients, i);
            }
        })
        .build(k, layer);
    TextInput::new(c.recipients(recipients))
        .label(recipients.label())
        .placeholder(placeholder)
        .width(None)
        .disabled(c.sending)
        // A screen reader reads the highlighted contact, while the field keeps the focus.
        .active_descendant(active.map(|i| recipients.suggestion_key(i)))
        .on_edit(move |s: &mut Mail| s.compose.recipients_mut(recipients))
        .on_change(move |s: &mut Mail, _| s.suggest(recipients))
        .on_key(move |s: &mut Mail, ev: &rune::KeyEvent, tx| s.suggestion_key(recipients, ev, tx))
        .build(k)
        .key(recipients.key())
}

/// One of the compose form's one-line fields, filling its column.
fn compose_input(
    k: &Kit,
    c: &ComposeForm,
    label: &str,
    placeholder: &str,
    edit: &TextEdit,
    get: fn(&mut Mail) -> &mut TextEdit,
) -> El {
    TextInput::new(edit)
        .label(label)
        .placeholder(placeholder)
        .width(None)
        .disabled(c.sending)
        .on_edit(get)
        .build(k)
}

/// "From: Ada <ada@example.com>", for a header line.
fn header_line(k: &Kit, name: &str, addresses: &[Address]) -> El {
    let list = addresses
        .iter()
        .map(Address::full)
        .collect::<Vec<_>>()
        .join(", ");
    row()
        .gap(6.0)
        .w_full()
        .child(
            text(format!("{name}:"))
                .font(k.ty.small_medium)
                .color(k.c.text_muted),
        )
        .child(
            text(list)
                .font(k.ty.small)
                .color(k.c.text_base)
                .wrap()
                .grow(1.0)
                .min_w(0.0)
                .selectable(true),
        )
}

/// The full date and time, in the local time zone.
fn long_date(unix: Option<i64>) -> Option<String> {
    let at = jiff::Timestamp::from_second(unix?).ok()?;
    Some(
        at.to_zoned_datetime(jiff::tz::TimeZone::system())
            .strftime("%A %-d %B %Y at %H:%M")
            .to_string(),
    )
}

fn form(f: &AccountForm, accounts: &[Account], k: &Kit) -> El {
    let page = match f.step {
        Step::Choose => choose(f, k),
        Step::Google => google_form(f, k),
        Step::Servers => servers_form(f, k),
        Step::Import => import_list(f, accounts, k),
    };
    col()
        .key("form-scroll")
        .w_full()
        .child(page)
        .scroll(ScrollSpec {
            stick_bottom: false,
        })
        .grow(1.0)
        .min_h(0.0)
}

/// One screen of the add-account form. The toolbar names it.
fn form_page(key: &str) -> El {
    col().key(key).w_full().max_w(560.0).padding(24.0).gap(16.0)
}

/// The form's error, if there is one, under its fields.
fn with_error(page: El, f: &AccountForm, k: &Kit) -> El {
    match &f.error {
        Some(error) => page.child(
            text(error.clone())
                .key("form-error")
                .font(k.ty.small)
                .color(k.c.danger.fg)
                .wrap(),
        ),
        None => page,
    }
}

/// The first screen: how to connect the account. Setting it up from another mail app is a
/// choice only when one is on this computer.
fn choose(f: &AccountForm, k: &Kit) -> El {
    let option = |key: &str, title: &str, about: &str, action: &str, go: fn(&mut Mail)| {
        col()
            .key(key)
            .w_full()
            .gap(8.0)
            .padding(16.0)
            .radius(8.0)
            .border(1.0, k.c.border_weak)
            .child(
                text(title.to_owned())
                    .font(k.ty.ui_medium)
                    .color(k.c.text_base),
            )
            .child(
                text(about.to_owned())
                    .font(k.ty.small)
                    .color(k.c.text_muted)
                    .wrap(),
            )
            .child(
                row().child(
                    Button::new(action)
                        .variant(ButtonVariant::Contrast)
                        .on_press(go)
                        .build(k),
                ),
            )
    };
    let mut page = form_page("form-choose");
    if !f.imports.apps.is_empty() {
        let apps = or_list(&f.imports.apps);
        page = page.child(option(
            "choose-import",
            "From another mail app",
            &format!(
                "Pick an account set up in {apps} on this computer, and its servers are filled \
                 in. Its password isn't shared: you type it, or sign in with Google again."
            ),
            "Choose an app",
            Mail::open_imports,
        ));
    }
    page.child(option(
        "choose-google",
        "Google",
        "Gmail and Google Workspace. Sign in in your browser; no app password needed.",
        "Sign in with Google",
        |s| s.form_step(Step::Google),
    ))
    .child(option(
        "choose-servers",
        "Other mail (IMAP and SMTP)",
        "Any provider: fill in its incoming and outgoing servers and your password.",
        "Set up IMAP and SMTP",
        |s| s.form_step(Step::Servers),
    ))
    .child(
        row().child(
            Button::new("Cancel")
                .variant(ButtonVariant::Ghost)
                .on_press(|s: &mut Mail| s.form = AccountForm::default())
                .build(k),
        ),
    )
}

/// `items` as a sentence would list them: "A", "A or B", "A, B or C".
fn or_list(items: &[&str]) -> String {
    match items {
        [] => String::new(),
        [only] => (*only).to_owned(),
        [rest @ .., last] => format!("{} or {last}", rest.join(", ")),
    }
}

/// The other mail apps on this computer, to pick one; or, once picked, the accounts it has.
fn import_list(f: &AccountForm, accounts: &[Account], k: &Kit) -> El {
    f.imports.app.map_or_else(
        || import_apps(f, k),
        |app| app_accounts(f, app, accounts, k),
    )
}

/// The other mail apps on this computer, each with how many accounts it has, as soon as
/// that's read.
fn import_apps(f: &AccountForm, k: &Kit) -> El {
    let mut page = form_page("form-import").child(
        text("Pick the app the account is set up in.".to_owned())
            .font(k.ty.small)
            .color(k.c.text_muted)
            .wrap(),
    );
    for app in &f.imports.apps {
        let app: &'static str = app;
        let summary = match f.imports.of(app) {
            None => "Reading its accounts…".to_owned(),
            Some(Ok(found)) => match found.len() {
                0 => "No mail accounts".to_owned(),
                1 => "1 account".to_owned(),
                n => format!("{n} accounts"),
            },
            Some(Err(Unreadable::NeedsFullDiskAccess)) => "Needs Full Disk Access".to_owned(),
            Some(Err(Unreadable::Failed(_))) => "Couldn't read its accounts".to_owned(),
        };
        page = page.child(
            row()
                .key(format!("import-app-{app}"))
                .w_full()
                .gap(8.0)
                .center_y()
                .padding(12.0)
                .radius(8.0)
                .border(1.0, k.c.border_weak)
                .child(
                    col()
                        .grow(1.0)
                        .min_w(0.0)
                        .child(
                            text(app.to_owned())
                                .font(k.ty.ui_medium)
                                .color(k.c.text_base),
                        )
                        .child(text(summary).font(k.ty.small).color(k.c.text_muted)),
                )
                .child(
                    Button::new("Choose")
                        .variant(ButtonVariant::Neutral)
                        .on_press(move |s: &mut Mail| s.pick_import_app(app))
                        .build(k),
                ),
        );
    }
    page.child(row().child(back(k, false)))
}

/// The accounts `app` has, each with a button to set it up, or why it can't be, or that it's
/// set up already; or why they can't be read.
fn app_accounts(f: &AccountForm, app: &str, accounts: &[Account], k: &Kit) -> El {
    let muted = |s: String| text(s).font(k.ty.small).color(k.c.text_muted).wrap();
    let page = form_page("form-import-app");
    let page = match f.imports.of(app) {
        None => page.child(
            row()
                .gap(8.0)
                .center_y()
                .child(Spinner::new().build(k))
                .child(muted("Reading the accounts…".into())),
        ),
        Some(Err(Unreadable::NeedsFullDiskAccess)) => {
            let page = page
                .child(muted(format!(
                    "macOS keeps {app}'s accounts private: Postden can only read them with Full \
                     Disk Access. Turn it on for Postden in System Settings, adding it with + if \
                     it isn't listed, then come back here."
                )))
                .child(muted(
                    "Postden only reads where the accounts are set up, never your mail.".to_owned(),
                ))
                .child(
                    row()
                        .gap(8.0)
                        .child(
                            Button::new("Open System Settings")
                                .variant(ButtonVariant::Contrast)
                                .on_press(Mail::open_full_disk_access)
                                .build(k),
                        )
                        .child(
                            Button::new("Check again")
                                .variant(ButtonVariant::Neutral)
                                .disabled(f.imports.reading)
                                .on_press(Mail::reread_imports)
                                .build(k),
                        ),
                );
            with_error(page, f, k)
        }
        Some(Err(Unreadable::Failed(e))) => {
            page.child(muted(format!("Couldn't read {app}'s accounts: {e}")))
        }
        Some(Ok(none)) if none.is_empty() => {
            page.child(muted(format!("{app} has no mail accounts set up.")))
        }
        Some(Ok(found)) => found.iter().fold(page, |page, account| {
            page.child(found_row(account, accounts, k))
        }),
    };
    page.child(row().child(back(k, false)))
}

/// One account another app has: its names and address, and setting it up.
fn found_row(found: &Found, accounts: &[Account], k: &Kit) -> El {
    let name = found
        .label
        .as_deref()
        .or(found.display_name.as_deref())
        .unwrap_or(&found.address);
    let mut about = col().grow(1.0).min_w(0.0).child(
        text(name.to_owned())
            .font(k.ty.ui_medium)
            .color(k.c.text_base),
    );
    if name != found.address {
        about = about.child(
            text(found.address.clone())
                .font(k.ty.small)
                .color(k.c.text_muted),
        );
    }
    let set_up = is_set_up(accounts, &found.address);
    let end = match &found.setup {
        _ if set_up => text("Set up already".to_owned())
            .font(k.ty.small)
            .color(k.c.text_muted),
        Setup::Unsupported(why) => {
            about = about.child(
                text(format!("Can't be set up: {why}"))
                    .font(k.ty.small)
                    .color(k.c.text_muted)
                    .wrap(),
            );
            col()
        }
        setup => {
            let address = found.address.clone();
            let action = if *setup == Setup::Google {
                "Sign in with Google"
            } else {
                "Set up"
            };
            Button::new(action)
                .variant(ButtonVariant::Neutral)
                .on_press(move |s: &mut Mail| s.set_up_found(&address))
                .build(k)
        }
    };
    row()
        .key(format!("found-{}", found.address))
        .w_full()
        .gap(8.0)
        .center_y()
        .padding(12.0)
        .radius(8.0)
        .border(1.0, k.c.border_weak)
        .child(about)
        .child(end)
}

/// Signing in with Google, or the sign-in under way.
fn google_form(f: &AccountForm, k: &Kit) -> El {
    let mut page = form_page("form-google")
        .child(
            text(
                "Google's sign-in page opens in your browser. Postden only gets access to \
                 your mail, and keeps the sign-in in your keyring."
                    .to_owned(),
            )
            .font(k.ty.small)
            .color(k.c.text_muted)
            .wrap(),
        )
        .child(
            row()
                .gap(12.0)
                .child(field(
                    k,
                    "Gmail address (optional)",
                    input(f, "Gmail address", "you@gmail.com", &f.email, |s| {
                        &mut s.form.email
                    })
                    .build(k),
                ))
                .child(field(
                    k,
                    "Your name (optional)",
                    input(f, "Your name", "Ada Lovelace", &f.name, |s| {
                        &mut s.form.name
                    })
                    .build(k),
                )),
        );
    if let Some(url) = f.google.as_ref().and_then(|g| g.url.as_ref()) {
        page = page.child(
            Markdown::new(format!(
                "Finish signing in in your browser. If it didn't open, \
                 [open Google's sign-in page]({url})."
            ))
            .style(
                MarkdownStyle::from_kit(k)
                    .body_font(k.ty.small)
                    .text(k.c.text_muted),
            )
            .on_link_tx(|s: &mut Mail, url: &str, tx: &mut Tx| s.open_link(url, tx))
            .build(k)
            .key("google-url"),
        );
    }
    let actions = if f.google.is_some() {
        row()
            .gap(8.0)
            .child(
                Button::new("Waiting for Google…")
                    .variant(ButtonVariant::Loading)
                    .disabled(true)
                    .build(k),
            )
            .child(
                Button::new("Cancel")
                    .variant(ButtonVariant::Ghost)
                    .on_press(Mail::cancel_sign_in)
                    .build(k),
            )
    } else {
        row()
            .gap(8.0)
            .child(
                Button::new("Sign in with Google")
                    .variant(ButtonVariant::Contrast)
                    .on_press_tx(Mail::submit_form)
                    .build(k),
            )
            .child(back(k, false))
    };
    with_error(page, f, k).child(actions)
}

/// Typing in IMAP and SMTP servers and a password.
fn servers_form(f: &AccountForm, k: &Kit) -> El {
    let imap_port = f.imap_security.default_imap_port().to_string();
    let smtp_port = f.smtp_security.default_smtp_port().to_string();
    let smtp_host = f.smtp_guess().unwrap_or_else(|| "smtp.example.com".into());
    let fields = form_page("form")
        .child(
            row()
                .gap(12.0)
                .child(field(
                    k,
                    "Email",
                    input(f, "Email", "you@example.com", &f.email, |s| {
                        &mut s.form.email
                    })
                    .build(k),
                ))
                .child(field(
                    k,
                    "Your name (optional)",
                    input(f, "Your name", "Ada Lovelace", &f.name, |s| {
                        &mut s.form.name
                    })
                    .build(k),
                )),
        )
        .child(
            row()
                .gap(12.0)
                .child(field(
                    k,
                    "Password or app password",
                    input(f, "Password", "", &f.password, |s| &mut s.form.password)
                        .secure()
                        .build(k),
                ))
                .child(field(
                    k,
                    "User name (if not the email)",
                    input(f, "User name", "", &f.username, |s| &mut s.form.username).build(k),
                )),
        )
        .child(server(
            k,
            "Incoming mail (IMAP)",
            input(f, "IMAP server", "imap.example.com", &f.imap_host, |s| {
                &mut s.form.imap_host
            })
            .build(k),
            input(f, "IMAP port", &imap_port, &f.imap_port, |s| {
                &mut s.form.imap_port
            })
            .build(k),
            security_picker(
                k,
                "Incoming mail (IMAP) security",
                f.imap_security,
                f.checking,
                |s, security| s.form.imap_security = security,
            ),
            None,
        ))
        .child(server(
            k,
            "Outgoing mail (SMTP)",
            input(f, "SMTP server", &smtp_host, &f.smtp_host, |s| {
                &mut s.form.smtp_host
            })
            .build(k),
            input(f, "SMTP port", &smtp_port, &f.smtp_port, |s| {
                &mut s.form.smtp_port
            })
            .build(k),
            security_picker(
                k,
                "Outgoing mail (SMTP) security",
                f.smtp_security,
                f.checking,
                |s, security| s.form.smtp_security = security,
            ),
            Some(smtp_login(f, k)),
        ));
    with_error(fields, f, k).child(form_actions(f, k))
}

/// Back to choosing how to connect.
fn back(k: &Kit, disabled: bool) -> El {
    Button::new("Back")
        .variant(ButtonVariant::Ghost)
        .disabled(disabled)
        .on_press(Mail::form_back)
        .build(k)
}

/// The SMTP user name and password, for servers that want different ones than IMAP.
fn smtp_login(f: &AccountForm, k: &Kit) -> El {
    row()
        .gap(12.0)
        .child(field(
            k,
            "User name (if different)",
            input(f, "SMTP user name", "", &f.smtp_username, |s| {
                &mut s.form.smtp_username
            })
            .build(k),
        ))
        .child(field(
            k,
            "Password (if different)",
            input(f, "SMTP password", "", &f.smtp_password, |s| {
                &mut s.form.smtp_password
            })
            .secure()
            .build(k),
        ))
}

fn form_actions(f: &AccountForm, k: &Kit) -> El {
    let (label, variant) = if f.checking {
        ("Checking the login…", ButtonVariant::Loading)
    } else {
        ("Add account", ButtonVariant::Contrast)
    };
    row()
        .gap(8.0)
        .child(
            Button::new(label)
                .variant(variant)
                .disabled(f.checking)
                .on_press_tx(Mail::submit_form)
                .build(k),
        )
        .child(back(k, f.checking))
}

fn server(k: &Kit, title: &str, host: El, port: El, security: El, login: Option<El>) -> El {
    let mut section = col()
        .gap(8.0)
        .child(
            text(title.to_owned())
                .font(k.ty.ui_medium)
                .color(k.c.text_base),
        )
        .child(
            row()
                .gap(12.0)
                .child(field(k, "Server", host).grow(3.0))
                .child(field(k, "Port", port).grow(1.0)),
        )
        .child(security);
    if let Some(login) = login {
        section = section.child(login);
    }
    section
}

/// The choice of how a server's connection is secured, which `set` changes.
fn security_picker(
    k: &Kit,
    label: &str,
    security: Security,
    disabled: bool,
    set: fn(&mut Mail, Security),
) -> El {
    let labels: Vec<&str> = SECURITY.iter().map(|(label, _)| *label).collect();
    let mut picker = SegmentedControl::new(&labels, Some(security_index(security)));
    if disabled {
        for i in 0..SECURITY.len() {
            picker = picker.segment_state(i, State::Disabled);
        }
    }
    picker
        .label(label)
        .fit()
        .on_pick(move |s: &mut Mail, i, _| {
            if let Some((_, security)) = SECURITY.get(i) {
                set(s, *security);
            }
        })
        .build(k)
}

/// One of the form's text fields, filling its column. `label` is its accessible name.
fn input(
    f: &AccountForm,
    label: &str,
    placeholder: &str,
    edit: &TextEdit,
    get: fn(&mut Mail) -> &mut TextEdit,
) -> TextInput {
    TextInput::new(edit)
        .label(label)
        .placeholder(placeholder)
        .width(None)
        .disabled(f.checking)
        .on_edit(get)
        .on_submit(Mail::submit_form)
}

/// A visible label over a control.
fn field(k: &Kit, label: &str, input: El) -> El {
    col()
        .gap(5.0)
        .grow(1.0)
        .min_w(0.0)
        .child(
            text(label.to_owned())
                .font(k.ty.small_medium)
                .color(k.c.text_muted),
        )
        .child(input)
}

/// The settings, in sections picked with the tabs at the page's top: General (the theme,
/// notifications, closing the window and updates), Accounts (each editable and removable, and
/// moving them to another computer) and Advanced (how often mail is checked and how many
/// messages are downloaded ahead).
fn settings(s: &Mail, k: &Kit, layer: &mut Vec<El>) -> El {
    let labels: Vec<&str> = SettingsSection::ALL.iter().map(|t| t.label()).collect();
    let mut tabs = SegmentedControl::new(
        &labels,
        SettingsSection::ALL
            .iter()
            .position(|t| *t == s.settings_section),
    );
    for (i, section) in SettingsSection::ALL.iter().enumerate() {
        if !s.can_show_settings_section(*section) {
            tabs = tabs.segment_state(i, State::Disabled);
        }
    }
    let tabs = tabs
        .label("Settings section")
        .fit()
        .on_pick(|s: &mut Mail, i, _| {
            if let Some(section) = SettingsSection::ALL.get(i) {
                s.show_settings_section(*section);
            }
        })
        .build(k)
        .key("settings-sections");
    let mut page = col()
        .key("settings")
        // Swapped with the list at once, never faded through it.
        .transition(Transition::none())
        .w_full()
        .max_w(560.0)
        .padding(24.0)
        .gap(24.0)
        .child(row().child(tabs));
    page = match s.settings_section {
        SettingsSection::General => general_settings(s, k, page, layer),
        SettingsSection::Accounts => page
            .child(settings_accounts(s, k))
            .child(transfer(&s.transfer, k)),
        SettingsSection::Advanced => advanced_settings(s, k, page),
    };
    col()
        .key("settings-scroll")
        // Swapped with the list at once, never faded through it.
        .transition(Transition::none())
        .w_full()
        .child(page)
        .scroll(ScrollSpec {
            stick_bottom: false,
        })
        .grow(1.0)
        .min_h(0.0)
}

/// The settings' General section: the theme, notifications, closing the window and updates.
fn general_settings(s: &Mail, k: &Kit, page: El, layer: &mut Vec<El>) -> El {
    let themes: Vec<&str> = Theme::ALL.iter().map(|t| t.label()).collect();
    let theme = SegmentedControl::new(
        &themes,
        Theme::ALL.iter().position(|t| *t == s.settings.theme),
    )
    .label("Theme")
    .fit()
    .on_pick(|s: &mut Mail, i, _| {
        if let Some(theme) = Theme::ALL.get(i) {
            s.set_theme(*theme);
        }
    })
    .build(k)
    .key("theme");
    page.child(setting_section(k, "Appearance", None, row().child(theme)))
        .child(setting_section(
            k,
            "Sorting mail",
            Some(
                "Learned on this computer from where you file mail, what you keep, star or \
                 delete, and who you write to. What sorting learns stays on this computer.",
            ),
            sorting_settings(s, k, layer),
        ))
        .child(setting_section(
            k,
            "Notifications",
            Some(
                "Postden tells you about new mail in your inboxes while its window isn't in \
                 front: who sent it and its subject, or how many messages came.",
            ),
            labeled_switch(
                "Notify me of new mail",
                Switch::new(s.settings.notify_new_mail)
                    .on_toggle(|s: &mut Mail| s.set_notify_new_mail(!s.settings.notify_new_mail)),
                "notify-new-mail",
                k,
            ),
        ))
        .child(setting_section(
            k,
            "Closing the window",
            Some(CLOSING_ABOUT),
            labeled_switch(
                "Keep running in the background",
                Switch::new(s.settings.keep_running)
                    .on_toggle(|s: &mut Mail| s.set_keep_running(!s.settings.keep_running)),
                "keep-running",
                k,
            ),
        ))
        .child(setting_section(
            k,
            "Sidebar",
            Some(
                "Keep each account's mailboxes with nothing unread in a Read group under it, \
                 closed until you open it, so the ones with new mail stand out. The inbox always \
                 stays listed, and the mailbox you're looking at stays where it is.",
            ),
            Switch::new(s.settings.group_read_mailboxes)
                .label("Group read mailboxes")
                .on_toggle(|s: &mut Mail| {
                    s.set_group_read_mailboxes(!s.settings.group_read_mailboxes);
                })
                .build(k)
                .key("group-read-mailboxes"),
        ))
        .child(contacts_setting(s, k))
        .child(setting_section(
            k,
            "Updates",
            Some(
                "Postden asks GitHub for new releases when it starts and once a day, and \
                 shows what changed before you update.",
            ),
            updates(s, k, layer),
        ))
}

/// The settings' Advanced section: how often mail is checked, and how many messages are
/// downloaded ahead.
fn advanced_settings(s: &Mail, k: &Kit, page: El) -> El {
    let intervals: Vec<String> = POLL_INTERVALS.iter().map(|d| interval_label(*d)).collect();
    let interval = SegmentedControl::new(
        &intervals,
        POLL_INTERVALS
            .iter()
            .position(|d| *d == s.settings.poll_interval),
    )
    .label("Check for new mail every")
    .fit()
    .on_pick(|s: &mut Mail, i, _| {
        if let Some(every) = POLL_INTERVALS.get(i) {
            s.set_poll_interval(*every);
        }
    })
    .build(k)
    .key("poll-interval");
    page.child(setting_section(
        k,
        "Checking for mail",
        Some(
            "Only for servers that can't tell Postden about new mail as it arrives (IMAP \
             IDLE). Changing it reconnects every account.",
        ),
        row().child(interval),
    ))
    .child(prefetch_setting(s, k))
}

/// Every account set up, with buttons to edit or remove it.
fn settings_accounts(s: &Mail, k: &Kit) -> El {
    let mut accounts = col().w_full().gap(8.0);
    if s.accounts.is_empty() {
        accounts = accounts.child(
            text("No accounts yet.")
                .font(k.ty.small)
                .color(k.c.text_muted),
        );
    }
    for account in &s.accounts {
        accounts = accounts.child(settings_account(s, account, k));
    }
    accounts
}

/// How many of the newest messages of each inbox, and each mailbox chosen, are downloaded ahead
/// of their opening, whether
/// opened ones stay downloaded, and a button to drop all of them.
fn prefetch_setting(s: &Mail, k: &Kit) -> El {
    let counts: Vec<String> = PREFETCH_COUNTS
        .iter()
        .map(|&n| {
            if n == 0 {
                "Off".to_owned()
            } else {
                n.to_string()
            }
        })
        .collect();
    let prefetch = SegmentedControl::new(
        &counts,
        PREFETCH_COUNTS
            .iter()
            .position(|&n| n == s.settings.prefetch),
    )
    .label("Newest messages per mailbox")
    .fit()
    .on_pick(|s: &mut Mail, i, _| {
        if let Some(&count) = PREFETCH_COUNTS.get(i) {
            s.set_prefetch(count);
        }
    })
    .build(k)
    .key("prefetch");
    let keep_opened = labeled_switch(
        "Keep opened messages downloaded",
        Switch::new(s.settings.keep_opened)
            .on_toggle(|s: &mut Mail| s.set_keep_opened(!s.settings.keep_opened)),
        "keep-opened",
        k,
    );
    let clear = Button::new("Clear downloaded messages")
        .on_press(Mail::clear_downloaded)
        .build(k)
        .key("clear-downloaded");
    setting_section(
        k,
        "Downloading ahead",
        Some(
            "Postden can download your newest messages as they arrive, so they open without \
             waiting for the server: those of each inbox, and of the mailboxes you choose by \
             right-clicking one and choosing Download ahead here. Off, a message is downloaded \
             only once you open it. \
             Messages over 5 MB are always downloaded when opened. Kept, a message you opened \
             opens again without waiting, in any mailbox. Clearing drops every downloaded \
             message; the newest are downloaded ahead again.",
        ),
        col()
            .gap(12.0)
            .child(row().child(prefetch))
            .child(keep_opened)
            .child(row().child(clear)),
    )
}

/// Whether the people mail is sent to are remembered, to suggest them as recipients, and a
/// button to forget them.
fn contacts_setting(s: &Mail, k: &Kit) -> El {
    let remember = labeled_switch(
        "Remember who I write to",
        Switch::new(s.settings.remember_contacts)
            .on_toggle(|s: &mut Mail| s.set_remember_contacts(!s.settings.remember_contacts)),
        "remember-contacts",
        k,
    );
    let forget = Button::new("Forget contacts")
        .on_press(Mail::forget_contacts)
        .build(k)
        .key("forget-contacts");
    setting_section(
        k,
        "Contacts",
        Some(
            "Postden can remember the addresses you send mail to, on this computer only, and \
             suggest them as you type a recipient. Turned off, it stops remembering and \
             suggesting, and keeps those it has until you forget them.",
        ),
        col().gap(12.0).child(remember).child(row().child(forget)),
    )
}

/// What keeping the app running does, said for where the window comes back from.
const CLOSING_ABOUT: &str = if cfg!(target_os = "macos") {
    "Postden keeps checking mail with its window closed, and shows what's unread on its Dock \
     icon. Click the Dock icon to bring the window back, and quit with ⌘Q."
} else {
    "Postden keeps checking mail with its window closed. Click its tray icon to bring the \
     window back, and quit from the tray icon's menu. Without a tray, closing the window quits."
};

/// A titled group of settings, with an optional explanation under its control.
fn setting_section(k: &Kit, title: &str, about: Option<&str>, control: El) -> El {
    let mut section = col().w_full().gap(8.0).child(
        text(title.to_owned())
            .font(k.ty.ui_medium)
            .color(k.c.text_base),
    );
    section = section.child(control);
    if let Some(about) = about {
        section = section.child(
            text(about.to_owned())
                .font(k.ty.small)
                .color(k.c.text_muted)
                .wrap(),
        );
    }
    section
}

/// An account in the settings, with its Edit button, and its Remove button, which asks for a
/// second press.
fn settings_account(s: &Mail, account: &Account, k: &Kit) -> El {
    let confirming = s.confirm_remove.as_deref() == Some(account.id.as_str());
    let (edit, remove) = (account.id.clone(), account.id.clone());
    let mut actions = row().gap(8.0).center_y();
    if !confirming {
        actions = actions.child(
            Button::new("Edit")
                .variant(ButtonVariant::Neutral)
                .help("Label, name, signature and servers")
                .on_press(move |s: &mut Mail| s.open_account_settings(&edit))
                .build(k)
                .key(format!("edit-account-{}", account.id)),
        );
    }
    if confirming {
        actions = actions.child(
            Button::new("Cancel")
                .variant(ButtonVariant::Ghost)
                .on_press(|s: &mut Mail| s.confirm_remove = None)
                .build(k),
        );
    }
    actions = actions.child(
        Button::new(if confirming {
            "Remove for good"
        } else {
            "Remove"
        })
        .variant(if confirming {
            ButtonVariant::Danger
        } else {
            ButtonVariant::Neutral
        })
        .on_press(move |s: &mut Mail| s.remove(&remove))
        .build(k),
    );
    let subtitle = if confirming {
        text(
            "Its mail is deleted from this computer, and its password or sign-in forgotten. \
              Nothing changes on the server.",
        )
        .color(k.c.danger.fg)
        .wrap()
    } else {
        text(account.id.clone()).color(k.c.text_muted).truncate()
    };
    row()
        .key(format!("settings-account-{}", account.id))
        .w_full()
        .gap(12.0)
        .padding(12.0)
        .radius(8.0)
        .border(1.0, k.c.border_weak)
        .center_y()
        .child(avatar(account.label(), k))
        .child(
            col()
                .gap(2.0)
                .grow(1.0)
                .min_w(0.0)
                .child(
                    text(account.label().to_owned())
                        .font(k.ty.ui_medium)
                        .color(k.c.text_base)
                        .truncate(),
                )
                .child(subtitle.font(k.ty.small)),
        )
        .child(actions)
}

/// An account's settings: what it's called, the name its mail goes under, and its signature.
fn account_page(s: &Mail, p: &AccountSettings, k: &Kit, layer: &mut Vec<El>) -> El {
    let Some(account) = s.accounts.iter().find(|a| a.id == p.account) else {
        return settings(s, k, layer);
    };
    let names_about = format!(
        "The label names the account in Postden only, such as Work; without one, it's \
         your name or address. Your name goes on the mail you send, beside {}.",
        account.id
    );
    let names = account_names(account, p, k);
    let signature = account_signature(p, k);
    let signature_about = format!(
        "Added below each message you send from {}, above the quote in a reply. Its images go \
         along with each message, so the people you write to see them without loading anything \
         from the web.",
        account.label()
    );
    let mut page = col()
        .key("account-settings")
        .w_full()
        .max_w(640.0)
        .padding(24.0)
        .gap(24.0)
        .child(setting_section(k, "Name", Some(&names_about), names))
        .child(setting_section(
            k,
            "Signature",
            Some(&signature_about),
            signature,
        ));
    if p.edits_servers() {
        let read_only = s.services.as_ref().is_ok_and(|s| s.read_only);
        let about = if read_only {
            "Where its mail is read from and sent through. Changes are saved once both \
             servers accept the login. Passwords come from POSTDEN_PASSWORD_COMMAND."
        } else {
            "Where its mail is read from and sent through. Changes are saved once both \
             servers accept the login. Leave a password blank to keep the one saved."
        };
        page = page.child(setting_section(
            k,
            "Servers",
            Some(about),
            account_servers(account, &p.servers, p.checking.is_some(), read_only, k),
        ));
    }
    if let Some(error) = &p.error {
        page = page.child(
            text(error.clone())
                .key("account-settings-error")
                .font(k.ty.small)
                .color(k.c.danger.fg)
                .wrap(),
        );
    }
    let changed = p.is_changed();
    let (save, variant) = if p.checking.is_some() {
        ("Checking the login…", ButtonVariant::Loading)
    } else {
        ("Save", ButtonVariant::Contrast)
    };
    page = page.child(
        row()
            .gap(8.0)
            .center_y()
            .child(
                Button::new(save)
                    .variant(variant)
                    .disabled(!changed || p.checking.is_some())
                    .on_press(Mail::save_account_settings)
                    .build(k)
                    .key("save-account"),
            )
            .child(
                Button::new(if changed { "Discard changes" } else { "Done" })
                    .variant(ButtonVariant::Ghost)
                    .on_press(Mail::close_account_settings)
                    .build(k)
                    .key("close-account"),
            ),
    );
    col()
        .key("account-settings-scroll")
        .w_full()
        .child(page)
        .scroll(ScrollSpec {
            stick_bottom: false,
        })
        .grow(1.0)
        .min_h(0.0)
}

/// The account's label and the name its mail goes under, side by side.
fn account_names(account: &Account, p: &AccountSettings, k: &Kit) -> El {
    let label_placeholder = account
        .display_name
        .clone()
        .unwrap_or_else(|| account.id.clone());
    let save = |s: &mut Mail, _: &mut Tx| s.save_account_settings();
    row()
        .gap(12.0)
        .child(field(
            k,
            "Label",
            TextInput::new(&p.label)
                .label("Label")
                .placeholder(&label_placeholder)
                .width(None)
                .on_edit(|s: &mut Mail| &mut s.account_settings.label)
                .on_submit(save)
                .build(k)
                .key("account-label"),
        ))
        .child(field(
            k,
            "Your name",
            TextInput::new(&p.name)
                .label("Your name")
                .placeholder("Ada Lovelace")
                .width(None)
                .on_edit(|s: &mut Mail| &mut s.account_settings.name)
                .on_submit(save)
                .build(k)
                .key("account-name"),
        ))
}

/// One of the servers section's text fields, filling its column.
fn server_input(
    label: &str,
    placeholder: &str,
    edit: &TextEdit,
    checking: bool,
    get: fn(&mut Mail) -> &mut TextEdit,
) -> TextInput {
    TextInput::new(edit)
        .label(label)
        .placeholder(placeholder)
        .width(None)
        .disabled(checking)
        .on_edit(get)
        .on_submit(|s: &mut Mail, _: &mut Tx| s.save_account_settings())
}

/// Changes the account's servers with `edit`, unless they're being checked: what's saved then
/// is what was checked.
fn edit_servers(s: &mut Mail, edit: impl FnOnce(&mut Servers)) {
    if s.account_settings.checking.is_none() {
        edit(&mut s.account_settings.servers);
    }
}

/// The account's IMAP and SMTP servers, and the user names and passwords for them.
fn account_servers(account: &Account, p: &Servers, checking: bool, read_only: bool, k: &Kit) -> El {
    let (login, smtp_login) = account_logins(account, p, checking, read_only, k);
    let imap_port = p.imap_security.default_imap_port().to_string();
    let smtp_port = p.smtp_security.default_smtp_port().to_string();
    col()
        .key("account-servers")
        .w_full()
        .gap(16.0)
        .child(login)
        .child(server(
            k,
            "Incoming mail (IMAP)",
            server_input(
                "IMAP server",
                "imap.example.com",
                &p.imap_host,
                checking,
                |s| &mut s.account_settings.servers.imap_host,
            )
            .build(k)
            .key("account-imap-host"),
            server_input("IMAP port", &imap_port, &p.imap_port, checking, |s| {
                &mut s.account_settings.servers.imap_port
            })
            .build(k)
            .key("account-imap-port"),
            security_picker(
                k,
                "Incoming mail (IMAP) security",
                p.imap_security,
                checking,
                |s, security| edit_servers(s, |p| p.imap_security = security),
            ),
            None,
        ))
        .child(server(
            k,
            "Outgoing mail (SMTP)",
            server_input(
                "SMTP server",
                "smtp.example.com",
                &p.smtp_host,
                checking,
                |s| &mut s.account_settings.servers.smtp_host,
            )
            .build(k)
            .key("account-smtp-host"),
            server_input("SMTP port", &smtp_port, &p.smtp_port, checking, |s| {
                &mut s.account_settings.servers.smtp_port
            })
            .build(k)
            .key("account-smtp-port"),
            security_picker(
                k,
                "Outgoing mail (SMTP) security",
                p.smtp_security,
                checking,
                |s, security| edit_servers(s, |p| p.smtp_security = security),
            ),
            Some(smtp_login),
        ))
}

/// The user name and new password for both servers, then those for SMTP if they differ.
/// Passwords that come from a command aren't asked for.
fn account_logins(
    account: &Account,
    p: &Servers,
    checking: bool,
    read_only: bool,
    k: &Kit,
) -> (El, El) {
    let keep = "Unchanged";
    let mut login = row().gap(12.0);
    if !read_only {
        login = login.child(field(
            k,
            "New password",
            server_input("New password", keep, &p.password, checking, |s| {
                &mut s.account_settings.servers.password
            })
            .secure()
            .build(k)
            .key("account-password"),
        ));
    }
    login = login.child(field(
        k,
        "User name (if not the email)",
        server_input("User name", &account.id, &p.username, checking, |s| {
            &mut s.account_settings.servers.username
        })
        .build(k)
        .key("account-username"),
    ));
    let mut smtp_login = row().gap(12.0).child(field(
        k,
        "User name (if different)",
        server_input("SMTP user name", "", &p.smtp_username, checking, |s| {
            &mut s.account_settings.servers.smtp_username
        })
        .build(k),
    ));
    if !read_only {
        smtp_login = smtp_login.child(field(
            k,
            "New password (if different)",
            server_input("New SMTP password", keep, &p.smtp_password, checking, |s| {
                &mut s.account_settings.servers.smtp_password
            })
            .secure()
            .build(k),
        ));
    }
    (login, smtp_login)
}

/// The signature's editor, its images, and how it will look.
fn account_signature(p: &AccountSettings, k: &Kit) -> El {
    let mut signature = col()
        .w_full()
        .gap(12.0)
        .child(editor(&p.signature, SIGNATURE, k))
        .child(signature_images(p, k));
    if let Some(preview) = signature_markdown(&p.preview, k) {
        signature = signature.child(
            col()
                .key("signature-preview")
                .w_full()
                .gap(6.0)
                .child(
                    text("Preview")
                        .font(k.ty.small_medium)
                        .color(k.c.text_muted),
                )
                .child(
                    col()
                        .w_full()
                        .padding(12.0)
                        .radius(8.0)
                        .border(1.0, k.c.border_weak)
                        .child(preview),
                ),
        );
    }
    signature
}

/// The signature's images, each with what can be done with it, and a button to add more.
/// Image files can also be dropped on it.
fn signature_images(p: &AccountSettings, k: &Kit) -> El {
    let hint = if p.images.is_empty() {
        "PNG, JPEG or GIF, up to 1 MB each. Each goes where the caret is: put it on a line of \
         its own to set it between two lines. Drop files here, or add them."
    } else {
        "Insert puts an image where the caret is in the signature, again or elsewhere."
    };
    let shown = p.shown();
    let mut list = col()
        .key("signature-images")
        .w_full()
        .gap(8.0)
        .padding(12.0)
        .radius(8.0)
        .border(
            1.0,
            if p.dropping {
                k.c.border_focus
            } else {
                k.c.border_weak
            },
        )
        .on_drop_files(|s: &mut Mail, files, _| s.add_signature_images(files))
        .accepts_extensions(&IMAGE_EXTENSIONS)
        .on_drop_hover(|s: &mut Mail, over, _| s.account_settings.dropping = over)
        .child(
            row()
                .w_full()
                .gap(10.0)
                .center_y()
                .child(text("Images").font(k.ty.small_medium).color(k.c.text_muted))
                .child(
                    text(hint)
                        .font(k.ty.small)
                        .color(k.c.text_faint)
                        .wrap()
                        .grow(1.0)
                        .min_w(0.0),
                )
                .child(
                    Button::new("Add image…")
                        .variant(ButtonVariant::Neutral)
                        .on_press_tx(|_: &mut Mail, tx| Mail::pick_signature_image(tx))
                        .build(k)
                        .key("add-signature-image"),
                ),
        );
    for image in &p.images {
        let cid = image.cid.clone();
        let button = |label: &str, disabled: bool, act: fn(&mut AccountSettings, &str)| {
            let cid = cid.clone();
            Button::new(label)
                .variant(ButtonVariant::Ghost)
                .disabled(disabled)
                .on_press(move |s: &mut Mail| s.edit_signature(|p| act(p, &cid)))
                .build(k)
        };
        let size = postden_app::signature::size(&image.data)
            .map(|(w, h)| format!(" · {w}×{h}"))
            .unwrap_or_default();
        let unused = if shown.contains(&image.cid) {
            ""
        } else {
            " · not in the signature"
        };
        let mut line = row()
            .key(format!("signature-image-{}", image.cid))
            .w_full()
            .gap(10.0)
            .center_y()
            .child(thumbnail(p, image, k))
            .child(
                col()
                    .gap(2.0)
                    .grow(1.0)
                    .min_w(0.0)
                    .child(
                        text(image.name.clone())
                            .font(k.ty.ui)
                            .color(k.c.text_base)
                            .truncate(),
                    )
                    .child(
                        text(format!(
                            "{} KB{size}{unused}",
                            image.data.len().div_ceil(1024)
                        ))
                        .font(k.ty.small)
                        .color(k.c.text_muted),
                    ),
            );
        line = line
            .child(button("Insert", false, AccountSettings::insert_image))
            .child(button("Remove", false, AccountSettings::remove_image));
        list = list.child(line);
    }
    list
}

/// A signature image, fitted into a small box.
fn thumbnail(p: &AccountSettings, image: &postden_core::message::InlineImage, k: &Kit) -> El {
    const BOX: (f32, f32) = (56.0, 36.0);
    let frame = row()
        .size(BOX.0, BOX.1)
        .radius(4.0)
        .center()
        .fill(k.c.surface_weak);
    let picture = p.preview.pictures.get(&format!("cid:{}", image.cid));
    let size = postden_app::signature::size(&image.data);
    let (Some(picture), Some((w, h))) = (picture, size) else {
        return frame;
    };
    let (w, h) = (
        f32::from(u16::try_from(w).unwrap_or(u16::MAX)),
        f32::from(u16::try_from(h).unwrap_or(u16::MAX)),
    );
    let scale = (BOX.0 / w).min(BOX.1 / h).min(1.0);
    frame.child(el().size(w * scale, h * scale).pixels(picture.pixels()))
}

/// A signature as it will look, its images drawn at the width they're sent at. `None` when it's
/// blank.
fn signature_markdown(preview: &Preview, k: &Kit) -> Option<El> {
    if preview.markdown.trim().is_empty() {
        return None;
    }
    let (pictures, widths) = (preview.pictures.clone(), preview.widths.clone());
    Some(
        Markdown::new(preview.markdown.clone())
            .images(move |src| pictures.get(src).cloned())
            .image_frame(move |e, frame| match widths.get(frame.src) {
                Some(width) => e.max_w(*width),
                None => e,
            })
            .selectable(true)
            .build(k),
    )
}

/// The signature below the message being written, and a button to leave it out.
fn signature_view(c: &ComposeForm, k: &Kit) -> Option<El> {
    c.signature.as_ref()?;
    let head = row()
        .w_full()
        .gap(8.0)
        .center_y()
        .child(
            text("Signature")
                .font(k.ty.small)
                .color(k.c.text_muted)
                .grow(1.0)
                .min_w(0.0),
        )
        .child(
            Button::new("Remove signature")
                .variant(ButtonVariant::Ghost)
                .disabled(c.sending)
                .on_press(|s: &mut Mail| s.compose.set_signature(None, None))
                .build(k)
                .key("remove-signature"),
        );
    let mut view = col().key("signature").w_full().gap(8.0).child(head);
    if let Some(preview) = signature_markdown(&c.signature_preview, k) {
        view = view.child(
            col()
                .key("signature-body")
                .w_full()
                .pl(12.0)
                .border_sides([0.0, 0.0, 0.0, 2.0], k.c.border_weak)
                .child(preview),
        );
    }
    Some(view)
}

/// What moving Postden to another computer takes along, and leaves.
const TRANSFER_ABOUT: &str = "Export every account, with its password or Google sign-in and its \
     signature, and these settings to a file locked with a passphrase, then import it in Postden \
     on the other computer. Or send them over the internet: the other computer types the code \
     this one shows. Mail isn't sent: the other computer fetches it again.";

/// The settings' section for moving to another computer.
fn transfer(t: &Transfer, k: &Kit) -> El {
    setting_section(
        k,
        "Moving to another computer",
        Some(TRANSFER_ABOUT),
        transfer_step(t, k),
    )
}

/// Exporting or importing every account and setting: two buttons, or the step under way.
fn transfer_step(t: &Transfer, k: &Kit) -> El {
    let section = match t.step {
        moving::Step::Idle => {
            return row()
                .gap(8.0)
                .child(
                    Button::new("Export…")
                        .on_press(Mail::start_export)
                        .build(k)
                        .key("export-profile"),
                )
                .child(
                    Button::new("Import…")
                        .on_press_tx(|_: &mut Mail, tx| Mail::pick_profile(tx))
                        .build(k)
                        .key("import-profile"),
                )
                .child(
                    Button::new("Send…")
                        .variant(ButtonVariant::Ghost)
                        .on_press(Mail::start_send)
                        .build(k)
                        .key("send-profile"),
                )
                .child(
                    Button::new("Receive…")
                        .variant(ButtonVariant::Ghost)
                        .on_press(Mail::start_receive)
                        .build(k)
                        .key("receive-profile"),
                );
        }
        moving::Step::Export | moving::Step::Send => export_form(t, k),
        moving::Step::Offering => offering(t, k),
        moving::Step::Receive => receive_form(t, k),
        moving::Step::Unlock => unlock_form(t, k),
        moving::Step::Review => review(t, k),
    };
    match &t.error {
        Some(error) => section.child(
            text(error.clone())
                .key("transfer-error")
                .font(k.ty.small)
                .color(k.c.danger.fg)
                .wrap(),
        ),
        None => section,
    }
}

/// A passphrase field of the export or import, which `submit` presses on Return.
fn passphrase_input(
    t: &Transfer,
    label: &str,
    edit: &TextEdit,
    get: fn(&mut Mail) -> &mut TextEdit,
    submit: fn(&mut Mail, &mut Tx),
) -> TextInput {
    TextInput::new(edit)
        .label(label)
        .secure()
        .width(None)
        .disabled(t.busy)
        .on_edit(get)
        .on_submit(submit)
}

/// The step's buttons: `go`, which does it, and Cancel.
fn transfer_actions(t: &Transfer, k: &Kit, go: El) -> El {
    row().gap(8.0).child(go).child(
        Button::new("Cancel")
            .variant(ButtonVariant::Ghost)
            .disabled(!t.can_cancel())
            .on_press(Mail::cancel_transfer)
            .build(k)
            .key("cancel-transfer"),
    )
}

/// Typing the passphrase to lock an export with, twice, then picking where to save it, or
/// going online to send it.
fn export_form(t: &Transfer, k: &Kit) -> El {
    let sending = t.step == moving::Step::Send;
    let (submit, key): (fn(&mut Mail, &mut Tx), _) = if sending {
        (|s, _| s.send_profile(), "send-start")
    } else {
        (Mail::export_profile, "export-save")
    };
    let (label, variant) = match (t.busy, sending) {
        (true, true) => ("Going online…", ButtonVariant::Loading),
        (true, false) => ("Encrypting…", ButtonVariant::Loading),
        (false, true) => ("Send", ButtonVariant::Contrast),
        (false, false) => ("Export…", ButtonVariant::Contrast),
    };
    let go = Button::new(label)
        .variant(variant)
        .disabled(t.busy)
        .on_press_tx(submit)
        .build(k)
        .key(key);
    col()
        .w_full()
        .gap(10.0)
        .child(
            row()
                .gap(12.0)
                .child(field(
                    k,
                    "Passphrase",
                    passphrase_input(
                        t,
                        "Passphrase",
                        &t.passphrase,
                        |s| &mut s.transfer.passphrase,
                        submit,
                    )
                    .build(k)
                    .key("transfer-passphrase"),
                ))
                .child(field(
                    k,
                    "The passphrase again",
                    passphrase_input(
                        t,
                        "The passphrase again",
                        &t.confirm,
                        |s| &mut s.transfer.confirm,
                        submit,
                    )
                    .build(k)
                    .key("transfer-confirm"),
                )),
        )
        .child(
            text(format!(
                "At least {} characters. Without it the file can't be opened, and anyone with \
                 both can read your passwords.",
                postden_app::MIN_PASSPHRASE
            ))
            .font(k.ty.small)
            .color(k.c.text_muted)
            .wrap(),
        )
        .child(
            Checkbox::new(t.with_sorting)
                .label("Include what sorting learned")
                .disabled(t.busy)
                .on_toggle(|s: &mut Mail| s.transfer.with_sorting = !s.transfer.with_sorting)
                .build(k)
                .key("export-sorting"),
        )
        .child(
            text(
                "So the other computer sorts mail as this one does from the start. It tells \
                 much of your mail: the words it learned are those of your mail, and who it's \
                 from, and it lists each message it learned from, by its Message-ID, with where \
                 it went and whether it mattered."
                    .to_owned(),
            )
            .font(k.ty.small)
            .color(k.c.text_muted)
            .wrap(),
        )
        .child(transfer_actions(t, k, go))
}

/// The code the other computer types, while waiting for it to take the profile.
fn offering(t: &Transfer, k: &Kit) -> El {
    let code = t
        .online
        .as_ref()
        .and_then(|online| online.code.clone())
        .unwrap_or_default();
    let copy = Button::new("Copy")
        .variant(ButtonVariant::Ghost)
        .on_press_tx(|s: &mut Mail, tx| {
            if let Some(code) = s.transfer.online.as_ref().and_then(|o| o.code.clone()) {
                tx.copy(code);
            }
        })
        .build(k)
        .key("copy-code");
    col()
        .w_full()
        .gap(10.0)
        .child(
            text(
                "On the other computer, open these settings, choose Receive… and type this code:"
                    .to_owned(),
            )
            .font(k.ty.ui)
            .color(k.c.text_base)
            .wrap(),
        )
        .child(
            text(code)
                .key("transfer-code")
                .font(k.ty.mono)
                .color(k.c.text_base)
                .selectable(true),
        )
        .child(
            text(
                "Waiting for the other computer… It will also ask for the passphrase. Anyone \
                 with both can read your passwords."
                    .to_owned(),
            )
            .font(k.ty.small)
            .color(k.c.text_muted)
            .wrap(),
        )
        .child(transfer_actions(t, k, copy))
}

/// Typing the code the sending computer shows.
fn receive_form(t: &Transfer, k: &Kit) -> El {
    let (label, variant) = if t.busy {
        ("Receiving…", ButtonVariant::Loading)
    } else {
        ("Receive", ButtonVariant::Contrast)
    };
    let go = Button::new(label)
        .variant(variant)
        .disabled(t.busy)
        .on_press(Mail::receive_profile)
        .build(k)
        .key("receive-start");
    col()
        .w_full()
        .gap(10.0)
        .child(field(
            k,
            "The code the other computer shows",
            TextInput::new(&t.code)
                .label("Code")
                .placeholder("k3f9-2xqa-7m0d-wnhc")
                .width(None)
                .disabled(t.busy)
                .on_edit(|s: &mut Mail| &mut s.transfer.code)
                .on_submit(|s: &mut Mail, _| s.receive_profile())
                .build(k)
                .key("receive-code"),
        ))
        .child(
            text(
                "On the other computer, open these settings and choose Send…, then type the code \
                 it shows."
                    .to_owned(),
            )
            .font(k.ty.small)
            .color(k.c.text_muted)
            .wrap(),
        )
        .child(transfer_actions(t, k, go))
}

/// Typing the passphrase a picked profile was exported with.
fn unlock_form(t: &Transfer, k: &Kit) -> El {
    let name = t.file.as_ref().map_or("", |(name, _)| name.as_str());
    let (label, variant) = if t.busy {
        ("Unlocking…", ButtonVariant::Loading)
    } else {
        ("Unlock", ButtonVariant::Contrast)
    };
    let go = Button::new(label)
        .variant(variant)
        .disabled(t.busy)
        .on_press(Mail::unlock_profile)
        .build(k)
        .key("unlock-profile");
    col()
        .w_full()
        .gap(10.0)
        .child(field(
            k,
            &format!("The passphrase {name} was exported with"),
            passphrase_input(
                t,
                "Passphrase",
                &t.passphrase,
                |s| &mut s.transfer.passphrase,
                |s, _| s.unlock_profile(),
            )
            .build(k)
            .key("transfer-passphrase"),
        ))
        .child(transfer_actions(t, k, go))
}

/// What an unlocked profile holds, what to do with accounts set up already and its settings,
/// and the Import button.
fn review(t: &Transfer, k: &Kit) -> El {
    let Some(profile) = &t.profile else {
        return col();
    };
    let mut accounts = col().w_full().gap(6.0);
    for account in &profile.accounts {
        let mut line = row().gap(8.0).center_y().child(
            col()
                .grow(1.0)
                .min_w(0.0)
                .child(
                    text(account.name().to_owned())
                        .font(k.ty.ui_medium)
                        .color(k.c.text_base),
                )
                .child(
                    text(account.id.clone())
                        .font(k.ty.small)
                        .color(k.c.text_muted),
                ),
        );
        if t.existing.contains(&account.id) {
            line = line.child(
                text("Set up already".to_owned())
                    .font(k.ty.small)
                    .color(k.c.text_muted),
            );
        }
        accounts = accounts.child(line);
    }
    let mut section = col().w_full().gap(10.0).child(accounts);
    if !t.existing.is_empty() {
        section = section.child(
            Switch::new(t.options.replace_accounts)
                .label("Replace the accounts set up already")
                .on_toggle(|s: &mut Mail| {
                    s.transfer.options.replace_accounts = !s.transfer.options.replace_accounts;
                })
                .build(k)
                .key("replace-accounts"),
        );
    }
    let go = Button::new("Import")
        .variant(ButtonVariant::Contrast)
        .on_press(Mail::import_profile)
        .build(k)
        .key("import-confirm");
    section = section.child(
        Switch::new(t.options.settings)
            .label("Use its settings")
            .on_toggle(|s: &mut Mail| {
                s.transfer.options.settings = !s.transfer.options.settings;
            })
            .build(k)
            .key("import-settings"),
    );
    if t.profile.as_ref().is_some_and(|p| p.sorting.is_some()) {
        section = section.child(
            Switch::new(t.options.sorting)
                .label("Use what its sorting learned, in place of this computer's")
                .on_toggle(|s: &mut Mail| {
                    s.transfer.options.sorting = !s.transfer.options.sorting;
                })
                .build(k)
                .key("import-sorting"),
        );
    }
    section.child(transfer_actions(t, k, go))
}

/// Which updates to get: releases only (Stable), or every nightly build of main too.
fn channel(s: &Mail, k: &Kit, layer: &mut Vec<El>) -> El {
    let names = CHANNELS.map(|(_, name)| name);
    let current = CHANNELS
        .iter()
        .find(|(track, _)| *track == s.settings.update_track)
        .map(|(_, name)| *name);
    let select = Select::new("update-channel", &names)
        .label("Update channel")
        .current(current)
        .open(s.updater.channel_open)
        .on_toggle(|s: &mut Mail| s.updater.channel_open = !s.updater.channel_open)
        .on_close(|s: &mut Mail| s.updater.channel_open = false)
        .on_choose(|s: &mut Mail, name, _| {
            s.updater.channel_open = false;
            if let Some((track, _)) = CHANNELS.iter().find(|(_, n)| *n == name) {
                s.set_update_track(*track);
            }
        })
        .build(k, layer);
    row()
        .gap(8.0)
        .center_y()
        .child(
            text("Update channel")
                .font(TextStyle {
                    line_height: 16.0,
                    ..k.ty.small
                })
                .color(k.c.text_faint),
        )
        .child(select)
}

/// The update channels the settings offer, with their names there.
const CHANNELS: [(Track, &str); 2] = [(Track::Stable, "Stable"), (Track::Nightly, "Nightly")];

/// The running version, whether updates are checked for and on which channel, a button to check
/// now, and how the last check went.
fn updates(s: &Mail, k: &Kit, layer: &mut Vec<El>) -> El {
    let u = &s.updater;
    let mut buttons = row().gap(8.0).center_y().child(
        Button::new("Check for updates")
            .disabled(!u.can_check())
            .on_press(|s: &mut Mail| s.check_for_updates(true))
            .build(k)
            .key("check-updates"),
    );
    if u.phase == Phase::Ready {
        buttons = buttons.child(
            Button::new("Restart to update")
                .variant(ButtonVariant::Contrast)
                .on_press_tx(Mail::restart_to_update)
                .build(k)
                .key("restart-to-update"),
        );
    } else if u.available.is_some() {
        buttons = buttons.child(
            Button::new("See what's new")
                .variant(ButtonVariant::Contrast)
                .on_press(Mail::open_update)
                .build(k)
                .key("see-update"),
        );
    }
    let mut section = col()
        .w_full()
        .gap(10.0)
        .child(
            text(format!("Version {}", update::current()))
                .font(k.ty.small)
                .color(k.c.text_muted)
                .selectable(true),
        )
        .child(labeled_switch(
            "Check for updates automatically",
            Switch::new(s.settings.check_updates)
                .on_toggle(|s: &mut Mail| s.set_check_updates(!s.settings.check_updates)),
            "auto-update",
            k,
        ))
        .child(channel(s, k, layer))
        .child(buttons);
    let status = u.status();
    if !status.is_empty() {
        let failed = matches!(u.phase, Phase::CheckFailed(_) | Phase::InstallFailed(_));
        section = section.child(
            text(status)
                .key("update-status")
                .font(k.ty.small)
                .color(if failed {
                    k.c.danger.fg
                } else {
                    k.c.text_muted
                })
                .wrap(),
        );
    }
    section
}

/// The announcement of a new release: what changed since the running version, then how to
/// update, which Postden does itself unless a package manager installed it.
fn update_dialog(s: &Mail, k: &Kit, layer: &mut Vec<El>) {
    let u = &s.updater;
    let Some(available) = u.available.as_ref().filter(|_| u.dialog_open) else {
        return;
    };
    let changelog = Markdown::new(available.changelog())
        .selectable(true)
        .on_link_tx(|s: &mut Mail, url: &str, tx: &mut Tx| s.open_link(url, tx))
        .build(k)
        .key("changelog");
    let changelog = col()
        .key("changelog-scroll")
        .w_full()
        .padding(16.0)
        .child(changelog)
        .scroll(ScrollSpec {
            stick_bottom: false,
        })
        .grow(1.0)
        .min_h(0.0);

    let mut footer = col()
        .w_full()
        .gap(10.0)
        .padding(16.0)
        .border_sides([1.0, 0.0, 0.0, 0.0], k.c.border_weak);
    let note = match &u.phase {
        Phase::Ready if s.compose.open => {
            Some("Restarting discards the message you're writing.".to_owned())
        }
        Phase::Downloading(_) | Phase::Ready | Phase::InstallFailed(_) => Some(u.status()),
        _ => u.install.instructions().map(str::to_owned),
    };
    if let Some(note) = note {
        let failed = matches!(u.phase, Phase::InstallFailed(_));
        footer = footer.child(
            text(note)
                .key("update-note")
                .font(k.ty.small)
                .color(if failed {
                    k.c.danger.fg
                } else {
                    k.c.text_muted
                })
                .wrap()
                .selectable(true),
        );
    }
    Dialog::new(
        "update",
        format!("Postden {} is available", available.latest.version),
    )
    .description(format!(
        "You have {}. Here's what changed since:",
        available.current
    ))
    .width(600.0)
    .on_close(Mail::close_update)
    .child(changelog)
    .child(footer.child(update_actions(s, k)))
    .open(true)
    .build(k, layer);
}

/// The command palette (⌘K), over everything: a field, and the commands that match what's typed
/// in it, each with its shortcut. ↑ and ↓ move the highlight, and Return runs the highlighted one.
fn palette(s: &Mail, k: &Kit, layer: &mut Vec<El>) {
    let p = &s.palette;
    if !p.open {
        return;
    }
    let shown = p.shown(s);
    let active = p.active_row(&shown);
    let input = TextInput::new(&p.query)
        .label("Command")
        .placeholder(p.placeholder())
        .leading_icon(&glyphs::SEARCH)
        .width(None)
        // A screen reader reads the highlighted row, while the field keeps the focus.
        .active_descendant(shown.get(active).map(|e| e.command.key()))
        .on_edit(|s: &mut Mail| &mut s.palette.query)
        .on_change(|s: &mut Mail, tx| {
            s.palette_changed();
            if let Some(first) = s.palette.shown(s).first() {
                tx.scroll_into_view(first.command.key());
            }
        })
        .on_submit(|s: &mut Mail, tx| s.run_palette(None, tx))
        .on_key(|s: &mut Mail, ev: &rune::KeyEvent, tx| {
            let by = match ev.key {
                rune::Key::Up => -1,
                rune::Key::Down => 1,
                _ => return false,
            };
            s.move_palette(by);
            if let Some(command) = &s.palette.active {
                tx.scroll_into_view(command.key());
            }
            true
        });
    let list = palette_list(&shown, active, k);
    let mut panel = col()
        .key("palette")
        .role("dialog")
        .label("Command palette")
        .w_full()
        .max_w(560.0)
        .shrink()
        .min_h(0.0)
        .radius(8.0)
        .fill(k.c.layer_01)
        .preserve_focus_on_press(true)
        .child(
            row()
                .w_full()
                .padding(10.0)
                .border_sides([0.0, 0.0, 1.0, 0.0], k.c.border_weak)
                .child(row().grow(1.0).min_w(0.0).child(input.build(k))),
        )
        .child(list)
        .token_motion(k.motion.overlay)
        .modal(true, Mail::close_palette);
    for shadow in k.shadow_layers(&k.elevation.overlay) {
        panel = panel.box_shadow(shadow);
    }
    layer.push(
        el().key("palette-scrim")
            .absolute(0.0, 0.0)
            .w_full()
            .h_full()
            .fill(k.c.scrim)
            .token_motion(k.motion.overlay)
            .a11y_hidden(),
    );
    layer.push(
        col()
            .key("palette-layer")
            .absolute(0.0, 0.0)
            .w_full()
            .h_full()
            .padding(16.0)
            .pt(96.0)
            .items_center()
            .child(panel),
    );
}

/// The palette's rows, or a word that nothing matches, in a list that scrolls.
fn palette_list(shown: &[Entry], active: usize, k: &Kit) -> El {
    // Virtual, so the highlighted row is revealed whenever the highlight moves to another one.
    // Rows coming in above it are followed where mail updates arrive, in `Mail::try_start`.
    let mut rows = col()
        .key("palette-rows")
        .w_full()
        .growth(GrowthSpec::default())
        .virtualize(32.0)
        .reveal_selected(true);
    for (i, entry) in shown.iter().enumerate() {
        rows = rows.child(palette_row(entry, i == active, k));
    }
    let list = if shown.is_empty() {
        el().key("palette-empty")
            .w_full()
            .h(32.0)
            .px(10.0)
            .center_y()
            .child(
                text("No matching command")
                    .font(k.ty.ui)
                    .color(k.c.text_muted),
            )
    } else {
        rows
    };
    col()
        .key("palette-scroll")
        .role("listbox")
        .label("Commands")
        .w_full()
        .padding(6.0)
        .child(list)
        .scroll(ScrollSpec {
            stick_bottom: false,
        })
        .max_h(360.0)
        // A short window shrinks the list, not the field, so the panel stays inside it.
        .shrink()
        .min_h(0.0)
}

/// One command in the palette: what it's called, what it applies to, and its shortcut. `on` when
/// it's highlighted.
fn palette_row(entry: &Entry, on: bool, k: &Kit) -> El {
    let (hovered, pressed) = (entry.command.clone(), entry.command.clone());
    let mut item = row()
        .key(entry.command.key())
        .role("option")
        .label(&entry.title)
        .description(entry.detail.clone().unwrap_or_default())
        .focus_selected(on)
        .w_full()
        .h(32.0)
        .px(10.0)
        .gap(8.0)
        .center_y()
        .radius(4.0)
        .fill(if on { k.c.hover } else { Color::TRANSPARENT })
        // The pointer moves the highlight, so just one row shows as the one Return runs. Only
        // when it moves: a row that appears under a resting pointer keeps the best match.
        .on_pointer_move_local(move |s: &mut Mail, _, tx| {
            if s.palette.active.as_ref() == Some(&hovered) {
                tx.live_only();
            } else {
                s.palette.active = Some(hovered.clone());
            }
        })
        .on_press_tx(move |s: &mut Mail, tx| s.run_palette(Some(pressed.clone()), tx))
        .child(
            text(&entry.title)
                .font(k.ty.ui)
                .color(k.c.text_base)
                .shrink()
                .truncate()
                .a11y_hidden(),
        );
    let detail = entry.detail.as_deref().map_or_else(el, |d| {
        text(d)
            .font(k.ty.ui)
            .color(k.c.text_muted)
            .truncate()
            .a11y_hidden()
    });
    item = item.child(detail.grow(1.0).shrink());
    if !entry.shortcuts.is_empty() {
        let hint: Vec<String> = entry.shortcuts.iter().map(|k| k.label()).collect();
        let keys = entry
            .shortcuts
            .iter()
            .map(|shortcut| Keybind::new(&shortcut.keycaps()).build(k));
        item = item
            .shortcut_hint(hint.join(", "))
            .child(row().gap(6.0).children(keys).a11y_hidden());
    }
    item
}

/// The announcement's buttons: Skip this version, Later, and Update, Restart now, or, for a
/// copy a package manager installed, Open release page.
fn update_actions(s: &Mail, k: &Kit) -> El {
    let u = &s.updater;
    let busy = matches!(u.phase, Phase::Downloading(_));
    let mut actions = row().w_full().gap(8.0).center_y();
    if u.phase != Phase::Ready {
        actions = actions.child(
            Button::new("Skip this version")
                .variant(ButtonVariant::Ghost)
                .disabled(busy)
                .on_press(Mail::skip_update)
                .build(k)
                .key("skip-update"),
        );
    }
    actions = actions.child(el().grow(1.0)).child(
        Button::new(if u.phase == Phase::Ready {
            "Restart later"
        } else {
            "Later"
        })
        .variant(ButtonVariant::Neutral)
        .on_press(Mail::close_update)
        .build(k)
        .key("later"),
    );
    let primary = if u.phase == Phase::Ready {
        Button::new("Restart now").on_press_tx(Mail::restart_to_update)
    } else if u.install.can_install() {
        Button::new(match u.phase {
            Phase::Downloading(_) => "Updating…",
            Phase::InstallFailed(_) => "Try again",
            _ => "Update",
        })
        .disabled(!u.can_install())
        .on_press(Mail::install_update)
    } else {
        Button::new("Open release page").on_press_tx(Mail::open_release_page)
    };
    actions = actions.child(
        primary
            .variant(if busy {
                ButtonVariant::Loading
            } else {
                ButtonVariant::Contrast
            })
            .build(k)
            .key("update-primary"),
    );

    actions
}

/// "30 s", "1 min", "15 min".
fn interval_label(every: std::time::Duration) -> String {
    let secs = every.as_secs();
    if secs < 60 {
        format!("{secs} s")
    } else {
        format!("{} min", secs / 60)
    }
}

fn dot(color: Color) -> El {
    el().size(8.0, 8.0).radius(4.0).fill(color)
}

static BOLD: Icon = Icon {
    name: "postden-bold",
    view: 16.0,
    body: r#"<path d="M5 3H8.6C9.98 3 11.1 4.12 11.1 5.5C11.1 6.88 9.98 8 8.6 8H5ZM5 8H9.1C10.48 8 11.6 9.12 11.6 10.5C11.6 11.88 10.48 13 9.1 13H5Z" stroke="currentColor" stroke-width="1.6" stroke-linejoin="round" fill="none"/>"#,
};
static ITALIC: Icon = Icon {
    name: "postden-italic",
    view: 16.0,
    body: r#"<path d="M7.5 3H12M4 13H8.5M9.75 3L6.25 13" stroke="currentColor" stroke-width="1.3" stroke-linecap="round" fill="none"/>"#,
};
static UNDERLINE: Icon = Icon {
    name: "postden-underline",
    view: 16.0,
    body: r#"<path d="M4.75 2.75V7.25C4.75 9.05 6.2 10.5 8 10.5C9.8 10.5 11.25 9.05 11.25 7.25V2.75M3.75 13.25H12.25" stroke="currentColor" stroke-width="1.3" stroke-linecap="round" fill="none"/>"#,
};
static STRIKE: Icon = Icon {
    name: "postden-strike",
    view: 16.0,
    body: r#"<path d="M2.75 8H13.25M10.9 4.9C10.55 3.8 9.4 3 8 3C6.35 3 5.1 3.95 5.1 5.3C5.1 6.1 5.55 6.7 6.4 7.2M5.1 11C5.45 12.2 6.6 13 8.1 13C9.85 13 11.1 12 11.1 10.65C11.1 10.05 10.9 9.55 10.45 9.15" stroke="currentColor" stroke-width="1.3" stroke-linecap="round" fill="none"/>"#,
};
static CODE: Icon = Icon {
    name: "postden-code",
    view: 16.0,
    body: r#"<path d="M5.5 4.5L2 8L5.5 11.5M10.5 4.5L14 8L10.5 11.5" stroke="currentColor" stroke-width="1.3" stroke-linecap="round" stroke-linejoin="round" fill="none"/>"#,
};
static LINK: Icon = Icon {
    name: "postden-link",
    view: 16.0,
    body: r#"<path d="M6.9 9.1C7.6 9.9 8.8 10 9.6 9.3L11.9 7C12.75 6.15 12.75 4.8 11.9 3.95C11.05 3.1 9.7 3.1 8.85 3.95L8.2 4.6M9.1 6.9C8.4 6.1 7.2 6 6.4 6.7L4.1 9C3.25 9.85 3.25 11.2 4.1 12.05C4.95 12.9 6.3 12.9 7.15 12.05L7.8 11.4" stroke="currentColor" stroke-width="1.3" stroke-linecap="round" fill="none"/>"#,
};
static BULLETS: Icon = Icon {
    name: "postden-bullets",
    view: 16.0,
    body: r#"<circle cx="3.5" cy="4.5" r="1" fill="currentColor"/><circle cx="3.5" cy="8" r="1" fill="currentColor"/><circle cx="3.5" cy="11.5" r="1" fill="currentColor"/><path d="M6.5 4.5H13M6.5 8H13M6.5 11.5H13" stroke="currentColor" stroke-width="1.3" stroke-linecap="round"/>"#,
};
static NUMBERS: Icon = Icon {
    name: "postden-numbers",
    view: 16.0,
    body: r#"<path d="M2.6 3.3L3.5 2.75V6.25M2.4 9.6C2.55 9.15 2.9 8.9 3.35 8.9C3.85 8.9 4.2 9.25 4.2 9.65C4.2 10.4 2.4 11 2.4 12.25H4.3" stroke="currentColor" stroke-width="1" stroke-linecap="round" stroke-linejoin="round" fill="none"/><path d="M6.5 4.5H13M6.5 11H13" stroke="currentColor" stroke-width="1.3" stroke-linecap="round"/>"#,
};
static QUOTE: Icon = Icon {
    name: "postden-quote",
    view: 16.0,
    body: r#"<path d="M3.25 3.5V12.5" stroke="currentColor" stroke-width="1.6" stroke-linecap="round"/><path d="M6.5 5H13M6.5 8H13M6.5 11H11" stroke="currentColor" stroke-width="1.3" stroke-linecap="round"/>"#,
};
static CODE_BLOCK: Icon = Icon {
    name: "postden-code-block",
    view: 16.0,
    body: r#"<rect x="2.25" y="3" width="11.5" height="10" rx="1.75" stroke="currentColor" stroke-width="1.2" fill="none"/><path d="M6.75 6.5L5.25 8L6.75 9.5M9.25 6.5L10.75 8L9.25 9.5" stroke="currentColor" stroke-width="1.2" stroke-linecap="round" stroke-linejoin="round" fill="none"/>"#,
};

/// Where a formatted text editor's state lives, and what editing it does: the message being
/// written, or a signature.
#[derive(Clone, Copy)]
struct Rich {
    /// Starts each element's key: `compose` or `signature`.
    name: &'static str,
    /// The text area's accessible name.
    label: &'static str,
    get: fn(&mut Mail) -> &mut RichEditor,
    /// Runs after each edit, as rescoring the markup or redrawing a preview.
    changed: fn(&mut Mail),
    set_mode: fn(&mut Mail, Mode),
    /// Handles keys the editor doesn't, such as ⌘↩ to send.
    on_key: fn(&mut Mail, &rune::KeyEvent, &mut Tx) -> bool,
    /// Offered when switching to the visual editor would drop markup.
    drops: &'static str,
    /// Asks for an image to put where the caret is, for text that can show images.
    add_image: Option<fn(&mut Mail, &mut Tx)>,
    disabled: bool,
    /// How many rows the text area grows from and to.
    rows: (usize, usize),
}

impl Rich {
    /// An element key, the same as before for the message: `format-bold`, `signature-format-bold`.
    fn key(self, what: &str) -> String {
        if self.name == "compose" {
            what.to_owned()
        } else {
            format!("{}-{what}", self.name)
        }
    }
}

const COMPOSE: Rich = Rich {
    name: "compose",
    label: "Message",
    get: |s| &mut s.compose.body,
    changed: |s| s.compose.refresh(),
    set_mode: |s, mode| s.compose.set_mode(mode),
    on_key: send_on_command_return,
    drops: "This HTML uses markup the visual editor can't show, such as styles, tables or \
            images. Choose Visual again to drop it, or keep editing the source.",
    add_image: None,
    disabled: false,
    rows: (12, 60),
};

const SIGNATURE: Rich = Rich {
    name: "signature",
    label: "Signature",
    get: |s| &mut s.account_settings.signature,
    changed: |s| s.edit_signature(|_| {}),
    set_mode: Mail::set_signature_mode,
    on_key: |_, _, _| false,
    drops: "This HTML uses markup the visual editor can't show, such as styles or tables. \
            Choose Visual again to drop it and keep the text and images, or keep editing the \
            source.",
    add_image: Some(|_, tx| Mail::pick_signature_image(tx)),
    disabled: false,
    rows: (4, 20),
};

/// `get`'s visual editor, for a text area to edit.
fn visual_of(
    get: fn(&mut Mail) -> &mut RichEditor,
) -> impl for<'a> Fn(&'a mut Mail) -> &'a mut TextEdit {
    move |s| &mut get(s).text
}

/// `get`'s source, for a text area to edit.
fn source_of(
    get: fn(&mut Mail) -> &mut RichEditor,
) -> impl for<'a> Fn(&'a mut Mail) -> &'a mut TextEdit {
    move |s| &mut get(s).source
}

/// `get`'s link address, for a field to edit.
fn link_of(
    get: fn(&mut Mail) -> &mut RichEditor,
) -> impl for<'a> Fn(&'a mut Mail) -> &'a mut TextEdit {
    move |s| get(s).link.get_or_insert_with(TextEdit::default)
}

/// Formatted text: a formatting bar over the visual editor, or the HTML source.
fn editor(e: &RichEditor, r: Rich, k: &Kit) -> El {
    let (area, key) = match e.mode {
        Mode::Visual => (
            TextArea::new(&e.text)
                .on_edit(visual_of(r.get))
                .pill_icon(|id| postden_app::signature::pill_image(id).map(|_| &PICTURE)),
            format!("{}-text", r.name),
        ),
        Mode::Source => (
            TextArea::new(&e.source).on_edit(source_of(r.get)),
            format!("{}-source", r.name),
        ),
    };
    let area = area
        .label(if e.mode == Mode::Visual {
            r.label.to_owned()
        } else {
            format!("{} HTML source", r.label)
        })
        .auto_grow(r.rows.0, r.rows.1)
        .width(None)
        .disabled(r.disabled)
        .on_change(move |s: &mut Mail, _| (r.changed)(s))
        .on_key(r.on_key)
        // Return breaks the line, as in any document; ⌘↩ sends a message.
        .on_submit(move |s: &mut Mail, _| {
            (r.get)(s).new_line();
            (r.changed)(s);
        })
        .build(k)
        .key(key);
    let mut editor = col().gap(8.0).w_full().child(format_bar(e, r, k));
    if e.confirm_visual {
        editor = editor.child(
            text(r.drops)
                .key(r.key("confirm-visual"))
                .font(k.ty.small)
                .color(k.c.warning.fg)
                .wrap(),
        );
    }
    if let Some(link) = &e.link {
        editor = editor.child(link_row(e, r, link, k));
    }
    editor.child(area)
}

/// ⌘↩ in the message sends it.
fn send_on_command_return(s: &mut Mail, ev: &rune::KeyEvent, _: &mut Tx) -> bool {
    if ev.key == rune::Key::Enter && ev.mods.primary() {
        s.send();
        return true;
    }
    false
}

/// Buttons for each kind of formatting, and the switch between visual and source editing.
fn format_bar(e: &RichEditor, r: Rich, k: &Kit) -> El {
    let enabled = e.mode == Mode::Visual && !r.disabled;
    let active = e.text.active_marks();
    let block = e.text.active_block();
    let mark = |icon: &'static Icon, label: &str, shortcut: Option<&str>, mark: Mark| {
        let on = enabled && active.iter().any(|m| m.same_kind(&mark));
        let mut button = tool(icon, label, on, enabled);
        if let Some(shortcut) = shortcut {
            button = button.shortcut(shortcut);
        }
        button
            .on_press(move |s: &mut Mail| {
                (r.get)(s).toggle_mark(mark.clone());
                (r.changed)(s);
            })
            .build(k)
            .checked(on)
            .key(r.key(&tool_key(label)))
    };
    let block_button = |icon: &'static Icon, label: &str, kind: Block| {
        let on = enabled && block.same_kind(kind);
        tool(icon, label, on, enabled)
            .on_press(move |s: &mut Mail| {
                (r.get)(s).toggle_block(kind);
                (r.changed)(s);
            })
            .build(k)
            .checked(on)
            .key(r.key(&tool_key(label)))
    };
    let linked = enabled && e.text.active_link().is_some();
    let link = tool(&LINK, "Link", linked || e.link.is_some(), enabled)
        .shortcut("⌘K")
        .on_press(move |s: &mut Mail| (r.get)(s).toggle_link())
        .build(k)
        .checked(linked)
        .key(r.key("format-link"));
    let separator = || el().w(1.0).h(16.0).fill(k.c.border_weak);
    // Works in both modes: a pill in the editor, an `<img>` in the source.
    let image = r.add_image.map(|add| {
        tool(&PICTURE, "Image", false, !r.disabled)
            .on_press_tx(add)
            .build(k)
            .key(r.key("format-image"))
    });
    let mode = SegmentedControl::new(
        &["Visual", "HTML"],
        Some(usize::from(e.mode == Mode::Source)),
    )
    .label("Edit as")
    .fit()
    .on_pick(move |s: &mut Mail, i, _| {
        (r.set_mode)(s, if i == 0 { Mode::Visual } else { Mode::Source });
    })
    .build(k)
    .key(r.key("edit-mode"));
    row()
        .key(r.key("format-bar"))
        .w_full()
        .gap(2.0)
        .center_y()
        .child(mark(&BOLD, "Bold", Some("⌘B"), Mark::Bold))
        .child(mark(&ITALIC, "Italic", Some("⌘I"), Mark::Italic))
        .child(mark(&UNDERLINE, "Underline", Some("⌘U"), Mark::Underline))
        .child(mark(&STRIKE, "Strikethrough", None, Mark::Strike))
        .child(mark(&CODE, "Code", None, Mark::Code))
        .child(link)
        .children(image)
        .child(row().px(6.0).child(separator()))
        .child(block_button(&BULLETS, "Bulleted list", Block::Bullet(0)))
        .child(block_button(&NUMBERS, "Numbered list", Block::Numbered(0)))
        .child(block_button(&QUOTE, "Quote", Block::Quote))
        .child(block_button(&CODE_BLOCK, "Code block", Block::Code))
        .child(el().grow(1.0))
        .child(mode)
}

/// A formatting button's element key: `format-bulleted-list`.
fn tool_key(label: &str) -> String {
    format!("format-{}", label.to_ascii_lowercase().replace(' ', "-"))
}

/// A formatting button, filled while its formatting is on.
fn tool(icon: &'static Icon, label: &str, on: bool, enabled: bool) -> IconButton {
    IconButton::new(icon)
        .variant(if on {
            IconButtonVariant::Neutral
        } else {
            IconButtonVariant::GhostMuted
        })
        .label(label)
        .help(label)
        .disabled(!enabled)
}

/// Where a link's address is typed, under the formatting bar.
fn link_row(e: &RichEditor, r: Rich, link: &TextEdit, k: &Kit) -> El {
    let apply = move |s: &mut Mail| {
        (r.get)(s).apply_link();
        (r.changed)(s);
    };
    let mut row = row()
        .key(r.key("link-row"))
        .w_full()
        .gap(8.0)
        .center_y()
        .child(
            col().grow(1.0).min_w(0.0).child(
                TextInput::new(link)
                    .label("Link address")
                    .placeholder("https://example.com or someone@example.com")
                    .width(None)
                    .on_edit(link_of(r.get))
                    .on_submit(move |s: &mut Mail, _| apply(s))
                    .build(k),
            ),
        )
        .child(
            Button::new("Link")
                .variant(ButtonVariant::Neutral)
                .on_press(apply)
                .build(k),
        );
    if e.text.active_link().is_some() {
        row = row.child(
            Button::new("Remove link")
                .variant(ButtonVariant::Ghost)
                .on_press(move |s: &mut Mail| {
                    (r.get)(s).remove_link();
                    (r.changed)(s);
                })
                .build(k),
        );
    }
    row.child(
        Button::new("Cancel")
            .variant(ButtonVariant::Ghost)
            .on_press(move |s: &mut Mail| (r.get)(s).toggle_link())
            .build(k),
    )
}

/// The message replied to, below the editor: its header, and the message itself when opened.
fn quote_view(c: &ComposeForm, k: &Kit) -> Option<El> {
    let quote = c.quote.as_ref()?;
    let head = row()
        .w_full()
        .gap(8.0)
        .center_y()
        .child(
            text(quote.header.clone())
                .font(k.ty.small)
                .color(k.c.text_muted)
                .truncate()
                .grow(1.0)
                .min_w(0.0),
        )
        .child(
            Button::new(if c.expanded.quote {
                "Hide quote"
            } else {
                "Show quote"
            })
            .variant(ButtonVariant::Ghost)
            .expanded(c.expanded.quote)
            .on_press(|s: &mut Mail| s.compose.expanded.quote = !s.compose.expanded.quote)
            .build(k),
        )
        .child(
            Button::new("Remove quote")
                .variant(ButtonVariant::Ghost)
                .disabled(c.sending)
                .on_press(|s: &mut Mail| s.compose.remove_quote())
                .build(k),
        );
    let mut view = col().key("quote").w_full().gap(8.0).child(head);
    if c.expanded.quote {
        let body = c.quote_markdown.as_ref().map_or_else(
            || {
                text(quote.text.clone())
                    .font(k.ty.ui)
                    .color(k.c.text_muted)
                    .wrap()
                    .selectable(true)
            },
            |markdown| Markdown::new(markdown.clone()).selectable(true).build(k),
        );
        view = view.child(
            col()
                .key("quote-body")
                .w_full()
                .pl(12.0)
                .border_sides([0.0, 0.0, 0.0, 2.0], k.c.border_base)
                .child(body),
        );
    }
    Some(view)
}

/// How well the major mail apps support the message's markup, and, when opened, what each
/// doesn't.
fn compatibility(c: &ComposeForm, k: &Kit) -> El {
    let report = &c.compat;
    let percent = report.percent();
    let color = match percent {
        100 => k.c.success.fg,
        80..=99 => k.c.warning.fg,
        _ => k.c.danger.fg,
    };
    let unsupported = report
        .issues
        .iter()
        .filter(|i| !i.unsupported.is_empty())
        .count();
    let caveats = report.issues.len() - unsupported;
    let plural = |n: usize| if n == 1 { "" } else { "s" };
    let summary = match (unsupported, caveats) {
        (0, 0) => {
            "Shows as written in Gmail, Outlook, Apple Mail, Yahoo and Thunderbird".to_owned()
        }
        (0, n) => format!(
            "Shows in every major mail app, with {n} caveat{}",
            plural(n)
        ),
        (n, _) => format!("{n} feature{} won't show in some mail apps", plural(n)),
    };
    let mut head = row()
        .w_full()
        .gap(10.0)
        .center_y()
        .child(
            text("Compatibility")
                .font(k.ty.small_medium)
                .color(k.c.text_muted),
        )
        .child(
            text(format!("{percent}%"))
                .key("compat-percent")
                .font(k.ty.ui_medium)
                .color(color)
                .help(
                    "The share of mail apps (Gmail, Outlook, Apple Mail, Yahoo and Thunderbird, on \
                     each platform) that show everything this message uses, each provider counted \
                     equally. Apps that support something only partly still count.",
                ),
        )
        .child(
            text(summary)
                .font(k.ty.small)
                .color(k.c.text_muted)
                .truncate()
                .grow(1.0)
                .min_w(0.0),
        );
    if report.has_issues() || !report.providers.is_empty() {
        head = head.child(
            Button::new(if c.expanded.compat {
                "Hide details"
            } else {
                "Details"
            })
            .variant(ButtonVariant::Ghost)
            .expanded(c.expanded.compat)
            .on_press(|s: &mut Mail| s.compose.expanded.compat = !s.compose.expanded.compat)
            .build(k)
            .key("compat-details"),
        );
    }
    let mut panel = col()
        .key("compat")
        .w_full()
        .gap(10.0)
        .padding(12.0)
        .radius(8.0)
        .border(1.0, k.c.border_weak)
        .child(head);
    if c.expanded.compat {
        panel = compat_details(panel, report, k);
    }
    panel
}

/// The compatibility panel's details: each provider's score, then each feature not every app
/// fully supports.
fn compat_details(mut panel: El, report: &compat::Report, k: &Kit) -> El {
    let mut providers = row().gap(16.0).wrap();
    for (provider, score) in &report.providers {
        providers = providers.child(
            row()
                .gap(4.0)
                .child(text(provider.name()).font(k.ty.small).color(k.c.text_muted))
                .child(
                    text(format!("{}%", compat::percent(*score)))
                        .font(k.ty.small_medium)
                        .color(k.c.text_base),
                ),
        );
    }
    panel = panel.child(providers);
    for issue in &report.issues {
        let mut item = col().gap(2.0).child(
            text(issue.feature.title.to_owned())
                .font(k.ty.small_medium)
                .color(k.c.text_base),
        );
        if !issue.unsupported.is_empty() {
            item = item.child(
                text(format!(
                    "Not shown in {}",
                    clients_label(&issue.unsupported)
                ))
                .font(k.ty.small)
                .color(k.c.danger.fg)
                .wrap(),
            );
        }
        if !issue.partial.is_empty() {
            item = item.child(
                text(format!(
                    "Partly supported in {}",
                    clients_label(&issue.partial)
                ))
                .font(k.ty.small)
                .color(k.c.text_muted)
                .wrap(),
            );
        }
        panel = panel.child(item.key(format!("issue-{}", issue.feature.slug)));
    }
    panel.child(
        text(format!(
            "Support data from caniemail.com, updated {}.",
            compat::data_date()
        ))
        .font(k.ty.small)
        .color(k.c.text_faint),
    )
}

/// Clients grouped by provider: "Gmail (iOS, Android), Outlook (Windows)".
fn clients_label(clients: &[Client]) -> String {
    let all = compat::clients();
    Provider::ALL
        .iter()
        .filter_map(|provider| {
            let platforms: Vec<&str> = clients
                .iter()
                .filter(|c| c.provider == *provider)
                .map(|c| c.platform)
                .collect();
            if platforms.is_empty() {
                return None;
            }
            let everywhere = all.iter().filter(|c| c.provider == *provider).count();
            Some(if platforms.len() == everywhere {
                format!("{} (all)", provider.name())
            } else {
                format!("{} ({})", provider.name(), platforms.join(", "))
            })
        })
        .collect::<Vec<_>>()
        .join(", ")
}

#[cfg(test)]
mod tests {
    use postden_core::account::{Auth, Server};

    use super::*;

    fn account(id: &str, label: Option<&str>) -> Account {
        let server = Server {
            host: "localhost".into(),
            port: 1,
            security: Security::None,
            username: "me".into(),
        };
        Account {
            id: id.into(),
            display_name: None,
            label: label.map(Into::into),
            imap: server.clone(),
            smtp: server,
            auth: Auth::Password,
        }
    }

    #[test]
    fn the_from_picker_shows_an_address_under_a_name_but_not_twice() {
        let named = from_option(&account("ada@work.com", Some("Work")));
        assert_eq!(
            (
                named.id.as_str(),
                named.label.as_str(),
                named.detail.as_str()
            ),
            ("ada@work.com", "Work", "ada@work.com")
        );
        let bare = from_option(&account("ada@home.com", None));
        assert_eq!(
            (bare.label.as_str(), bare.detail.as_str()),
            ("ada@home.com", "")
        );
    }
}
