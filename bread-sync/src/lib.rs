/// Bread sync: snapshot and restore system state via a Git remote.
pub mod config;
pub mod delegates;
pub mod git;
pub mod machine;
pub mod packages;

pub use config::SyncConfig;
pub use git::SyncRepo;
