//! Drives the real window through every page the way a user would and writes
//! a PNG of each, so the layout can be checked without a desktop.
//!
//!   dev/walkthrough.sh /some/output/dir
//!
//! Needs a GDK display (dev/walkthrough.sh provides an isolated Broadway one).
//! Not run in CI: there is no display there.

use std::cell::RefCell;
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::time::{Duration, Instant};

use breadbug::Env;
use gtk4::prelude::*;
use gtk4::{gdk, glib};

fn pump(ms: u64) {
    let end = Instant::now() + Duration::from_millis(ms);
    let ctx = glib::MainContext::default();
    while Instant::now() < end {
        while ctx.iteration(false) {}
        std::thread::sleep(Duration::from_millis(10));
    }
}

fn shot(ui: &breadbug_gui::Ui, dir: &Path, name: &str) {
    pump(300);
    let paintable = gtk4::WidgetPaintable::new(Some(&ui.window));
    let (w, h) = (ui.window.width().max(1), ui.window.height().max(1));
    let snapshot = gtk4::Snapshot::new();
    paintable.snapshot(&snapshot, f64::from(w), f64::from(h));
    let Some(node) = snapshot.to_node() else {
        eprintln!("{name}: nothing to render");
        return;
    };
    let Some(renderer) = ui.window.native().and_then(|n| n.renderer()) else {
        eprintln!("{name}: no renderer");
        return;
    };
    let texture: gdk::Texture = renderer.render_texture(&node, None);
    let path = dir.join(format!("{name}.png"));
    texture.save_to_png(&path).expect("save png");
    println!("wrote {}", path.display());
}

fn main() {
    let dir = PathBuf::from(std::env::args().nth(1).expect("usage: walkthrough OUT_DIR"));
    std::fs::create_dir_all(&dir).unwrap();
    let app = gtk4::Application::builder()
        .application_id("com.breadway.breadbug.walkthrough")
        .flags(gtk4::gio::ApplicationFlags::NON_UNIQUE)
        .build();
    let dir2 = dir.clone();
    let done = Rc::new(RefCell::new(false));
    let done2 = done.clone();
    app.connect_activate(move |app| {
        let ui = breadbug_gui::build_window(app, Env::from_environment(), None);
        pump(500);
        if ui.stack.visible_child_name().as_deref() == Some("consent") {
            shot(&ui, &dir2, "1-consent");
            ui.consent_continue.emit_clicked();
            pump(200);
        }
        shot(&ui, &dir2, "2-pick");
        ui.list.select_row(ui.list.row_at_index(0).as_ref());
        ui.next.emit_clicked();
        pump(200);
        ui.notes.buffer().set_text("It crashed when I copied a long URL from mail@example.org");
        shot(&ui, &dir2, "3-details");
        ui.preview.emit_clicked();
        // The report is built on a worker thread; wait for the page to change.
        for _ in 0..200 {
            if ui.stack.visible_child_name().as_deref() == Some("preview") {
                break;
            }
            pump(50);
        }
        shot(&ui, &dir2, "4-preview");
        if ui.send.is_sensitive() {
            ui.send.emit_clicked();
            for _ in 0..200 {
                if ui.stack.visible_child_name().as_deref() == Some("done") {
                    break;
                }
                pump(50);
            }
            shot(&ui, &dir2, "5-done");
        }
        *done2.borrow_mut() = true;
        ui.window.close();
    });
    app.run_with_args::<&str>(&[]);
    assert!(*done.borrow(), "walkthrough did not finish");
}
