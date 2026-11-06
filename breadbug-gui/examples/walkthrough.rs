//! Drives the real window through every page the way a user would and writes
//! a PNG of each, so the layout can be checked without a desktop.
//!
//!   dev/walkthrough.sh /some/output/dir
//!
//! Needs a GDK display (dev/walkthrough.sh provides an isolated Broadway one).
//! Not run in CI: there is no display there. Every page is required: a page
//! that cannot be rendered fails the run instead of being skipped.

use std::cell::RefCell;
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::time::{Duration, Instant};

use breadbug::Env;
use gtk4::prelude::*;
use gtk4::{gdk, glib};

type Step = Result<(), String>;

fn pump(ms: u64) {
    let end = Instant::now() + Duration::from_millis(ms);
    let ctx = glib::MainContext::default();
    while Instant::now() < end {
        while ctx.iteration(false) {}
        std::thread::sleep(Duration::from_millis(10));
    }
}

/// Pump the loop until `cond` holds. Rendering is frame-driven, so fixed
/// sleeps are not enough on a slow headless display.
fn wait_until(what: &str, mut cond: impl FnMut() -> bool) -> Step {
    let deadline = Instant::now() + Duration::from_secs(10);
    let ctx = glib::MainContext::default();
    while !cond() {
        if Instant::now() >= deadline {
            return Err(format!("timed out waiting for {what}"));
        }
        while ctx.iteration(false) {}
        std::thread::sleep(Duration::from_millis(5));
    }
    Ok(())
}

fn shot(ui: &breadbug_gui::Ui, dir: &Path, name: &str) -> Step {
    wait_until("window mapped", || ui.window.is_mapped())?;
    wait_until("page mapped", || {
        ui.stack
            .visible_child()
            .is_some_and(|page| page.is_mapped() && page.width() > 0 && page.height() > 0)
    })?;

    // A freshly mapped page has no render node until GTK has drawn it. Ask
    // again each frame instead of writing a blank image.
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        let paintable = gtk4::WidgetPaintable::new(Some(&ui.window));
        let (w, h) = (ui.window.width().max(1), ui.window.height().max(1));
        let snapshot = gtk4::Snapshot::new();
        paintable.snapshot(&snapshot, f64::from(w), f64::from(h));
        if let Some(node) = snapshot.to_node() {
            if let Some(renderer) = ui.window.native().and_then(|n| n.renderer()) {
                let texture: gdk::Texture = renderer.render_texture(&node, None);
                let path = dir.join(format!("{name}.png"));
                texture
                    .save_to_png(&path)
                    .map_err(|e| format!("{name}: saving {}: {e}", path.display()))?;
                let bytes = std::fs::metadata(&path).map(|m| m.len()).unwrap_or(0);
                if bytes <= 1_000 {
                    return Err(format!("{name}: wrote only {bytes} bytes"));
                }
                println!("wrote {}", path.display());
                return Ok(());
            }
        }
        if Instant::now() >= deadline {
            return Err(format!("{name}: nothing to render"));
        }
        ui.window.queue_draw();
        pump(50);
    }
}

fn wait_for_page(ui: &breadbug_gui::Ui, name: &str) -> Step {
    wait_until(name, || {
        ui.stack.visible_child_name().as_deref() == Some(name)
    })
}

fn walk(ui: &breadbug_gui::Ui, dir: &Path) -> Step {
    if ui.stack.visible_child_name().as_deref() == Some("consent") {
        shot(ui, dir, "1-consent")?;
        ui.consent_continue.emit_clicked();
        pump(200);
    }

    shot(ui, dir, "2-pick")?;
    let first = ui
        .list
        .row_at_index(0)
        .ok_or_else(|| "no app rows to select".to_string())?;
    ui.list.select_row(Some(&first));
    shot(ui, dir, "2-selected")?;
    ui.next.emit_clicked();
    pump(200);

    ui.notes
        .buffer()
        .set_text("It crashed when I copied a long URL from mail@example.org");
    shot(ui, dir, "3-details")?;

    ui.preview.emit_clicked();
    wait_for_page(ui, "preview")?;
    shot(ui, dir, "4-preview")?;

    ui.window.set_default_size(620, 680);
    pump(300);
    shot(ui, dir, "4-compact")?;

    ui.window.set_default_size(820, 760);
    pump(300);
    if ui.send.is_sensitive() {
        ui.send.emit_clicked();
        wait_for_page(ui, "done")?;
        shot(ui, dir, "5-done")?;
    } else if std::env::var_os("BREADBUG_WALKTHROUGH_CHECK_ERRORS").is_some() {
        // A failed rebuild must not leave the last report available to save.
        // Only enabled by the isolated fixture script: a marker that cannot
        // fit under the report cap forces a real builder error.
        let env = Env::from_environment();
        let marker_path = env.store_root.join("apps/clip/markers.jsonl");
        let original = std::fs::read(&marker_path).map_err(|e| e.to_string())?;
        let oversized = format!(
            "{{\"t\":1,\"kind\":\"panic\",\"message\":\"{}\"}}\n",
            "sample crash message ".repeat(60_000)
        );
        std::fs::write(&marker_path, oversized).map_err(|e| e.to_string())?;
        ui.stack.set_visible_child_name("details");
        ui.preview.emit_clicked();
        wait_for_page(ui, "preview")?;
        std::fs::write(&marker_path, original).map_err(|e| e.to_string())?;
        if !ui.status_line.text().contains("Could not build the report") {
            return Err(format!(
                "expected a build error, got {:?}",
                ui.status_line.text()
            ));
        }
        if ui.save.is_sensitive() || ui.send.is_sensitive() {
            return Err("stale report still available after a build error".to_string());
        }
        shot(ui, dir, "6-build-error")?;
    }
    Ok(())
}

fn main() {
    let dir = PathBuf::from(std::env::args().nth(1).expect("usage: walkthrough OUT_DIR"));
    std::fs::create_dir_all(&dir).unwrap();
    let app = gtk4::Application::builder()
        .application_id("com.breadway.breadbug.walkthrough")
        .flags(gtk4::gio::ApplicationFlags::NON_UNIQUE)
        .build();
    let failure: Rc<RefCell<Option<String>>> = Rc::new(RefCell::new(None));
    let failure2 = failure.clone();
    app.connect_activate(move |app| {
        let env = Env::from_environment();
        let ui = breadbug_gui::build_window(app, env, None);
        pump(200);
        if let Err(err) = walk(&ui, &dir) {
            *failure2.borrow_mut() = Some(err);
        }
        // Closing the last window ends the application run loop. The error,
        // if any, is reported after GTK has returned: panicking here would
        // unwind across the C callback and abort with a bus error.
        ui.window.close();
    });
    app.run_with_args::<&str>(&[]);
    let err = failure.borrow().clone();
    if let Some(err) = err {
        eprintln!("walkthrough failed: {err}");
        std::process::exit(1);
    }
}
