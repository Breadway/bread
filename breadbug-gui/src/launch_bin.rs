use breadbug_gui::launch::{self, Frontend};

const USAGE: &str = "usage: breadbug-launch [--app APP] [--tty]\n\
                     \n\
                     Picks the windowed breadbug reporter when a display is\n\
                     available and the terminal client otherwise. BOS keybinds\n\
                     and notification actions should call this instead of\n\
                     calling either front end directly.";

fn main() -> std::process::ExitCode {
    let args = match launch::parse(std::env::args().skip(1)) {
        Ok(args) => args,
        Err(err) => {
            eprintln!("{err}");
            return std::process::ExitCode::FAILURE;
        }
    };
    if args.help {
        println!("{USAGE}");
        return std::process::ExitCode::SUCCESS;
    }

    let frontend = launch::choose(args.tty, launch::display_available());
    let program = match frontend {
        Frontend::Gui => launch::sibling("breadbug-gui"),
        Frontend::Tty => launch::sibling("breadbug"),
    };
    let err = launch::exec(&program, &args.forwarded());
    eprintln!(
        "breadbug-launch: could not start {}: {err}",
        program.display()
    );
    std::process::ExitCode::FAILURE
}
