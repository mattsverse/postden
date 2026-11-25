//! The tray icon that brings the window back once it's closed, on Linux and Windows. On macOS
//! the Dock icon does that instead, as it does for Mail.

use rune::{AppHandle, Tray, TrayEvent, TrayIcon, TrayItem, TraySpec};

use crate::state::Mail;

const OPEN: &str = "open";
const QUIT: &str = "quit";

/// Starts the tray icon off the UI thread, as Rune waits for the session bus to set it up on
/// Linux, and hands it to the app once it's there. Until then, and where there's no tray (macOS,
/// or the icon failed to start), closing the window quits.
pub(crate) fn spawn(handle: AppHandle<Mail>) {
    let spawned = std::thread::Builder::new()
        .name("tray".into())
        .spawn(move || {
            if let Some(tray) = start(handle.clone()) {
                handle.update(move |s: &mut Mail, _| s.attach_tray(tray));
            }
        });
    // Without a thread there's no tray, as where there's none.
    drop(spawned);
}

/// The tray icon: a click on it shows the window, and its menu shows the window or quits.
fn start(handle: AppHandle<Mail>) -> Option<Tray> {
    let menu = vec![
        TrayItem::action(OPEN, "Open Postden"),
        TrayItem::Separator,
        TrayItem::action(QUIT, "Quit Postden"),
    ];
    let spec = TraySpec::new(crate::APP_ID, "Postden", icon()?)
        .tooltip("Postden")
        .icon_name(crate::APP_ID)
        .menu(menu);
    Tray::new(spec, move |event| match event {
        TrayEvent::Activate { token } => {
            crate::state::reopened();
            handle.activate(token.unwrap_or_default());
        }
        TrayEvent::Menu(id) if id == OPEN => {
            crate::state::reopened();
            handle.open();
        }
        TrayEvent::Menu(id) if id == QUIT => handle.quit(),
        _ => {}
    })
    .ok()
}

/// Whether a tray host shows tray icons: Rune makes the icon either way, and GNOME has no host
/// without an extension. Asks the session bus's `StatusNotifierWatcher`; no watcher, no answer
/// within a second, or any other failure counts as no host.
#[cfg(target_os = "linux")]
pub(crate) async fn host_registered() -> bool {
    const WATCHER: &str = "org.kde.StatusNotifierWatcher";

    let ask = async {
        let bus = zbus::Connection::session().await?;
        let watcher = zbus::Proxy::new(&bus, WATCHER, "/StatusNotifierWatcher", WATCHER).await?;
        watcher
            .get_property::<bool>("IsStatusNotifierHostRegistered")
            .await
    };
    matches!(
        tokio::time::timeout(std::time::Duration::from_secs(1), ask).await,
        Ok(Ok(true))
    )
}

/// The app icon as RGBA pixels, decoded from the PNG the Linux packages install.
fn icon() -> Option<TrayIcon> {
    const PNG: &[u8] = include_bytes!("../../../assets/linux/icons/32.png");

    let mut decoder = png::Decoder::new(std::io::Cursor::new(PNG));
    decoder.set_transformations(png::Transformations::EXPAND | png::Transformations::ALPHA);
    let mut reader = decoder.read_info().ok()?;
    let mut pixels = vec![0; reader.output_buffer_size()?];
    let frame = reader.next_frame(&mut pixels).ok()?;
    if frame.color_type != png::ColorType::Rgba || frame.bit_depth != png::BitDepth::Eight {
        return None;
    }
    pixels.truncate(frame.buffer_size());
    Some(TrayIcon::rgba(frame.width, frame.height, pixels))
}

#[cfg(test)]
mod tests {
    #[test]
    fn the_icon_decodes_to_rgba() {
        assert!(super::icon().is_some(), "the tray icon decodes");
    }
}
