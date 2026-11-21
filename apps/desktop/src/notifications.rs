//! System notifications for new mail: Notification Center on macOS (for the bundled app only),
//! toasts on Windows, and the desktop's notification server over D-Bus on Linux. Clicking one
//! brings the window back with the message open, or the account's inbox for several.

use std::sync::Mutex;

use rune::AppHandle;

use crate::state::Mail;

/// What a click on a notification shows: one message, or an account's inbox.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Target {
    pub(crate) account: String,
    pub(crate) mailbox: String,
    /// The mailbox's UIDVALIDITY when the notification was posted. Once it changes, the server
    /// may give the message's UID to another one.
    pub(crate) uid_validity: u32,
    /// The message, when the notification is about just one.
    pub(crate) uid: Option<u32>,
}

impl Target {
    /// The message to open, given the mailbox's UIDVALIDITY now (`None` when it isn't cached):
    /// none once the mailbox started over, as its UID may name another message by then.
    pub(crate) fn message(&self, uid_validity: Option<u32>) -> Option<u32> {
        self.uid.filter(|_| uid_validity == Some(self.uid_validity))
    }
}

/// A notification about new mail, ready to post.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Alert {
    pub(crate) title: String,
    pub(crate) body: String,
    pub(crate) target: Target,
}

impl Alert {
    /// Unique per batch of mail, so a notification never replaces one still worth reading.
    #[cfg(not(target_os = "linux"))]
    fn id(&self) -> String {
        let Target {
            account,
            mailbox,
            uid,
            ..
        } = &self.target;
        let batch = uid.map_or_else(
            || jiff::Timestamp::now().as_millisecond().to_string(),
            |uid| uid.to_string(),
        );
        format!("{account}/{mailbox}/{batch}")
    }
}

const ACCOUNT: &str = "account";
const MAILBOX: &str = "mailbox";
const UID: &str = "uid";
const UID_VALIDITY: &str = "uid_validity";

/// Who clicks are handed to, once [`listen`] ran.
static CLICKS: Mutex<Option<AppHandle<Mail>>> = Mutex::new(None);

/// Gives the process the identity its toasts come from, which is also what the taskbar groups
/// the window under. Call it first thing in `main`, before the window opens. macOS takes the
/// identity from the bundle, and Linux's notifications name the app themselves.
#[cfg(target_os = "windows")]
pub(crate) fn register() {
    let mut identity = rune_notify::Identity::new(crate::APP_ID, "Postden");
    if let Some(icon) = windows_icon() {
        identity = identity.icon(icon);
    }
    // Without an identity nothing posts, which is all there is to do about it.
    drop(rune_notify::register(&identity));
}

/// The app icon as a file, for Windows to show on toasts: it names the icon by path. Written to
/// the data directory, and rewritten only when it changed.
#[cfg(target_os = "windows")]
fn windows_icon() -> Option<std::path::PathBuf> {
    const PNG: &[u8] = include_bytes!("../../../assets/linux/icons/256.png");

    let path = postden_app::data_dir(None).ok()?.join("icon.png");
    if std::fs::read(&path).ok().as_deref() != Some(PNG) {
        std::fs::create_dir_all(path.parent()?).ok()?;
        std::fs::write(&path, PNG).ok()?;
    }
    Some(path)
}

/// Hands clicks on the app's notifications to `handle` from now on. Call it at launch, so a click
/// that launched the app on macOS arrives too.
pub(crate) fn listen(handle: AppHandle<Mail>) {
    *CLICKS
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(handle);
    rune_notify::on_click(|click| {
        let number = |key| click.get(key).and_then(|n| n.parse().ok());
        if let (Some(account), Some(mailbox), Some(uid_validity)) =
            (click.get(ACCOUNT), click.get(MAILBOX), number(UID_VALIDITY))
        {
            clicked(Target {
                account: account.to_owned(),
                mailbox: mailbox.to_owned(),
                uid_validity,
                uid: number(UID),
            });
        }
    });
}

/// A notification was clicked, on any thread.
fn clicked(target: Target) {
    let clicks = CLICKS
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    if let Some(handle) = clicks.as_ref() {
        crate::state::reopened();
        handle.update(move |s: &mut Mail, _| s.notification_clicked(&target));
    }
}

/// Posts `alert`. On Linux it goes over D-Bus on `runtime`, so the UI thread doesn't wait for
/// the session bus.
#[cfg_attr(
    not(target_os = "linux"),
    expect(unused_variables, reason = "only Linux posts off the UI thread")
)]
pub(crate) fn post(alert: &Alert, runtime: &tokio::runtime::Handle) {
    #[cfg(target_os = "linux")]
    {
        let alert = alert.clone();
        runtime.spawn(async move { linux::post(alert).await });
    }
    #[cfg(not(target_os = "linux"))]
    {
        let Target {
            account,
            mailbox,
            uid_validity,
            uid,
        } = &alert.target;
        let mut n = rune_notify::Notification::new(alert.id(), &alert.title)
            .body(&alert.body)
            .info(ACCOUNT, account)
            .info(MAILBOX, mailbox)
            .info(UID_VALIDITY, uid_validity.to_string());
        if let Some(uid) = uid {
            n = n.info(UID, uid.to_string());
        }
        // Where the app can't post, as outside a bundle on macOS, new mail goes unannounced.
        rune_notify::post(&n);
    }
}

/// The freedesktop notification spec, which every Linux desktop's notification server speaks.
#[cfg(target_os = "linux")]
mod linux {
    use std::collections::HashMap;
    use std::sync::Mutex;

    use futures::StreamExt;
    use tokio::sync::OnceCell;
    use zbus::Proxy;
    use zbus::zvariant::Value;

    use super::{Alert, Target, clicked};

    const SERVER: &str = "org.freedesktop.Notifications";
    const PATH: &str = "/org/freedesktop/Notifications";
    /// The action a click on the notification itself invokes.
    const DEFAULT: &str = "default";

    /// The notification server, once connected; `None` when there's none to connect to.
    static SERVER_PROXY: OnceCell<Option<Proxy<'static>>> = OnceCell::const_new();
    /// What each notification still shown opens, by the id the server gave it.
    static POSTED: Mutex<Option<HashMap<u32, Target>>> = Mutex::new(None);

    fn posted<R>(f: impl FnOnce(&mut HashMap<u32, Target>) -> R) -> R {
        let mut posted = POSTED
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        f(posted.get_or_insert_with(HashMap::new))
    }

    pub(super) async fn post(alert: Alert) {
        let Some(server) = SERVER_PROXY.get_or_init(connect).await else {
            return;
        };
        let hints = HashMap::from([
            ("desktop-entry", Value::from(crate::APP_ID)),
            ("category", Value::from("email.arrived")),
        ]);
        let text = escape(&alert.body);
        let body = (
            "Postden",
            0_u32,
            crate::APP_ID,
            alert.title.as_str(),
            text.as_str(),
            vec![DEFAULT, "Open"],
            hints,
            -1_i32,
        );
        if let Ok(id) = server.call::<_, _, u32>("Notify", &body).await {
            posted(|p| p.insert(id, alert.target));
        }
    }

    /// `text` as the body's markup, which servers may read: mail's subjects and senders show as
    /// written, never as formatting, links or images.
    fn escape(text: &str) -> String {
        text.replace('&', "&amp;")
            .replace('<', "&lt;")
            .replace('>', "&gt;")
    }

    /// Connects to the notification server and starts hearing its clicks.
    async fn connect() -> Option<Proxy<'static>> {
        let bus = zbus::Connection::session().await.ok()?;
        let server = Proxy::new(&bus, SERVER, PATH, SERVER).await.ok()?;
        // One stream for both signals, so they're handled in the order the server sent them: a
        // click is followed by its notification closing, which mustn't forget what it opens first.
        let mut signals = server.receive_all_signals().await.ok()?;
        tokio::spawn(async move {
            while let Some(message) = signals.next().await {
                let header = message.header();
                let Some(member) = header.member() else {
                    continue;
                };
                let body = message.body();
                match member.as_str() {
                    "ActionInvoked" => {
                        let Ok((id, action)) = body.deserialize::<(u32, String)>() else {
                            continue;
                        };
                        if action == DEFAULT
                            && let Some(target) = posted(|p| p.remove(&id))
                        {
                            clicked(target);
                        }
                    }
                    "NotificationClosed" => {
                        if let Ok((id, _reason)) = body.deserialize::<(u32, u32)>() {
                            posted(|p| p.remove(&id));
                        }
                    }
                    _ => {}
                }
            }
        });
        Some(server)
    }

    #[cfg(test)]
    mod tests {
        #[test]
        fn bodies_show_markup_as_text() {
            assert_eq!(
                super::escape("<b>Q&A</b> > soon"),
                "&lt;b&gt;Q&amp;A&lt;/b&gt; &gt; soon"
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use super::Target;

    #[test]
    fn a_message_opens_only_while_its_mailbox_holds_the_same_uids() {
        let target = |uid| Target {
            account: "me@example.com".into(),
            mailbox: "INBOX".into(),
            uid_validity: 7,
            uid,
        };
        assert_eq!(target(Some(3)).message(Some(7)), Some(3), "same mailbox");
        assert_eq!(target(Some(3)).message(Some(8)), None, "started over");
        assert_eq!(target(Some(3)).message(None), None, "not cached");
        assert_eq!(target(None).message(Some(7)), None, "several messages");
    }
}
