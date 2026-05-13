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

pub mod hyprland;
pub mod network;
pub mod network_rtnetlink;
pub mod power;
pub mod power_upower;
pub mod udev;

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum AdapterStatus {
    Connected,
    Disconnected,
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
                self.spawn_adapter(network::NetworkAdapter);
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
