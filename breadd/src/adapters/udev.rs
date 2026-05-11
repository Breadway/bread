use std::collections::HashMap;
use std::fs;
use std::path::Path;

use anyhow::Result;
use bread_shared::{now_unix_ms, AdapterSource, RawEvent};
use serde_json::json;
use tokio::sync::mpsc;
use tokio::time::{sleep, Duration};
use tracing::debug;

use crate::adapters::Adapter;

#[derive(Clone)]
pub struct UdevAdapter {
    subsystems: Vec<String>,
}

impl UdevAdapter {
    pub fn new(subsystems: Vec<String>) -> Self {
        Self { subsystems }
    }

    pub async fn enumerate_existing(&self, tx: &mpsc::Sender<RawEvent>) -> Result<()> {
        let devices = enumerate_with_udev(&self.subsystems).unwrap_or_else(|_| {
            scan_devices(&self.subsystems).unwrap_or_default()
        });

        for device in devices {
            tx.send(RawEvent {
                source: AdapterSource::Udev,
                kind: "udev.enumerate".to_string(),
                payload: json!({
                    "action": "add",
                    "id": device.id,
                    "name": device.name,
                    "subsystem": device.subsystem,
                }),
                timestamp: now_unix_ms(),
            })
            .await?;
        }
        Ok(())
    }
}

#[async_trait::async_trait]
impl Adapter for UdevAdapter {
    fn name(&self) -> &'static str {
        "udev"
    }

    async fn run(&self, tx: mpsc::Sender<RawEvent>) -> Result<()> {
        debug!("udev adapter started");
        match run_udev_monitor(self.subsystems.clone(), tx.clone()).await {
            Ok(()) => return Ok(()),
            Err(err) => {
                tracing::warn!(error = %err, "udev netlink monitor unavailable, falling back to sysfs polling (add user to 'plugdev' group for real-time events)");
            }
        }

        // Fallback: poll sysfs every 2 seconds for environments where the
        // netlink socket is unavailable (missing plugdev membership, containers, etc).
        let mut known: HashMap<String, ScannedDevice> = scan_devices(&self.subsystems)
            .unwrap_or_default()
            .into_iter()
            .map(|d| (d.id.clone(), d))
            .collect();

        loop {
            let current = scan_devices(&self.subsystems).unwrap_or_default();
            let current_map: HashMap<String, ScannedDevice> = current
                .into_iter()
                .map(|d| (d.id.clone(), d))
                .collect();

            for (id, dev) in &current_map {
                if !known.contains_key(id) {
                    if tx.send(raw_change_event("add", dev)).await.is_err() {
                        return Ok(());
                    }
                }
            }

            for (id, dev) in &known {
                if !current_map.contains_key(id) {
                    if tx.send(raw_change_event("remove", dev)).await.is_err() {
                        return Ok(());
                    }
                }
            }

            known = current_map;
            sleep(Duration::from_secs(2)).await;
        }
    }
}

#[derive(Clone, Debug)]
struct ScannedDevice {
    id: String,
    name: String,
    subsystem: String,
}

async fn run_udev_monitor(subsystems: Vec<String>, tx: mpsc::Sender<RawEvent>) -> Result<()> {
    tokio::task::spawn_blocking(move || -> Result<()> {
        let mut builder = udev::MonitorBuilder::new()?;
        for subsystem in &subsystems {
            builder = builder.match_subsystem(subsystem)?;
        }
        let monitor = builder.listen()?;

        for event in monitor.iter() {
            let action = event
                .action()
                .map(|a| a.to_string_lossy().to_string())
                .unwrap_or_else(|| "change".to_string());
            let subsystem = event
                .subsystem()
                .map(|s| s.to_string_lossy().to_string())
                .unwrap_or_else(|| "unknown".to_string());
            let name = event
                .property_value("ID_MODEL")
                .or_else(|| event.property_value("NAME"))
                .map(|v| v.to_string_lossy().to_string())
                .or_else(|| event.devnode().map(|n| n.display().to_string()))
                .unwrap_or_else(|| "unknown".to_string());
            let id = event
                .syspath()
                .to_string_lossy()
                .to_string();

            let msg = RawEvent {
                source: AdapterSource::Udev,
                kind: "udev.change".to_string(),
                payload: json!({
                    "action": action,
                    "id": id,
                    "name": name,
                    "subsystem": subsystem,
                    "id_input_keyboard": prop_bool(&event, "ID_INPUT_KEYBOARD"),
                    "id_input_mouse": prop_bool(&event, "ID_INPUT_MOUSE"),
                    "id_input_joystick": prop_bool(&event, "ID_INPUT_JOYSTICK"),
                    "id_input_touchpad": prop_bool(&event, "ID_INPUT_TOUCHPAD"),
                    "id_input_tablet": prop_bool(&event, "ID_INPUT_TABLET"),
                    "id_usb_class": prop_str(&event, "ID_USB_CLASS"),
                    "id_usb_interfaces": prop_str(&event, "ID_USB_INTERFACES"),
                    "id_vendor": prop_str(&event, "ID_VENDOR"),
                    "id_model": prop_str(&event, "ID_MODEL"),
                }),
                timestamp: now_unix_ms(),
            };

            if tx.blocking_send(msg).is_err() {
                break;
            }
        }

        Ok(())
    })
    .await??;

    Ok(())
}

fn enumerate_with_udev(subsystems: &[String]) -> Result<Vec<ScannedDevice>> {
    let mut enumerator = udev::Enumerator::new()?;
    for subsystem in subsystems {
        enumerator.match_subsystem(subsystem)?;
    }

    let mut out = Vec::new();
    for dev in enumerator.scan_devices()? {
        let subsystem = dev
            .subsystem()
            .map(|s| s.to_string_lossy().to_string())
            .unwrap_or_else(|| "unknown".to_string());
        let name = dev
            .property_value("ID_MODEL")
            .or_else(|| dev.property_value("NAME"))
            .map(|v| v.to_string_lossy().to_string())
            .or_else(|| dev.sysname().to_str().map(ToString::to_string))
            .unwrap_or_else(|| "unknown".to_string());
        let id = dev.syspath().to_string_lossy().to_string();

        out.push(ScannedDevice {
            id,
            name,
            subsystem,
        });
    }

    Ok(out)
}

fn raw_change_event(action: &str, dev: &ScannedDevice) -> RawEvent {
    RawEvent {
        source: AdapterSource::Udev,
        kind: "udev.change".to_string(),
        payload: json!({
            "action": action,
            "id": dev.id,
            "name": dev.name,
            "subsystem": dev.subsystem,
        }),
        timestamp: now_unix_ms(),
    }
}

fn scan_devices(subsystems: &[String]) -> Result<Vec<ScannedDevice>> {
    let mut out = Vec::new();

    if subsystems.iter().any(|s| s == "drm") {
        let drm_dir = Path::new("/sys/class/drm");
        if drm_dir.exists() {
            for entry in fs::read_dir(drm_dir)? {
                let entry = entry?;
                let name = entry.file_name().to_string_lossy().to_string();
                if !name.contains('-') {
                    continue;
                }
                let status = fs::read_to_string(entry.path().join("status")).unwrap_or_default();
                if status.trim() == "connected" {
                    out.push(ScannedDevice {
                        id: format!("drm:{name}"),
                        name,
                        subsystem: "drm".to_string(),
                    });
                }
            }
        }
    }

    if subsystems.iter().any(|s| s == "input") {
        let input_dir = Path::new("/dev/input/by-id");
        if input_dir.exists() {
            for entry in fs::read_dir(input_dir)? {
                let entry = entry?;
                let name = entry.file_name().to_string_lossy().to_string();
                out.push(ScannedDevice {
                    id: format!("input:{name}"),
                    name,
                    subsystem: "input".to_string(),
                });
            }
        }
    }

    if subsystems.iter().any(|s| s == "power_supply") {
        let pwr_dir = Path::new("/sys/class/power_supply");
        if pwr_dir.exists() {
            for entry in fs::read_dir(pwr_dir)? {
                let entry = entry?;
                let name = entry.file_name().to_string_lossy().to_string();
                out.push(ScannedDevice {
                    id: format!("power_supply:{name}"),
                    name,
                    subsystem: "power_supply".to_string(),
                });
            }
        }
    }

    if subsystems.iter().any(|s| s == "usb") {
        let usb_dir = Path::new("/sys/bus/usb/devices");
        if usb_dir.exists() {
            for entry in fs::read_dir(usb_dir)? {
                let entry = entry?;
                let name = entry.file_name().to_string_lossy().to_string();
                if !name.contains(':') && name.chars().any(|c| c.is_ascii_digit()) {
                    out.push(ScannedDevice {
                        id: format!("usb:{name}"),
                        name,
                        subsystem: "usb".to_string(),
                    });
                }
            }
        }
    }

    Ok(out)
}

fn prop_bool(event: &udev::Event, key: &str) -> bool {
    event
        .property_value(key)
        .and_then(|v| v.to_str())
        .map(|v| v == "1")
        .unwrap_or(false)
}

fn prop_str(event: &udev::Event, key: &str) -> Option<String> {
    event
        .property_value(key)
        .map(|v| v.to_string_lossy().to_string())
}
