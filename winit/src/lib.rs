//! A windowing shell for Iced, on top of [`winit`].
//!
//! ![The native path of the Iced ecosystem](https://github.com/iced-rs/iced/blob/0525d76ff94e828b7b21634fa94a747022001c83/docs/graphs/native.png?raw=true)
//!
//! `iced_winit` offers some convenient abstractions on top of [`iced_runtime`]
//! to quickstart development when using [`winit`].
//!
//! It exposes a renderer-agnostic [`Program`] trait that can be implemented
//! and then run with a simple call. The use of this trait is optional.
//!
//! Additionally, a [`conversion`] module is available for users that decide to
//! implement a custom event loop.
//!
//! [`iced_runtime`]: https://github.com/iced-rs/iced/tree/0.14/runtime
//! [`winit`]: https://github.com/rust-windowing/winit
//! [`conversion`]: crate::conversion
#![doc(
    html_logo_url = "https://raw.githubusercontent.com/iced-rs/iced/9ab6923e943f784985e9ef9ca28b10278297225d/docs/logo.svg"
)]
#![cfg_attr(docsrs, feature(doc_cfg))]
pub use iced_debug as debug;
pub use iced_program as program;
pub use program::core;
pub use program::graphics;
pub use program::runtime;
pub use runtime::futures;
pub use winit;

pub mod clipboard;
pub mod conversion;
pub mod icm;

mod broadcast;
mod error;
mod proxy;
mod window;

pub use clipboard::Clipboard;
pub use error::Error;
pub use proxy::Proxy;

mod safe_area;
pub use safe_area::{SafeArea, safe_area};

use crate::core::mouse;
use crate::core::renderer;
use crate::core::theme;
use crate::core::time::Instant;
use crate::core::widget::operation;
use crate::core::{Point, Size};
use crate::futures::futures::channel::mpsc;
use crate::futures::futures::channel::oneshot;
use crate::futures::futures::task;
use crate::futures::futures::{Future, StreamExt};
use crate::futures::subscription;
use crate::futures::{Executor, Runtime};
use crate::graphics::{Compositor, Shell, compositor};
use crate::runtime::image;
use crate::runtime::system;
use crate::runtime::user_interface::{self, UserInterface};
use crate::runtime::{Action, Task};

use program::Program;
use window::WindowManager;

use rustc_hash::FxHashMap;
use std::borrow::Cow;
use std::mem::ManuallyDrop;
use std::slice;
use std::sync::Arc;

#[cfg(any(target_os = "android", target_os = "ios", test))]
mod appearance;
#[cfg(any(target_os = "ios", test))]
mod ios_sdk;
#[cfg(target_os = "ios")]
mod scene;

#[cfg(target_os = "android")]
static ANDROID_APP: std::sync::Mutex<
    Option<winit::platform::android::activity::AndroidApp>,
> = std::sync::Mutex::new(None);

/// Hands the `AndroidApp` received by `android_main` to the shell.
///
/// Must be called before [`run`] (or `iced::application(..).run()`), each
/// time `android_main` runs: Android calls `android_main` again, with a new
/// `AndroidApp`, for every Activity it starts in the same process, and
/// [`run`] takes the one it is given.
#[cfg(target_os = "android")]
pub fn set_android_app(app: winit::platform::android::activity::AndroidApp) {
    *ANDROID_APP.lock().expect("Lock AndroidApp") = Some(app);
}

#[cfg(target_os = "android")]
std::thread_local! {
    /// The event loop that last ran on this thread ended because Android
    /// destroyed its Activity.
    static ACTIVITY_DESTROYED: std::cell::Cell<bool> =
        const { std::cell::Cell::new(false) };
}

/// Whether the event loop that last ran on this thread, through [`run`],
/// ended because Android destroyed its Activity.
///
/// Android destroys an Activity on Back, for a configuration change the
/// manifest's `android:configChanges` does not list, or with the developer
/// option "Don't keep activities", and may then start a new one in the same
/// process, which calls `android_main` again. (To free memory it kills the
/// whole process instead: nothing returns, and the next launch is a cold
/// start.) When the Activity is destroyed, [`run`] returns
/// once the application is dropped, and `android_main` must return too: the
/// Activity's `onDestroy` waits for it.
///
/// When this is `false` after [`run`] returned, the application stopped by
/// itself (an error, such as no usable graphics backend), and its Activity
/// is still on screen. android-activity finishes that Activity once
/// `android_main` returns, but with android-activity 0.6.0 the Activity's
/// `onPause` then waits for the stopped thread and the app stops responding
/// (0.6.1 fixed it), so ending the process is the safe choice there.
/// `iced::android_main!` does all this for you.
#[cfg(target_os = "android")]
pub fn activity_destroyed() -> bool {
    ACTIVITY_DESTROYED.get()
}

mod lifecycle;

pub use lifecycle::{Lifecycle, lifecycle, on_lifecycle};

/// Runs a [`Program`] with the provided settings.
pub fn run<P>(program: P) -> Result<(), Error>
where
    P: Program + 'static,
    P::Theme: theme::Base,
{
    use winit::event_loop::EventLoop;

    icm::start();
    lifecycle::start();

    let boot_span = debug::boot();
    let settings = program.settings();
    let window_settings = program.window();

    #[allow(unused_mut)]
    let mut builder = EventLoop::with_user_event();

    #[cfg(target_os = "android")]
    {
        use winit::platform::android::EventLoopBuilderExtAndroid;

        ACTIVITY_DESTROYED.set(false);

        let app = ANDROID_APP
            .lock()
            .expect("Lock AndroidApp")
            .take()
            .expect(
                "No AndroidApp: define the entry point with \
                iced::android_main!(run), or call iced::mobile::set_android_app \
                (iced_winit::set_android_app) with the AndroidApp that \
                android_main receives before running the application, every \
                time android_main runs: each Activity brings its own, and an \
                application runs once per AndroidApp. If that is done, the \
                build holds two copies of iced_winit and the call filled the \
                other one: depend on iced_winit from exactly the same source \
                as iced (the same git URL and rev, character for character), \
                or not at all (`cargo tree -d` lists both copies).",
            );

        safe_area::set_android_app(app.clone());

        let _ = builder.with_android_app(app);
    }

    // Before winit's `UIApplicationMain`, so that the scene's connection is
    // heard.
    #[cfg(target_os = "ios")]
    scene::adopt();

    #[cfg(not(target_os = "android"))]
    let event_loop = builder.build().expect("Create event loop");

    // winit runs one event loop at a time. On Android it ends the loop when
    // the Activity is destroyed, and allows a new one once the old one is
    // dropped, so the next Activity's `android_main` can build its own; it
    // cannot while another Activity's loop still runs.
    #[cfg(target_os = "android")]
    let event_loop = builder.build().unwrap_or_else(|error| match error {
        winit::error::EventLoopError::RecreationAttempt => panic!(
            "Create event loop: an event loop is already running in this \
            process, and winit runs one at a time. Android started an \
            Activity of this application, which runs android_main again, \
            while another Activity of it still runs the application. Either \
            the app was launched again before Android had destroyed the \
            Activity it was finishing (a moment after Back), and a launch a \
            moment later works; or the activity was started a second time, \
            into another task (from another app or a notification) or in a \
            second window. iced runs one Activity at a time: declare \
            android:launchMode=\"singleTask\" on the activity in \
            AndroidManifest.xml, so that Android hands such a launch to the \
            running Activity instead."
        ),
        error => panic!("Create event loop: {error:?}"),
    });

    let graphics_settings = settings.clone().into();
    let display_handle = event_loop.owned_display_handle();

    let (proxy, worker) = Proxy::new(event_loop.create_proxy());

    #[cfg(feature = "debug")]
    {
        let proxy = proxy.clone();

        debug::on_hotpatch(move || {
            proxy.send_action(Action::Reload);
        });
    }

    let mut runtime = {
        let executor =
            P::Executor::new().map_err(Error::ExecutorCreationFailed)?;
        executor.spawn(worker);

        Runtime::new(executor, proxy.clone())
    };

    let (program, task) = runtime.enter(|| program::Instance::new(program));
    let is_daemon = window_settings.is_none();

    let task = if let Some(window_settings) = window_settings {
        let mut task = Some(task);

        let (_id, open) = runtime::window::open(window_settings);

        open.then(move |_| task.take().unwrap_or_else(Task::none))
    } else {
        task
    };

    if let Some(stream) = runtime::task::into_stream(task) {
        runtime.run(stream);
    }

    // Before the subscriptions start: `safe_area()` replays the latest safe
    // area, which must not be the last application's (Android runs one per
    // Activity in the same process).
    safe_area::reset();

    runtime.track(subscription::into_recipes(
        runtime.enter(|| program.subscription().map(Action::Output)),
    ));

    let (event_sender, event_receiver) = mpsc::unbounded();
    let (control_sender, control_receiver) = mpsc::unbounded();
    let (system_theme_sender, system_theme_receiver) = oneshot::channel();

    let instance = Box::pin(run_instance::<P>(
        program,
        runtime,
        proxy.clone(),
        event_receiver,
        control_sender,
        display_handle,
        is_daemon,
        graphics_settings,
        settings.fonts,
        system_theme_receiver,
    ));

    let context = task::Context::from_waker(task::noop_waker_ref());

    struct Runner<Message: 'static, F> {
        instance: std::pin::Pin<Box<F>>,
        context: task::Context<'static>,
        id: Option<String>,
        sender: mpsc::UnboundedSender<Event<Action<Message>>>,
        receiver: mpsc::UnboundedReceiver<Control>,
        error: Option<Error>,
        system_theme: Option<oneshot::Sender<theme::Mode>>,

        #[cfg(target_arch = "wasm32")]
        canvas: Option<web_sys::HtmlCanvasElement>,

        /// The instance has returned. Callbacks after that are ignored: iOS
        /// keeps calling, since winit cannot stop its event loop there.
        finished: bool,

        /// Android: there is no native window, before the first `Resumed`
        /// and between `Suspended` and `Resumed`.
        #[cfg(target_os = "android")]
        suspended: bool,
        /// Android: a window to create once the native window exists.
        #[cfg(target_os = "android")]
        held_window: Option<Control>,
        /// Android: the one native window is taken by an iced window.
        #[cfg(target_os = "android")]
        has_window: bool,
        /// Android and iOS: the system's light or dark mode, last read.
        #[cfg(any(target_os = "android", target_os = "ios"))]
        appearance: appearance::Appearance,
    }

    let runner = Runner {
        instance,
        context,
        id: settings.id,
        sender: event_sender,
        receiver: control_receiver,
        error: None,
        system_theme: Some(system_theme_sender),

        #[cfg(target_arch = "wasm32")]
        canvas: None,

        finished: false,

        #[cfg(target_os = "android")]
        suspended: true,
        #[cfg(target_os = "android")]
        held_window: None,
        #[cfg(target_os = "android")]
        has_window: false,
        #[cfg(any(target_os = "android", target_os = "ios"))]
        appearance: appearance::Appearance::default(),
    };

    boot_span.finish();

    impl<Message, F> winit::application::ApplicationHandler<Action<Message>>
        for Runner<Message, F>
    where
        F: Future<Output = ()>,
    {
        fn resumed(&mut self, event_loop: &winit::event_loop::ActiveEventLoop) {
            lifecycle::report(lifecycle::Input::Resumed);

            if let Some(sender) = self.system_theme.take() {
                let _ = sender.send(
                    event_loop
                        .system_theme()
                        .map(conversion::theme_mode)
                        .unwrap_or_default(),
                );
            }

            // Android: the native window arrives with `Resumed`, the first
            // time and again after every `Suspended`. The instance builds a
            // surface on it for every window, then a window held while
            // suspended is created (see `next_control`).
            #[cfg(target_os = "android")]
            {
                self.suspended = false;

                self.process_event(
                    event_loop,
                    Event::EventLoopAwakened(winit::event::Event::Resumed),
                );
            }
        }

        fn suspended(
            &mut self,
            event_loop: &winit::event_loop::ActiveEventLoop,
        ) {
            lifecycle::report(lifecycle::Input::Suspended);

            // Android: the native window is going away. Every surface on it
            // is dropped before this returns, as winit requires.
            #[cfg(target_os = "android")]
            {
                self.suspended = true;

                self.process_event(
                    event_loop,
                    Event::EventLoopAwakened(winit::event::Event::Suspended),
                );
            }

            #[cfg(not(target_os = "android"))]
            let _ = event_loop;
        }

        fn new_events(
            &mut self,
            event_loop: &winit::event_loop::ActiveEventLoop,
            cause: winit::event::StartCause,
        ) {
            #[cfg(any(target_os = "android", target_os = "ios"))]
            self.sync_theme(event_loop);

            self.process_event(
                event_loop,
                Event::EventLoopAwakened(winit::event::Event::NewEvents(cause)),
            );
        }

        fn window_event(
            &mut self,
            event_loop: &winit::event_loop::ActiveEventLoop,
            window_id: winit::window::WindowId,
            event: winit::event::WindowEvent,
        ) {
            if let Some(input) = lifecycle::Input::of(&event) {
                lifecycle::report(input);
            }

            #[cfg(target_os = "windows")]
            let is_move_or_resize = matches!(
                event,
                winit::event::WindowEvent::Resized(_)
                    | winit::event::WindowEvent::Moved(_)
            );

            self.process_event(
                event_loop,
                Event::EventLoopAwakened(winit::event::Event::WindowEvent {
                    window_id,
                    event,
                }),
            );

            // TODO: Remove when unnecessary
            // On Windows, we emulate an `AboutToWait` event after every `Resized` event
            // since the event loop does not resume during resize interaction.
            // More details: https://github.com/rust-windowing/winit/issues/3272
            #[cfg(target_os = "windows")]
            {
                if is_move_or_resize {
                    self.process_event(
                        event_loop,
                        Event::EventLoopAwakened(
                            winit::event::Event::AboutToWait,
                        ),
                    );
                }
            }
        }

        fn user_event(
            &mut self,
            event_loop: &winit::event_loop::ActiveEventLoop,
            action: Action<Message>,
        ) {
            self.process_event(
                event_loop,
                Event::EventLoopAwakened(winit::event::Event::UserEvent(
                    action,
                )),
            );
        }

        fn about_to_wait(
            &mut self,
            event_loop: &winit::event_loop::ActiveEventLoop,
        ) {
            #[cfg(any(target_os = "android", target_os = "ios"))]
            self.sync_theme(event_loop);

            self.process_event(
                event_loop,
                Event::EventLoopAwakened(winit::event::Event::AboutToWait),
            );

            #[cfg(target_os = "android")]
            lifecycle::wake_up(event_loop);
        }

        /// Android: winit ends the event loop by itself only when the
        /// Activity is destroyed (`Suspended` came first). The instance then
        /// ends as on `iced::exit`, so that everything it holds (the
        /// application's state, its windows, the renderer, the executor) is
        /// dropped before `run` returns, and so before `android_main` does.
        #[cfg(target_os = "android")]
        fn exiting(
            &mut self,
            _event_loop: &winit::event_loop::ActiveEventLoop,
        ) {
            // iced ended the loop itself: the instance returned, or it
            // cannot go on.
            if self.finished || self.error.is_some() {
                return;
            }

            ACTIVITY_DESTROYED.set(true);

            log::info!(
                "The Activity was destroyed: the application ends with its \
                event loop"
            );

            // Dropped unfinished, the instance would leak the user
            // interfaces it keeps in `ManuallyDrop`. It waits on nothing but
            // its events, so one poll sees `Exit` and returns.
            self.sender.start_send(Event::Exit).expect("Send event");

            if self.instance.as_mut().poll(&mut self.context).is_pending() {
                log::warn!(
                    "The application did not end on exit; it is dropped \
                    unfinished"
                );
            }

            self.finished = true;
        }

        /// iOS and Android: the system is short of memory.
        fn memory_warning(
            &mut self,
            _event_loop: &winit::event_loop::ActiveEventLoop,
        ) {
            lifecycle::report(lifecycle::Input::MemoryWarning);
        }
    }

    impl<Message, F> Runner<Message, F>
    where
        F: Future<Output = ()>,
    {
        fn process_event(
            &mut self,
            event_loop: &winit::event_loop::ActiveEventLoop,
            event: Event<Action<Message>>,
        ) {
            if self.finished || event_loop.exiting() {
                return;
            }

            self.sender.start_send(event).expect("Send event");

            loop {
                // A nested call (`WindowCreated`, `Exit`) may have seen the
                // instance return: it must not be polled again.
                if self.finished {
                    break;
                }

                let poll = self.instance.as_mut().poll(&mut self.context);

                match poll {
                    task::Poll::Pending => match self.next_control() {
                        Ok(Some(control)) => match control {
                            Control::ChangeFlow(flow) => {
                                use winit::event_loop::ControlFlow;

                                match (event_loop.control_flow(), flow) {
                                    (
                                        ControlFlow::WaitUntil(current),
                                        ControlFlow::WaitUntil(new),
                                    ) if current > Instant::now()
                                        && current < new => {}
                                    (
                                        ControlFlow::WaitUntil(current),
                                        ControlFlow::Wait,
                                    ) if current > Instant::now() => {}
                                    _ => {
                                        event_loop.set_control_flow(flow);
                                    }
                                }
                            }
                            Control::CreateWindow {
                                id,
                                settings,
                                title,
                                scale_factor,
                                monitor,
                                on_open,
                            } => {
                                let exit_on_close_request =
                                    settings.exit_on_close_request;

                                let visible = settings.visible;

                                #[cfg(target_arch = "wasm32")]
                                let target =
                                    settings.platform_specific.target.clone();

                                let window_attributes =
                                    conversion::window_attributes(
                                        settings,
                                        &title,
                                        scale_factor,
                                        monitor
                                            .or(event_loop.primary_monitor()),
                                        self.id.clone(),
                                    )
                                    .with_visible(false);

                                #[cfg(target_arch = "wasm32")]
                                let window_attributes = {
                                    use winit::platform::web::WindowAttributesExtWebSys;
                                    window_attributes
                                        .with_canvas(self.canvas.take())
                                };

                                log::info!(
                                    "Window attributes for id `{id:#?}`: {window_attributes:#?}"
                                );

                                // On macOS, the `position` in `WindowAttributes` represents the "inner"
                                // position of the window; while on other platforms it's the "outer" position.
                                // We fix the inconsistency on macOS by positioning the window after creation.
                                #[cfg(target_os = "macos")]
                                let mut window_attributes = window_attributes;

                                #[cfg(target_os = "macos")]
                                let position =
                                    window_attributes.position.take();

                                let window = event_loop
                                    .create_window(window_attributes)
                                    .expect("Create window");

                                #[cfg(target_os = "macos")]
                                if let Some(position) = position {
                                    window.set_outer_position(position);
                                }

                                #[cfg(target_arch = "wasm32")]
                                {
                                    use winit::platform::web::WindowExtWebSys;

                                    let canvas = window
                                        .canvas()
                                        .expect("Get window canvas");

                                    let _ = canvas.set_attribute(
                                        "style",
                                        "display: block; width: 100%; height: 100%",
                                    );

                                    let window = web_sys::window().unwrap();
                                    let document = window.document().unwrap();
                                    let body = document.body().unwrap();

                                    let target = target.and_then(|target| {
                                        body.query_selector(&format!(
                                            "#{target}"
                                        ))
                                        .ok()
                                        .unwrap_or(None)
                                    });

                                    match target {
                                        Some(node) => {
                                            let _ = node
                                                .replace_with_with_node_1(
                                                    &canvas,
                                                )
                                                .expect(&format!(
                                                    "Could not replace #{}",
                                                    node.id()
                                                ));
                                        }
                                        None => {
                                            let _ = body
                                                .append_child(&canvas)
                                                .expect(
                                                "Append canvas to HTML body",
                                            );
                                        }
                                    };
                                }

                                self.process_event(
                                    event_loop,
                                    Event::WindowCreated {
                                        id,
                                        window: Arc::new(window),
                                        exit_on_close_request,
                                        make_visible: visible,
                                        on_open,
                                    },
                                );
                            }
                            Control::Exit => {
                                self.process_event(event_loop, Event::Exit);
                                event_loop.exit();
                                break;
                            }
                            Control::Crash(error) => {
                                log::error!("{error}: {error:?}");

                                // iOS: `run` never returns its error, and
                                // the window stays black.
                                #[cfg(target_os = "ios")]
                                panic!("{error}: {error:?}");

                                #[cfg(not(target_os = "ios"))]
                                {
                                    self.error = Some(error);
                                    event_loop.exit();
                                }
                            }
                            Control::SetAutomaticWindowTabbing(_enabled) => {
                                #[cfg(target_os = "macos")]
                                {
                                    use winit::platform::macos::ActiveEventLoopExtMacOS;
                                    event_loop
                                        .set_allows_automatic_window_tabbing(
                                            _enabled,
                                        );
                                }
                            }
                        },
                        _ => {
                            break;
                        }
                    },
                    task::Poll::Ready(_) => {
                        self.finished = true;
                        event_loop.exit();
                        break;
                    }
                };
            }
        }

        /// The next control from the instance.
        ///
        /// Android has one native window, which exists only between
        /// `Resumed` and `Suspended`: a window opened while suspended is
        /// held until the next `Resumed`, and a second window is refused.
        fn next_control(
            &mut self,
        ) -> Result<Option<Control>, mpsc::TryRecvError> {
            #[cfg(target_os = "android")]
            loop {
                if !self.suspended
                    && let Some(control) = self.held_window.take()
                {
                    return Ok(Some(control));
                }

                match try_next(&mut self.receiver)? {
                    Some(Control::CreateWindow { id, on_open, .. })
                        if self.has_window =>
                    {
                        log::error!(
                            "window::open is refused on Android: the \
                            application has one native window, and an iced \
                            window already uses it. Window {id:?} is not \
                            opened, and the task window::open returned ends \
                            without an id."
                        );

                        // The task waiting on it ends without output.
                        drop(on_open);
                    }
                    Some(control @ Control::CreateWindow { .. }) => {
                        self.has_window = true;

                        if !self.suspended {
                            return Ok(Some(control));
                        }

                        self.held_window = Some(control);
                    }
                    control => return Ok(control),
                }
            }

            #[cfg(not(target_os = "android"))]
            try_next(&mut self.receiver)
        }

        /// Android and iOS: winit reports no system theme there, so the
        /// runner reads it whenever the event loop turns, and hands the
        /// first mode and every change to the instance (see `appearance`).
        #[cfg(any(target_os = "android", target_os = "ios"))]
        fn sync_theme(
            &mut self,
            event_loop: &winit::event_loop::ActiveEventLoop,
        ) {
            if self.finished || event_loop.exiting() {
                return;
            }

            if let Some(mode) = self.appearance.poll(event_loop) {
                self.process_event(event_loop, Event::SystemTheme(mode));
            }
        }
    }

    #[cfg(not(target_arch = "wasm32"))]
    {
        let mut runner = runner;
        let _ = event_loop.run_app(&mut runner);

        #[cfg(target_os = "android")]
        let destroyed = ACTIVITY_DESTROYED.get();

        #[cfg(not(target_os = "android"))]
        let destroyed = false;

        icm::exit(if runner.error.is_some() { 1 } else { 0 }, destroyed);

        runner.error.map(Err).unwrap_or(Ok(()))
    }

    #[cfg(target_arch = "wasm32")]
    {
        use winit::platform::web::EventLoopExtWebSys;
        let _ = event_loop.spawn_app(runner);

        Ok(())
    }
}

/// The graphics backend to ask the compositor for, or `None` for its own
/// choice (`ICED_BACKEND`, then its defaults).
///
/// An Android application cannot be given environment variables, so there
/// the system property `debug.iced.backend` stands in for `ICED_BACKEND`
/// (`adb shell setprop debug.iced.backend tiny-skia`), which still wins.
fn backend_preference() -> Option<String> {
    #[cfg(target_os = "android")]
    {
        if std::env::var_os("ICED_BACKEND").is_some() {
            return None;
        }

        let backend = icm::system_property(c"debug.iced.backend")
            .filter(|backend| !backend.trim().is_empty())?;

        log::info!(
            "Graphics backend from the system property debug.iced.backend: \
            {backend:?}"
        );

        Some(backend)
    }

    #[cfg(not(target_os = "android"))]
    None
}

#[derive(Debug)]
enum Event<Message: 'static> {
    WindowCreated {
        id: window::Id,
        window: Arc<winit::window::Window>,
        exit_on_close_request: bool,
        make_visible: bool,
        on_open: oneshot::Sender<window::Id>,
    },
    EventLoopAwakened(winit::event::Event<Message>),
    Exit,
    /// Android and iOS: the system's light or dark mode, read by the runner.
    /// Not a user event, so it takes no slot of the proxy.
    #[cfg(any(target_os = "android", target_os = "ios"))]
    SystemTheme(theme::Mode),
}

#[derive(Debug)]
enum Control {
    ChangeFlow(winit::event_loop::ControlFlow),
    Exit,
    Crash(Error),
    CreateWindow {
        id: window::Id,
        settings: window::Settings,
        title: String,
        monitor: Option<winit::monitor::MonitorHandle>,
        on_open: oneshot::Sender<window::Id>,
        scale_factor: f32,
    },
    SetAutomaticWindowTabbing(bool),
}

async fn run_instance<P>(
    mut program: program::Instance<P>,
    mut runtime: Runtime<P::Executor, Proxy<P::Message>, Action<P::Message>>,
    mut proxy: Proxy<P::Message>,
    mut event_receiver: mpsc::UnboundedReceiver<Event<Action<P::Message>>>,
    mut control_sender: mpsc::UnboundedSender<Control>,
    display_handle: winit::event_loop::OwnedDisplayHandle,
    is_daemon: bool,
    graphics_settings: graphics::Settings,
    default_fonts: Vec<Cow<'static, [u8]>>,
    mut _system_theme: oneshot::Receiver<theme::Mode>,
) where
    P: Program + 'static,
    P::Theme: theme::Base,
{
    use winit::event;
    use winit::event_loop::ControlFlow;

    let mut window_manager = WindowManager::new();
    let mut is_window_opening = !is_daemon;
    let mut safe_area = safe_area::Shell::new();

    let mut compositor = None;
    let mut events = Vec::new();
    let mut messages = Vec::new();
    let mut actions = 0;

    let mut ui_caches = FxHashMap::default();
    let mut user_interfaces = ManuallyDrop::new(FxHashMap::default());
    let mut clipboard = Clipboard::unconnected();

    #[cfg(all(feature = "linux-theme-detection", target_os = "linux"))]
    let mut system_theme = {
        let to_mode = |color_scheme| match color_scheme {
            mundy::ColorScheme::NoPreference => theme::Mode::None,
            mundy::ColorScheme::Light => theme::Mode::Light,
            mundy::ColorScheme::Dark => theme::Mode::Dark,
        };

        runtime.run(
            mundy::Preferences::stream(mundy::Interest::ColorScheme)
                .map(move |preferences| {
                    Action::System(system::Action::NotifyTheme(to_mode(
                        preferences.color_scheme,
                    )))
                })
                .boxed(),
        );

        runtime
            .enter(|| {
                mundy::Preferences::once_blocking(
                    mundy::Interest::ColorScheme,
                    core::time::Duration::from_millis(200),
                )
            })
            .map(|preferences| to_mode(preferences.color_scheme))
            .unwrap_or_default()
    };

    #[cfg(not(all(feature = "linux-theme-detection", target_os = "linux")))]
    let mut system_theme =
        _system_theme.try_recv().ok().flatten().unwrap_or_default();

    log::info!("System theme: {system_theme:?}");

    'next_event: loop {
        // Empty the queue if possible
        let event = if let Ok(event) = try_next(&mut event_receiver) {
            event
        } else {
            event_receiver.next().await
        };

        let Some(event) = event else {
            break;
        };

        match event {
            Event::WindowCreated {
                id,
                window,
                exit_on_close_request,
                make_visible,
                on_open,
            } => {
                if compositor.is_none() {
                    let (compositor_sender, compositor_receiver) =
                        oneshot::channel();

                    let create_compositor = {
                        let window = window.clone();
                        let display_handle = display_handle.clone();
                        let proxy = proxy.clone();
                        let default_fonts = default_fonts.clone();
                        let default_font = graphics_settings.default_font;
                        let backend = backend_preference();

                        async move {
                            let shell = Shell::new(proxy.clone());

                            let mut compositor =
                                <P::Renderer as compositor::Default>::Compositor::with_backend(
                                    graphics_settings,
                                    display_handle,
                                    window,
                                    shell,
                                    backend.as_deref(),
                                ).await;

                            if let Ok(compositor) = &mut compositor {
                                for font in default_fonts {
                                    compositor.load_font(font.clone());
                                }

                                // A named default font nothing loaded is
                                // drawn with a fallback, or not at all where
                                // there are no system fonts (the web).
                                if let crate::core::font::Family::Name(name) =
                                    default_font.family
                                    && icm::enabled()
                                    && !graphics::text::is_loaded(default_font)
                                {
                                    icm::warning(
                                        "font.default_missing",
                                        &format!(
                                            "the default font \"{name}\" is \
                                            not loaded: embed it (a font \
                                            feature or Settings::fonts), or \
                                            text in it falls back to another \
                                            font, or is not drawn at all \
                                            where there are no system fonts \
                                            (the web)"
                                        ),
                                    );
                                }
                            }

                            compositor_sender
                                .send(compositor)
                                .ok()
                                .expect("Send compositor");

                            // HACK! Send a proxy event on completion to trigger
                            // a runtime re-poll
                            // TODO: Send compositor through proxy (?)
                            {
                                let (sender, _receiver) = oneshot::channel();

                                proxy.send_action(Action::Window(
                                    runtime::window::Action::GetLatest(sender),
                                ));
                            }
                        }
                    };

                    #[cfg(target_arch = "wasm32")]
                    wasm_bindgen_futures::spawn_local(create_compositor);

                    #[cfg(not(target_arch = "wasm32"))]
                    runtime.block_on(create_compositor);

                    match compositor_receiver
                        .await
                        .expect("Wait for compositor")
                    {
                        Ok(new_compositor) => {
                            compositor = Some(new_compositor);
                        }
                        Err(error) => {
                            let _ = control_sender
                                .start_send(Control::Crash(error.into()));
                            continue;
                        }
                    }
                }

                // Android and iOS report no window theme: the runner reads
                // the system's instead (`Event::SystemTheme`).
                #[cfg(not(any(target_os = "android", target_os = "ios")))]
                let window_theme = window
                    .theme()
                    .map(conversion::theme_mode)
                    .unwrap_or_default();

                #[cfg(not(any(target_os = "android", target_os = "ios")))]
                if system_theme != window_theme {
                    system_theme = window_theme;

                    runtime.broadcast(subscription::Event::SystemThemeChanged(
                        window_theme,
                    ));
                }

                let is_first = window_manager.is_empty();
                let window = window_manager.insert(
                    id,
                    window,
                    &program,
                    compositor
                        .as_mut()
                        .expect("Compositor must be initialized"),
                    exit_on_close_request,
                    system_theme,
                );

                window.raw.set_theme(conversion::window_theme(
                    window.state.theme_mode(),
                ));

                debug::theme_changed(|| {
                    if is_first {
                        theme::Base::palette(window.state.theme())
                    } else {
                        None
                    }
                });

                let logical_size = window.state.logical_size();

                let _ = user_interfaces.insert(
                    id,
                    build_user_interface(
                        &program,
                        user_interface::Cache::default(),
                        &mut window.renderer,
                        logical_size,
                        id,
                    ),
                );
                let _ = ui_caches.insert(id, user_interface::Cache::default());

                if make_visible {
                    window.raw.set_visible(true);
                }

                events.push((
                    id,
                    core::Event::Window(window::Event::Opened {
                        position: window.position(),
                        size: window.logical_size(),
                    }),
                ));

                safe_area.refresh(window);

                if clipboard.window_id().is_none() {
                    clipboard = Clipboard::connect(window.raw.clone());
                }

                let _ = on_open.send(id);
                is_window_opening = false;
            }
            Event::EventLoopAwakened(event) => {
                match event {
                    event::Event::NewEvents(event::StartCause::Init) => {
                        for (_id, window) in window_manager.iter_mut() {
                            window.raw.request_redraw();
                        }
                    }
                    event::Event::NewEvents(
                        event::StartCause::ResumeTimeReached { .. },
                    ) => {
                        let now = Instant::now();

                        for (_id, window) in window_manager.iter_mut() {
                            if let Some(redraw_at) = window.redraw_at
                                && redraw_at <= now
                            {
                                window.raw.request_redraw();
                                window.redraw_at = None;
                            }
                        }

                        if let Some(redraw_at) = window_manager.redraw_at() {
                            let _ =
                                control_sender.start_send(Control::ChangeFlow(
                                    ControlFlow::WaitUntil(redraw_at),
                                ));
                        } else {
                            let _ = control_sender.start_send(
                                Control::ChangeFlow(ControlFlow::Wait),
                            );
                        }

                        safe_area
                            .poll(&mut window_manager, &mut control_sender);
                    }
                    event::Event::UserEvent(action) => {
                        run_action(
                            action,
                            &program,
                            &mut runtime,
                            &mut compositor,
                            &mut events,
                            &mut messages,
                            &mut clipboard,
                            &mut control_sender,
                            &mut user_interfaces,
                            &mut window_manager,
                            &mut ui_caches,
                            &mut is_window_opening,
                            &mut system_theme,
                        );
                        actions += 1;
                    }
                    event::Event::WindowEvent {
                        window_id: id,
                        event: event::WindowEvent::RedrawRequested,
                        ..
                    } => {
                        let Some(mut current_compositor) = compositor.as_mut()
                        else {
                            continue;
                        };

                        let Some((id, mut window)) =
                            window_manager.get_mut_alias(id)
                        else {
                            continue;
                        };

                        // No surface: the native window is gone (Android,
                        // between `Suspended` and `Resumed`).
                        if window.surface.is_none() {
                            continue;
                        }

                        let physical_size = window.state.physical_size();
                        let mut logical_size = window.state.logical_size();

                        if physical_size.width == 0 || physical_size.height == 0
                        {
                            continue;
                        }

                        // Window was resized between redraws
                        if window.surface_version
                            != window.state.surface_version()
                        {
                            let ui = user_interfaces
                                .remove(&id)
                                .expect("Remove user interface");

                            let layout_span = debug::layout(id);
                            let _ = user_interfaces.insert(
                                id,
                                ui.relayout(logical_size, &mut window.renderer),
                            );
                            layout_span.finish();

                            if let Some(surface) = window.surface.as_mut() {
                                current_compositor.configure_surface(
                                    surface,
                                    physical_size.width,
                                    physical_size.height,
                                );
                            }

                            window.surface_version =
                                window.state.surface_version();
                        }

                        let redraw_event = core::Event::Window(
                            window::Event::RedrawRequested(Instant::now()),
                        );

                        let cursor = window.state.cursor();

                        let mut interface = user_interfaces
                            .get_mut(&id)
                            .expect("Get user interface");

                        let interact_span = debug::interact(id);
                        let mut redraw_count = 0;

                        let state = loop {
                            let message_count = messages.len();
                            let (state, _) = interface.update(
                                slice::from_ref(&redraw_event),
                                cursor,
                                &mut window.renderer,
                                &mut clipboard,
                                &mut messages,
                            );

                            if message_count == messages.len()
                                && !state.has_layout_changed()
                            {
                                break state;
                            }

                            if redraw_count >= 2 {
                                log::warn!(
                                    "More than 3 consecutive RedrawRequested events \
                                    produced layout invalidation"
                                );

                                break state;
                            }

                            redraw_count += 1;

                            if !messages.is_empty() {
                                let caches: FxHashMap<_, _> =
                                    ManuallyDrop::into_inner(user_interfaces)
                                        .into_iter()
                                        .map(|(id, interface)| {
                                            (id, interface.into_cache())
                                        })
                                        .collect();

                                let actions = update(
                                    &mut program,
                                    &mut runtime,
                                    &mut messages,
                                );

                                user_interfaces =
                                    ManuallyDrop::new(build_user_interfaces(
                                        &program,
                                        &mut window_manager,
                                        caches,
                                    ));

                                for action in actions {
                                    // Defer all window actions to avoid compositor
                                    // race conditions while redrawing
                                    if let Action::Window(_) = action {
                                        proxy.send_action(action);
                                        continue;
                                    }

                                    run_action(
                                        action,
                                        &program,
                                        &mut runtime,
                                        &mut compositor,
                                        &mut events,
                                        &mut messages,
                                        &mut clipboard,
                                        &mut control_sender,
                                        &mut user_interfaces,
                                        &mut window_manager,
                                        &mut ui_caches,
                                        &mut is_window_opening,
                                        &mut system_theme,
                                    );
                                }

                                for (window_id, window) in
                                    window_manager.iter_mut()
                                {
                                    // We are already redrawing this window
                                    if window_id == id {
                                        continue;
                                    }

                                    window.raw.request_redraw();
                                }

                                let Some(next_compositor) = compositor.as_mut()
                                else {
                                    continue 'next_event;
                                };

                                current_compositor = next_compositor;
                                window = window_manager.get_mut(id).unwrap();

                                // Window scale factor changed during a redraw request
                                if logical_size != window.state.logical_size() {
                                    logical_size = window.state.logical_size();

                                    log::debug!(
                                        "Window scale factor changed during a redraw request"
                                    );

                                    let ui = user_interfaces
                                        .remove(&id)
                                        .expect("Remove user interface");

                                    let layout_span = debug::layout(id);
                                    let _ = user_interfaces.insert(
                                        id,
                                        ui.relayout(
                                            logical_size,
                                            &mut window.renderer,
                                        ),
                                    );
                                    layout_span.finish();
                                }

                                interface =
                                    user_interfaces.get_mut(&id).unwrap();
                            }
                        };
                        interact_span.finish();

                        let draw_span = debug::draw(id);
                        interface.draw(
                            &mut window.renderer,
                            window.state.theme(),
                            &renderer::Style {
                                text_color: window.state.text_color(),
                            },
                            cursor,
                        );
                        draw_span.finish();

                        if let user_interface::State::Updated {
                            redraw_request,
                            input_method,
                            mouse_interaction,
                            ..
                        } = state
                        {
                            window.request_redraw(redraw_request);
                            window.request_input_method(input_method);
                            window.update_mouse(mouse_interaction);
                        }

                        runtime.broadcast(subscription::Event::Interaction {
                            window: id,
                            event: redraw_event,
                            status: core::event::Status::Ignored,
                        });

                        window.draw_preedit();

                        let Some(surface) = window.surface.as_mut() else {
                            continue;
                        };

                        let present_span = debug::present(id);
                        match current_compositor.present(
                            &mut window.renderer,
                            surface,
                            window.state.viewport(),
                            window.state.background_color(),
                            || window.raw.pre_present_notify(),
                        ) {
                            Ok(()) => {
                                present_span.finish();

                                if icm::awaits_ready() {
                                    use core::renderer::Headless as _;

                                    icm::ready(
                                        window.state.logical_size(),
                                        window.state.physical_size(),
                                        window.state.scale_factor(),
                                        &window.renderer.name(),
                                        &current_compositor.information(),
                                    );
                                }
                            }
                            Err(error) => match error {
                                compositor::SurfaceError::OutOfMemory => {
                                    // This is an unrecoverable error.
                                    panic!("{error:?}");
                                }
                                compositor::SurfaceError::Outdated
                                | compositor::SurfaceError::Lost => {
                                    present_span.finish();

                                    // Reconfigure surface and try redrawing
                                    let physical_size =
                                        window.state.physical_size();

                                    if error == compositor::SurfaceError::Lost {
                                        // Drop the lost surface before its
                                        // replacement is built: a native
                                        // window may take only one surface
                                        // at a time (Android).
                                        window.surface = None;
                                        window.surface = Some(
                                            current_compositor.create_surface(
                                                window.raw.clone(),
                                                physical_size.width,
                                                physical_size.height,
                                            ),
                                        );
                                    } else {
                                        current_compositor.configure_surface(
                                            surface,
                                            physical_size.width,
                                            physical_size.height,
                                        );
                                    }

                                    window.raw.request_redraw();
                                }
                                _ => {
                                    present_span.finish();

                                    log::error!(
                                        "Error {error:?} when \
                                        presenting surface."
                                    );

                                    // Try rendering all windows again next frame.
                                    for (_id, window) in
                                        window_manager.iter_mut()
                                    {
                                        window.raw.request_redraw();
                                    }
                                }
                            },
                        }
                    }
                    event::Event::WindowEvent {
                        event: window_event,
                        window_id,
                    } => {
                        if !is_daemon
                            && matches!(
                                window_event,
                                winit::event::WindowEvent::Destroyed
                            )
                            && !is_window_opening
                            && window_manager.is_empty()
                        {
                            control_sender
                                .start_send(Control::Exit)
                                .expect("Send control action");

                            continue;
                        }

                        let Some((id, window)) =
                            window_manager.get_mut_alias(window_id)
                        else {
                            continue;
                        };

                        match window_event {
                            winit::event::WindowEvent::Resized(_) => {
                                window.raw.request_redraw();
                            }
                            winit::event::WindowEvent::ThemeChanged(theme) => {
                                let mode = conversion::theme_mode(theme);

                                if mode != system_theme {
                                    system_theme = mode;

                                    runtime.broadcast(
                                        subscription::Event::SystemThemeChanged(
                                            mode,
                                        ),
                                    );
                                }
                            }
                            _ => {}
                        }

                        if matches!(
                            window_event,
                            winit::event::WindowEvent::CloseRequested
                        ) && window.exit_on_close_request
                        {
                            run_action(
                                Action::Window(runtime::window::Action::Close(
                                    id,
                                )),
                                &program,
                                &mut runtime,
                                &mut compositor,
                                &mut events,
                                &mut messages,
                                &mut clipboard,
                                &mut control_sender,
                                &mut user_interfaces,
                                &mut window_manager,
                                &mut ui_caches,
                                &mut is_window_opening,
                                &mut system_theme,
                            );
                        } else {
                            let resized = matches!(
                                window_event,
                                winit::event::WindowEvent::Resized(_)
                                    | winit::event::WindowEvent::ScaleFactorChanged {
                                        ..
                                    }
                            );

                            window.state.update(
                                &program,
                                &window.raw,
                                &window_event,
                            );

                            if let Some(event) = conversion::window_event(
                                window_event,
                                window.state.scale_factor(),
                                window.state.modifiers(),
                            ) {
                                events.push((id, event));
                            }

                            if resized {
                                safe_area.refresh(window);
                            }
                        }
                    }
                    event::Event::AboutToWait => {
                        if actions > 0 {
                            proxy.free_slots(actions);
                            actions = 0;
                        }

                        // Before the idle check, which would skip it.
                        safe_area
                            .poll(&mut window_manager, &mut control_sender);

                        if events.is_empty()
                            && messages.is_empty()
                            && window_manager.is_idle()
                        {
                            continue;
                        }

                        let mut uis_stale = false;

                        for (id, window) in window_manager.iter_mut() {
                            let interact_span = debug::interact(id);
                            let mut window_events = vec![];

                            events.retain(|(window_id, event)| {
                                if *window_id == id {
                                    window_events.push(event.clone());
                                    false
                                } else {
                                    true
                                }
                            });

                            if window_events.is_empty() && messages.is_empty() {
                                continue;
                            }

                            let (ui_state, statuses) = user_interfaces
                                .get_mut(&id)
                                .expect("Get user interface")
                                .update(
                                    &window_events,
                                    window.state.cursor(),
                                    &mut window.renderer,
                                    &mut clipboard,
                                    &mut messages,
                                );

                            #[cfg(feature = "unconditional-rendering")]
                            window.request_redraw(
                                window::RedrawRequest::NextFrame,
                            );

                            match ui_state {
                                user_interface::State::Updated {
                                    redraw_request: _redraw_request,
                                    mouse_interaction,
                                    ..
                                } => {
                                    window.update_mouse(mouse_interaction);

                                    #[cfg(not(
                                        feature = "unconditional-rendering"
                                    ))]
                                    window.request_redraw(_redraw_request);
                                }
                                user_interface::State::Outdated => {
                                    uis_stale = true;
                                }
                            }

                            for (event, status) in
                                window_events.into_iter().zip(statuses)
                            {
                                runtime.broadcast(
                                    subscription::Event::Interaction {
                                        window: id,
                                        event,
                                        status,
                                    },
                                );
                            }

                            interact_span.finish();
                        }

                        for (id, event) in events.drain(..) {
                            runtime.broadcast(
                                subscription::Event::Interaction {
                                    window: id,
                                    event,
                                    status: core::event::Status::Ignored,
                                },
                            );
                        }

                        if !messages.is_empty() || uis_stale {
                            let cached_interfaces: FxHashMap<_, _> =
                                ManuallyDrop::into_inner(user_interfaces)
                                    .into_iter()
                                    .map(|(id, ui)| (id, ui.into_cache()))
                                    .collect();

                            let actions = update(
                                &mut program,
                                &mut runtime,
                                &mut messages,
                            );

                            user_interfaces =
                                ManuallyDrop::new(build_user_interfaces(
                                    &program,
                                    &mut window_manager,
                                    cached_interfaces,
                                ));

                            for action in actions {
                                run_action(
                                    action,
                                    &program,
                                    &mut runtime,
                                    &mut compositor,
                                    &mut events,
                                    &mut messages,
                                    &mut clipboard,
                                    &mut control_sender,
                                    &mut user_interfaces,
                                    &mut window_manager,
                                    &mut ui_caches,
                                    &mut is_window_opening,
                                    &mut system_theme,
                                );
                            }

                            for (_id, window) in window_manager.iter_mut() {
                                window.raw.request_redraw();

                                // iOS: winit draws a redraw requested during
                                // `AboutToWait` only when the run loop turns
                                // again, so make it turn now.
                                #[cfg(target_os = "ios")]
                                window.request_redraw(
                                    window::RedrawRequest::At(Instant::now()),
                                );
                            }
                        }

                        if let Some(redraw_at) = window_manager.redraw_at() {
                            let _ =
                                control_sender.start_send(Control::ChangeFlow(
                                    ControlFlow::WaitUntil(redraw_at),
                                ));
                        } else {
                            let _ = control_sender.start_send(
                                Control::ChangeFlow(ControlFlow::Wait),
                            );
                        }
                    }
                    event::Event::Suspended => {
                        // The native window is about to be destroyed: winit
                        // requires every surface on it to be dropped before
                        // `suspended` returns, which this does, since the
                        // runner polls this future until it waits again. A
                        // window without a surface is not drawn until
                        // `Resumed`.
                        for (_id, window) in window_manager.iter_mut() {
                            window.surface = None;
                        }
                    }
                    event::Event::Resumed => {
                        // A new native window: build a surface on it for
                        // every window that has none.
                        if let Some(compositor) = compositor.as_mut() {
                            for (_id, window) in window_manager.iter_mut() {
                                if window.surface.is_some() {
                                    continue;
                                }

                                let size = window.state.physical_size();

                                window.surface =
                                    Some(compositor.create_surface(
                                        window.raw.clone(),
                                        size.width,
                                        size.height,
                                    ));

                                window.raw.request_redraw();

                                safe_area.refresh(window);
                            }
                        }
                    }
                    _ => {}
                }
            }
            Event::Exit => break,
            #[cfg(any(target_os = "android", target_os = "ios"))]
            Event::SystemTheme(mode) => {
                log::info!("System theme: {mode:?}");
                icm::theme(mode);

                run_action(
                    Action::System(system::Action::NotifyTheme(mode)),
                    &program,
                    &mut runtime,
                    &mut compositor,
                    &mut events,
                    &mut messages,
                    &mut clipboard,
                    &mut control_sender,
                    &mut user_interfaces,
                    &mut window_manager,
                    &mut ui_caches,
                    &mut is_window_opening,
                    &mut system_theme,
                );

                // iOS: the redraw it requested waits for the run loop to
                // turn again when the mode was read in `AboutToWait`.
                #[cfg(target_os = "ios")]
                for (_id, window) in window_manager.iter_mut() {
                    window.request_redraw(window::RedrawRequest::At(
                        Instant::now(),
                    ));
                }
            }
        }
    }

    let _ = ManuallyDrop::into_inner(user_interfaces);
}

/// Builds a window's [`UserInterface`] for the [`Program`].
fn build_user_interface<'a, P: Program>(
    program: &'a program::Instance<P>,
    cache: user_interface::Cache,
    renderer: &mut P::Renderer,
    size: Size,
    id: window::Id,
) -> UserInterface<'a, P::Message, P::Theme, P::Renderer>
where
    P::Theme: theme::Base,
{
    let view_span = debug::view(id);
    let view = program.view(id);
    view_span.finish();

    let layout_span = debug::layout(id);
    let user_interface = UserInterface::build(view, size, cache, renderer);
    layout_span.finish();

    user_interface
}

fn update<P: Program, E: Executor>(
    program: &mut program::Instance<P>,
    runtime: &mut Runtime<E, Proxy<P::Message>, Action<P::Message>>,
    messages: &mut Vec<P::Message>,
) -> Vec<Action<P::Message>>
where
    P::Theme: theme::Base,
{
    use futures::futures;

    let mut actions = Vec::new();

    for message in messages.drain(..) {
        let task = runtime.enter(|| program.update(message));

        if let Some(mut stream) = runtime::task::into_stream(task) {
            let waker = futures::task::noop_waker_ref();
            let mut context = futures::task::Context::from_waker(waker);

            // Run immediately available actions synchronously (e.g. widget operations)
            loop {
                match runtime.enter(|| stream.poll_next_unpin(&mut context)) {
                    futures::task::Poll::Ready(Some(action)) => {
                        actions.push(action);
                    }
                    futures::task::Poll::Ready(None) => {
                        break;
                    }
                    futures::task::Poll::Pending => {
                        runtime.run(stream);
                        break;
                    }
                }
            }
        }
    }

    let subscription = runtime.enter(|| program.subscription());
    let recipes = subscription::into_recipes(subscription.map(Action::Output));

    runtime.track(recipes);

    actions
}

fn run_action<'a, P, C>(
    action: Action<P::Message>,
    program: &'a program::Instance<P>,
    runtime: &mut Runtime<P::Executor, Proxy<P::Message>, Action<P::Message>>,
    compositor: &mut Option<C>,
    events: &mut Vec<(window::Id, core::Event)>,
    messages: &mut Vec<P::Message>,
    clipboard: &mut Clipboard,
    control_sender: &mut mpsc::UnboundedSender<Control>,
    interfaces: &mut FxHashMap<
        window::Id,
        UserInterface<'a, P::Message, P::Theme, P::Renderer>,
    >,
    window_manager: &mut WindowManager<P, C>,
    ui_caches: &mut FxHashMap<window::Id, user_interface::Cache>,
    is_window_opening: &mut bool,
    system_theme: &mut theme::Mode,
) where
    P: Program,
    C: Compositor<Renderer = P::Renderer> + 'static,
    P::Theme: theme::Base,
{
    use crate::runtime::clipboard;
    use crate::runtime::window;

    match action {
        Action::Output(message) => {
            messages.push(message);
        }
        Action::Clipboard(action) => match action {
            clipboard::Action::Read { target, channel } => {
                let _ = channel.send(clipboard.read(target));
            }
            clipboard::Action::Write { target, contents } => {
                clipboard.write(target, contents);
            }
        },
        Action::Window(action) => match action {
            window::Action::Open(id, settings, channel) => {
                let monitor = window_manager.last_monitor();

                control_sender
                    .start_send(Control::CreateWindow {
                        id,
                        settings,
                        title: program.title(id),
                        scale_factor: program.scale_factor(id),
                        monitor,
                        on_open: channel,
                    })
                    .expect("Send control action");

                *is_window_opening = true;
            }
            window::Action::Close(id) => {
                // Mobile winit never reports a window destroyed, which is
                // when iced exits: closing the last window would leave the
                // application running with nothing on screen. On iOS a
                // window already being opened takes its place, as in
                // `Task::batch([window::open(..), window::close(old)])`, so
                // the close stands; Android refuses a second window.
                #[cfg(target_os = "android")]
                let is_replaced = false;

                #[cfg(target_os = "ios")]
                let is_replaced = *is_window_opening;

                #[cfg(any(target_os = "android", target_os = "ios"))]
                if window_manager.is_last(id) && !is_replaced {
                    log::warn!(
                        "window::close of the last window is ignored on {}: \
                        the system, not the application, ends a mobile \
                        application, and with no window left it would show \
                        a black screen.{}",
                        std::env::consts::OS,
                        if cfg!(target_os = "ios") {
                            " To replace the window, open the new one first: \
                            window::open(..) before window::close(old) in \
                            the same batch, or window::close(old) once the \
                            task window::open returned has its id."
                        } else {
                            ""
                        },
                    );

                    return;
                }

                let _ = ui_caches.remove(&id);
                let _ = interfaces.remove(&id);

                if let Some(window) = window_manager.remove(id) {
                    if clipboard.window_id() == Some(window.raw.id()) {
                        *clipboard = window_manager
                            .first()
                            .map(|window| window.raw.clone())
                            .map(Clipboard::connect)
                            .unwrap_or_else(Clipboard::unconnected);
                    }

                    events.push((
                        id,
                        core::Event::Window(core::window::Event::Closed),
                    ));
                }

                if window_manager.is_empty() {
                    *compositor = None;
                }
            }
            window::Action::GetOldest(channel) => {
                let id =
                    window_manager.iter_mut().next().map(|(id, _window)| id);

                let _ = channel.send(id);
            }
            window::Action::GetLatest(channel) => {
                let id =
                    window_manager.iter_mut().last().map(|(id, _window)| id);

                let _ = channel.send(id);
            }
            window::Action::Drag(id) => {
                if let Some(window) = window_manager.get_mut(id) {
                    let _ = window.raw.drag_window();
                }
            }
            window::Action::DragResize(id, direction) => {
                if let Some(window) = window_manager.get_mut(id) {
                    let _ = window.raw.drag_resize_window(
                        conversion::resize_direction(direction),
                    );
                }
            }
            window::Action::Resize(id, size) => {
                if let Some(window) = window_manager.get_mut(id) {
                    let _ = window.raw.request_inner_size(
                        winit::dpi::LogicalSize {
                            width: size.width,
                            height: size.height,
                        }
                        .to_physical::<f32>(f64::from(
                            window.state.scale_factor(),
                        )),
                    );
                }
            }
            window::Action::SetMinSize(id, size) => {
                if let Some(window) = window_manager.get_mut(id) {
                    window.raw.set_min_inner_size(size.map(|size| {
                        winit::dpi::LogicalSize {
                            width: size.width,
                            height: size.height,
                        }
                        .to_physical::<f32>(f64::from(
                            window.state.scale_factor(),
                        ))
                    }));
                }
            }
            window::Action::SetMaxSize(id, size) => {
                if let Some(window) = window_manager.get_mut(id) {
                    window.raw.set_max_inner_size(size.map(|size| {
                        winit::dpi::LogicalSize {
                            width: size.width,
                            height: size.height,
                        }
                        .to_physical::<f32>(f64::from(
                            window.state.scale_factor(),
                        ))
                    }));
                }
            }
            window::Action::SetResizeIncrements(id, increments) => {
                if let Some(window) = window_manager.get_mut(id) {
                    window.raw.set_resize_increments(increments.map(|size| {
                        winit::dpi::LogicalSize {
                            width: size.width,
                            height: size.height,
                        }
                        .to_physical::<f32>(f64::from(
                            window.state.scale_factor(),
                        ))
                    }));
                }
            }
            window::Action::SetResizable(id, resizable) => {
                if let Some(window) = window_manager.get_mut(id) {
                    window.raw.set_resizable(resizable);
                }
            }
            window::Action::GetSize(id, channel) => {
                if let Some(window) = window_manager.get_mut(id) {
                    let size = window.logical_size();
                    let _ = channel.send(Size::new(size.width, size.height));
                }
            }
            window::Action::GetMaximized(id, channel) => {
                if let Some(window) = window_manager.get_mut(id) {
                    let _ = channel.send(window.raw.is_maximized());
                }
            }
            window::Action::Maximize(id, maximized) => {
                if let Some(window) = window_manager.get_mut(id) {
                    window.raw.set_maximized(maximized);
                }
            }
            window::Action::GetMinimized(id, channel) => {
                if let Some(window) = window_manager.get_mut(id) {
                    let _ = channel.send(window.raw.is_minimized());
                }
            }
            window::Action::Minimize(id, minimized) => {
                if let Some(window) = window_manager.get_mut(id) {
                    window.raw.set_minimized(minimized);
                }
            }
            window::Action::GetPosition(id, channel) => {
                if let Some(window) = window_manager.get(id) {
                    let position = window
                        .raw
                        .outer_position()
                        .map(|position| {
                            let position = position
                                .to_logical::<f32>(window.raw.scale_factor());

                            Point::new(position.x, position.y)
                        })
                        .ok();

                    let _ = channel.send(position);
                }
            }
            window::Action::GetScaleFactor(id, channel) => {
                if let Some(window) = window_manager.get_mut(id) {
                    let scale_factor = window.raw.scale_factor();

                    let _ = channel.send(scale_factor as f32);
                }
            }
            window::Action::Move(id, position) => {
                if let Some(window) = window_manager.get_mut(id) {
                    window.raw.set_outer_position(
                        winit::dpi::LogicalPosition {
                            x: position.x,
                            y: position.y,
                        },
                    );
                }
            }
            window::Action::SetMode(id, mode) => {
                if let Some(window) = window_manager.get_mut(id) {
                    window.raw.set_visible(conversion::visible(mode));
                    window.raw.set_fullscreen(conversion::fullscreen(
                        window.raw.current_monitor(),
                        mode,
                    ));
                }
            }
            window::Action::SetIcon(id, icon) => {
                if let Some(window) = window_manager.get_mut(id) {
                    window.raw.set_window_icon(conversion::icon(icon));
                }
            }
            window::Action::GetMode(id, channel) => {
                if let Some(window) = window_manager.get_mut(id) {
                    let mode = if window.raw.is_visible().unwrap_or(true) {
                        conversion::mode(window.raw.fullscreen())
                    } else {
                        core::window::Mode::Hidden
                    };

                    let _ = channel.send(mode);
                }
            }
            window::Action::ToggleMaximize(id) => {
                if let Some(window) = window_manager.get_mut(id) {
                    window.raw.set_maximized(!window.raw.is_maximized());
                }
            }
            window::Action::ToggleDecorations(id) => {
                if let Some(window) = window_manager.get_mut(id) {
                    window.raw.set_decorations(!window.raw.is_decorated());
                }
            }
            window::Action::RequestUserAttention(id, attention_type) => {
                if let Some(window) = window_manager.get_mut(id) {
                    window.raw.request_user_attention(
                        attention_type.map(conversion::user_attention),
                    );
                }
            }
            window::Action::GainFocus(id) => {
                if let Some(window) = window_manager.get_mut(id) {
                    window.raw.focus_window();
                }
            }
            window::Action::SetLevel(id, level) => {
                if let Some(window) = window_manager.get_mut(id) {
                    window
                        .raw
                        .set_window_level(conversion::window_level(level));
                }
            }
            window::Action::ShowSystemMenu(id) => {
                if let Some(window) = window_manager.get_mut(id)
                    && let mouse::Cursor::Available(point) =
                        window.state.cursor()
                {
                    window.raw.show_window_menu(winit::dpi::LogicalPosition {
                        x: point.x,
                        y: point.y,
                    });
                }
            }
            window::Action::GetRawId(id, channel) => {
                if let Some(window) = window_manager.get_mut(id) {
                    let _ = channel.send(window.raw.id().into());
                }
            }
            window::Action::Run(id, f) => {
                if let Some(window) = window_manager.get_mut(id) {
                    f(window);
                }
            }
            window::Action::Screenshot(id, channel) => {
                if let Some(window) = window_manager.get_mut(id)
                    && let Some(compositor) = compositor
                {
                    let bytes = compositor.screenshot(
                        &mut window.renderer,
                        window.state.viewport(),
                        window.state.background_color(),
                    );

                    let _ = channel.send(core::window::Screenshot::new(
                        bytes,
                        window.state.physical_size(),
                        window.state.scale_factor(),
                    ));
                }
            }
            window::Action::EnableMousePassthrough(id) => {
                if let Some(window) = window_manager.get_mut(id) {
                    let _ = window.raw.set_cursor_hittest(false);
                }
            }
            window::Action::DisableMousePassthrough(id) => {
                if let Some(window) = window_manager.get_mut(id) {
                    let _ = window.raw.set_cursor_hittest(true);
                }
            }
            window::Action::GetMonitorSize(id, channel) => {
                if let Some(window) = window_manager.get(id) {
                    let size = window.raw.current_monitor().map(|monitor| {
                        let scale = window.state.scale_factor();
                        let size = monitor.size().to_logical(f64::from(scale));

                        Size::new(size.width, size.height)
                    });

                    let _ = channel.send(size);
                }
            }
            window::Action::SetAllowAutomaticTabbing(enabled) => {
                control_sender
                    .start_send(Control::SetAutomaticWindowTabbing(enabled))
                    .expect("Send control action");
            }
            window::Action::RedrawAll => {
                for (_id, window) in window_manager.iter_mut() {
                    window.raw.request_redraw();
                }
            }
            window::Action::RelayoutAll => {
                for (id, window) in window_manager.iter_mut() {
                    if let Some(ui) = interfaces.remove(&id) {
                        let _ = interfaces.insert(
                            id,
                            ui.relayout(
                                window.state.logical_size(),
                                &mut window.renderer,
                            ),
                        );
                    }

                    window.raw.request_redraw();
                }
            }
        },
        Action::System(action) => match action {
            system::Action::GetInformation(_channel) => {
                #[cfg(feature = "sysinfo")]
                {
                    if let Some(compositor) = compositor {
                        let graphics_info = compositor.information();

                        let _ = std::thread::spawn(move || {
                            let information = system_information(graphics_info);

                            let _ = _channel.send(information);
                        });
                    }
                }
            }
            system::Action::GetTheme(channel) => {
                let _ = channel.send(*system_theme);
            }
            system::Action::NotifyTheme(mode) => {
                if mode != *system_theme {
                    *system_theme = mode;

                    runtime.broadcast(subscription::Event::SystemThemeChanged(
                        mode,
                    ));
                }

                let Some(theme) = conversion::window_theme(mode) else {
                    return;
                };

                for (_id, window) in window_manager.iter_mut() {
                    window.state.update(
                        program,
                        &window.raw,
                        &winit::event::WindowEvent::ThemeChanged(theme),
                    );
                }
            }
        },
        Action::Widget(operation) => {
            let mut current_operation = Some(operation);

            while let Some(mut operation) = current_operation.take() {
                for (id, ui) in interfaces.iter_mut() {
                    if let Some(window) = window_manager.get_mut(*id) {
                        ui.operate(&window.renderer, operation.as_mut());
                    }
                }

                match operation.finish() {
                    operation::Outcome::None => {}
                    operation::Outcome::Some(()) => {}
                    operation::Outcome::Chain(next) => {
                        current_operation = Some(next);
                    }
                }
            }
        }
        Action::Image(action) => match action {
            image::Action::Allocate(handle, sender) => {
                use core::Renderer as _;

                // TODO: Shared image cache in compositor
                if let Some((_id, window)) = window_manager.iter_mut().next() {
                    window.renderer.allocate_image(
                        &handle,
                        move |allocation| {
                            let _ = sender.send(allocation);
                        },
                    );
                }
            }
        },
        Action::LoadFont { bytes, channel } => {
            if let Some(compositor) = compositor {
                // TODO: Error handling (?)
                compositor.load_font(bytes.clone());

                let _ = channel.send(Ok(()));
            }
        }
        Action::Reload => {
            for (id, window) in window_manager.iter_mut() {
                let Some(ui) = interfaces.remove(&id) else {
                    continue;
                };

                let cache = ui.into_cache();
                let size = window.logical_size();

                let _ = interfaces.insert(
                    id,
                    build_user_interface(
                        program,
                        cache,
                        &mut window.renderer,
                        size,
                        id,
                    ),
                );

                window.raw.request_redraw();
            }
        }
        Action::Exit => {
            // winit cannot stop its event loop on iOS: once the instance
            // returned, the application would run on with a frozen screen.
            #[cfg(target_os = "ios")]
            log::warn!(
                "iced::exit is ignored on iOS: an iOS application does not \
                end itself; the system ends it."
            );

            // The Activity would stay on screen with no event loop behind it.
            #[cfg(target_os = "android")]
            log::warn!(
                "iced::exit is ignored on Android: Android, not the \
                application, ends an Activity, and an event loop that ended \
                on its own would leave the Activity on screen with nothing \
                behind it. To leave the screen, finish the Activity \
                (Activity.finish) or move its task to the back \
                (Activity.moveTaskToBack) through JNI: once Android destroys \
                the Activity, the event loop and the application end with it. \
                To end the process, call libc::_exit: std::process::exit runs \
                exit handlers that make Android's renderer threads abort."
            );

            #[cfg(not(any(target_os = "android", target_os = "ios")))]
            control_sender
                .start_send(Control::Exit)
                .expect("Send control action");
        }
    }
}

/// Build the user interface for every window.
pub fn build_user_interfaces<'a, P: Program, C>(
    program: &'a program::Instance<P>,
    window_manager: &mut WindowManager<P, C>,
    mut cached_user_interfaces: FxHashMap<window::Id, user_interface::Cache>,
) -> FxHashMap<window::Id, UserInterface<'a, P::Message, P::Theme, P::Renderer>>
where
    C: Compositor<Renderer = P::Renderer>,
    P::Theme: theme::Base,
{
    for (id, window) in window_manager.iter_mut() {
        window.state.synchronize(program, id, &window.raw);
    }

    debug::theme_changed(|| {
        window_manager
            .first()
            .and_then(|window| theme::Base::palette(window.state.theme()))
    });

    cached_user_interfaces
        .drain()
        .filter_map(|(id, cache)| {
            let window = window_manager.get_mut(id)?;

            Some((
                id,
                build_user_interface(
                    program,
                    cache,
                    &mut window.renderer,
                    window.state.logical_size(),
                    id,
                ),
            ))
        })
        .collect()
}

/// Returns true if the provided event should cause a [`Program`] to
/// exit.
pub fn user_force_quit(
    event: &winit::event::WindowEvent,
    _modifiers: winit::keyboard::ModifiersState,
) -> bool {
    match event {
        #[cfg(target_os = "macos")]
        winit::event::WindowEvent::KeyboardInput {
            event:
                winit::event::KeyEvent {
                    logical_key: winit::keyboard::Key::Character(c),
                    state: winit::event::ElementState::Pressed,
                    ..
                },
            ..
        } if c == "q" && _modifiers.super_key() => true,
        _ => false,
    }
}

#[cfg(feature = "sysinfo")]
fn system_information(
    graphics: compositor::Information,
) -> system::Information {
    use sysinfo::{Process, System};

    let mut system = System::new_all();
    system.refresh_all();

    let cpu_brand = system
        .cpus()
        .first()
        .map(|cpu| cpu.brand().to_string())
        .unwrap_or_default();

    let memory_used = sysinfo::get_current_pid()
        .and_then(|pid| system.process(pid).ok_or("Process not found"))
        .map(Process::memory)
        .ok();

    system::Information {
        system_name: System::name(),
        system_kernel: System::kernel_version(),
        system_version: System::long_os_version(),
        system_short_version: System::os_version(),
        cpu_brand,
        cpu_cores: system.physical_core_count(),
        memory_total: system.total_memory(),
        memory_used,
        graphics_adapter: graphics.adapter,
        graphics_backend: graphics.backend,
    }
}

/// `UnboundedReceiver::try_next`, which futures-channel 0.3.32 deprecates
/// for `try_recv`. 0.3.31, which many lockfiles still hold, has no
/// `try_recv`, so this keeps builds against either free of warnings.
#[allow(deprecated)]
fn try_next<T>(
    receiver: &mut mpsc::UnboundedReceiver<T>,
) -> Result<Option<T>, mpsc::TryRecvError> {
    receiver.try_next()
}
