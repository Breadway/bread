use anyhow::{anyhow, Result};
use async_trait::async_trait;
use bread_shared::{AdapterSource, RawEvent};
use futures_util::StreamExt;
use serde_json::json;
use std::collections::HashMap;
use tokio::sync::mpsc;
use tracing::{debug, info};
use zbus::{Message, MessageStream};
use zbus::zvariant::{OwnedObjectPath, OwnedValue};

use super::Adapter;

#[derive(Clone, Debug)]
pub struct UPowerAdapter;

impl UPowerAdapter {
    pub fn new() -> Result<Self> {
        // Attempt to connect to system bus to validate availability
        // We don't actually open the connection here because zbus::Connection::system() is async.
        Ok(Self)
    }
}

#[async_trait]
impl Adapter for UPowerAdapter {
    fn name(&self) -> &'static str {
        "upower"
    }

    async fn run(&self, tx: mpsc::Sender<RawEvent>) -> Result<()> {
        info!("UPower adapter starting (attempting DBus subscription)");

        // Defer loading zbus until runtime to avoid build-time optional complexity
        match zbus::Connection::system().await {
            Ok(conn) => {
                let payload = json!({"message": "upower:connected"});
                let _ = tx
                    .send(RawEvent {
                        source: AdapterSource::Power,
                        kind: "power.upower.connected".to_string(),
                        payload,
                        timestamp: bread_shared::now_unix_ms(),
                    })
                    .await;

                let mut stream = MessageStream::from(&conn);
                while let Some(result) = stream.next().await {
                    match result {
                        Ok(message) => match parse_upower_message(&message) {
                            Ok(event) => {
                                let _ = tx.send(event).await;
                            }
                            Err(err) => {
                                debug!("upower parse error: {err:?}");
                            }
                        },
                        Err(err) => {
                            debug!("upower stream error: {err:?}");
                        }
                    }
                }

                Ok(())
            }
            Err(e) => {
                // If DBus connection fails, fall back to periodic polling handled elsewhere
                Err(anyhow!(e))
            }
        }
    }
}

fn parse_upower_message(message: &Message) -> Result<RawEvent> {
    let header = message.header()?;
    let interface = header.interface()?.map(|v| v.as_str()).unwrap_or("");
    let member = header.member()?.map(|v| v.as_str()).unwrap_or("");
    let path = header.path()?.map(|v| v.as_str()).unwrap_or("");

    if interface == "org.freedesktop.UPower" {
        match member {
            "DeviceAdded" => {
                let (device_path,): (OwnedObjectPath,) = message.body()?;
                let payload = json!({"device_path": device_path.as_str()});
                return Ok(RawEvent {
                    source: AdapterSource::Power,
                    kind: "power.device.added".to_string(),
                    payload,
                    timestamp: bread_shared::now_unix_ms(),
                });
            }
            "DeviceRemoved" => {
                let (device_path,): (OwnedObjectPath,) = message.body()?;
                let payload = json!({"device_path": device_path.as_str()});
                return Ok(RawEvent {
                    source: AdapterSource::Power,
                    kind: "power.device.removed".to_string(),
                    payload,
                    timestamp: bread_shared::now_unix_ms(),
                });
            }
            _ => {}
        }
    }

    if interface == "org.freedesktop.DBus.Properties" && member == "PropertiesChanged" {
        let (iface, changed, invalidated): (String, HashMap<String, OwnedValue>, Vec<String>) =
            message.body()?;
        if iface == "org.freedesktop.UPower.Device" {
            let changed_json = serde_json::to_value(&changed).unwrap_or_else(|_| json!({}));
            let normalized = json!({
                "percentage": changed_json.get("Percentage").and_then(|v| v.as_f64()),
                "state": changed_json.get("State").and_then(|v| v.as_u64()),
                "time_to_empty": changed_json.get("TimeToEmpty").and_then(|v| v.as_i64()),
                "time_to_full": changed_json.get("TimeToFull").and_then(|v| v.as_i64()),
                "is_present": changed_json.get("IsPresent").and_then(|v| v.as_bool()),
                "battery_type": changed_json.get("Type").and_then(|v| v.as_u64()),
                "online": changed_json.get("Online").and_then(|v| v.as_bool()),
                "native_path": changed_json.get("NativePath").and_then(|v| v.as_str()),
                "model": changed_json.get("Model").and_then(|v| v.as_str()),
                "vendor": changed_json.get("Vendor").and_then(|v| v.as_str()),
                "serial": changed_json.get("Serial").and_then(|v| v.as_str()),
                "update_time": changed_json.get("UpdateTime").and_then(|v| v.as_u64()),
            });
            let payload = json!({
                "path": path,
                "properties": changed_json,
                "invalidated": invalidated,
                "normalized": normalized
            });

            return Ok(RawEvent {
                source: AdapterSource::Power,
                kind: "power.device.changed".to_string(),
                payload,
                timestamp: bread_shared::now_unix_ms(),
            });
        }
    }

    Ok(RawEvent {
        source: AdapterSource::Power,
        kind: "power.upower.signal".to_string(),
        payload: json!({"interface": interface, "member": member, "path": path}),
        timestamp: bread_shared::now_unix_ms(),
    })
}
