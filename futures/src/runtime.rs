//! Run commands and keep track of subscriptions.
use crate::subscription;
use crate::{BoxStream, Executor, MaybeSend};

use futures::channel::{mpsc, oneshot};
use futures::future::{self, FutureExt, Shared};
use futures::{Sink, SinkExt};
use std::marker::PhantomData;

/// A batteries-included runtime of commands and subscriptions.
///
/// If you have an [`Executor`], a [`Runtime`] can be leveraged to run any
/// `Command` or [`Subscription`] and get notified of the results!
///
/// Dropping a [`Runtime`] ends the futures it spawned that are still
/// running, the next time their executor polls them. An executor that
/// lives on (another runtime of the same process, as when Android starts a
/// new Activity) is not left running them.
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

/// Ends, once dropped, the futures it guards.
#[derive(Debug)]
struct Stop {
    _sender: oneshot::Sender<()>,
    stopped: Shared<oneshot::Receiver<()>>,
}

impl Stop {
    fn new() -> Self {
        let (sender, receiver) = oneshot::channel();

        Self {
            _sender: sender,
            stopped: receiver.shared(),
        }
    }

    /// Runs `future` until it completes or this [`Stop`] is dropped.
    fn guard(
        &self,
        future: impl Future<Output = ()> + MaybeSend + 'static,
    ) -> impl Future<Output = ()> + MaybeSend + 'static {
        let stopped = self.stopped.clone();

        async move {
            let future = std::pin::pin!(future);
            let _ = future::select(future, stopped).await;
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
        Self {
            executor,
            sender,
            subscriptions: subscription::Tracker::new(),
            stop: Stop::new(),
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
            ..
        } = self;

        let futures = executor.enter(|| {
            subscriptions.update(recipes.into_iter(), sender.clone())
        });

        for future in futures {
            executor.spawn(future);
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

    #[test]
    fn dropping_the_runtime_ends_what_it_spawned() {
        let executor = Spawned::default();
        let (sender, _receiver) = mpsc::channel::<()>(1);
        let mut runtime = Runtime::new(executor.clone(), sender);

        runtime.run(Box::pin(futures::stream::pending()));

        let mut future = executor
            .0
            .lock()
            .expect("Lock futures")
            .pop()
            .expect("A spawned future");

        let mut context = Context::from_waker(noop_waker_ref());

        assert!(future.as_mut().poll(&mut context).is_pending());

        drop(runtime);

        assert!(future.as_mut().poll(&mut context).is_ready());
    }
}
