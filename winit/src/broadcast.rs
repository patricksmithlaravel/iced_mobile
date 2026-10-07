//! Values the shell publishes to subscriptions, beside the runtime's event
//! stream: `subscription::Event` takes no new variant without breaking
//! exhaustive matches downstream.
use crate::futures::futures::channel::mpsc;

use std::sync::{Mutex, PoisonError};

/// A value published to every live subscriber.
pub(crate) struct Broadcast<T> {
    /// Whether a new subscriber first receives the latest value.
    replay: bool,
    inner: Mutex<Inner<T>>,
}

struct Inner<T> {
    latest: Option<T>,
    subscribers: Vec<mpsc::UnboundedSender<T>>,
}

impl<T: Clone> Broadcast<T> {
    pub(crate) const fn new(replay: bool) -> Self {
        Self {
            replay,
            inner: Mutex::new(Inner {
                latest: None,
                subscribers: Vec::new(),
            }),
        }
    }

    /// Sends `value` to every subscriber still listening, and keeps it as
    /// the latest.
    pub(crate) fn publish(&self, value: T) {
        let mut inner =
            self.inner.lock().unwrap_or_else(PoisonError::into_inner);

        inner.subscribers.retain(|subscriber| {
            subscriber.unbounded_send(value.clone()).is_ok()
        });
        inner.latest = Some(value);
    }

    /// Forgets the latest value, when a new application starts (Android
    /// runs one per Activity in the same process).
    pub(crate) fn reset(&self) {
        self.inner
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .latest = None;
    }

    /// The values published from now on; with `replay`, the latest first.
    pub(crate) fn subscribe(&self) -> mpsc::UnboundedReceiver<T> {
        let (sender, receiver) = mpsc::unbounded();
        let mut inner =
            self.inner.lock().unwrap_or_else(PoisonError::into_inner);

        if self.replay
            && let Some(latest) = inner.latest.clone()
        {
            let _ = sender.unbounded_send(latest);
        }

        inner.subscribers.push(sender);
        receiver
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The next value already sent, if any.
    fn next<T>(receiver: &mut mpsc::UnboundedReceiver<T>) -> Option<T> {
        crate::try_next(receiver).ok().flatten()
    }

    #[test]
    fn replays_the_latest_and_prunes_dropped_subscribers() {
        let broadcast = Broadcast::new(true);
        broadcast.publish(1);

        let mut first = broadcast.subscribe();
        assert_eq!(next(&mut first), Some(1));

        drop(broadcast.subscribe());
        broadcast.publish(2);
        assert_eq!(next(&mut first), Some(2));
        assert_eq!(broadcast.inner.lock().unwrap().subscribers.len(), 1);

        broadcast.reset();
        let mut late = broadcast.subscribe();
        assert_eq!(next(&mut late), None);

        broadcast.publish(3);
        assert_eq!(next(&mut late), Some(3));
    }

    #[test]
    fn without_replay_a_subscriber_sees_only_what_follows() {
        let broadcast = Broadcast::new(false);
        broadcast.publish(1);

        let mut subscriber = broadcast.subscribe();
        assert_eq!(next(&mut subscriber), None);

        broadcast.publish(2);
        assert_eq!(next(&mut subscriber), Some(2));
    }
}
