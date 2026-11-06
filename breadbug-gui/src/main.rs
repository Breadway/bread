use breadbug::Env;
use breadbug_gui::launch::{self, Frontend};
use gtk4::prelude::*;

const USAGE: &str = "usage: breadbug-gui [--app APP] [--tty]\n\
                     \n\
                     Opens the windowed reporter when a display is available.\n\
                     With --tty, or with neither WAYLAND_DISPLAY nor DISPLAY set,\n\
                     it hands off to the breadbug terminal client instead.";

fn main() -> gtk4::glib::ExitCode {
    let args = match launch::parse(std::env::args().skip(1)) {
        Ok(args) => args,
        Err(err) => {
            eprintln!("{err}");
            return gtk4::glib::ExitCode::FAILURE;
        }
    };
    if args.help {
        println!("{USAGE}");
        return gtk4::glib::ExitCode::SUCCESS;
    }

    if launch::choose(args.tty, launch::display_available()) == Frontend::Tty {
        // No display: the same invocation belongs to the terminal client.
        let cli = launch::sibling("breadbug");
        let err = launch::exec(&cli, &args.forwarded());
        eprintln!(
            "breadbug-gui: could not start {}: {err}\n{USAGE}",
            cli.display()
        );
        return gtk4::glib::ExitCode::FAILURE;
    }

    let preselect = args.app.clone();
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
