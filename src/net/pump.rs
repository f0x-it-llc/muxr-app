//! What a reconnecting watch pump shares: the stop latch, a two-way race and a backoff nap that
//! gives its thread back as soon as the pump drops it.

use std::future::Future;
use std::sync::atomic::{AtomicBool, Ordering};
use std::task::Poll;
use std::time::Duration;

use tokio::sync::Notify;

/// Longest a nap holds the blocking pool before it checks whether it was dropped.
const NAP_SLICE: Duration = Duration::from_millis(100);

#[derive(Default)]
pub struct Stop {
    stopped: AtomicBool,
    wake: Notify,
}

impl Stop {
    pub fn stop(&self) {
        self.stopped.store(true, Ordering::Release);
        self.wake.notify_one();
    }

    pub fn is_stopped(&self) -> bool {
        self.stopped.load(Ordering::Acquire)
    }

    /// The stored permit makes a stop raised between the check and the wait unmissable.
    pub async fn stopped(&self) {
        while !self.is_stopped() {
            self.wake.notified().await;
        }
    }
}

/// Production `tokio` has no timer, so the app naps on frust's blocking pool, a slice at a time:
/// a nap raced away stops after its current slice instead of sleeping out the whole delay.
pub enum Nap {
    Frust,
    #[cfg(test)]
    Tokio,
}

impl Nap {
    pub async fn sleep(&self, delay: Duration) {
        match self {
            Nap::Frust => {
                sliced(delay, NAP_SLICE, |slice| async move {
                    let _ = frust::spawn_blocking(move || std::thread::sleep(slice)).await;
                })
                .await;
            }
            #[cfg(test)]
            Nap::Tokio => tokio::time::sleep(delay).await,
        }
    }
}

async fn sliced<F, Fut>(delay: Duration, slice: Duration, pause: F)
where
    F: Fn(Duration) -> Fut,
    Fut: Future<Output = ()>,
{
    let mut left = delay;
    while !left.is_zero() {
        let step = left.min(slice);
        pause(step).await;
        left -= step;
    }
}

pub enum Winner<A, B> {
    First(A),
    Second(B),
}

/// Production `tokio` has no `select!`; `first` wins a tie.
pub async fn race<A: Future, B: Future>(first: A, second: B) -> Winner<A::Output, B::Output> {
    let mut first = std::pin::pin!(first);
    let mut second = std::pin::pin!(second);
    std::future::poll_fn(move |cx| {
        if let Poll::Ready(value) = first.as_mut().poll(cx) {
            return Poll::Ready(Winner::First(value));
        }
        if let Poll::Ready(value) = second.as_mut().poll(cx) {
            return Poll::Ready(Winner::Second(value));
        }
        Poll::Pending
    })
    .await
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;
    use std::sync::atomic::AtomicUsize;
    use std::time::Instant;

    use super::*;

    #[tokio::test]
    async fn a_stopped_nap_ends_within_a_slice_not_the_whole_delay() {
        let stop = Arc::new(Stop::default());
        let slept = Arc::new(AtomicUsize::new(0));
        let counter = Arc::clone(&slept);
        let nap = sliced(
            Duration::from_secs(16),
            Duration::from_millis(10),
            move |s| {
                counter.fetch_add(1, Ordering::SeqCst);
                tokio::time::sleep(s)
            },
        );
        let raiser = Arc::clone(&stop);
        tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(30)).await;
            raiser.stop();
        });
        let began = Instant::now();
        let Winner::First(()) = race(stop.stopped(), nap).await else {
            panic!("the stop wins a 16 s nap");
        };
        assert!(began.elapsed() < Duration::from_secs(2));
        assert!(
            slept.load(Ordering::SeqCst) < 20,
            "no slice is started after the drop"
        );
    }

    #[tokio::test]
    async fn an_unraced_nap_sleeps_its_whole_delay_in_slices() {
        let slept = Arc::new(AtomicUsize::new(0));
        let counter = Arc::clone(&slept);
        sliced(
            Duration::from_millis(35),
            Duration::from_millis(10),
            move |s| {
                counter.fetch_add(1, Ordering::SeqCst);
                tokio::time::sleep(s)
            },
        )
        .await;
        assert_eq!(slept.load(Ordering::SeqCst), 4);
    }

    #[tokio::test]
    async fn race_prefers_the_first_future_on_a_tie() {
        let winner = race(std::future::ready(1), std::future::ready(2)).await;
        assert!(matches!(winner, Winner::First(1)));
    }
}
