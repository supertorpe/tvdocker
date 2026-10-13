//! CLI surface : `tvdocker [--help|-h] [--version|-V] [<search_text>]`.
//! The positional arg pre-fills the Containers window filter.

/// Parsed command line.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Cli {
    /// Run the TUI, optionally pre-filling the container filter.
    Run { filter: Option<String> },
    /// Print the help text and exit.
    Help,
    /// Print the version and exit.
    Version,
}

/// Parse CLI arguments (program name already stripped).
pub fn parse<S: Into<String>>(args: impl IntoIterator<Item = S>) -> Result<Cli, String> {
    let args: Vec<String> = args.into_iter().map(Into::into).collect();
    let mut filter: Option<String> = None;
    for arg in &args {
        match arg.as_str() {
            "--help" | "-h" => return Ok(Cli::Help),
            "--version" | "-V" => return Ok(Cli::Version),
            s if s.starts_with('-') && s != "-" => {
                return Err(format!("unknown option `{s}` (try --help)"));
            }
            s => {
                if filter.is_some() {
                    return Err(format!("unexpected extra argument `{s}`"));
                }
                filter = Some(s.to_string());
            }
        }
    }
    Ok(Cli::Run { filter })
}

/// The `--help` text.
pub fn help_text() -> String {
    "tvdocker — multi-window Docker TUI\n\n\
     Usage: tvdocker [--help|-h] [--version|-V] [<search_text>]\n\n\
     Arguments:\n  <search_text>  pre-fill the Containers window filter\n"
        .to_string()
}

/// The `--version` line.
pub fn version_text() -> String {
    format!("tvdocker {}\n", env!("CARGO_PKG_VERSION"))
}
