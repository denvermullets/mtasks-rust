//! Wake-from-sleep notices (JRST-520).
//!
//! A thin edge over `NSWorkspaceDidWakeNotification`: one `()` per wake. The
//! updater machine decides whether a wake is worth a check.

use iced::futures::stream::BoxStream;
use iced::futures::StreamExt;

/// One item per wake from sleep. Never ends; empty off macOS.
pub fn wakes() -> BoxStream<'static, ()> {
    #[cfg(target_os = "macos")]
    if let Some(rx) = mac::subscribe() {
        return rx.boxed();
    }
    iced::futures::stream::pending().boxed()
}

#[cfg(target_os = "macos")]
mod mac {
    use iced::futures::channel::mpsc::{unbounded, UnboundedReceiver, UnboundedSender};
    use objc2::ffi;
    use objc2::runtime::{AnyClass, AnyObject, Sel};
    use objc2::{msg_send, sel};
    use objc2_foundation::NSString;
    use std::sync::OnceLock;

    static SENDER: OnceLock<UnboundedSender<()>> = OnceLock::new();

    extern "C" fn did_wake(_this: *mut AnyObject, _cmd: Sel, _note: *mut AnyObject) {
        if let Some(tx) = SENDER.get() {
            let _ = tx.unbounded_send(());
        }
    }

    /// Registers the observer once. None if already registered or AppKit's
    /// classes can't be found (the subscription then just never fires).
    pub fn subscribe() -> Option<UnboundedReceiver<()>> {
        let (tx, rx) = unbounded();
        SENDER.set(tx).ok()?;
        unsafe {
            let ns_object = AnyClass::get("NSObject")?;
            let workspace_class = AnyClass::get("NSWorkspace")?;
            let class = ffi::objc_allocateClassPair(
                ns_object as *const _ as *mut ffi::objc_class,
                c"JaitWakeObserver".as_ptr(),
                0,
            );
            if class.is_null() {
                return None;
            }
            let imp: unsafe extern "C" fn() = std::mem::transmute(
                did_wake as extern "C" fn(*mut AnyObject, Sel, *mut AnyObject),
            );
            ffi::class_addMethod(class, sel!(didWake:).as_ptr(), Some(imp), c"v@:@".as_ptr());
            ffi::objc_registerClassPair(class);
            let observer: *mut AnyObject = msg_send![class as *mut AnyObject, new];
            let workspace: *mut AnyObject = msg_send![workspace_class, sharedWorkspace];
            if observer.is_null() || workspace.is_null() {
                return None;
            }
            let center: *mut AnyObject = msg_send![workspace, notificationCenter];
            let name = NSString::from_str("NSWorkspaceDidWakeNotification");
            let _: () = msg_send![
                center,
                addObserver: observer,
                selector: sel!(didWake:),
                name: &*name,
                object: std::ptr::null::<AnyObject>()
            ];
        }
        Some(rx)
    }
}
