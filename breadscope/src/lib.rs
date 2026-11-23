//! breadscope: a GTK4 observability window for breadd.
//!
//! The daemon remains the source of truth. This crate is a read-mostly client:
//! it subscribes to events, polls the introspection methods, and only writes
//! through the documented `emit`, `trace.set`, and `modules.reload` methods.

pub mod ipc;
pub mod model;
pub mod style;
pub mod ui;

use gtk4::{Application, ApplicationWindow};

pub fn build_window(app: &Application, initial_view: Option<ui::View>) -> ApplicationWindow {
    ui::build_window(app, initial_view)
}
