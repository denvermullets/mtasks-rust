//! Native macOS banners for new Notifications (JRST-96).
//!
//! The decision lives in `db::notifications::banners_to_fire`; this module
//! only posts. Banners work only when launched from the `.app` bundle
//! (bundle id `com.jait.desktop`); unbundled (`cargo run`) posting is skipped.

use std::path::Path;

use crate::db::notifications::Banner;

/// Fixed in `Cargo.toml` `[package.metadata.bundle]`; macOS keys permission off it.
pub const BUNDLE_ID: &str = "com.jait.desktop";

/// True when `exe` lives inside an app bundle (`X.app/Contents/MacOS/`).
pub fn is_bundled_path(exe: &Path) -> bool {
    exe.to_string_lossy().contains(".app/Contents/MacOS")
}

fn is_bundled() -> bool {
    std::env::current_exe().map(|p| is_bundled_path(&p)).unwrap_or(false)
}

/// Post banners on a background thread. No-op unbundled; failures are ignored.
pub fn post(banners: Vec<Banner>) {
    if banners.is_empty() || !is_bundled() {
        return;
    }
    std::thread::spawn(move || {
        // Errors if already set; harmless.
        let _ = mac_notification_sys::set_application(BUNDLE_ID);
        for b in banners {
            let _ = mac_notification_sys::Notification::new()
                .title(&b.title)
                .message(&b.body)
                .asynchronous(true)
                .send();
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detects_bundled_executable() {
        assert!(is_bundled_path(Path::new("/Applications/JAIT.app/Contents/MacOS/mtasks-rusty")));
        assert!(!is_bundled_path(Path::new("/repo/target/debug/mtasks-rusty")));
    }
}
