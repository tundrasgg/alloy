//! Tests of the command line as a whole, through [`run`]; in [`e2e`], of
//! `login` and `node list` against a control node that `alloyd` serves; and
//! in [`setup`], of `init` and `join`.

mod e2e;
mod setup;

use std::ffi::OsStr;

use super::*;

struct Ran {
    code: u8,
    out: String,
    err: String,
}

fn cli_in(args: &[&str], env: &Env) -> Ran {
    let mut out = Vec::new();
    let mut err = Vec::new();
    let code = run(args.iter().map(OsString::from), env, &mut out, &mut err);
    Ran {
        code,
        out: String::from_utf8(out).unwrap(),
        err: String::from_utf8(err).unwrap(),
    }
}

fn cli(args: &[&str]) -> Ran {
    cli_in(args, &Env::default())
}

/// An environment whose standard error is a color terminal.
fn terminal() -> Env {
    Env {
        stdout_terminal: true,
        stderr_terminal: true,
        vars: [("TERM".to_owned(), OsString::from("xterm-256color"))].into(),
        ..Env::default()
    }
}

#[test]
fn version_prints_the_version_and_nothing_else() {
    let ran = cli(&["version"]);
    assert_eq!(ran.code, EXIT_SUCCESS);
    assert_eq!(ran.out, format!("{}\n", env!("CARGO_PKG_VERSION")));
    assert_eq!(ran.err, "");
}

#[test]
fn an_unknown_command_is_a_usage_error_on_standard_error() {
    let ran = cli(&["verison"]);
    assert_eq!(ran.code, EXIT_USAGE);
    assert_eq!(ran.out, "");
    assert_eq!(
        ran.err,
        "`verison` is not an alloy command.\n\
         Usage: alloy <command> [flags]\n\
         Try: alloy version\n"
    );
    for later in ["logout", "doctor"] {
        let ran = cli(&[later]);
        assert_eq!(ran.code, EXIT_USAGE, "{later}");
        assert!(ran.err.starts_with(&format!("`{later}` is not")), "{later}");
    }
    let ran = cli(&["node"]);
    assert_eq!(ran.code, EXIT_USAGE);
    assert!(
        ran.err.starts_with("`alloy node` needs one of: list.\n"),
        "{}",
        ran.err
    );
}

/// T-0091 review F2 and F3 through the binary's entry point.
#[test]
fn try_lines_run_as_printed_and_arguments_are_escaped() {
    let ran = cli(&["verison", "now"]);
    assert!(ran.err.ends_with("Try: alloy version\n"), "{}", ran.err);
    let ran = cli(&["verison", "a b"]);
    assert!(ran.err.ends_with("Try: alloy version\n"), "{}", ran.err);
    let ran = cli(&["\u{1b}]0;pwned\u{7}"]);
    assert!(!ran.err.contains('\u{1b}'), "{}", ran.err);
    assert!(!ran.err.contains('\u{7}'), "{}", ran.err);
    assert!(
        ran.err.starts_with("`\\u{1b}]0;pwned\\u{7}` is not"),
        "{}",
        ran.err
    );
}

#[test]
fn the_try_command_is_bold_only_when_styled() {
    let ran = cli_in(&["verison"], &terminal());
    assert!(
        ran.err.ends_with("Try: \x1b[1malloy version\x1b[0m\n"),
        "{}",
        ran.err
    );
    let ran = cli_in(&["verison", "--color", "never"], &terminal());
    assert!(ran.err.ends_with("Try: alloy version\n"), "{}", ran.err);
    let mut no_color = terminal();
    no_color
        .vars
        .insert("NO_COLOR".to_owned(), OsString::from("1"));
    let ran = cli_in(&["verison"], &no_color);
    assert!(ran.err.ends_with("Try: alloy version\n"), "{}", ran.err);
}

#[test]
fn color_takes_auto_always_or_never() {
    for when in ["auto", "always", "never"] {
        assert_eq!(cli(&["version", "--color", when]).code, EXIT_SUCCESS);
    }
    let ran = cli(&["version", "--color", "sometimes"]);
    assert_eq!(ran.code, EXIT_USAGE);
    assert!(
        ran.err.starts_with("`--color` takes auto, always, or never, and `sometimes` was given.\nUsage: alloy version\n"),
        "{}",
        ran.err
    );
    let env = terminal();
    assert!(env.style(false, ColorMode::Always).on);
    assert!(!env.style(true, ColorMode::Never).on);
    let windows = Env {
        windows: true,
        ..terminal()
    };
    assert!(!windows.style(true, ColorMode::Always).on);
}

#[test]
fn help_goes_to_standard_output() {
    let ran = cli(&["--help"]);
    assert_eq!(ran.code, EXIT_SUCCESS);
    assert!(ran.out.starts_with("Usage: alloy <command> [flags]\n"));
    assert_eq!(ran.err, "");
    let ran = cli(&["version", "-h"]);
    assert_eq!(ran.code, EXIT_SUCCESS);
    assert!(ran.out.starts_with("Usage: alloy version\n"));
    let ran = cli(&["node", "list", "--help"]);
    assert!(ran.out.contains("--output <format>"), "{}", ran.out);
}

/// Usage errors that only the command's own checks find come before any
/// file is read, so they need no configuration directory and no network.
#[test]
fn command_usage_errors_need_nothing_else() {
    let cases: [(&[&str], &str); 9] = [
        (&["login"], "`alloy login` needs a login code"),
        (
            &["login", "alyc_x"],
            "This is not a valid login code: the login code is not valid base32.",
        ),
        (
            &["login", "nope"],
            "This is not a valid login code: a login code starts with `alyc_`.",
        ),
        (
            &["login", "a", "b"],
            "`alloy login` takes one argument, <login code>, and 2 were given.",
        ),
        (
            &["login", "--issue-code", "alyc_x"],
            "`alloy login --issue-code` takes no login code",
        ),
        (
            &["login", "--issue-code", "--context", "x"],
            "`--context` is not taken with `--issue-code`",
        ),
        (
            &["login", "--user", "bob"],
            "`--user` is taken only with `--issue-code`",
        ),
        (
            &["node", "list", "--output", "xml"],
            "`--output` takes table, json, or yaml, and `xml` was given.",
        ),
        (
            &["login", "--issue-code", "--output", "json"],
            "`--output` is not a flag of `alloy login`.",
        ),
    ];
    for (args, start) in cases {
        let ran = cli(args);
        assert_eq!(ran.code, EXIT_USAGE, "{args:?}: {}", ran.err);
        assert_eq!(ran.out, "", "{args:?}");
        assert!(ran.err.starts_with(start), "{args:?}: {}", ran.err);
        assert!(ran.err.contains("\nTry: alloy "), "{args:?}: {}", ran.err);
    }
    // A login code is never echoed (ARFC 0019 clause 3.1.13).
    let ran = cli(&["login", "alyc_secretsecret", "--context", "a b"]);
    assert_eq!(ran.code, EXIT_USAGE);
    assert!(!ran.err.contains("secretsecret"), "{}", ran.err);
    let ran = cli(&["login", "alyc_secretsecret", "alyc_other"]);
    assert!(!ran.err.contains("secretsecret"), "{}", ran.err);
}

#[test]
fn a_command_with_no_configuration_directory_fails_and_says_why() {
    let valid = {
        use alloy_id::login::{CodeId, CodeSecret};
        alloy_id::LoginCode {
            cluster_id: alloy_id::ClusterId::from_bytes([1; 16]),
            api_ca_fp: [2; 16],
            code_id: CodeId([3; 8]),
            secret: CodeSecret::from_bytes([4; 16]),
            endpoints: vec!["127.0.0.1:443".parse().unwrap()],
        }
        .encode()
        .unwrap()
    };
    let ran = cli(&["login", &valid]);
    assert_eq!(ran.code, EXIT_FAILURE);
    assert!(
        ran.err
            .starts_with("alloy cannot find its configuration directory"),
        "{}",
        ran.err
    );
    assert!(!ran.err.contains(&valid), "{}", ran.err);
}

#[test]
fn a_failed_write_is_a_failure() {
    struct Closed;
    impl Write for Closed {
        fn write(&mut self, _: &[u8]) -> std::io::Result<usize> {
            Err(std::io::ErrorKind::BrokenPipe.into())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }
    let mut err = Vec::new();
    let args = [OsString::from("version")];
    assert_eq!(
        run(args, &Env::default(), &mut Closed, &mut err),
        EXIT_FAILURE
    );
    assert!(err.is_empty());
}

#[test]
fn style_follows_principle_t12() {
    let none = None::<&OsStr>;
    let set = Some(OsStr::new("1"));
    let empty = Some(OsStr::new(""));
    let dumb = Some(OsStr::new("dumb"));
    let xterm = Some(OsStr::new("xterm-256color"));
    assert!(Style::of_stream(true, none, xterm, false).on);
    assert!(Style::of_stream(true, empty, none, false).on);
    assert!(!Style::of_stream(false, none, xterm, false).on);
    assert!(!Style::of_stream(true, set, xterm, false).on);
    assert!(!Style::of_stream(true, none, dumb, false).on);
    assert!(!Style::of_stream(true, none, xterm, true).on);
    let on = Style { on: true };
    assert_eq!(on.bold("x"), "\x1b[1mx\x1b[0m");
    assert_eq!(on.red("x"), "\x1b[31mx\x1b[0m");
    assert_eq!(on.yellow("x"), "\x1b[33mx\x1b[0m");
    assert_eq!(on.green("x"), "\x1b[32mx\x1b[0m");
    assert_eq!(Style::default().red("x"), "x");
}

/// The command-line checks of `init` and `join` end with exit 2 before
/// anything is read, and never echo a join token (ARFC 0019 clause 3.1.13).
#[test]
fn init_and_join_usage_errors_need_nothing_else() {
    let cases: [(&[&str], &str); 7] = [
        (
            &["init", "--mesh-port", "0"],
            "`--mesh-port` takes a port from 1 to 65535, and `0` was given.",
        ),
        (
            &["init", "--advertise", "10.0.0.1,nope"],
            "`--advertise` takes IP addresses separated by commas, and `nope` is not one.",
        ),
        (&["init", "--name", "Bad Name"], "The node name `Bad Name` "),
        (&["init", "extra"], "`alloy init` takes no arguments"),
        (&["join"], "`alloy join` needs a join token"),
        (
            &["join", "nope"],
            "This is not a valid join token: a join token starts with `alyt_`.",
        ),
        (
            &["join", "alyt_secretsecret"],
            "This is not a valid join token: ",
        ),
    ];
    for (args, start) in cases {
        let ran = cli(args);
        assert_eq!(ran.code, EXIT_USAGE, "{args:?}: {}", ran.err);
        assert_eq!(ran.out, "", "{args:?}");
        assert!(ran.err.starts_with(start), "{args:?}: {}", ran.err);
        assert!(ran.err.contains("\nTry: alloy "), "{args:?}: {}", ran.err);
        assert!(!ran.err.contains("secretsecret"), "{}", ran.err);
    }
    let ran = cli(&["init", "--help"]);
    assert_eq!(ran.code, EXIT_SUCCESS);
    for flag in [
        "--advertise <addr>[,<addr>...]",
        "--name <name>",
        "--mesh-port <port>",
        "--api-port <port>",
    ] {
        assert!(ran.out.contains(flag), "{flag}: {}", ran.out);
    }
    let ran = cli(&["join", "--help"]);
    assert!(ran.out.contains("--roles <list>"), "{}", ran.out);
    assert!(
        ran.out.contains("Usage: alloy join <join token> [flags]"),
        "{}",
        ran.out
    );
}
