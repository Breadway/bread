use anyhow::{anyhow, Result};
use async_trait::async_trait;
use bread_shared::{now_unix_ms, AdapterSource, RawEvent};
use futures_util::StreamExt;
use serde_json::json;
use std::collections::HashMap;
use tokio::sync::mpsc;
use tracing::{debug, info};
use zbus::zvariant::{OwnedObjectPath, OwnedValue};
use zbus::{Message, MessageStream};

use super::Adapter;

#[derive(Clone, Debug)]
pub struct BluetoothAdapter;

impl BluetoothAdapter {
    pub fn new() -> Self {
        Self
    }

    /// Emit `bluetooth.enumerate` events for every device that is currently connected.
    /// Errors are swallowed — Bluetooth hardware being absent is not a daemon startup failure.
    pub async fn enumerate_existing(&self, tx: &mpsc::Sender<RawEvent>) {
        match try_enumerate(tx).await {
            Ok(n) => debug!("bluetooth enumerated {n} connected device(s)"),
            Err(e) => debug!("bluetooth enumeration skipped: {e}"),
        }
    }
}

#[async_trait]
impl Adapter for BluetoothAdapter {
    fn name(&self) -> &'static str {
        "bluetooth"
    }

    async fn run(&self, tx: mpsc::Sender<RawEvent>) -> Result<()> {
        info!("bluetooth adapter starting");

        let conn = zbus::Connection::system()
            .await
            .map_err(|e| anyhow!("bluetooth D-Bus unavailable: {e}"))?;

        let mut stream = MessageStream::from(&conn);
        while let Some(result) = stream.next().await {
            match result {
                Ok(message) => {
                    if let Some(event) = parse_bluetooth_message(&message) {
                        if tx.send(event).await.is_err() {
                            return Ok(());
                        }
                    }
                }
                Err(e) => debug!("bluetooth stream error: {e}"),
            }
        }

        Ok(())
    }
}

async fn try_enumerate(tx: &mpsc::Sender<RawEvent>) -> Result<usize> {
    let conn = zbus::Connection::system().await?;
    let msg = conn
        .call_method(
            Some("org.bluez"),
            "/",
            Some("org.freedesktop.DBus.ObjectManager"),
            "GetManagedObjects",
            &(),
        )
        .await?;

    let objects: HashMap<OwnedObjectPath, HashMap<String, HashMap<String, OwnedValue>>> =
        msg.body()?;

    let mut count = 0;
    for (path, interfaces) in objects {
        let Some(props) = interfaces.get("org.bluez.Device1") else {
            continue;
        };
        let props_json = serde_json::to_value(props).unwrap_or_else(|_| json!({}));
        if !props_json
            .get("Connected")
            .and_then(|v| v.as_bool())
            .unwrap_or(false)
        {
            continue;
        }

        let name = props_json
            .get("Name")
            .or_else(|| props_json.get("Alias"))
            .and_then(|v| v.as_str())
            .unwrap_or("unknown")
            .to_string();
        let address = props_json
            .get("Address")
            .and_then(|v| v.as_str())
            .unwrap_or("unknown")
            .to_string();

        let _ = tx
            .send(RawEvent {
                source: AdapterSource::Bluetooth,
                kind: "bluetooth.enumerate".to_string(),
                payload: json!({
                    "path": path.as_str(),
                    "address": address,
                    "name": name,
                    "properties": props_json,
                }),
                timestamp: now_unix_ms(),
            })
            .await;
        count += 1;
    }

    Ok(count)
}

fn parse_bluetooth_message(message: &Message) -> Option<RawEvent> {
    let header = message.header().ok()?;
    let interface = header.interface().ok()??.as_str().to_string();
    let member = header.member().ok()??.as_str().to_string();
    let path = header
        .path()
        .ok()
        .flatten()
        .map(|p| p.as_str().to_string())
        .unwrap_or_default();

    // Connected / disconnected — PropertiesChanged on a BlueZ device object
    if interface == "org.freedesktop.DBus.Properties" && member == "PropertiesChanged" {
        if !path.starts_with("/org/bluez/") {
            return None;
        }
        let (iface, changed, _): (String, HashMap<String, OwnedValue>, Vec<String>) =
            message.body().ok()?;
        if iface != "org.bluez.Device1" {
            return None;
        }
        let changed_json = serde_json::to_value(&changed).ok()?;
        let connected = changed_json.get("Connected").and_then(|v| v.as_bool())?;
        let address = address_from_path(&path);
        let kind = if connected {
            "bluetooth.device.connected"
        } else {
            "bluetooth.device.disconnected"
        };
        return Some(RawEvent {
            source: AdapterSource::Bluetooth,
            kind: kind.to_string(),
            payload: json!({
                "path": path,
                "address": address,
                "properties": changed_json,
            }),
            timestamp: now_unix_ms(),
        });
    }

    // Device paired / discovered — InterfacesAdded from BlueZ ObjectManager
    if interface == "org.freedesktop.DBus.ObjectManager" && member == "InterfacesAdded" {
        let (obj_path, interfaces): (
            OwnedObjectPath,
            HashMap<String, HashMap<String, OwnedValue>>,
        ) = message.body().ok()?;
        let obj_str = obj_path.as_str();
        if !obj_str.starts_with("/org/bluez/") {
            return None;
        }
        let props = interfaces.get("org.bluez.Device1")?;
        let props_json = serde_json::to_value(props).ok()?;
        let name = props_json
            .get("Name")
            .or_else(|| props_json.get("Alias"))
            .and_then(|v| v.as_str())
            .unwrap_or("unknown")
            .to_string();
        let address = props_json
            .get("Address")
            .and_then(|v| v.as_str())
            .map(|s| s.to_string())
            .unwrap_or_else(|| address_from_path(obj_str));
        return Some(RawEvent {
            source: AdapterSource::Bluetooth,
            kind: "bluetooth.device.added".to_string(),
            payload: json!({
                "path": obj_str,
                "address": address,
                "name": name,
                "properties": props_json,
            }),
            timestamp: now_unix_ms(),
        });
    }

    // Device unpaired — InterfacesRemoved from BlueZ ObjectManager
    if interface == "org.freedesktop.DBus.ObjectManager" && member == "InterfacesRemoved" {
        let (obj_path, interfaces): (OwnedObjectPath, Vec<String>) = message.body().ok()?;
        let obj_str = obj_path.as_str();
        if !obj_str.starts_with("/org/bluez/") {
            return None;
        }
        if !interfaces.iter().any(|i| i == "org.bluez.Device1") {
            return None;
        }
        let address = address_from_path(obj_str);
        return Some(RawEvent {
            source: AdapterSource::Bluetooth,
            kind: "bluetooth.device.removed".to_string(),
            payload: json!({
                "path": obj_str,
                "address": address,
            }),
            timestamp: now_unix_ms(),
        });
    }

    None
}

/// `/org/bluez/hci0/dev_AA_BB_CC_DD_EE_FF` → `"AA:BB:CC:DD:EE:FF"`
fn address_from_path(path: &str) -> String {
    path.rsplit('/')
        .next()
        .and_then(|s| s.strip_prefix("dev_"))
        .map(|s| s.replace('_', ":"))
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn address_from_path_parses_standard_bluez_path() {
        assert_eq!(
            address_from_path("/org/bluez/hci0/dev_AA_BB_CC_DD_EE_FF"),
            "AA:BB:CC:DD:EE:FF"
        );
    }

    #[test]
    fn address_from_path_returns_empty_for_adapter_path() {
        assert_eq!(address_from_path("/org/bluez/hci0"), "");
    }

    #[test]
    fn address_from_path_returns_empty_for_root() {
        assert_eq!(address_from_path("/"), "");
    }
}
