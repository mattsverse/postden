//! Postden desktop app, built with Rune.
//!
//! One window: accounts and their connection status on the left, every inbox's mail on the right,
//! updated as it arrives. Accounts are added, and mail written, from forms in the window.
//! Closing the window keeps the app running in the tray (the Dock on macOS), the app's icon
//! shows how much mail is unread, and new mail is announced with a system notification.

mod account_settings;
mod compose;
mod form;
mod notifications;
mod rich;
mod search;
mod state;
mod tray;
mod update;
mod view;

use rune::{App, AppSpec, Chrome, Key, KeyEvent, Tx};
use rune_kit::shell;

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

/// ⇧⌘B (and ⌃⌘S on macOS) shows and hides the sidebar, ⌘, opens the settings and Escape closes
/// them (or the update announcement over them, or an account's settings with nothing unsaved),
/// and ⌘N starts a new message. ⌘F searches the list, and Escape in the search field ends the
/// search. With a message open, ⇧⌘U marks it read or unread, ⌘R and ⇧⌘R
/// reply to the sender or everyone, and Escape goes back to the list. While writing, ⌘↩ sends,
/// and ⌘K links the selected text, as it does in a signature.
fn on_key(s: &mut Mail, ev: &KeyEvent, _: &mut Tx) -> bool {
    if shell::is_sidebar_toggle(ev) {
        s.sidebar.toggle();
        return true;
    }
    let link = ev.is_command('k') && !ev.mods.shift;
    let page = &mut s.account_settings;
    if link && s.settings_open && page.open {
        if page.signature.mode == Mode::Visual {
            page.signature.toggle_link();
            return true;
        }
    } else if link && s.compose.open && !s.settings_open && s.compose.body.mode == Mode::Visual {
        s.compose.body.toggle_link();
        return true;
    }
    if s.updater.dialog_open && ev.key == Key::Escape {
        s.close_update();
        return true;
    }
    if ev.is_command(',') && !ev.mods.shift {
        s.open_settings();
        return true;
    }
    if s.settings_open {
        if ev.key != Key::Escape {
            return false;
        }
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
    if s.form.open || s.compose.open {
        return false;
    }
    if ev.is_command('n') && !ev.mods.shift {
        s.compose();
        return true;
    }
    if ev.is_command('f') && !ev.mods.shift && !s.accounts.is_empty() {
        s.focus_search();
        return true;
    }
    if s.reader.is_some() {
        if ev.is_command('r') {
            s.reply(ev.mods.shift);
            return true;
        }
        // ⇧⌘U, as in Mail.
        if ev.is_command('u') && ev.mods.shift {
            s.toggle_seen();
            return true;
        }
        if ev.key == Key::Escape {
            s.close_reader();
            return true;
        }
    }
    false
}
