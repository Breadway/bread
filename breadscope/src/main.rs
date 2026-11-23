use gtk4::prelude::*;

fn main() -> gtk4::glib::ExitCode {
    let mut initial_view = None;
    let mut initial_tab = None;
    let mut args = std::env::args().skip(1);
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--version" | "-V" => {
                println!("breadscope {}", env!("CARGO_PKG_VERSION"));
                return gtk4::glib::ExitCode::SUCCESS;
            }
            "--help" | "-h" => {
                println!("usage: breadscope [--view stream|flow|modules|state|adapters] [--tab runtime|workflows|widgets]");
                println!();
                println!("Opens the local breadd observability window. Ctrl+F filters the");
                println!("event stream, Ctrl+Shift+E opens the emit dialog.");
                return gtk4::glib::ExitCode::SUCCESS;
            }
            "--view" => {
                let Some(value) = args.next() else {
                    eprintln!("breadscope: --view needs a name");
                    return gtk4::glib::ExitCode::FAILURE;
                };
                match breadscope::ui::View::parse(&value) {
                    Some(view) => initial_view = Some(view),
                    None => {
                        eprintln!("breadscope: unknown view '{value}'");
                        return gtk4::glib::ExitCode::FAILURE;
                    }
                }
            }
            "--tab" => {
                let Some(value) = args.next() else {
                    eprintln!("breadscope: --tab needs runtime, workflows, or widgets");
                    return gtk4::glib::ExitCode::FAILURE;
                };
                if !matches!(value.as_str(), "runtime" | "workflows" | "widgets") {
                    eprintln!("breadscope: unknown tab '{value}'");
                    return gtk4::glib::ExitCode::FAILURE;
                }
                initial_tab = Some(value);
                if initial_view.is_none() {
                    initial_view = Some(breadscope::ui::View::State);
                }
            }
            other => {
                eprintln!("breadscope: unknown argument {other}");
                return gtk4::glib::ExitCode::FAILURE;
            }
        }
    }

    let app = gtk4::Application::builder()
        .application_id("com.breadway.breadscope")
        .build();
    app.connect_activate(move |app| {
        // The window keeps itself alive through the application.
        let _window = breadscope::build_window(app, initial_view, initial_tab.clone());
    });
    app.run_with_args::<&str>(&[])
}
