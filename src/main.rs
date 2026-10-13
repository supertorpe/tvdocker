//! Binary entry point: parse the CLI , then run the TUI .
#![forbid(unsafe_code)]

use tvdocker::cli::{Cli, help_text, parse, version_text};

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let cli = parse(std::env::args().skip(1)).map_err(anyhow::Error::msg)?;
    match cli {
        Cli::Run { filter } => tvdocker::app::run(filter).await,
        Cli::Help => {
            print!("{}", help_text());
            Ok(())
        }
        Cli::Version => {
            print!("{}", version_text());
            Ok(())
        }
    }
}
