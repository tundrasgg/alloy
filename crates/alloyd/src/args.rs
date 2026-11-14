//! The command line of `alloyd`.

use std::ffi::OsString;
use std::fmt;

/// The usage line of `alloyd`.
pub const USAGE: &str = "Usage: alloyd [version]";

/// What `alloyd --help` prints on standard output.
pub const HELP: &str = "\
Usage: alloyd [version]

Runs the Alloy node daemon until SIGTERM, SIGINT, or the Windows service
control manager stops it. The service manager starts it: run alloy init or
alloy join instead of starting it by hand.

Commands:
  version  Print the release of this alloyd and nothing else

Flags:
  -h, --help  Print this help. Default: off

Example:
  alloyd version
";

/// What the command line asks `alloyd` to do.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Command {
    /// No command: run the daemon.
    Run,
    /// `version`: print the release.
    Version,
    /// `-h` or `--help`: print [`HELP`].
    Help,
}

/// A command line that `alloyd` does not accept. Its `Display` output is
/// what `alloyd` prints on standard error: the message naming the one wrong
/// argument, the usage line, and a `Try:` line (ARFC 0019 clause 3.1.8).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct UsageError {
    message: String,
}

impl UsageError {
    fn new(message: String) -> UsageError {
        UsageError { message }
    }

    /// The message alone, naming the wrong argument.
    pub fn message(&self) -> &str {
        &self.message
    }
}

impl fmt::Display for UsageError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}\n{USAGE}\nTry: alloyd --help", self.message)
    }
}

impl std::error::Error for UsageError {}

/// Parses the arguments after the program name.
///
/// # Errors
///
/// [`UsageError`] for anything but no argument, `version`, `-h`, or
/// `--help`, naming the first argument that is wrong.
pub fn parse_args<I: IntoIterator<Item = OsString>>(args: I) -> Result<Command, UsageError> {
    let mut args = args.into_iter();
    let Some(first) = args.next() else {
        return Ok(Command::Run);
    };
    let command = match first.to_str() {
        Some("version") => Command::Version,
        Some("-h" | "--help") => Command::Help,
        Some(other) if other.starts_with('-') => {
            return Err(UsageError::new(format!(
                "`{}` is not an alloyd flag.",
                shown(other)
            )));
        }
        Some(other) => {
            return Err(UsageError::new(format!(
                "`{}` is not an alloyd command.",
                shown(other)
            )));
        }
        None => {
            return Err(UsageError::new(format!(
                "The argument {} is not valid UTF-8.",
                shown(&first.to_string_lossy())
            )));
        }
    };
    match args.next() {
        None => Ok(command),
        Some(extra) => Err(UsageError::new(format!(
            "`alloyd {}` takes no arguments, and `{}` was given.",
            shown(&first.to_string_lossy()),
            shown(&extra.to_string_lossy())
        ))),
    }
}

/// `arg` with every control character escaped as Rust escapes it (`\n`,
/// `\u{1b}`), so that no argument echoed to a terminal moves its cursor or
/// changes its state (T-0091 review F3).
fn shown(arg: &str) -> String {
    let mut out = String::with_capacity(arg.len());
    for c in arg.chars() {
        if c.is_control() {
            out.extend(c.escape_default());
        } else {
            out.push(c);
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(args: &[&str]) -> Result<Command, UsageError> {
        parse_args(args.iter().map(OsString::from))
    }

    #[test]
    fn accepts_run_version_and_help() {
        assert_eq!(parse(&[]), Ok(Command::Run));
        assert_eq!(parse(&["version"]), Ok(Command::Version));
        assert_eq!(parse(&["-h"]), Ok(Command::Help));
        assert_eq!(parse(&["--help"]), Ok(Command::Help));
    }

    #[test]
    fn names_the_wrong_argument() {
        let cases: [(&[&str], &str); 4] = [
            (&["start"], "`start` is not an alloyd command."),
            (&["--version"], "`--version` is not an alloyd flag."),
            (
                &["version", "now"],
                "`alloyd version` takes no arguments, and `now` was given.",
            ),
            (
                &["--help", "version"],
                "`alloyd --help` takes no arguments, and `version` was given.",
            ),
        ];
        for (args, message) in cases {
            assert_eq!(parse(args).unwrap_err().message(), message, "{args:?}");
        }
    }

    #[test]
    fn control_characters_in_arguments_are_escaped() {
        let message = parse(&["\u{1b}]0;x\u{7}"]).unwrap_err().to_string();
        assert!(
            !message.chars().any(|c| c.is_control() && c != '\n'),
            "{message:?}"
        );
        assert!(message.starts_with("`\\u{1b}]0;x\\u{7}` is not an alloyd command."));
        let message = parse(&["version", "a\nb"]).unwrap_err().to_string();
        assert!(message.starts_with("`alloyd version` takes no arguments, and `a\\nb` was given."));
        let message = parse(&["-\r"]).unwrap_err().to_string();
        assert!(message.starts_with("`-\\r` is not an alloyd flag."));
    }

    #[test]
    fn a_usage_error_prints_its_usage_line_and_try_line() {
        assert_eq!(
            parse(&["start"]).unwrap_err().to_string(),
            "`start` is not an alloyd command.\nUsage: alloyd [version]\nTry: alloyd --help"
        );
    }

    #[test]
    fn help_fits_80_columns_and_has_an_example() {
        assert!(HELP.lines().all(|line| line.len() <= 80));
        assert!(HELP.starts_with(USAGE));
        assert!(HELP.contains("Example:\n  alloyd version\n"));
    }

    #[cfg(unix)]
    #[test]
    fn a_non_utf8_argument_is_a_usage_error() {
        use std::os::unix::ffi::OsStringExt;
        let arg = OsString::from_vec(vec![0x66, 0xFF]);
        assert!(parse_args([arg]).is_err());
    }

    #[cfg(windows)]
    #[test]
    fn a_non_utf8_argument_is_a_usage_error() {
        use std::os::windows::ffi::OsStringExt;
        let arg = OsString::from_wide(&[0x66, 0xD800]);
        assert!(parse_args([arg]).is_err());
    }
}
