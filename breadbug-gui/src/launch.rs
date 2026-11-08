//! Front-end selection shared by `breadbug-gui` and `breadbug-launch`.
//!
//! BOS notifications and the keybind must not exec one front end blind. The
//! launcher picks the GUI when a display is available and the TTY client
//! otherwise; `breadbug-gui` makes the same check itself so launching it
//! directly from a text console still works.

use std::path::PathBuf;

/// Which front end to run.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Frontend {
    Gui,
    Tty,
}

/// Arguments accepted by both front ends.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Args {
    /// App id passed through as `--app`.
    pub app: Option<String>,
    /// Force the terminal client even when a display is available.
    pub tty: bool,
    /// Print usage instead of launching.
    pub help: bool,
}

impl Args {
    /// The arguments to hand to the chosen program, without launcher-only flags.
    pub fn forwarded(&self) -> Vec<String> {
        match &self.app {
            Some(app) => vec!["--app".to_string(), app.clone()],
            None => Vec::new(),
        }
    }
}

/// Whether the environment advertises a display GTK can open. An empty value
/// counts as absent (`WAYLAND_DISPLAY=""` is a common inherited placeholder).
pub fn display_available() -> bool {
    ["WAYLAND_DISPLAY", "DISPLAY"]
        .iter()
        .any(|key| std::env::var_os(key).is_some_and(|value| !value.is_empty()))
}

/// Pick the front end. An explicit terminal request always wins.
pub fn choose(tty: bool, display: bool) -> Frontend {
    if tty || !display {
        Frontend::Tty
    } else {
        Frontend::Gui
    }
}

/// Parse the shared argument set.
pub fn parse<I, S>(args: I) -> Result<Args, String>
where
    I: IntoIterator<Item = S>,
    S: Into<String>,
{
    let mut parsed = Args::default();
    let mut args = args.into_iter().map(Into::into);
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--app" => {
                let value = args
                    .next()
                    .filter(|value| !value.starts_with('-'))
                    .ok_or_else(|| "breadbug: --app needs an app id".to_string())?;
                parsed.app = Some(value);
            }
            "--tty" => parsed.tty = true,
            "--help" | "-h" => parsed.help = true,
            other => return Err(format!("breadbug: unknown argument {other}")),
        }
    }
    Ok(parsed)
}

/// A program next to this executable, so bakery's bin directory is used even
/// when `~/.local/bin` is not on PATH for a notification process.
pub fn sibling(name: &str) -> PathBuf {
    std::env::current_exe()
        .ok()
        .and_then(|exe| exe.parent().map(|dir| dir.join(name)))
        .filter(|path| path.is_file())
        .unwrap_or_else(|| PathBuf::from(name))
}

/// Replace this process with `program`. Never returns on success.
pub fn exec(program: &PathBuf, args: &[String]) -> std::io::Error {
    use std::os::unix::process::CommandExt;
    std::process::Command::new(program).args(args).exec()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_display_selects_the_gui_unless_the_terminal_is_forced() {
        assert_eq!(choose(false, true), Frontend::Gui);
        assert_eq!(choose(false, false), Frontend::Tty);
        assert_eq!(choose(true, true), Frontend::Tty);
        assert_eq!(choose(true, false), Frontend::Tty);
    }

    #[test]
    fn app_and_terminal_flags_are_parsed_and_forwarded() {
        let args = parse(["--app", "clip", "--tty"]).unwrap();
        assert_eq!(args.app.as_deref(), Some("clip"));
        assert!(args.tty);
        assert!(!args.help);
        assert_eq!(args.forwarded(), vec!["--app", "clip"]);
        assert!(parse(["--help"]).unwrap().help);
    }

    #[test]
    fn unknown_or_incomplete_arguments_are_refused() {
        assert!(parse(["--app"]).is_err());
        assert!(parse(["--nope"]).is_err());
    }
}
