//! Native macOS banners for new Notifications (JRST-96).
//!
//! The decision lives in `db::notifications::banners_to_fire`; this module
//! only posts. Banners work only when launched from the `.app` bundle
//! (bundle id `com.jait.desktop`); unbundled (`cargo run`) posting is skipped.
//!
//! Clicks (JRST-100): each banner's thread blocks until the banner is clicked
//! or dismissed. A click sends the banner's `BannerTarget` down a process-wide
//! channel that `clicks()` turns into an Iced subscription.

use std::path::Path;
use std::sync::{Mutex, Once, OnceLock};

use iced::futures::channel::mpsc;
use iced::futures::{stream, Stream, StreamExt};

use crate::db::notifications::{Banner, BannerTarget};

struct ClickChannel {
    tx: mpsc::UnboundedSender<BannerTarget>,
    rx: Mutex<Option<mpsc::UnboundedReceiver<BannerTarget>>>,
}

fn channel() -> &'static ClickChannel {
    static CHANNEL: OnceLock<ClickChannel> = OnceLock::new();
    CHANNEL.get_or_init(|| {
        let (tx, rx) = mpsc::unbounded();
        ClickChannel { tx, rx: Mutex::new(Some(rx)) }
    })
}

/// Banner clicks, as an Iced subscription. Only the first stream built gets
/// the receiver; Iced keeps a `run` subscription alive (built once) for as
/// long as it stays in the batch.
pub fn clicks() -> iced::Subscription<BannerTarget> {
    iced::Subscription::run(click_stream)
}

fn click_stream() -> impl Stream<Item = BannerTarget> {
    let rx = channel().rx.lock().ok().and_then(|mut r| r.take());
    match rx {
        Some(rx) => rx.left_stream(),
        None => stream::pending().right_stream(),
    }
}

/// Fixed in `Cargo.toml` `[package.metadata.bundle]`; macOS keys permission off it.
pub const BUNDLE_ID: &str = "com.jait.desktop";

/// True when `exe` lives inside an app bundle (`X.app/Contents/MacOS/`).
pub fn is_bundled_path(exe: &Path) -> bool {
    exe.to_string_lossy().contains(".app/Contents/MacOS")
}

fn is_bundled() -> bool {
    std::env::current_exe().map(|p| is_bundled_path(&p)).unwrap_or(false)
}

/// Post each banner on its own background thread, which waits for a click
/// and forwards the banner's target to `clicks()`. No-op unbundled; failures
/// are ignored.
pub fn post(banners: Vec<Banner>) {
    if banners.is_empty() || !is_bundled() {
        return;
    }
    // The crate wants this at most once, before any send.
    static SET_APP: Once = Once::new();
    let tx = channel().tx.clone();
    for b in banners {
        let tx = tx.clone();
        std::thread::spawn(move || {
            SET_APP.call_once(|| {
                let _ = mac_notification_sys::set_application(BUNDLE_ID);
            });
            let response = mac_notification_sys::Notification::new()
                .title(&b.title)
                .message(&b.body)
                .wait_for_click(true)
                .send();
            if let Ok(mac_notification_sys::NotificationResponse::Click) = response {
                let _ = tx.unbounded_send(b.target);
            }
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detects_bundled_executable() {
        assert!(is_bundled_path(Path::new("/Applications/JAIT.app/Contents/MacOS/JAIT")));
        assert!(!is_bundled_path(Path::new("/repo/target/debug/JAIT")));
    }
}
