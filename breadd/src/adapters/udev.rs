use std::os::unix::io::AsRawFd;

use anyhow::Result;
use bread_shared::{now_unix_ms, AdapterSource, RawEvent};
use serde_json::{json, Value};
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
        let mut enumerator = udev::Enumerator::new()?;
        for subsystem in &self.subsystems {
            enumerator.match_subsystem(subsystem)?;
        }
        for device in enumerator.scan_devices()? {
            tx.send(build_device_event(&device, "add", "udev.enumerate"))
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

// udev::MonitorSocket uses a non-blocking socket; calling iter().next() without
// first polling the fd returns None immediately and exits the loop: which is
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
    build_device_event(event, &action, "udev.change")
}

/// Shared live/enumerate payload. `udev::Event` deref's to `Device`, so
/// boot-time enumerate of an already-plugged device is equivalent to an
/// `add` of that same device (same identity + classification fields
/// `resolve_device` needs).
fn build_device_event(device: &udev::Device, action: &str, kind: &str) -> RawEvent {
    let subsystem = device
        .subsystem()
        .map(|s| s.to_string_lossy().to_string())
        .unwrap_or_else(|| "unknown".to_string());
    let name = device
        .property_value("ID_MODEL")
        .or_else(|| device.property_value("NAME"))
        .map(|v| v.to_string_lossy().to_string())
        .or_else(|| device.devnode().map(|n| n.display().to_string()))
        .unwrap_or_else(|| "unknown".to_string());
    let id = device.syspath().to_string_lossy().to_string();

    RawEvent {
        source: AdapterSource::Udev,
        kind: kind.to_string(),
        payload: udev_event_payload(
            action,
            &id,
            &name,
            &subsystem,
            UdevClassification::from_device(device),
        ),
        timestamp: now_unix_ms(),
    }
}

/// Classification / identity fields copied onto every udev payload so
/// `resolve_device` can name a device after boot the same way it names a
/// live plug-in.
struct UdevClassification {
    id_input_keyboard: bool,
    id_input_mouse: bool,
    id_input_joystick: bool,
    id_input_touchpad: bool,
    id_input_tablet: bool,
    id_usb_class: Option<String>,
    id_usb_interfaces: Option<String>,
    id_vendor: Option<String>,
    id_model: Option<String>,
    vendor_id: Option<String>,
    product_id: Option<String>,
}

impl UdevClassification {
    fn from_device(device: &udev::Device) -> Self {
        Self {
            id_input_keyboard: prop_bool(device, "ID_INPUT_KEYBOARD"),
            id_input_mouse: prop_bool(device, "ID_INPUT_MOUSE"),
            id_input_joystick: prop_bool(device, "ID_INPUT_JOYSTICK"),
            id_input_touchpad: prop_bool(device, "ID_INPUT_TOUCHPAD"),
            id_input_tablet: prop_bool(device, "ID_INPUT_TABLET"),
            id_usb_class: prop_str(device, "ID_USB_CLASS"),
            id_usb_interfaces: prop_str(device, "ID_USB_INTERFACES"),
            id_vendor: prop_str(device, "ID_VENDOR"),
            id_model: prop_str(device, "ID_MODEL"),
            vendor_id: prop_str(device, "ID_VENDOR_ID"),
            product_id: prop_str(device, "ID_MODEL_ID"),
        }
    }
}

fn udev_event_payload(
    action: &str,
    id: &str,
    name: &str,
    subsystem: &str,
    class: UdevClassification,
) -> Value {
    json!({
        "action": action,
        "id": id,
        "name": name,
        "subsystem": subsystem,
        "id_input_keyboard": class.id_input_keyboard,
        "id_input_mouse": class.id_input_mouse,
        "id_input_joystick": class.id_input_joystick,
        "id_input_touchpad": class.id_input_touchpad,
        "id_input_tablet": class.id_input_tablet,
        "id_usb_class": class.id_usb_class,
        "id_usb_interfaces": class.id_usb_interfaces,
        "id_vendor": class.id_vendor,
        "id_model": class.id_model,
        "vendor_id": class.vendor_id,
        "product_id": class.product_id,
    })
}

fn prop_bool(device: &udev::Device, key: &str) -> bool {
    device
        .property_value(key)
        .and_then(|v| v.to_str())
        .map(|v| v == "1")
        .unwrap_or(false)
}

fn prop_str(device: &udev::Device, key: &str) -> Option<String> {
    device
        .property_value(key)
        .map(|v| v.to_string_lossy().to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn enumerate_payload_includes_classification_fields() {
        // Boot-time enumerate used to send only {action,id,name,subsystem},
        // so resolve_device could never match vendor/product/input rules
        // and bread.state.devices stayed "unknown" until the next unplug.
        let payload = udev_event_payload(
            "add",
            "/sys/devices/pci0000:00/usb1/1-3",
            "Keychron K2",
            "usb",
            UdevClassification {
                id_input_keyboard: true,
                id_input_mouse: false,
                id_input_joystick: false,
                id_input_touchpad: false,
                id_input_tablet: false,
                id_usb_class: None,
                id_usb_interfaces: None,
                id_vendor: Some("Keychron".into()),
                id_model: Some("Keychron K2".into()),
                vendor_id: Some("3434".into()),
                product_id: Some("d030".into()),
            },
        );
        assert_eq!(payload["action"], "add");
        assert_eq!(payload["id"], "/sys/devices/pci0000:00/usb1/1-3");
        assert_eq!(payload["name"], "Keychron K2");
        assert_eq!(payload["subsystem"], "usb");
        assert_eq!(payload["vendor_id"], "3434");
        assert_eq!(payload["product_id"], "d030");
        assert_eq!(payload["id_vendor"], "Keychron");
        assert_eq!(payload["id_model"], "Keychron K2");
        assert_eq!(payload["id_input_keyboard"], true);
        assert_eq!(payload["id_input_mouse"], false);
        assert_eq!(payload["id_input_joystick"], false);
        assert_eq!(payload["id_input_touchpad"], false);
        assert_eq!(payload["id_input_tablet"], false);
        assert!(payload["id_usb_class"].is_null());
        assert!(payload["id_usb_interfaces"].is_null());
    }
}
