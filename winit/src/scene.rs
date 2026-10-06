//! iOS: UIKit's scene life cycle.
//!
//! An app linked with the iOS 27 SDK must adopt the scene life cycle, or
//! UIKit stops it at launch (Apple's technical note TN3187); built with an
//! older SDK it still runs. winit 0.30 has no scene support: it makes its
//! window with `initWithFrame:` and no scene, and in the scene life cycle a
//! window without a scene is not shown.
//!
//! This puts every window winit shows into the application's window scene.
//! It works from UIKit's notifications, so no Objective-C class is declared.
//! The scene connects and the window is shown in either order: usually the
//! scene first, since winit sends `Resumed`, on which iced makes its
//! windows, from `UIApplicationDidBecomeActiveNotification`; but a window
//! shown first is held until the scene connects.
//!
//! The application's `Info.plist` must declare the scene, which iced cannot
//! do for it:
//!
//! ```xml
//! <key>UIApplicationSceneManifest</key>
//! <dict>
//!     <key>UIApplicationSupportsMultipleScenes</key>
//!     <false/>
//!     <key>UISceneConfigurations</key>
//!     <dict>
//!         <key>UIWindowSceneSessionRoleApplication</key>
//!         <array>
//!             <dict>
//!                 <key>UISceneConfigurationName</key>
//!                 <string>Default</string>
//!             </dict>
//!         </array>
//!     </dict>
//! </dict>
//! ```
//!
//! Without it UIKit wraps the app in a scene of its own and puts the window
//! in it, so nothing here acts; with the iOS 27 SDK the app is stopped.

// UIKit is reached only through Objective-C calls; each says why it is sound.
#![allow(unsafe_code)]

use std::cell::RefCell;
use std::ptr::NonNull;

use block2::RcBlock;
use objc2::rc::Retained;
use objc2::runtime::NSObjectProtocol;
use objc2_foundation::{
    MainThreadMarker, NSNotification, NSNotificationCenter, NSNotificationName,
    NSOperationQueue,
};
use objc2_ui_kit::{
    UIScene, UISceneWillConnectNotification, UIWindow,
    UIWindowDidBecomeVisibleNotification, UIWindowScene,
};

thread_local! {
    /// The application's window scene, once UIKit has connected it.
    static SCENE: RefCell<Option<Retained<UIWindowScene>>> =
        const { RefCell::new(None) };
    /// Windows shown before the scene connected.
    static PENDING: RefCell<Vec<Retained<UIWindow>>> =
        const { RefCell::new(Vec::new()) };
}

/// Observes the scene and the windows, for the life of the process.
///
/// Called by [`crate::run`] on the main thread, before winit's
/// `UIApplicationMain`.
pub(crate) fn adopt() {
    if MainThreadMarker::new().is_none() {
        log::warn!("iOS scene: not on the main thread; windows left as made");
        return;
    }

    type Notice = (&'static NSNotificationName, fn(&NSNotification));

    // SAFETY: the notification names are UIKit's own constants.
    let notices: [Notice; 2] = unsafe {
        [
            (UISceneWillConnectNotification, scene_connected),
            (UIWindowDidBecomeVisibleNotification, window_visible),
        ]
    };

    // SAFETY: the main queue and the default center are always valid.
    let (center, queue) = unsafe {
        (
            NSNotificationCenter::defaultCenter(),
            NSOperationQueue::mainQueue(),
        )
    };

    for (name, act) in notices {
        // SAFETY: the center passes a valid notification for the call.
        let block = RcBlock::new(move |note: NonNull<NSNotification>| {
            act(unsafe { note.as_ref() });
        });

        // SAFETY: the block runs on the main queue, as UIKit requires.
        let observer = unsafe {
            center.addObserverForName_object_queue_usingBlock(
                Some(name),
                None,
                Some(&queue),
                &block,
            )
        };

        std::mem::forget(observer);
    }
}

fn scene_connected(note: &NSNotification) {
    // SAFETY: this notification's object is the UIScene that connects.
    let Some(scene) = (unsafe { note.object() })
        .map(|object| unsafe { Retained::cast::<UIScene>(object) })
    else {
        return;
    };

    if !scene.is_kind_of::<UIWindowScene>() {
        return;
    }

    // SAFETY: checked just above.
    let scene = unsafe { Retained::cast::<UIWindowScene>(scene) };
    log::debug!("iOS scene: connected");

    for window in PENDING.take() {
        place(&window, &scene);
    }

    SCENE.set(Some(scene));
}

fn window_visible(note: &NSNotification) {
    // SAFETY: this notification's object is the UIWindow.
    let Some(window) = (unsafe { note.object() })
        .map(|object| unsafe { Retained::cast::<UIWindow>(object) })
    else {
        return;
    };

    // SAFETY: a UIKit call on the main thread with a valid window.
    if unsafe { window.windowScene() }.is_some() {
        return;
    }

    match SCENE.with_borrow(Clone::clone) {
        Some(scene) => place(&window, &scene),
        None => PENDING.with_borrow_mut(|pending| pending.push(window)),
    }
}

fn place(window: &UIWindow, scene: &UIWindowScene) {
    // SAFETY: UIKit calls on the main thread with valid objects.
    unsafe {
        window.setWindowScene(Some(scene));
        window.makeKeyAndVisible();
    }

    log::debug!("iOS scene: window placed");
}
