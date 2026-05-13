use std::os::unix::io::AsRawFd;

use anyhow::Result;
use bread_shared::{now_unix_ms, AdapterSource, RawEvent};
use serde_json::json;
use tokio::sync::mpsc;
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
        let devices = enumerate_with_udev(&self.subsystems)?;
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
        run_udev_monitor(self.subsystems.clone(), tx).await
    }
}

struct ScannedDevice {
    id: String,
    name: String,
    subsystem: String,
}

// udev::MonitorSocket uses a non-blocking socket; calling iter().next() without
// first polling the fd returns None immediately and exits the loop — which is
// why the old code silently fell back to sysfs on every start.  We use poll(2)
// inside spawn_blocking so the thread truly blocks until events are available.
async fn run_udev_monitor(subsystems: Vec<String>, tx: mpsc::Sender<RawEvent>) -> Result<()> {
    tokio::task::spawn_blocking(move || -> Result<()> {
        let mut builder = udev::MonitorBuilder::new()?;
        for subsystem in &subsystems {
            builder = builder.match_subsystem(subsystem)?;
        }
        let socket = builder.listen()?;
        let fd = socket.as_raw_fd();

        loop {
            let mut pfd = libc::pollfd {
                fd,
                events: libc::POLLIN,
                revents: 0,
            };

            let ret = unsafe { libc::poll(&mut pfd, 1, 1000) };
            if ret < 0 {
                let err = std::io::Error::last_os_error();
                if err.kind() == std::io::ErrorKind::Interrupted {
                    continue;
                }
                return Err(err.into());
            }
            if ret == 0 {
                // Timeout: bail if the downstream channel has been dropped.
                if tx.is_closed() {
                    return Ok(());
                }
                continue;
            }
            if pfd.revents & libc::POLLIN != 0 {
                while let Some(event) = socket.iter().next() {
                    if tx.blocking_send(build_event(&event)).is_err() {
                        return Ok(());
                    }
                }
            }
        }
    })
    .await??;

    Ok(())
}

fn build_event(event: &udev::Event) -> RawEvent {
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
    let id = event.syspath().to_string_lossy().to_string();

    RawEvent {
        source: AdapterSource::Udev,
        kind: "udev.change".to_string(),
        payload: json!({
            "action": action,
            "id": id,
            "name": name,
            "subsystem": subsystem,
            "id_input_keyboard": prop_bool(event, "ID_INPUT_KEYBOARD"),
            "id_input_mouse": prop_bool(event, "ID_INPUT_MOUSE"),
            "id_input_joystick": prop_bool(event, "ID_INPUT_JOYSTICK"),
            "id_input_touchpad": prop_bool(event, "ID_INPUT_TOUCHPAD"),
            "id_input_tablet": prop_bool(event, "ID_INPUT_TABLET"),
            "id_usb_class": prop_str(event, "ID_USB_CLASS"),
            "id_usb_interfaces": prop_str(event, "ID_USB_INTERFACES"),
            "id_vendor": prop_str(event, "ID_VENDOR"),
            "id_model": prop_str(event, "ID_MODEL"),
            "vendor_id": prop_str(event, "ID_VENDOR_ID"),
            "product_id": prop_str(event, "ID_MODEL_ID"),
        }),
        timestamp: now_unix_ms(),
    }
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
