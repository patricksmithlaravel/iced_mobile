//! Run commands and keep track of subscriptions.
use crate::subscription;
use crate::{BoxStream, Executor, MaybeSend};

use futures::channel::{mpsc, oneshot};
use futures::future::{self, Either, FutureExt, Shared};
use futures::{Sink, SinkExt};
use std::marker::PhantomData;

/// A batteries-included runtime of commands and subscriptions.
///
/// If you have an [`Executor`], a [`Runtime`] can be leveraged to run any
/// `Command` or [`Subscription`] and get notified of the results!
///
/// On Android and iOS, dropping a [`Runtime`] ends the futures it spawned
/// that are still running, subscriptions included, the next time their
/// executor polls them. On Android a new Activity starts a new application
/// in the same process, and an executor that lives on is not left running
/// the last one's futures. Elsewhere, as in upstream iced, they keep running
/// on their executor.
///
/// [`Subscription`]: crate::Subscription
#[derive(Debug)]
pub struct Runtime<Executor, Sender, Message> {
    executor: Executor,
    sender: Sender,
    subscriptions: subscription::Tracker,
    stop: Stop,
    _message: PhantomData<Message>,
}

/// Whether dropping a [`Runtime`] ends the futures it spawned: on Android and
/// iOS only.
const ENDS_ON_DROP: bool = cfg!(any(target_os = "android", target_os = "ios"));

/// Ends, once dropped, the futures it guards, if it was made to.
#[derive(Debug)]
struct Stop {
    /// The sender, whose drop ends the futures, and what they wait on; `None`
    /// when they outlive the [`Stop`].
    signal: Option<(oneshot::Sender<()>, Shared<oneshot::Receiver<()>>)>,
}

impl Stop {
    /// A [`Stop`] that ends the futures it guards once dropped if `ends`,
    /// and otherwise leaves them running.
    fn new(ends: bool) -> Self {
        Self {
            signal: ends.then(|| {
                let (sender, receiver) = oneshot::channel();

                (sender, receiver.shared())
            }),
        }
    }

    /// Runs `future` until it completes or, if this [`Stop`] ends what it
    /// guards, until it is dropped.
    fn guard(
        &self,
        future: impl Future<Output = ()> + MaybeSend + 'static,
    ) -> impl Future<Output = ()> + MaybeSend + 'static {
        match &self.signal {
            Some((_sender, stopped)) => {
                let stopped = stopped.clone();

                Either::Left(async move {
                    let future = std::pin::pin!(future);
                    let _ = future::select(future, stopped).await;
                })
            }
            None => Either::Right(future),
        }
    }
}

impl<Executor, Sender, Message> Runtime<Executor, Sender, Message>
where
    Executor: self::Executor,
    Sender: Sink<Message, Error = mpsc::SendError>
        + Unpin
        + MaybeSend
        + Clone
        + 'static,
    Message: MaybeSend + 'static,
{
    /// Creates a new empty [`Runtime`].
    ///
    /// You need to provide:
    /// - an [`Executor`] to spawn futures
    /// - a `Sender` implementing `Sink` to receive the results
    pub fn new(executor: Executor, sender: Sender) -> Self {
        Self::with_stop(executor, sender, Stop::new(ENDS_ON_DROP))
    }

    fn with_stop(executor: Executor, sender: Sender, stop: Stop) -> Self {
        Self {
            executor,
            sender,
            subscriptions: subscription::Tracker::new(),
            stop,
            _message: PhantomData,
        }
    }

    /// Runs the given closure inside the [`Executor`] of the [`Runtime`].
    ///
    /// See [`Executor::enter`] to learn more.
    pub fn enter<R>(&self, f: impl FnOnce() -> R) -> R {
        self.executor.enter(f)
    }

    /// Runs a future to completion in the current thread within the [`Runtime`].
    #[cfg(not(target_arch = "wasm32"))]
    pub fn block_on<T>(&mut self, future: impl Future<Output = T>) -> T {
        self.executor.block_on(future)
    }

    /// Runs a [`Stream`] in the [`Runtime`] until completion.
    ///
    /// The resulting `Message`s will be forwarded to the `Sender` of the
    /// [`Runtime`].
    ///
    /// [`Stream`]: BoxStream
    pub fn run(&mut self, stream: BoxStream<Message>) {
        use futures::StreamExt;

        let sender = self.sender.clone();
        let future =
            stream.map(Ok).forward(sender).map(|result| match result {
                Ok(()) => (),
                Err(error) => {
                    log::warn!(
                        "Stream could not run until completion: {error}"
                    );
                }
            });

        self.executor.spawn(self.stop.guard(future));
    }

    /// Sends a message concurrently through the [`Runtime`].
    pub fn send(&mut self, message: Message) {
        let mut sender = self.sender.clone();

        self.executor.spawn(self.stop.guard(async move {
            let _ = sender.send(message).await;
        }));
    }

    /// Tracks a [`Subscription`] in the [`Runtime`].
    ///
    /// It will spawn new streams or close old ones as necessary! See
    /// [`Tracker::update`] to learn more about this!
    ///
    /// [`Tracker::update`]: subscription::Tracker::update
    /// [`Subscription`]: crate::Subscription
    pub fn track(
        &mut self,
        recipes: impl IntoIterator<
            Item = Box<dyn subscription::Recipe<Output = Message>>,
        >,
    ) {
        let Runtime {
            executor,
            subscriptions,
            sender,
            stop,
            ..
        } = self;

        let futures = executor.enter(|| {
            subscriptions.update(recipes.into_iter(), sender.clone())
        });

        // The Tracker cancels a subscription only while it waits for its
        // stream. On Android and iOS, one blocked on sending to a full
        // `Sender` would outlive the Runtime without the guard.
        for future in futures {
            executor.spawn(stop.guard(future));
        }
    }

    /// Broadcasts an event to all the subscriptions currently alive in the
    /// [`Runtime`].
    ///
    /// See [`Tracker::broadcast`] to learn more.
    ///
    /// [`Tracker::broadcast`]: subscription::Tracker::broadcast
    pub fn broadcast(&mut self, event: subscription::Event) {
        self.subscriptions.broadcast(event);
    }
}

#[cfg(all(test, not(target_arch = "wasm32")))]
mod tests {
    use super::*;

    use futures::future::BoxFuture;
    use futures::task::{Context, noop_waker_ref};
    use std::sync::{Arc, Mutex};

    /// Keeps what it is given to spawn, for the test to poll.
    #[derive(Clone, Default)]
    struct Spawned(Arc<Mutex<Vec<BoxFuture<'static, ()>>>>);

    impl crate::Executor for Spawned {
        fn new() -> Result<Self, futures::io::Error> {
            Ok(Self::default())
        }

        fn spawn(
            &self,
            future: impl Future<Output = ()> + MaybeSend + 'static,
        ) {
            self.0.lock().expect("Lock futures").push(Box::pin(future));
        }

        fn block_on<T>(&self, _future: impl Future<Output = T>) -> T {
            unreachable!("not used by the test")
        }
    }

    /// The future that `executor` was given last, polled once: it must be
    /// running.
    fn running(executor: &Spawned) -> BoxFuture<'static, ()> {
        let mut future = executor
            .0
            .lock()
            .expect("Lock futures")
            .pop()
            .expect("A spawned future");

        assert!(!ended(&mut future), "the spawned future is running");

        future
    }

    /// Whether `future` ends when polled.
    fn ended(future: &mut BoxFuture<'static, ()>) -> bool {
        let mut context = Context::from_waker(noop_waker_ref());

        future.as_mut().poll(&mut context).is_ready()
    }

    #[test]
    fn a_dropped_runtime_ends_its_futures_on_android_and_ios_only() {
        let executor = Spawned::default();
        let (sender, _receiver) = mpsc::channel::<()>(1);
        let mut runtime = Runtime::new(executor.clone(), sender);

        runtime.run(Box::pin(futures::stream::pending()));

        let mut future = running(&executor);

        drop(runtime);

        assert_eq!(
            ended(&mut future),
            cfg!(any(target_os = "android", target_os = "ios"))
        );
    }

    #[test]
    fn a_runtime_that_leaves_its_futures_leaves_what_it_spawned_running() {
        let executor = Spawned::default();
        let (sender, _receiver) = mpsc::channel::<()>(1);
        let mut runtime =
            Runtime::with_stop(executor.clone(), sender, Stop::new(false));

        runtime.run(Box::pin(futures::stream::pending()));

        let mut future = running(&executor);

        drop(runtime);

        assert!(!ended(&mut future));
    }

    #[test]
    fn a_runtime_that_ends_its_futures_ends_what_it_spawned() {
        let executor = Spawned::default();
        let (sender, _receiver) = mpsc::channel::<()>(1);
        let mut runtime =
            Runtime::with_stop(executor.clone(), sender, Stop::new(true));

        runtime.run(Box::pin(futures::stream::pending()));

        let mut future = running(&executor);

        drop(runtime);

        assert!(ended(&mut future));
    }

    #[test]
    fn a_runtime_that_ends_its_futures_ends_a_blocked_subscription() {
        use std::sync::atomic::{AtomicBool, Ordering};

        static DROPPED: AtomicBool = AtomicBool::new(false);

        struct Flag;

        impl Drop for Flag {
            fn drop(&mut self) {
                DROPPED.store(true, Ordering::SeqCst);
            }
        }

        // A subscription that produces on its own, as a clock does.
        fn ticking() -> impl futures::Stream<Item = ()> {
            futures::stream::unfold(Flag, |flag| async { Some(((), flag)) })
        }

        let executor = Spawned::default();

        // Nothing reads it: the subscription's first message fills it, and
        // the second one waits.
        let (sender, _receiver) = mpsc::channel::<()>(0);
        let mut runtime =
            Runtime::with_stop(executor.clone(), sender, Stop::new(true));

        runtime.track(subscription::into_recipes(crate::Subscription::run(
            ticking,
        )));

        let mut future = running(&executor);

        drop(runtime);

        assert!(ended(&mut future));
        assert!(DROPPED.load(Ordering::SeqCst));
    }
}
