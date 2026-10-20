use breadbug::Env;
use gtk4::prelude::*;

fn main() -> gtk4::glib::ExitCode {
    // `breadbug-gui [--app ID]`: preselect an app (the crash notification
    // passes the app that crashed).
    let mut args = std::env::args().skip(1);
    let mut preselect = None;
    while let Some(a) = args.next() {
        match a.as_str() {
            "--app" => preselect = args.next(),
            "--help" | "-h" => {
                println!("usage: breadbug-gui [--app APP]");
                return gtk4::glib::ExitCode::SUCCESS;
            }
            other => {
                eprintln!("breadbug-gui: unknown argument {other}");
                return gtk4::glib::ExitCode::FAILURE;
            }
        }
    }

    let app = gtk4::Application::builder()
        .application_id("com.breadway.breadbug")
        .build();
    app.connect_activate(move |app| {
        // The window keeps itself alive through the application.
        let _ui = breadbug_gui::build_window(app, Env::from_environment(), preselect.clone());
    });
    // GTK would try to parse our own flags; we already did.
    app.run_with_args::<&str>(&[])
}
