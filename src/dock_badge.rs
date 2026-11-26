//! The Dock icon's red badge while there are unread Notifications (JRST-551).
//!
//! iced doesn't expose the Dock tile, so this sets `NSApp.dockTile.badgeLabel`
//! directly. macOS draws any non-empty label as a red badge; a nil label
//! clears it.

use std::sync::atomic::{AtomicU8, Ordering};

/// Badge text. A bullet reads as a dot (as Slack does) instead of a count.
#[cfg(target_os = "macos")]
const LABEL: &str = "\u{2022}";

/// What the Dock last got: 0 = nothing yet, 1 = cleared, 2 = shown. Skips the
/// AppKit call when nothing changed, since this runs after every message.
static SHOWN: AtomicU8 = AtomicU8::new(0);

/// Show the badge when `unread`, clear it otherwise. Must run on the main
/// thread (iced's `update` does); elsewhere, and off macOS, it does nothing.
pub fn set_unread(unread: bool) {
    let want = if unread { 2 } else { 1 };
    if SHOWN.load(Ordering::Relaxed) == want {
        return;
    }
    #[cfg(target_os = "macos")]
    {
        if !mac::set(unread) {
            return;
        }
    }
    SHOWN.store(want, Ordering::Relaxed);
}

#[cfg(target_os = "macos")]
mod mac {
    use super::LABEL;
    use objc2::msg_send;
    use objc2::runtime::AnyObject;
    use objc2_app_kit::NSApplication;
    use objc2_foundation::{MainThreadMarker, NSString};

    /// Set or clear the badge. False when it couldn't (not on the main thread).
    pub fn set(unread: bool) -> bool {
        let Some(mtm) = MainThreadMarker::new() else {
            return false;
        };
        let app = NSApplication::sharedApplication(mtm);
        let tile: *mut AnyObject = unsafe { msg_send![&app, dockTile] };
        if tile.is_null() {
            return false;
        }
        unsafe {
            if unread {
                let label = NSString::from_str(LABEL);
                let _: () = msg_send![tile, setBadgeLabel: &*label];
            } else {
                let none: *const NSString = std::ptr::null();
                let _: () = msg_send![tile, setBadgeLabel: none];
            }
        }
        true
    }
}
