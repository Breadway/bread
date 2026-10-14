use std::io::{stdin, stdout, IsTerminal};

use anyhow::Result;
use breadbug::{run, Cli, Env};
use clap::Parser;

fn main() -> Result<()> {
    let cli = Cli::parse();
    let env = Env::from_environment();
    let interactive = stdin().is_terminal() && stdout().is_terminal();
    run(
        &cli,
        &env,
        &mut stdin().lock(),
        &mut stdout().lock(),
        interactive,
    )
}
