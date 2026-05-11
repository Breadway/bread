use std::future::Future;

use tokio::sync::watch;
use tokio::time::{sleep, Duration};
use tracing::{error, info, warn};

pub fn spawn_supervised<F, Fut>(
    name: &'static str,
    mut shutdown_rx: watch::Receiver<bool>,
    mut task_factory: F,
)
where
    F: FnMut() -> Fut + Send + 'static,
    Fut: Future<Output = anyhow::Result<()>> + Send + 'static,
{
    tokio::spawn(async move {
        let mut attempt: u32 = 0;

        loop {
            if *shutdown_rx.borrow() {
                info!(adapter = name, "shutdown requested");
                break;
            }

            let result = tokio::select! {
                _ = shutdown_rx.changed() => {
                    if *shutdown_rx.borrow() {
                        info!(adapter = name, "shutdown requested");
                        break;
                    }
                    continue;
                }
                result = task_factory() => result,
            };

            match result {
                Ok(()) => {
                    info!(adapter = name, "adapter task exited cleanly");
                    attempt = 0;
                }
                Err(err) => {
                    error!(adapter = name, error = %err, "adapter task failed");
                    attempt = attempt.saturating_add(1);
                }
            }

            if *shutdown_rx.borrow() {
                info!(adapter = name, "shutdown requested");
                break;
            }

            let wait_ms = 500u64.saturating_mul(2u64.saturating_pow(attempt.min(6)));
            warn!(adapter = name, delay_ms = wait_ms, "restarting adapter after failure");
            tokio::select! {
                _ = sleep(Duration::from_millis(wait_ms)) => {},
                _ = shutdown_rx.changed() => {
                    if *shutdown_rx.borrow() {
                        info!(adapter = name, "shutdown requested");
                        break;
                    }
                }
            }
        }
    });
}
