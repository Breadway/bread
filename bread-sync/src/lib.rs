/// Bread sync: snapshot and restore system state via a Git remote.
pub mod config;
pub mod delegates;
pub mod export;
pub mod git;
pub mod machine;
pub mod packages;

pub use config::SyncConfig;
pub use export::{apply_import, stage_export, ExportManifest};
pub use git::SyncRepo;
