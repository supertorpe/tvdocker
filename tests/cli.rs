#![forbid(unsafe_code)]
//! CLI surface tests .
use tvdocker::cli::{Cli, help_text, parse, version_text};

#[test]
fn no_args_runs_without_filter() {
    assert_eq!(
        parse(Vec::<String>::new()).unwrap(),
        Cli::Run { filter: None }
    );
}

#[test]
fn positional_arg_becomes_the_filter() {
    assert_eq!(
        parse(["web"]).unwrap(),
        Cli::Run {
            filter: Some("web".into())
        }
    );
}

#[test]
fn help_and_version_flags_short_and_long() {
    for flag in ["--help", "-h"] {
        assert_eq!(parse([flag]).unwrap(), Cli::Help);
    }
    for flag in ["--version", "-V"] {
        assert_eq!(parse([flag]).unwrap(), Cli::Version);
    }
}

#[test]
fn unknown_option_errors_with_hint() {
    let err = parse(["--nope"]).unwrap_err();
    assert!(
        err.contains("--nope") && err.contains("--help"),
        "got: {err}"
    );
}

#[test]
fn extra_positional_errors() {
    assert!(
        parse(["one", "two"]).is_err(),
        "only one positional is allowed"
    );
}

#[test]
fn help_text_documents_usage_and_filter() {
    let text = help_text();
    assert!(text.contains("Usage: tvdocker") && text.contains("search_text"));
    assert!(version_text().contains(env!("CARGO_PKG_VERSION")));
}
