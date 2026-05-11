use std::collections::{BTreeMap, HashMap};

use serde::{Deserialize, Serialize};
use serde_json::Value;

#[derive(Debug, Clone, Serialize, Deserialize)]
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

impl Default for RuntimeState {
    fn default() -> Self {
        Self {
            monitors: Vec::new(),
            workspaces: Vec::new(),
            active_workspace: None,
            active_window: None,
            devices: DeviceTopology::default(),
            network: NetworkState::default(),
            power: PowerState::default(),
            profile: ProfileState::default(),
            modules: Vec::new(),
        }
    }
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
    pub class: DeviceClass,
    pub subsystem: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, Eq, PartialEq)]
#[serde(rename_all = "snake_case")]
pub enum DeviceClass {
    Dock,
    Keyboard,
    Mouse,
    Tablet,
    Display,
    Storage,
    Audio,
    Unknown,
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

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PowerState {
    pub ac_connected: bool,
    pub battery_percent: Option<u8>,
    pub battery_low: bool,
}

impl Default for PowerState {
    fn default() -> Self {
        Self {
            ac_connected: false,
            battery_percent: None,
            battery_low: false,
        }
    }
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
