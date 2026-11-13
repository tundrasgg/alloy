//! The Alloy command line interface, `alloy` (ARFC 0019 section 3.1).
//!
//! Commands are `alloy <noun> <verb> [name] [flags]` and the top-level
//! commands of ARFC 0019 clause 3.1.2. A command exists once its task
//! registers it in [`COMMANDS`]; until then it is a usage error, like any
//! unknown command. This release registers `version`.
//!
//! [`run`] is the whole command line: the binary passes it the process's
//! arguments, its standard output and error, and the [`Style`] of standard
//! error, and returns the exit code it gives back.

#![forbid(unsafe_code)]

mod grammar;
mod text;

use std::ffi::{OsStr, OsString};
use std::io::Write;

pub use grammar::{COMMANDS, CommandId, CommandSpec, Parsed, UsageError, parse};

/// The release this `alloy` is: the package version at build time, which the
/// release workflow sets from the release's tag (ARFC 0017 clause 3.13).
pub const VERSION: &str = env!("CARGO_PKG_VERSION");

/// Exit code 0: success (ARFC 0019 clause 3.1.7).
pub const EXIT_SUCCESS: u8 = 0;
/// Exit code 1: failure (ARFC 0019 clause 3.1.7).
pub const EXIT_FAILURE: u8 = 1;
/// Exit code 2: usage error (ARFC 0019 clause 3.1.7).
pub const EXIT_USAGE: u8 = 2;

/// How text on standard error may be styled.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Style {
    /// Whether the command on a `Try:` line is bold (principles T9 and T12).
    pub bold: bool,
}

impl Style {
    /// The style of a stream by principle T12: bold only on a terminal, with
    /// `NO_COLOR` unset or empty and `TERM` other than `dumb`. Never on
    /// Windows, where this release does not check whether the console
    /// accepts ANSI sequences.
    pub fn of_stream(
        is_terminal: bool,
        no_color: Option<&OsStr>,
        term: Option<&OsStr>,
        windows: bool,
    ) -> Style {
        let bold = is_terminal
            && !windows
            && no_color.is_none_or(OsStr::is_empty)
            && term.is_none_or(|term| term != "dumb");
        Style { bold }
    }
}

/// Runs the command line `args`, the arguments after the program name, and
/// returns the exit code.
///
/// The result goes to `out` and nothing else does (principle T1). A usage
/// error goes to `err` as its message, the usage line, and a `Try:` line, and
/// returns [`EXIT_USAGE`]. A failed write to `out` returns [`EXIT_FAILURE`].
pub fn run<I>(args: I, out: &mut dyn Write, err: &mut dyn Write, style: Style) -> u8
where
    I: IntoIterator<Item = OsString>,
{
    let parsed = grammar::utf8_args(args).and_then(|args| parse(COMMANDS, &args));
    let result = match parsed {
        Ok(Parsed::Help(spec)) => grammar::help(COMMANDS, spec),
        Ok(Parsed::Run(spec)) => match spec.id {
            CommandId::Version => format!("{VERSION}\n"),
        },
        Err(e) => {
            // Nothing else can report a failed write to standard error.
            let _ = err.write_all(e.render(style).as_bytes());
            return EXIT_USAGE;
        }
    };
    match out.write_all(result.as_bytes()).and_then(|()| out.flush()) {
        Ok(()) => EXIT_SUCCESS,
        Err(_) => EXIT_FAILURE,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Ran {
        code: u8,
        out: String,
        err: String,
    }

    fn cli(args: &[&str], style: Style) -> Ran {
        let mut out = Vec::new();
        let mut err = Vec::new();
        let code = run(args.iter().map(OsString::from), &mut out, &mut err, style);
        Ran {
            code,
            out: String::from_utf8(out).unwrap(),
            err: String::from_utf8(err).unwrap(),
        }
    }

    #[test]
    fn version_prints_the_version_and_nothing_else() {
        let ran = cli(&["version"], Style::default());
        assert_eq!(ran.code, EXIT_SUCCESS);
        assert_eq!(ran.out, format!("{}\n", env!("CARGO_PKG_VERSION")));
        assert_eq!(ran.err, "");
    }

    #[test]
    fn an_unknown_command_is_a_usage_error_on_standard_error() {
        let ran = cli(&["verison"], Style::default());
        assert_eq!(ran.code, EXIT_USAGE);
        assert_eq!(ran.out, "");
        assert_eq!(
            ran.err,
            "`verison` is not an alloy command.\n\
             Usage: alloy <command> [flags]\n\
             Try: alloy version\n"
        );
        for later in ["init", "join", "login", "node", "doctor"] {
            let ran = cli(&[later], Style::default());
            assert_eq!(ran.code, EXIT_USAGE, "{later}");
            assert!(ran.err.starts_with(&format!("`{later}` is not")), "{later}");
        }
    }

    #[test]
    fn the_try_command_is_bold_only_when_styled() {
        let ran = cli(&["verison"], Style { bold: true });
        assert!(ran.err.ends_with("Try: \x1b[1malloy version\x1b[0m\n"));
    }

    #[test]
    fn help_goes_to_standard_output() {
        let ran = cli(&["--help"], Style::default());
        assert_eq!(ran.code, EXIT_SUCCESS);
        assert!(ran.out.starts_with("Usage: alloy <command> [flags]\n"));
        assert_eq!(ran.err, "");
        let ran = cli(&["version", "-h"], Style::default());
        assert_eq!(ran.code, EXIT_SUCCESS);
        assert!(ran.out.starts_with("Usage: alloy version\n"));
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
            run(args, &mut Closed, &mut err, Style::default()),
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
        assert!(Style::of_stream(true, none, xterm, false).bold);
        assert!(Style::of_stream(true, empty, none, false).bold);
        assert!(!Style::of_stream(false, none, xterm, false).bold);
        assert!(!Style::of_stream(true, set, xterm, false).bold);
        assert!(!Style::of_stream(true, none, dumb, false).bold);
        assert!(!Style::of_stream(true, none, xterm, true).bold);
    }
}
