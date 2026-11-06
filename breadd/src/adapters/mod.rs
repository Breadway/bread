use anyhow::Result;
use async_trait::async_trait;
use bread_shared::RawEvent;
use serde::Serialize;
use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::{mpsc, watch, RwLock};
use tracing::info;

use crate::core::config::Config;
use crate::core::supervisor::spawn_supervised;

pub mod bluetooth;
pub mod filesystem;
pub mod git;
pub mod hyprland;
pub mod network;
pub mod network_rtnetlink;
pub mod podman;
pub mod power;
pub mod power_upower;
pub mod systemd;
pub mod udev;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum AdapterStatus {
    Connected,
    Disconnected,
    /// An optional adapter was enabled but its external dependency (for
    /// example the `podman` binary) is not installed. Unlike `Disconnected`
    /// this is terminal, not a reconnect that is still being retried, so
    /// `bread doctor` can tell "not installed" from "should be running".
    Unavailable,
}

#[async_trait]
pub trait Adapter: Send + Sync {
    fn name(&self) -> &'static str;
    async fn run(&self, tx: mpsc::Sender<RawEvent>) -> Result<()>;
    async fn on_connect(&self) -> Result<()> {
        Ok(())
    }
    async fn on_disconnect(&self) -> Result<()> {
        Ok(())
    }
}

pub struct Manager {
    raw_tx: mpsc::Sender<RawEvent>,
    config: Config,
    shutdown_rx: watch::Receiver<bool>,
    status: Arc<RwLock<HashMap<String, AdapterStatus>>>,
}

impl Manager {
    pub fn new(
        raw_tx: mpsc::Sender<RawEvent>,
        config: Config,
        shutdown_rx: watch::Receiver<bool>,
    ) -> Self {
        Self {
            raw_tx,
            config,
            shutdown_rx,
            status: Arc::new(RwLock::new(HashMap::new())),
        }
    }

    pub fn status_handle(&self) -> Arc<RwLock<HashMap<String, AdapterStatus>>> {
        self.status.clone()
    }

    pub async fn start_all(&self) -> Result<()> {
        info!("starting adapters");

        if self.config.adapters.udev.enabled {
            let adapter = udev::UdevAdapter::new(self.config.adapters.udev.subsystems.clone());
            adapter.enumerate_existing(&self.raw_tx).await?;
            self.spawn_adapter(adapter);
        }

        if self.config.adapters.hyprland.enabled {
            self.spawn_adapter(hyprland::HyprlandAdapter);
        }

        if self.config.adapters.power.enabled {
            // Prefer UPower D-Bus adapter; fall back to sysfs poller if D-Bus is unavailable.
            match power_upower::UPowerAdapter::probe().await {
                Ok(adapter) => self.spawn_adapter(adapter),
                Err(e) => {
                    info!("upower unavailable ({e}), falling back to sysfs power poller");
                    self.spawn_adapter(power::PowerAdapter::new(
                        self.config.adapters.power.poll_interval_secs,
                    ));
                }
            }
        }

        if self.config.adapters.bluetooth.enabled {
            let adapter = bluetooth::BluetoothAdapter::new();
            adapter.enumerate_existing(&self.raw_tx).await;
            self.spawn_adapter(adapter);
        }

        if self.config.adapters.network.enabled {
            // Prefer rtnetlink-based adapter; fall back to existing sysfs-based adapter
            let rt = network_rtnetlink::RtnetlinkAdapter::new();
            if let Ok(adapter) = rt {
                self.spawn_adapter(adapter);
            } else {
                self.spawn_adapter(network::NetworkAdapter);
            }
        }

        // Filesystem/git/systemd all default to "no roots/units configured",
        // in which case they'd have nothing to do: skip spawning them
        // entirely rather than running an adapter that can never emit.
        if self.config.adapters.filesystem.enabled
            && !self.config.adapters.filesystem.roots.is_empty()
        {
            let adapter =
                filesystem::FilesystemAdapter::new(self.config.adapters.filesystem.roots.clone());
            adapter.enumerate_existing(&self.raw_tx).await;
            self.spawn_adapter(adapter);
        }

        if self.config.adapters.git.enabled && !self.config.adapters.git.roots.is_empty() {
            let adapter = git::GitAdapter::new(self.config.adapters.git.roots.clone());
            self.spawn_adapter(adapter);
        }

        if self.config.adapters.systemd.enabled && !self.config.adapters.systemd.units.is_empty() {
            let adapter = systemd::SystemdAdapter::new(self.config.adapters.systemd.units.clone());
            self.spawn_adapter(adapter);
        }

        if self.config.adapters.podman.enabled {
            // Podman is optional. Probe once so a machine without the binary
            // gets a single informational line and a terminal "unavailable"
            // status instead of an error and a 32-second backoff restart loop
            // for the lifetime of the daemon. A binary that exists but fails
            // later still goes through the normal supervisor retry path.
            self.start_optional_adapter(podman::PodmanAdapter::new(), podman::binary_available())
                .await;
        }

        Ok(())
    }

    /// Spawn an adapter backed by an optional external dependency, or mark it
    /// [`AdapterStatus::Unavailable`] without entering the retry loop.
    ///
    /// Returns whether the adapter was started. Kept separate from
    /// [`Self::spawn_adapter`] so the absent-dependency path is unit-testable
    /// without manipulating the process `PATH` or spawning a real binary.
    async fn start_optional_adapter<A>(&self, adapter: A, available: bool) -> bool
    where
        A: Adapter + Clone + 'static,
    {
        if !available {
            info!(
                adapter = adapter.name(),
                "optional adapter disabled: external dependency not found"
            );
            self.status
                .write()
                .await
                .insert(adapter.name().to_string(), AdapterStatus::Unavailable);
            return false;
        }

        self.spawn_adapter(adapter);
        true
    }

    fn spawn_adapter<A>(&self, adapter: A)
    where
        A: Adapter + Clone + 'static,
    {
        let name = adapter.name();
        let tx = self.raw_tx.clone();
        let shutdown_rx = self.shutdown_rx.clone();
        let shutdown_for_task = shutdown_rx.clone();
        let status = self.status.clone();
        spawn_supervised(name, shutdown_rx, move || {
            let adapter = adapter.clone();
            let tx = tx.clone();
            let mut shutdown_rx = shutdown_for_task.clone();
            let status = status.clone();
            async move {
                adapter.on_connect().await?;
                {
                    let mut guard = status.write().await;
                    guard.insert(adapter.name().to_string(), AdapterStatus::Connected);
                }
                let result = tokio::select! {
                    result = adapter.run(tx) => result,
                    _ = shutdown_rx.changed() => Ok(()),
                };
                adapter.on_disconnect().await?;
                {
                    let mut guard = status.write().await;
                    guard.insert(adapter.name().to_string(), AdapterStatus::Disconnected);
                }
                result
            }
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;
    use std::sync::Mutex;
    use std::time::Duration;
    use tracing_subscriber::fmt::MakeWriter;

    #[derive(Clone, Default)]
    struct LogBuffer(Arc<Mutex<Vec<u8>>>);

    impl Write for LogBuffer {
        fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
            self.0.lock().unwrap().extend_from_slice(buf);
            Ok(buf.len())
        }

        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }

    impl<'a> MakeWriter<'a> for LogBuffer {
        type Writer = Self;

        fn make_writer(&'a self) -> Self::Writer {
            self.clone()
        }
    }

    impl LogBuffer {
        fn contents(&self) -> String {
            String::from_utf8(self.0.lock().unwrap().clone()).unwrap()
        }
    }

    #[derive(Clone)]
    struct StubAdapter;

    #[async_trait]
    impl Adapter for StubAdapter {
        fn name(&self) -> &'static str {
            "stub"
        }

        async fn run(&self, _tx: mpsc::Sender<RawEvent>) -> Result<()> {
            std::future::pending::<()>().await;
            Ok(())
        }
    }

    fn manager(shutdown_rx: watch::Receiver<bool>) -> (Manager, mpsc::Receiver<RawEvent>) {
        let (raw_tx, raw_rx) = mpsc::channel(8);
        (Manager::new(raw_tx, Config::default(), shutdown_rx), raw_rx)
    }

    #[tokio::test]
    async fn absent_optional_dependency_logs_once_and_reports_unavailable() {
        let (_shutdown_tx, shutdown_rx) = watch::channel(false);
        let (manager, mut raw_rx) = manager(shutdown_rx);

        let logs = LogBuffer::default();
        let subscriber = tracing_subscriber::fmt()
            .with_max_level(tracing::Level::INFO)
            .with_ansi(false)
            .with_writer(logs.clone())
            .finish();
        let _guard = tracing::subscriber::set_default(subscriber);

        let started = manager
            .start_optional_adapter(podman::PodmanAdapter::new(), false)
            .await;
        assert!(!started, "an absent dependency must not spawn the adapter");

        // Give a hypothetical retry loop time to log again, then confirm the
        // status never flips to connected/disconnected and nothing was sent.
        tokio::time::sleep(Duration::from_millis(100)).await;
        let status = manager.status_handle().read().await.clone();
        assert_eq!(status.get("podman"), Some(&AdapterStatus::Unavailable));
        assert!(raw_rx.try_recv().is_err());

        let contents = logs.contents();
        assert_eq!(
            contents
                .matches("optional adapter disabled: external dependency not found")
                .count(),
            1,
            "expected exactly one informational line, got: {contents}"
        );
        assert!(
            !contents.contains("adapter task failed"),
            "absent optional dependency must not reach the error/retry path: {contents}"
        );
    }

    #[tokio::test]
    async fn present_optional_dependency_spawns_adapter() {
        let (shutdown_tx, shutdown_rx) = watch::channel(false);
        let (manager, _raw_rx) = manager(shutdown_rx);

        let started = manager.start_optional_adapter(StubAdapter, true).await;
        assert!(
            started,
            "an available dependency must keep the adapter active"
        );

        let status = manager.status_handle();
        for _ in 0..50 {
            if status.read().await.get("stub") == Some(&AdapterStatus::Connected) {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        assert_eq!(
            status.read().await.get("stub"),
            Some(&AdapterStatus::Connected)
        );

        let _ = shutdown_tx.send(true);
    }
}
