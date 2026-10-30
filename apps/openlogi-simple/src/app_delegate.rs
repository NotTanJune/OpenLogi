//! The application delegate and the main menu.
//!
//! The app behaves like the upstream GUI: closing the last window quits it
//! (the agent keeps running and the menu-bar item reopens the window), a
//! `openlogi://show`-style link or a Dock click brings the window back, and
//! `openlogi://quit` quits the app.

#![expect(
    unsafe_code,
    reason = "objc2 calls: super-init, the delegate protocol methods and menu item actions"
)]

use objc2::rc::Retained;
use objc2::runtime::{NSObject, NSObjectProtocol, Sel};
use objc2::{MainThreadMarker, MainThreadOnly, define_class, msg_send, sel};
use objc2_app_kit::{NSApplication, NSApplicationDelegate, NSMenu, NSMenuItem};
use objc2_foundation::{NSArray, NSString, NSURL};
use openlogi_core::brand::DeeplinkCommand;
use openlogi_simple::launch;
use tracing::warn;

use crate::{ui, window};

define_class!(
    // SAFETY: NSObject has no subclassing requirements, and `AppDelegate` does
    // not implement `Drop`.
    #[unsafe(super(NSObject))]
    #[thread_kind = MainThreadOnly]
    #[name = "OpenLogiSimpleAppDelegate"]
    pub(crate) struct AppDelegate;

    unsafe impl NSObjectProtocol for AppDelegate {}

    unsafe impl NSApplicationDelegate for AppDelegate {
        #[unsafe(method(applicationShouldTerminateAfterLastWindowClosed:))]
        fn should_terminate_after_last_window_closed(&self, _app: &NSApplication) -> bool {
            true
        }

        #[unsafe(method(applicationShouldHandleReopen:hasVisibleWindows:))]
        fn should_handle_reopen(&self, _app: &NSApplication, _has_visible_windows: bool) -> bool {
            self.show_window();
            false
        }

        #[unsafe(method(application:openURLs:))]
        fn open_urls(&self, app: &NSApplication, urls: &NSArray<NSURL>) {
            for url in urls {
                let text = url.absoluteString().map(|text| text.to_string());
                match text.as_deref().and_then(DeeplinkCommand::parse_url) {
                    Some(DeeplinkCommand::Quit) => {
                        // Before terminating, or the agent link's reconnect
                        // reflex can start the agent again during teardown.
                        launch::mark_suite_quitting();
                        app.terminate(None);
                    }
                    // This window is the only one: Settings, About and the
                    // update check all land on it.
                    Some(_) => self.show_window(),
                    None => warn!(url = ?text, "unknown openlogi:// command, ignoring"),
                }
            }
        }
    }
);

impl AppDelegate {
    /// A delegate for the application.
    pub fn new(mtm: MainThreadMarker) -> Retained<Self> {
        let this = Self::alloc(mtm).set_ivars(());
        // SAFETY: `init` initializes our freshly allocated NSObject subclass.
        unsafe { msg_send![super(this), init] }
    }

    fn show_window(&self) {
        if let Some(window) = ui::window() {
            window::present(self.mtm(), &window);
        }
    }
}

/// Install the main menu: Hide and Quit under the app menu, Minimise and Close
/// under Window. Without it the app has no Cmd+Q or Cmd+W.
pub fn install_main_menu(mtm: MainThreadMarker, app: &NSApplication) {
    let bar = NSMenu::new(mtm);

    let app_menu = NSMenu::new(mtm);
    app_menu.addItem(&item(mtm, "Hide OpenLogi", sel!(hide:), "h"));
    app_menu.addItem(&NSMenuItem::separatorItem(mtm));
    app_menu.addItem(&item(mtm, "Quit OpenLogi", sel!(terminate:), "q"));
    bar.addItem(&holder(mtm, &app_menu));

    let window_menu = NSMenu::initWithTitle(mtm.alloc(), &NSString::from_str("Window"));
    window_menu.addItem(&item(mtm, "Minimize", sel!(performMiniaturize:), "m"));
    window_menu.addItem(&item(mtm, "Close", sel!(performClose:), "w"));
    bar.addItem(&holder(mtm, &window_menu));

    app.setMainMenu(Some(&bar));
    app.setWindowsMenu(Some(&window_menu));
}

/// A menu-bar entry that holds `submenu`.
fn holder(mtm: MainThreadMarker, submenu: &NSMenu) -> Retained<NSMenuItem> {
    let holder = NSMenuItem::new(mtm);
    holder.setSubmenu(Some(submenu));
    holder
}

/// An item sending `action` down the responder chain (no target), with
/// Command+`key` as its shortcut.
fn item(mtm: MainThreadMarker, title: &str, action: Sel, key: &str) -> Retained<NSMenuItem> {
    // SAFETY: `initWithTitle:action:keyEquivalent:` is NSMenuItem's designated
    // initializer; the strings outlive the call, and `action` is a standard
    // NSApplication or NSWindow action the responder chain resolves.
    unsafe {
        NSMenuItem::initWithTitle_action_keyEquivalent(
            NSMenuItem::alloc(mtm),
            &NSString::from_str(title),
            Some(action),
            &NSString::from_str(key),
        )
    }
}
