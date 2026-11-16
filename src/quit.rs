//! The app-owned quit flow.
//!
//! The window's close button, Cmd+Q, Dock Quit and an AppleScript quit all end
//! up as one `Message::QuitRequested`, handled by `App::quit`. The close button
//! arrives as a window close request (`main.rs` turns off iced's exit-on-close
//! so the app decides). The others reach AppKit's `terminate:`, which winit's
//! app delegate doesn't intercept, so `install_terminate_hook` adds an
//! `applicationShouldTerminate:` method to that delegate at runtime. It answers
//! `NSTerminateLater` and forwards the request to the app; the app answers
//! later with `reply_to_terminate` (YES once the quit has finished, NO when the
//! user cancels).

use std::sync::atomic::{AtomicBool, Ordering};

/// Whether an `applicationShouldTerminate:` request is waiting on our reply.
/// Guards two things: a second terminate during a quit doesn't start a second
/// quit, and a reply is sent only when AppKit is actually waiting for one.
pub struct PendingTerminate(AtomicBool);

impl PendingTerminate {
    pub const fn new() -> Self {
        Self(AtomicBool::new(false))
    }

    /// A terminate request arrived. True if it is the first, so the app should
    /// start a quit; false if one is already pending (it waits on the same reply).
    pub fn claim(&self) -> bool {
        !self.0.swap(true, Ordering::SeqCst)
    }

    /// The app is about to answer. True if AppKit is waiting for it.
    pub fn take(&self) -> bool {
        self.0.swap(false, Ordering::SeqCst)
    }
}

#[cfg(target_os = "macos")]
static PENDING: PendingTerminate = PendingTerminate::new();

#[cfg(target_os = "macos")]
mod mac {
    use super::PENDING;
    use iced::futures::channel::mpsc::{unbounded, UnboundedReceiver, UnboundedSender};
    use objc2::ffi;
    use objc2::runtime::{AnyObject, Sel};
    use objc2::{msg_send, sel};
    use objc2_app_kit::NSApplication;
    use objc2_foundation::MainThreadMarker;
    use std::sync::{Mutex, OnceLock};

    // NSApplicationTerminateLater, as NSUInteger.
    const NS_TERMINATE_LATER: usize = 2;

    type Channel = (UnboundedSender<()>, Mutex<Option<UnboundedReceiver<()>>>);

    /// Terminate requests from the delegate method to the app's subscription.
    /// Made on first use by either side, so install and subscribe can run in
    /// any order.
    fn channel() -> &'static Channel {
        static CHANNEL: OnceLock<Channel> = OnceLock::new();
        CHANNEL.get_or_init(|| {
            let (tx, rx) = unbounded();
            (tx, Mutex::new(Some(rx)))
        })
    }

    pub fn take_receiver() -> Option<UnboundedReceiver<()>> {
        channel().1.lock().ok()?.take()
    }

    /// `-applicationShouldTerminate:`. Always defers; the app decides.
    extern "C" fn should_terminate(_this: *mut AnyObject, _cmd: Sel, _sender: *mut AnyObject) -> usize {
        if PENDING.claim() {
            let _ = channel().0.unbounded_send(());
        }
        NS_TERMINATE_LATER
    }

    pub fn install() {
        let Some(mtm) = MainThreadMarker::new() else {
            return;
        };
        let app = NSApplication::sharedApplication(mtm);
        let delegate: *mut AnyObject = unsafe { msg_send![&app, delegate] };
        if delegate.is_null() {
            return;
        }
        let class = unsafe { (*delegate).class() };
        // Adding is a no-op (returns NO) if the class already has the method.
        unsafe {
            let imp: unsafe extern "C" fn() = std::mem::transmute(
                should_terminate as extern "C" fn(*mut AnyObject, Sel, *mut AnyObject) -> usize,
            );
            ffi::class_addMethod(
                class as *const _ as *mut ffi::objc_class,
                sel!(applicationShouldTerminate:).as_ptr(),
                Some(imp),
                c"Q@:@".as_ptr(),
            );
        }
    }

    pub fn reply(allow: bool) {
        if !PENDING.take() {
            return;
        }
        let Some(mtm) = MainThreadMarker::new() else {
            return;
        };
        let app = NSApplication::sharedApplication(mtm);
        let _: () = unsafe { msg_send![&app, replyToApplicationShouldTerminate: allow] };
    }
}

/// Adds `applicationShouldTerminate:` to winit's app delegate so Cmd+Q, Dock
/// Quit and AppleScript quit reach `QuitRequested`. Call once the event loop is
/// up (the delegate exists then). A no-op off macOS.
pub fn install_terminate_hook() {
    #[cfg(target_os = "macos")]
    mac::install();
}

/// Answers a pending `applicationShouldTerminate:`: `true` lets the process
/// terminate, `false` cancels it (and any logout, restart or shutdown behind it).
/// Does nothing when no request is pending, as for the close button.
pub fn reply_to_terminate(allow: bool) {
    #[cfg(target_os = "macos")]
    mac::reply(allow);
    #[cfg(not(target_os = "macos"))]
    let _ = allow;
}

/// One item per terminate request from the system.
pub fn terminate_requests() -> iced::futures::stream::BoxStream<'static, ()> {
    use iced::futures::StreamExt;
    #[cfg(target_os = "macos")]
    if let Some(rx) = mac::take_receiver() {
        return rx.boxed();
    }
    iced::futures::stream::pending().boxed()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_the_first_terminate_starts_a_quit() {
        let p = PendingTerminate::new();
        assert!(p.claim());
        assert!(!p.claim());
    }

    #[test]
    fn a_reply_is_owed_only_while_a_request_is_pending() {
        let p = PendingTerminate::new();
        assert!(!p.take());
        p.claim();
        assert!(p.take());
        assert!(!p.take());
    }

    #[test]
    fn after_a_cancel_the_next_terminate_starts_a_new_quit() {
        let p = PendingTerminate::new();
        p.claim();
        p.take();
        assert!(p.claim());
    }
}
