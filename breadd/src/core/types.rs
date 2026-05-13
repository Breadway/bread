use std::collections::{BTreeMap, HashMap};

use serde::{Deserialize, Serialize};
use serde_json::Value;

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct RuntimeState {
    pub monitors: Vec<Monitor>,
    pub workspaces: Vec<Workspace>,
    pub active_workspace: Option<String>,
    pub active_window: Option<String>,
    pub devices: DeviceTopology,
    pub network: NetworkState,
    pub power: PowerState,
    pub profile: ProfileState,
    pub modules: Vec<ModuleStatus>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Monitor {
    pub name: String,
    pub connected: bool,
    pub resolution: Option<String>,
    pub position: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Workspace {
    pub id: String,
    pub monitor: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct DeviceTopology {
    pub connected: Vec<Device>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Device {
    pub id: String,
    pub name: String,
    pub device: String,
    pub subsystem: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub vendor_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub product_id: Option<String>,
}

/// One set of match conditions. All provided fields must match.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct MatchCondition {
    pub vendor_id: Option<String>,
    pub product_id: Option<String>,
    pub name: Option<String>,
    pub vendor: Option<String>,
    pub name_contains: Option<String>,
    pub id_input_keyboard: Option<bool>,
    pub id_input_mouse: Option<bool>,
    pub id_input_tablet: Option<bool>,
    /// True triggers the compound USB hub + secondary-interface check.
    pub usb_hub: Option<bool>,
    pub id_usb_class: Option<String>,
    pub subsystem: Option<String>,
}

/// A device rule from `devices.lua`. The device name is assigned if ANY
/// condition in `conditions` matches (OR semantics across conditions,
/// AND semantics within a condition).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DeviceRule {
    pub device: String,
    pub conditions: Vec<MatchCondition>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct NetworkState {
    pub interfaces: HashMap<String, InterfaceState>,
    pub online: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct InterfaceState {
    pub up: bool,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct PowerState {
    pub ac_connected: bool,
    pub battery_percent: Option<u8>,
    pub battery_low: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProfileState {
    pub active: String,
    pub history: Vec<String>,
    pub profiles: BTreeMap<String, String>,
}

impl Default for ProfileState {
    fn default() -> Self {
        Self {
            active: "default".to_string(),
            history: Vec::new(),
            profiles: BTreeMap::new(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ModuleStatus {
    pub name: String,
    pub status: ModuleLoadState,
    pub last_error: Option<String>,
    #[serde(default)]
    pub builtin: bool,
    #[serde(default)]
    pub store: HashMap<String, Value>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ModuleLoadState {
    Loaded,
    LoadError,
    NotFound,
    Degraded,
    Disabled,
}
