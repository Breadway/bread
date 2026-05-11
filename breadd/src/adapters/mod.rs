use anyhow::Result;
use async_trait::async_trait;
use bread_shared::RawEvent;
use tokio::sync::{mpsc, watch};
use tracing::info;

use crate::core::config::Config;
use crate::core::supervisor::spawn_supervised;

pub mod hyprland;
pub mod network;
pub mod power;
pub mod udev;
pub mod network_rtnetlink;
pub mod power_upower;

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
        }
    }

    pub async fn start_all(&self) -> Result<()> {
        info!("starting adapters");

        if self.config.adapters.udev.enabled {
            let adapter = udev::UdevAdapter::new(self.config.adapters.udev.subsystems.clone());
            adapter.enumerate_existing(&self.raw_tx).await?;
            self.spawn_adapter(adapter);
        }

        if self.config.adapters.hyprland.enabled {
            self.spawn_adapter(hyprland::HyprlandAdapter::default());
        }

        if self.config.adapters.power.enabled {
            // Prefer UPower DBus adapter; fall back to sysfs poller
            let upower = power_upower::UPowerAdapter::new();
            if let Ok(adapter) = upower {
                self.spawn_adapter(adapter);
            } else {
                self.spawn_adapter(power::PowerAdapter::new(
                    self.config.adapters.power.poll_interval_secs,
                ));
            }
        }

        if self.config.adapters.network.enabled {
            // Prefer rtnetlink-based adapter; fall back to existing sysfs-based adapter
            let rt = network_rtnetlink::RtnetlinkAdapter::new();
            if let Ok(adapter) = rt {
                self.spawn_adapter(adapter);
            } else {
                self.spawn_adapter(network::NetworkAdapter::default());
            }
        }

        Ok(())
    }

    fn spawn_adapter<A>(&self, adapter: A)
    where
        A: Adapter + Clone + 'static,
    {
        let name = adapter.name();
        let tx = self.raw_tx.clone();
        let shutdown_rx = self.shutdown_rx.clone();
        let shutdown_for_task = shutdown_rx.clone();
        spawn_supervised(name, shutdown_rx, move || {
            let adapter = adapter.clone();
            let tx = tx.clone();
            let mut shutdown_rx = shutdown_for_task.clone();
            async move {
                adapter.on_connect().await?;
                let result = tokio::select! {
                    result = adapter.run(tx) => result,
                    _ = shutdown_rx.changed() => Ok(()),
                };
                adapter.on_disconnect().await?;
                result
            }
        });
    }
}
