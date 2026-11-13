//! The command grammar of ARFC 0019 clause 3.1.1: the registered commands,
//! parsing, help, and usage errors.

use std::ffi::OsString;
use std::fmt::Write as _;

use crate::Style;
use crate::text::{nearest, wrap};

/// The width that prose wraps at (principle T3).
const WIDTH: usize = 80;

/// The usage line of `alloy` itself.
const TOP_USAGE: &str = "Usage: alloy <command> [flags]";

/// The flag that prints help, with its short form, on every command.
const HELP_FLAGS: [&str; 2] = ["-h", "--help"];

/// What a registered command does when run.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CommandId {
    /// `alloy version`: print the release (ARFC 0017 clause 3.13).
    Version,
}

/// One registered command.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct CommandSpec {
    /// What it does.
    pub id: CommandId,
    /// Its words: one for a top-level command (`["version"]`), a noun and a
    /// verb otherwise (`["node", "list"]`).
    pub path: &'static [&'static str],
    /// One line for the command list of `alloy --help`.
    pub summary: &'static str,
    /// What `alloy <command> --help` says it does: one paragraph.
    pub description: &'static str,
    /// Command lines that run as printed (principles T5 and T8).
    pub examples: &'static [&'static str],
}

impl CommandSpec {
    /// The command's words joined by spaces.
    fn name(&self) -> String {
        self.path.join(" ")
    }

    /// The command's usage line.
    fn usage(&self) -> String {
        format!("Usage: alloy {}", self.name())
    }
}

/// The commands of this release.
pub const COMMANDS: &[CommandSpec] = &[CommandSpec {
    id: CommandId::Version,
    path: &["version"],
    summary: "Print the release of this alloy",
    description: "Prints the release of this alloy, and nothing else, on standard output.",
    examples: &["alloy version"],
}];

/// What a command line asks for.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Parsed<'a> {
    /// Run this command.
    Run(&'a CommandSpec),
    /// Print the help of this command, or of `alloy` itself when `None`.
    Help(Option<&'a CommandSpec>),
}

/// A command line that `alloy` does not accept (exit code 2, ARFC 0019
/// clause 3.1.7).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct UsageError {
    /// What is wrong, naming the one wrong argument or flag (principle T29).
    pub message: String,
    /// The usage line of the command, or of `alloy`.
    pub usage: String,
    /// The command for the `Try:` line, which runs as printed.
    pub try_command: String,
}

impl UsageError {
    fn new(message: String, usage: String, try_command: String) -> UsageError {
        UsageError {
            message,
            usage,
            try_command,
        }
    }

    /// The text for standard error: the message wrapped at 80 columns, the
    /// usage line, and the `Try:` line, its command bold when `style` says
    /// so (ARFC 0019 clause 3.1.8, principles T3, T9, T27, and T29).
    pub fn render(&self, style: Style) -> String {
        let command = if style.bold {
            format!("\x1b[1m{}\x1b[0m", self.try_command)
        } else {
            self.try_command.clone()
        };
        format!(
            "{}\n{}\nTry: {command}\n",
            wrap(&self.message, WIDTH),
            self.usage
        )
    }
}

/// The arguments as UTF-8 strings.
///
/// # Errors
///
/// A [`UsageError`] naming the first argument that is not valid UTF-8.
pub(crate) fn utf8_args<I>(args: I) -> Result<Vec<String>, UsageError>
where
    I: IntoIterator<Item = OsString>,
{
    args.into_iter()
        .map(|arg| {
            arg.into_string().map_err(|arg| {
                UsageError::new(
                    format!("The argument {} is not valid UTF-8.", arg.display()),
                    TOP_USAGE.to_owned(),
                    "alloy --help".to_owned(),
                )
            })
        })
        .collect()
}

/// Parses `args`, the arguments after the program name, against `commands`.
///
/// Flags may stand anywhere; `--` ends them. The words that are not flags
/// name the command; a command registered with a longer path wins.
///
/// # Errors
///
/// A [`UsageError`] for no command, an unknown command or verb, an unknown
/// flag, or an argument the command does not take, naming the first one found
/// in that order, with the nearest valid word on the `Try:` line when one is
/// within two edits.
pub fn parse<'a>(commands: &'a [CommandSpec], args: &[String]) -> Result<Parsed<'a>, UsageError> {
    let mut words: Vec<&str> = Vec::new();
    let mut help = false;
    let mut unknown_flag = None;
    let mut flags_end = false;
    for arg in args {
        let arg = arg.as_str();
        if flags_end || arg == "-" || !arg.starts_with('-') {
            words.push(arg);
        } else if arg == "--" {
            flags_end = true;
        } else if HELP_FLAGS.contains(&arg) {
            help = true;
        } else if unknown_flag.is_none() {
            unknown_flag = Some(arg);
        }
    }
    let found = resolve(commands, &words)?;
    if let Some(flag) = unknown_flag {
        return Err(flag_error(commands, found.map(|(spec, _)| spec), flag));
    }
    match found {
        _ if help => Ok(Parsed::Help(found.map(|(spec, _)| spec))),
        None => Err(UsageError::new(
            "alloy needs a command.".to_owned(),
            TOP_USAGE.to_owned(),
            "alloy --help".to_owned(),
        )),
        Some((spec, [])) => Ok(Parsed::Run(spec)),
        Some((spec, [extra, ..])) => Err(UsageError::new(
            format!(
                "`alloy {}` takes no arguments, and `{extra}` was given.",
                spec.name()
            ),
            spec.usage(),
            format!("alloy {}", spec.name()),
        )),
    }
}

/// The command `words` name and the words after its path, or `None` when
/// there are no words.
type Found<'a, 'w> = Option<(&'a CommandSpec, &'w [&'w str])>;

fn resolve<'a, 'w>(
    commands: &'a [CommandSpec],
    words: &'w [&'w str],
) -> Result<Found<'a, 'w>, UsageError> {
    if words.is_empty() {
        return Ok(None);
    }
    let matched = commands
        .iter()
        .filter(|spec| words.starts_with(spec.path))
        .max_by_key(|spec| spec.path.len());
    if let Some(spec) = matched {
        return Ok(Some((spec, &words[spec.path.len()..])));
    }
    // The longest run of leading words that starts some command's path.
    let depth = commands
        .iter()
        .map(|spec| {
            spec.path
                .iter()
                .zip(words)
                .take_while(|(want, word)| want == word)
                .count()
        })
        .max()
        .unwrap_or(0);
    let prefix = &words[..depth];
    let mut next: Vec<&str> = commands
        .iter()
        .filter(|spec| spec.path.len() > depth && spec.path.starts_with(prefix))
        .map(|spec| spec.path[depth])
        .collect();
    next.sort_unstable();
    next.dedup();
    let lead = prefix.join(" ");
    let Some(word) = words.get(depth) else {
        return Err(UsageError::new(
            format!("`alloy {lead}` needs one of: {}.", next.join(", ")),
            TOP_USAGE.to_owned(),
            "alloy --help".to_owned(),
        ));
    };
    let message = if depth == 0 {
        format!("`{word}` is not an alloy command.")
    } else {
        format!("`{word}` is not a verb of `alloy {lead}`.")
    };
    // The command line with the wrong word replaced by the nearest valid one,
    // offered only when it names a command, so that it runs as printed.
    let fixed = nearest(word, &next).map(|near| {
        let mut fixed = words.to_vec();
        fixed[depth] = near;
        fixed
    });
    let try_command = match fixed {
        Some(fixed) if commands.iter().any(|spec| fixed.starts_with(spec.path)) => {
            format!("alloy {}", fixed.join(" "))
        }
        _ => "alloy --help".to_owned(),
    };
    Err(UsageError::new(message, TOP_USAGE.to_owned(), try_command))
}

fn flag_error(commands: &[CommandSpec], spec: Option<&CommandSpec>, flag: &str) -> UsageError {
    let (message, usage, help) = match spec {
        Some(spec) => (
            format!("`{flag}` is not a flag of `alloy {}`.", spec.name()),
            spec.usage(),
            format!("alloy {} --help", spec.name()),
        ),
        None => (
            format!("`{flag}` is not an alloy flag."),
            TOP_USAGE.to_owned(),
            "alloy --help".to_owned(),
        ),
    };
    // `--version` and the like name a top-level command.
    let bare = flag.trim_start_matches('-');
    let try_command = if let Some(near) = nearest(flag, &HELP_FLAGS) {
        match spec {
            Some(spec) => format!("alloy {} {near}", spec.name()),
            None => format!("alloy {near}"),
        }
    } else if spec.is_none() && commands.iter().any(|c| c.path == [bare]) {
        format!("alloy {bare}")
    } else {
        help
    };
    UsageError::new(message, usage, try_command)
}

/// The help of `spec`, or of `alloy` itself when `None` (principle T8): the
/// usage line, what it does, the commands or the flags with their defaults,
/// and examples.
pub(crate) fn help(commands: &[CommandSpec], spec: Option<&CommandSpec>) -> String {
    let mut text = String::new();
    match spec {
        None => {
            text.push_str(TOP_USAGE);
            text.push_str("\n\nThe command line interface of Alloy.\n\nCommands:\n");
            let names: Vec<String> = commands.iter().map(CommandSpec::name).collect();
            let width = names.iter().map(String::len).max().unwrap_or(0);
            for (name, spec) in names.iter().zip(commands) {
                let _ = writeln!(text, "  {name:<width$}  {}", spec.summary);
            }
        }
        Some(spec) => {
            text.push_str(&spec.usage());
            text.push_str("\n\n");
            text.push_str(&wrap(spec.description, WIDTH));
            text.push('\n');
        }
    }
    text.push_str("\nFlags:\n  -h, --help  Print this help. Default: off\n");
    let examples: Vec<&str> = match spec {
        Some(spec) => spec.examples.to_vec(),
        None => commands
            .iter()
            .flat_map(|spec| spec.examples)
            .copied()
            .collect(),
    };
    text.push_str(if examples.len() == 1 {
        "\nExample:\n"
    } else {
        "\nExamples:\n"
    });
    for example in examples {
        let _ = writeln!(text, "  {example}");
    }
    text
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A table with nouns and verbs, as later releases register them.
    const TABLE: &[CommandSpec] = &[
        CommandSpec {
            id: CommandId::Version,
            path: &["version"],
            summary: "Print the release",
            description: "Prints the release.",
            examples: &["alloy version"],
        },
        CommandSpec {
            id: CommandId::Version,
            path: &["node", "list"],
            summary: "List nodes",
            description: "Lists nodes.",
            examples: &["alloy node list"],
        },
        CommandSpec {
            id: CommandId::Version,
            path: &["node", "approve"],
            summary: "Approve a node",
            description: "Approves a node.",
            examples: &["alloy node approve node2"],
        },
    ];

    fn words(args: &[&str]) -> Vec<String> {
        args.iter().map(|&arg| arg.to_owned()).collect()
    }

    fn ok(args: &[&str]) -> Parsed<'static> {
        parse(TABLE, &words(args)).unwrap()
    }

    fn fail(args: &[&str]) -> UsageError {
        parse(TABLE, &words(args)).unwrap_err()
    }

    #[test]
    fn parses_top_level_and_noun_verb_commands() {
        assert_eq!(ok(&["version"]), Parsed::Run(&TABLE[0]));
        assert_eq!(ok(&["node", "list"]), Parsed::Run(&TABLE[1]));
        assert_eq!(ok(&["node", "approve"]), Parsed::Run(&TABLE[2]));
    }

    #[test]
    fn help_flags_stand_anywhere() {
        assert_eq!(ok(&["--help"]), Parsed::Help(None));
        assert_eq!(ok(&["-h"]), Parsed::Help(None));
        assert_eq!(ok(&["node", "-h", "list"]), Parsed::Help(Some(&TABLE[1])));
        assert_eq!(ok(&["version", "--help"]), Parsed::Help(Some(&TABLE[0])));
        assert_eq!(ok(&["--", "version"]), Parsed::Run(&TABLE[0]));
    }

    #[test]
    fn usage_errors_name_the_wrong_word_and_suggest_the_nearest() {
        let cases: [(&[&str], &str, &str); 12] = [
            (&[], "alloy needs a command.", "alloy --help"),
            (
                &["verison"],
                "`verison` is not an alloy command.",
                "alloy version",
            ),
            (
                &["nod", "list"],
                "`nod` is not an alloy command.",
                "alloy node list",
            ),
            (&["nod"], "`nod` is not an alloy command.", "alloy --help"),
            (
                &["frobnicate"],
                "`frobnicate` is not an alloy command.",
                "alloy --help",
            ),
            (
                &["node"],
                "`alloy node` needs one of: approve, list.",
                "alloy --help",
            ),
            (
                &["node", "lst"],
                "`lst` is not a verb of `alloy node`.",
                "alloy node list",
            ),
            (
                &["node", "zap"],
                "`zap` is not a verb of `alloy node`.",
                "alloy --help",
            ),
            (
                &["version", "--json"],
                "`--json` is not a flag of `alloy version`.",
                "alloy version --help",
            ),
            (
                &["version", "--hepl"],
                "`--hepl` is not a flag of `alloy version`.",
                "alloy version --help",
            ),
            (
                &["--version"],
                "`--version` is not an alloy flag.",
                "alloy version",
            ),
            (
                &["version", "now"],
                "`alloy version` takes no arguments, and `now` was given.",
                "alloy version",
            ),
        ];
        for (args, message, try_command) in cases {
            let e = fail(args);
            assert_eq!(e.message, message, "{args:?}");
            assert_eq!(e.try_command, try_command, "{args:?}");
        }
        assert_eq!(fail(&["version", "now"]).usage, "Usage: alloy version");
        assert_eq!(fail(&["verison"]).usage, TOP_USAGE);
    }

    #[test]
    fn an_unknown_command_is_named_before_an_unknown_flag() {
        let e = fail(&["--frob", "verison"]);
        assert_eq!(e.message, "`verison` is not an alloy command.");
    }

    #[test]
    fn non_utf8_arguments_are_usage_errors() {
        assert_eq!(
            utf8_args([OsString::from("version")]),
            Ok(vec!["version".to_owned()])
        );
        #[cfg(unix)]
        let bad = {
            use std::os::unix::ffi::OsStringExt;
            OsString::from_vec(vec![0x66, 0xFF])
        };
        #[cfg(windows)]
        let bad = {
            use std::os::windows::ffi::OsStringExt;
            OsString::from_wide(&[0x66, 0xD800])
        };
        let e = utf8_args([bad]).unwrap_err();
        assert!(e.message.contains("is not valid UTF-8"), "{}", e.message);
    }

    #[test]
    fn help_shows_usage_flags_defaults_and_examples() {
        let top = help(COMMANDS, None);
        assert_eq!(
            top,
            "Usage: alloy <command> [flags]\n\
             \n\
             The command line interface of Alloy.\n\
             \n\
             Commands:\n  \
               version  Print the release of this alloy\n\
             \n\
             Flags:\n  \
               -h, --help  Print this help. Default: off\n\
             \n\
             Example:\n  \
               alloy version\n"
        );
        let version = help(COMMANDS, Some(&COMMANDS[0]));
        assert!(version.starts_with("Usage: alloy version\n\nPrints the release"));
        assert!(version.ends_with("Example:\n  alloy version\n"));
        let table = help(TABLE, None);
        assert!(
            table.contains("  version       Print the release\n"),
            "{table}"
        );
        assert!(
            table.contains("  node approve  Approve a node\n"),
            "{table}"
        );
        assert!(table.contains("Examples:\n"), "{table}");
        for text in [top, version, table] {
            assert!(text.lines().all(|line| line.len() <= WIDTH), "{text}");
            assert!(!text.contains("\n\n\n"), "{text}");
            assert!(text.lines().all(|line| !line.ends_with(' ')), "{text}");
        }
    }

    #[test]
    fn every_registered_command_is_valid() {
        for spec in COMMANDS {
            assert!(!spec.path.is_empty() && spec.path.len() <= 2);
            assert!(!spec.examples.is_empty());
            for example in spec.examples {
                let args: Vec<&str> = example.split(' ').skip(1).collect();
                assert_eq!(ok_in(COMMANDS, &args), Parsed::Run(spec), "{example}");
            }
        }
    }

    fn ok_in<'a>(commands: &'a [CommandSpec], args: &[&str]) -> Parsed<'a> {
        parse(commands, &words(args)).unwrap()
    }
}
