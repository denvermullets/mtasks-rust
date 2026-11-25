//! Native window effects iced doesn't expose: the background blur behind a
//! translucent window (see `theme::set_window_opacity`).
//!
//! The blur uses `CGSSetWindowBackgroundBlurRadius`, a private CoreGraphics
//! call that Terminal.app, winit's own `set_blur`, Ghostty and kitty all use.
//! Being private, it would block a Mac App Store release.

use iced::window;
use iced::Task;

/// Largest blur radius Settings offers.
pub const MAX_BLUR_RADIUS: u8 = 60;

/// Set the background blur behind the latest window. `radius` 0 turns it off.
pub fn set_blur<M: Send + 'static>(radius: u8) -> Task<M> {
    let radius = radius.min(MAX_BLUR_RADIUS);
    window::latest()
        .and_then(move |id| {
            window::run(id, move |w| {
                if let Ok(handle) = w.window_handle() {
                    apply_blur(handle, radius);
                }
            })
        })
        .discard()
}

#[cfg(target_os = "macos")]
fn apply_blur(handle: window::raw_window_handle::WindowHandle<'_>, radius: u8) {
    use objc2::msg_send;
    use objc2::runtime::AnyObject;
    use window::raw_window_handle::RawWindowHandle;

    #[link(name = "CoreGraphics", kind = "framework")]
    extern "C" {
        fn CGSMainConnectionID() -> *mut AnyObject;
        fn CGSSetWindowBackgroundBlurRadius(
            connection_id: *mut AnyObject,
            window_id: isize,
            radius: i64,
        ) -> i32;
    }

    let RawWindowHandle::AppKit(appkit) = handle.as_raw() else {
        return;
    };
    // SAFETY: iced runs this on the main thread with the window alive; the
    // handle's `ns_view` is the window's content view.
    unsafe {
        let view = appkit.ns_view.as_ptr() as *mut AnyObject;
        let ns_window: *mut AnyObject = msg_send![view, window];
        if ns_window.is_null() {
            return;
        }
        let number: isize = msg_send![ns_window, windowNumber];
        CGSSetWindowBackgroundBlurRadius(CGSMainConnectionID(), number, radius as i64);
    }
}

#[cfg(not(target_os = "macos"))]
fn apply_blur(_handle: window::raw_window_handle::WindowHandle<'_>, _radius: u8) {}
