//! iOS: UIKit's scene life cycle.
//!
//! An app linked with the iOS 27 SDK must adopt the scene life cycle, or
//! UIKit stops it at launch (Apple's technical note TN3187); built with an
//! older SDK it still runs. winit 0.30 has no scene support: it makes its
//! window with `initWithFrame:` and no scene, and in the scene life cycle a
//! window without a scene is not shown.
//!
//! This puts winit's windows into the application's window scene. It works
//! from UIKit's notifications, so no Objective-C class is declared.
//!
//! - Only winit's own windows (class `WinitUIWindow`) are placed, and only
//!   into a scene with the application role. A window of UIKit's or of
//!   another library, or a scene for an external display, is left alone.
//! - The scene connects and the window is shown in either order. iced makes
//!   its windows when it handles the `window::open` action it sends itself
//!   at boot through winit's event loop proxy, and winit hands that over
//!   only once its run loop turns after launch: usually after the scene has
//!   connected. A window shown first is held until the scene connects.
//! - When UIKit disconnects a scene (to reclaim memory, or when it is
//!   closed), the windows in it move into the newest application scene still
//!   connected, if there is one, or are held for the next one that connects.
//!   Every connected application scene is tracked, so with several scenes
//!   (an iPad app with `UIApplicationSupportsMultipleScenes`) closing the
//!   newest one hands its windows to an older one. A window iced has hidden
//!   stays hidden when it moves.
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
//! [`adopt`] checks for it first: when it is missing it prints the block
//! above to stderr and the log, and panics in a debug build, since UIKit's
//! stop leaves a trace only in the system log.

// UIKit is reached only through Objective-C calls; each says why it is sound.
#![allow(unsafe_code)]

use std::cell::RefCell;
use std::ptr::NonNull;

use block2::RcBlock;
use objc2::Message;
use objc2::rc::{Retained, Weak};
use objc2::runtime::{AnyClass, NSObjectProtocol};
use objc2_foundation::{
    MainThreadMarker, NSBundle, NSNotification, NSNotificationCenter,
    NSNotificationName, NSOperationQueue, ns_string,
};
use objc2_ui_kit::{
    UIScene, UISceneDidDisconnectNotification, UISceneWillConnectNotification,
    UIWindow, UIWindowDidBecomeVisibleNotification, UIWindowScene,
    UIWindowSceneSessionRoleApplication,
};

/// The `UIApplicationSceneManifest` block of the module docs, which
/// [`adopt`] prints when the `Info.plist` lacks it. Keep the two the same.
const MANIFEST: &str = "\
<key>UIApplicationSceneManifest</key>
<dict>
    <key>UIApplicationSupportsMultipleScenes</key>
    <false/>
    <key>UISceneConfigurations</key>
    <dict>
        <key>UIWindowSceneSessionRoleApplication</key>
        <array>
            <dict>
                <key>UISceneConfigurationName</key>
                <string>Default</string>
            </dict>
        </array>
    </dict>
</dict>";

/// The class of winit's windows (winit 0.30.13
/// src/platform_impl/ios/window.rs:34-43, the same in 0.30.12).
const WINIT_WINDOW: &str = "WinitUIWindow";

thread_local! {
    /// The application scenes UIKit has connected and not yet disconnected,
    /// oldest first. A window shown goes into the newest. Weak: UIKit keeps
    /// a connected scene alive, and a disconnected one must not be.
    static SCENES: RefCell<Vec<Weak<UIWindowScene>>> =
        const { RefCell::new(Vec::new()) };
    /// winit's windows, once shown. Weak, since iced owns them: a window it
    /// has closed must not be kept alive or shown again.
    static WINDOWS: RefCell<Vec<Weak<UIWindow>>> =
        const { RefCell::new(Vec::new()) };
    /// winit's windows waiting for an application scene: shown before one
    /// connected, or left behind by one that disconnected.
    static PENDING: RefCell<Vec<Weak<UIWindow>>> =
        const { RefCell::new(Vec::new()) };
}

/// Checks the `Info.plist`, then observes the scenes and the windows for the
/// life of the process.
///
/// Called by [`crate::run`] on the main thread, before winit's
/// `UIApplicationMain`.
pub(crate) fn adopt() {
    check_manifest();

    if MainThreadMarker::new().is_none() {
        log::warn!("iOS scene: not on the main thread; windows left as made");
        return;
    }

    type Notice = (&'static NSNotificationName, fn(&NSNotification));

    // SAFETY: the notification names are UIKit's own constants.
    let notices: [Notice; 3] = unsafe {
        [
            (UISceneWillConnectNotification, scene_connected),
            (UISceneDidDisconnectNotification, scene_disconnected),
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

/// Says loudly that the `Info.plist` has no scene manifest: UIKit stops such
/// an app at launch with the iOS 27 SDK, and says why only in the system log.
fn check_manifest() {
    let declared =
        NSBundle::mainBundle().infoDictionary().is_some_and(|info| {
            info.get(ns_string!("UIApplicationSceneManifest")).is_some()
        });

    if declared {
        return;
    }

    let message = format!(
        "iced: the app's Info.plist has no UIApplicationSceneManifest. \
         An app built with the iOS 27 SDK must adopt UIKit's scene life \
         cycle, and without this key UIKit stops it at launch (Apple TN3187), \
         saying why only in the system log. Add this to the top-level <dict> \
         of the app's Info.plist:\n\n{MANIFEST}\n"
    );

    eprintln!("{message}");
    log::error!("{message}");

    if cfg!(debug_assertions) {
        panic!(
            "iOS scene: the Info.plist has no UIApplicationSceneManifest \
             (see the message above)"
        );
    }
}

fn scene_connected(note: &NSNotification) {
    let Some(scene) = application_scene(note) else {
        return;
    };

    log::debug!("iOS scene: connected");

    SCENES.with_borrow_mut(|scenes| remember(scenes, &scene));

    for window in live(&PENDING.take()) {
        place(&window, &scene);
    }
}

fn scene_disconnected(note: &NSNotification) {
    let Some(scene) = application_scene(note) else {
        return;
    };

    log::debug!("iOS scene: disconnected");

    SCENES.with_borrow_mut(|scenes| {
        scenes.retain(|known| {
            known
                .load()
                .is_some_and(|known| !std::ptr::addr_eq(&*known, &*scene))
        });
    });

    // The windows the scene held, or that UIKit has already taken out of it.
    let orphans: Vec<_> = WINDOWS
        .with_borrow(|windows| live(windows))
        .into_iter()
        .filter(|window| {
            // SAFETY: a UIKit call on the main thread with a valid window.
            unsafe { window.windowScene() }
                .is_none_or(|held| std::ptr::addr_eq(&*held, &*scene))
        })
        .collect();

    // Another application scene may still be connected; else the next one.
    match newest_scene() {
        Some(scene) => {
            for window in orphans {
                place(&window, &scene);
            }
        }
        None => PENDING.with_borrow_mut(|pending| {
            for window in &orphans {
                remember(pending, window);
            }
        }),
    }
}

fn window_visible(note: &NSNotification) {
    // SAFETY: this notification's object is the UIWindow.
    let Some(window) = (unsafe { note.object() })
        .map(|object| unsafe { Retained::cast::<UIWindow>(object) })
    else {
        return;
    };

    // Not a window of winit's: a system or another library's window, which
    // must keep its own scene and must not take the key window from iced.
    if !AnyClass::get(WINIT_WINDOW)
        .is_some_and(|class| window.isKindOfClass(class))
    {
        return;
    }

    WINDOWS.with_borrow_mut(|windows| remember(windows, &window));

    // SAFETY: a UIKit call on the main thread with a valid window.
    if unsafe { window.windowScene() }.is_some() {
        return;
    }

    match newest_scene() {
        Some(scene) => place(&window, &scene),
        None => PENDING.with_borrow_mut(|pending| remember(pending, &window)),
    }
}

/// The newest application scene still connected, if any.
fn newest_scene() -> Option<Retained<UIWindowScene>> {
    SCENES.with_borrow(|scenes| scenes.iter().rev().find_map(Weak::load))
}

/// The scene a scene notification is about, if it is an application scene:
/// a window scene with the application role, and not, say, an external
/// display's or CarPlay's.
fn application_scene(note: &NSNotification) -> Option<Retained<UIWindowScene>> {
    // SAFETY: a scene notification's object is the UIScene it is about.
    let scene = unsafe { note.object() }
        .map(|object| unsafe { Retained::cast::<UIScene>(object) })?;

    if !scene.is_kind_of::<UIWindowScene>() {
        return None;
    }

    // SAFETY: UIKit calls on the main thread with a valid scene, which has
    // its session from the start; the role is UIKit's own constant.
    let is_application = unsafe {
        scene
            .session()
            .role()
            .isEqualToString(UIWindowSceneSessionRoleApplication)
    };

    if !is_application {
        log::debug!("iOS scene: ignored a scene of another role");
        return None;
    }

    // SAFETY: a `UIWindowScene`, checked above.
    Some(unsafe { Retained::cast::<UIWindowScene>(scene) })
}

/// Adds `object` to `objects` once, at the end, dropping the objects that
/// are gone.
fn remember<T>(objects: &mut Vec<Weak<T>>, object: &Retained<T>)
where
    T: Message + objc2::mutability::IsIdCloneable,
{
    objects.retain(|known| known.load().is_some());

    let known = objects.iter().any(|known| {
        known
            .load()
            .is_some_and(|known| std::ptr::addr_eq(&*known, &**object))
    });

    if !known {
        objects.push(Weak::from_retained(object));
    }
}

/// The windows still alive.
fn live(windows: &[Weak<UIWindow>]) -> Vec<Retained<UIWindow>> {
    windows.iter().filter_map(Weak::load).collect()
}

/// Puts `window` into `scene`, and makes it the key window unless it is
/// hidden: a window iced has hidden is moved, not shown.
fn place(window: &UIWindow, scene: &UIWindowScene) {
    // SAFETY: UIKit calls on the main thread with valid objects.
    unsafe {
        window.setWindowScene(Some(scene));
    }

    if window.isHidden() {
        log::debug!("iOS scene: hidden window placed");
        return;
    }

    window.makeKeyAndVisible();

    log::debug!("iOS scene: window placed");
}
