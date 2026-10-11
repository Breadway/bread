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

fn expand_report(widget: &impl IsA<gtk4::Widget>) -> bool {
    let widget = widget.as_ref();
    if let Some(expander) = widget.downcast_ref::<gtk4::Expander>() {
        if expander
            .label()
            .is_some_and(|label| label.starts_with("Full report"))
        {
            expander.set_expanded(true);
            return true;
        }
    }
    let mut child = widget.first_child();
    while let Some(widget) = child {
        if expand_report(&widget) {
            return true;
        }
        child = widget.next_sibling();
    }
    false
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
        let env = Env::from_environment();
        let marker_path = env.store_root.join("apps/clip/markers.jsonl");
        let ui = breadbug_gui::build_window(app, env, None);
        pump(500);
        if ui.stack.visible_child_name().as_deref() == Some("consent") {
            shot(&ui, &dir2, "1-consent");
            ui.consent_continue.emit_clicked();
            pump(200);
        }
        shot(&ui, &dir2, "2-pick");
        ui.list.select_row(ui.list.row_at_index(0).as_ref());
        shot(&ui, &dir2, "2-selected");
        ui.next.emit_clicked();
        pump(200);
        ui.notes
            .buffer()
            .set_text("It crashed when I copied a long URL from mail@example.org");
        shot(&ui, &dir2, "3-details");
        ui.preview.emit_clicked();
        // The report is built on a worker thread; wait for the page to change.
        for _ in 0..200 {
            if ui.stack.visible_child_name().as_deref() == Some("preview") {
                break;
            }
            pump(50);
        }
        assert_eq!(
            ui.stack.visible_child_name().as_deref(),
            Some("preview"),
            "preview did not finish building"
        );
        shot(&ui, &dir2, "4-preview");
        ui.window.set_default_size(620, 680);
        pump(300);
        shot(&ui, &dir2, "4-compact");
        ui.window.set_default_size(820, 760);
        assert!(expand_report(&ui.window), "full report disclosure missing");
        shot(&ui, &dir2, "4-full-report");
        if ui.send.is_sensitive() {
            ui.send.emit_clicked();
            for _ in 0..200 {
                if ui.stack.visible_child_name().as_deref() == Some("done") {
                    break;
                }
                pump(50);
            }
            shot(&ui, &dir2, "5-done");
        } else if std::env::var_os("BREADBUG_WALKTHROUGH_CHECK_ERRORS").is_some() {
            // A failed rebuild must not leave the last report available to save.
            // Only enabled by the isolated fixture script: a marker that cannot
            // fit under the report cap forces a real builder error.
            let original = std::fs::read(&marker_path).unwrap();
            let oversized = format!(
                "{{\"t\":1,\"kind\":\"panic\",\"message\":\"{}\"}}\n",
                "sample crash message ".repeat(60_000)
            );
            std::fs::write(&marker_path, oversized).unwrap();
            ui.stack.set_visible_child_name("details");
            ui.preview.emit_clicked();
            for _ in 0..200 {
                if ui.stack.visible_child_name().as_deref() == Some("preview") {
                    break;
                }
                pump(50);
            }
            std::fs::write(&marker_path, original).unwrap();
            assert!(ui.status_line.text().contains("Could not build the report"));
            assert!(
                !ui.save.is_sensitive(),
                "stale report still available to save"
            );
            assert!(
                !ui.send.is_sensitive(),
                "stale report still available to send"
            );
            shot(&ui, &dir2, "6-build-error");
        }
        *done2.borrow_mut() = true;
        ui.window.close();
    });
    app.run_with_args::<&str>(&[]);
    assert!(*done.borrow(), "walkthrough did not finish");
}
