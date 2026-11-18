//! Postden desktop app, built with Rune.
//!
//! One window: accounts and their connection status on the left, every inbox's mail on the right,
//! updated as it arrives. Accounts are added, and mail written, from forms in the window.
//! Closing the window keeps the app running in the tray (the Dock on macOS), the app's icon
//! shows how much mail is unread, and new mail is announced with a system notification.

mod account_settings;
mod commands;
mod compose;
mod form;
mod notifications;
mod rich;
mod search;
mod state;
mod transfer;
mod tray;
mod update;
mod view;

use rune::{App, AppSpec, Chrome, Key, KeyEvent, Tx};

use crate::compose::Mode;
use crate::state::Mail;

/// The bundle identifier, also the Linux app id: Wayland and X11 report it so desktops match the
/// window to `com.matteogassend.postden.desktop`. Keep in sync with `assets/`.
const APP_ID: &str = "com.matteogassend.postden";

fn main() {
    rune::set_app_id(APP_ID);
    // A headless run (`--snap`, `--tree`, `--help`) is a tool's, not the user's: it neither hands
    // itself to the running app nor puts an icon in the tray.
    let headless = rune::is_headless();
    // Launching Postden again brings back the running one's window, closed or not, instead of
    // starting a second app on the same mail (Linux and Windows; macOS does this itself).
    if !headless && !rune::single_instance(APP_ID) {
        return;
    }
    #[cfg(target_os = "windows")]
    if !headless {
        notifications::register();
    }
    // Content runs under a transparent titlebar; the traffic lights sit centred on the toolbar,
    // which the shell frames 8 points down (as rune-kit's shell example sets it up).
    let chrome = Chrome {
        full_size_content: true,
        traffic_lights: Some((16.0, 25.0)),
        drag_regions: true,
    };
    let app = App::new(AppSpec {
        min_size: Some((720.0, 480.0)),
        on_key: Some(on_key),
        ..AppSpec::new("Postden", (1040.0, 680.0), Mail::not_started, view::view)
    })
    .chrome(chrome)
    // Closing the window keeps mail coming in, unless the settings say otherwise (`on_close`):
    // the tray icon, or the Dock icon on macOS, brings it back.
    .keep_running()
    .on_close(Mail::window_closed)
    .on_focus(Mail::focus_changed)
    .on_reopen(on_reopen);
    let handle = app.handle();
    let window = handle.clone();
    app.on_window(move |w| {
        let w = w.clone();
        window.update(move |s: &mut Mail, _| s.attach_window(w));
    })
    .init(move || {
        if !headless {
            tray::spawn(handle.clone());
            notifications::listen(handle.clone());
        }
        Mail::start(handle)
    })
    .run();
}

/// The window comes back (the Dock icon, or launching Postden again). On Linux that also brings
/// it to the front, as Windows and macOS do; Rune's default there only shows a hidden window.
fn on_reopen(s: &mut Mail, _visible: bool, _: &mut Tx) -> bool {
    state::reopened();
    cfg!(target_os = "linux") && s.bring_forward()
}

/// ⌘K opens the command palette, and closes it as Escape does. The shortcuts of the commands it
/// lists (`commands`) run them where they apply: ⇧⌘B (and ⌃⌘S on macOS) shows and hides the
/// sidebar, ⌘, opens the settings, C or ⌘N starts a new message, / or ⌘F searches the list and
/// ⇧⌘N refreshes what it shows.
/// J and K (or ↓ and ↑) move the list's cursor and ↩ opens its message; with a message open, J
/// and K open the next or previous one and Escape goes back to the list. On the message open or
/// under the cursor, U or ⇧⌘U marks it read or unread, S or ⇧⌘L stars it, R or ⌘R and ⇧⌘R reply
/// to the sender or everyone, F or ⇧⌘F forwards it, H snoozes it, V moves it, and E, # and ! or
/// ⇧⌘J move it to the account's Archive, Trash or Junk mailbox (or, from Junk, back to the
/// inbox). Z undoes the last of those, replies and forwards aside. G then I, T, E, H, #, ! or L
/// goes to all inboxes, Sent, the account's Archive, Snoozed, Trash or Junk, or any mailbox. Keys
/// on their own only count while no field takes what's typed, and none at all while a mailbox is
/// being named, whose dialog Escape closes. Escape also closes the settings (or the update announcement over them, or an account's
/// settings with nothing unsaved), and in the search field ends the search and leaves the field.
/// While writing, ⌘↩ sends, and ⌘K links the selected text, as it does in a signature.
fn on_key(s: &mut Mail, ev: &KeyEvent, tx: &mut Tx) -> bool {
    let go = s.take_go();
    let command_k = ev.is_command('k') && !ev.mods.shift;
    if s.palette.open && (command_k || ev.key == Key::Escape) {
        s.close_palette();
        return true;
    }
    // Naming a mailbox, the keys are the dialog's: with its button focused, they'd otherwise run
    // the list's shortcuts behind it.
    if s.new_mailbox.open {
        if ev.key == Key::Escape {
            s.close_new_mailbox();
            return true;
        }
        return false;
    }
    let page = &mut s.account_settings;
    if command_k && s.settings_open && page.open {
        if page.signature.mode == Mode::Visual {
            page.signature.toggle_link();
        }
        return true;
    }
    if command_k && s.compose.open && !s.settings_open {
        if s.compose.body.mode == Mode::Visual {
            s.compose.body.toggle_link();
        }
        return true;
    }
    if command_k && !s.updater.dialog_open && s.services.is_ok() {
        s.toggle_palette();
        return true;
    }
    if s.updater.dialog_open && ev.key == Key::Escape {
        s.close_update();
        return true;
    }
    if s.run_keys(ev, go, tx) {
        return true;
    }
    if s.settings_open && ev.key == Key::Escape {
        // Escape doesn't throw away a signature being written.
        if s.account_settings.is_changed() {
            return false;
        }
        if s.account_settings.open {
            s.close_account_settings();
        } else {
            s.close_settings();
        }
        return true;
    }
    false
}
