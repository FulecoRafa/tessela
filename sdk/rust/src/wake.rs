use std::future::Future;
use std::time::Duration;

use async_io::Timer;
use futures_lite::future;

/// "Something changed, look again": wakes one waiter early; wakes before
/// anyone waits are kept (once). Cheap to clone.
#[derive(Clone)]
pub struct Wake {
    tx: async_channel::Sender<()>,
    rx: async_channel::Receiver<()>,
}

impl Default for Wake {
    fn default() -> Self {
        Self::new()
    }
}

impl Wake {
    pub fn new() -> Self {
        let (tx, rx) = async_channel::bounded(1);
        Self { tx, rx }
    }

    pub fn wake(&self) {
        let _ = self.tx.try_send(());
    }

    pub async fn wait(&self) {
        let _ = self.rx.recv().await;
    }

    /// Wait for a wake or `timeout`; true if woken.
    pub async fn wait_for(&self, timeout: Duration) -> bool {
        future::or(async { self.rx.recv().await.is_ok() }, async {
            Timer::after(timeout).await;
            false
        })
        .await
    }
}

/// Call `read` now, then every `interval`, or sooner when `wake` fires.
/// The poll loop most data sources without change events need.
pub async fn poll<F, Fut>(interval: Duration, wake: &Wake, mut read: F)
where
    F: FnMut() -> Fut,
    Fut: Future<Output = ()>,
{
    loop {
        read().await;
        wake.wait_for(interval).await;
    }
}
