//! The app-owned quit flow.
//!
//! The window's close button and Cmd+Q both end up as one `Message::QuitRequested`,
//! handled by `App::quit`. The close button arrives as a window close request
//! (`main.rs` turns off iced's exit-on-close so the app decides). Cmd+Q does not:
//! winit's default Quit menu item calls `terminate:`, which exits the process
//! without telling the app, so `route_menu_quit_to_close` points that item at
//! `performClose:` instead. The key window then treats Cmd+Q like a click on its
//! close button.

/// Points the macOS Quit menu item at the key window's close request.
/// A no-op elsewhere, and on macOS if the menu isn't shaped as winit builds it.
#[cfg(target_os = "macos")]
pub fn route_menu_quit_to_close() {
    use objc2::sel;
    use objc2_app_kit::NSApplication;
    use objc2_foundation::MainThreadMarker;

    let Some(mtm) = MainThreadMarker::new() else {
        return;
    };
    let app = NSApplication::sharedApplication(mtm);
    let Some(main_menu) = (unsafe { app.mainMenu() }) else {
        return;
    };
    for top in unsafe { main_menu.itemArray() }.iter() {
        let Some(submenu) = (unsafe { top.submenu() }) else {
            continue;
        };
        for item in unsafe { submenu.itemArray() }.iter() {
            if unsafe { item.action() } == Some(sel!(terminate:)) {
                unsafe {
                    // A nil target walks the responder chain to the key window.
                    item.setTarget(None);
                    item.setAction(Some(sel!(performClose:)));
                }
            }
        }
    }
}

#[cfg(not(target_os = "macos"))]
pub fn route_menu_quit_to_close() {}
