//! The command grammar of ARFC 0019 clause 3.1.1: the registered commands,
//! their flags and operands, parsing, help, and usage errors.

use std::collections::BTreeMap;
use std::ffi::OsString;
use std::fmt::Write as _;

use crate::Style;
use crate::text::{escape, nearest, wrap};

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
    /// `alloy init`: make this machine the first node of a new cluster
    /// (ARFC 0002 section 3.1).
    Init,
    /// `alloy join`: make this machine a node of a cluster (ARFC 0002
    /// section 3.3).
    Join,
    /// `alloy login`: exchange a login code, or issue one with
    /// `--issue-code` (ARFC 0019 clause 3.1.11).
    Login,
    /// `alloy node list`: print every node (ARFC 0019 clause 3.1.15).
    NodeList,
}

/// One flag of a command.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct FlagSpec {
    /// Its name with the leading `--`, as in `--context`.
    pub name: &'static str,
    /// The placeholder of its value, as in `<name>`, or `None` for a switch,
    /// which takes no value.
    pub value: Option<&'static str>,
    /// What applies when it is not given, for `--help` (principle T8).
    pub default: &'static str,
    /// One line saying what it does.
    pub help: &'static str,
}

/// One registered command.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct CommandSpec {
    /// What it does.
    pub id: CommandId,
    /// Its words: one for a top-level command (`["version"]`), a noun and a
    /// verb otherwise (`["node", "list"]`).
    pub path: &'static [&'static str],
    /// The placeholder of the one operand it may take, as in `<login code>`,
    /// or `None` when it takes none.
    pub operand: Option<&'static str>,
    /// Its flags, besides [`GLOBAL_FLAGS`] and `-h`, `--help`.
    pub flags: &'static [FlagSpec],
    /// One line for the command list of `alloy --help`.
    pub summary: &'static str,
    /// What `alloy <command> --help` says it does: one paragraph.
    pub description: &'static str,
    /// Command lines that run as printed (principles T5 and T8).
    pub examples: &'static [&'static str],
}

impl CommandSpec {
    /// The command's words joined by spaces.
    pub fn name(&self) -> String {
        self.path.join(" ")
    }

    /// The command's usage line.
    pub fn usage(&self) -> String {
        let mut usage = format!("Usage: alloy {}", self.name());
        if let Some(operand) = self.operand {
            usage.push(' ');
            usage.push_str(operand);
        }
        if !self.flags.is_empty() {
            usage.push_str(" [flags]");
        }
        usage
    }

    fn flag(&self, name: &str) -> Option<&'static FlagSpec> {
        self.flags
            .iter()
            .chain(GLOBAL_FLAGS)
            .find(|flag| flag.name == name)
    }
}

/// The flags every command takes.
pub const GLOBAL_FLAGS: &[FlagSpec] = &[FlagSpec {
    name: "--color",
    value: Some("<when>"),
    default: "auto",
    help: "Color the output: auto, always, or never",
}];

/// `--output` of a command that prints the API representation (ARFC 0019
/// clause 3.1.4).
const OUTPUT: FlagSpec = FlagSpec {
    name: "--output",
    value: Some("<format>"),
    default: "table",
    help: "The output format: table, json, or yaml",
};

/// `--name` of `init` and `join`.
const NAME: FlagSpec = FlagSpec {
    name: "--name",
    value: Some("<name>"),
    default: "the hostname",
    help: "The node's name",
};

/// `--advertise` of `init` and `join`.
const ADVERTISE: FlagSpec = FlagSpec {
    name: "--advertise",
    value: Some("<addr>[,<addr>...]"),
    default: "the address of the default route",
    help: "The addresses other machines reach this node at",
};

/// `--mesh-port` of `init` and `join`.
const MESH_PORT: FlagSpec = FlagSpec {
    name: "--mesh-port",
    value: Some("<port>"),
    default: "7476",
    help: "The UDP port of the mesh",
};

/// `--api-port` of `init` and `join`.
const API_PORT: FlagSpec = FlagSpec {
    name: "--api-port",
    value: Some("<port>"),
    default: "443",
    help: "The TCP port of the API, on an anchor or control node",
};

/// The commands of this release: those M11 rung 2 builds (ARFC 0002
/// section 3.15).
pub const COMMANDS: &[CommandSpec] = &[
    CommandSpec {
        id: CommandId::Version,
        path: &["version"],
        operand: None,
        flags: &[],
        summary: "Print the release of this alloy",
        description: "Prints the release of this alloy, and nothing else, on standard output.",
        examples: &["alloy version"],
    },
    CommandSpec {
        id: CommandId::Init,
        path: &["init"],
        operand: None,
        flags: &[ADVERTISE, NAME, MESH_PORT, API_PORT],
        summary: "Make this machine the first node of a new cluster",
        description: "Checks this machine, creates a cluster with this machine as its \
            first node, and starts alloyd. Prints a join token for adding machines and a \
            login code for logging in, once each. Runs as root on Linux.",
        examples: &[
            "sudo alloy init",
            "sudo alloy init --advertise 203.0.113.10",
        ],
    },
    CommandSpec {
        id: CommandId::Join,
        path: &["join"],
        operand: Some("<join token>"),
        flags: &[
            FlagSpec {
                name: "--roles",
                value: Some("<list>"),
                default: "the token's roles, else compute,storage; none on Windows",
                help: "The node's roles: anchor, control, compute, storage, or none",
            },
            FlagSpec {
                name: "--zone",
                value: Some("<name>"),
                default: "the token's zone, else default",
                help: "The node's zone",
            },
            NAME,
            ADVERTISE,
            MESH_PORT,
            API_PORT,
        ],
        summary: "Make this machine a node of a cluster",
        description: "Checks this machine, joins the cluster the join token names, and \
            starts alloyd. Runs as root on Linux, and from an elevated administrator \
            prompt on Windows, where the node is a client node.",
        examples: &["sudo alloy join <join-token>"],
    },
    CommandSpec {
        id: CommandId::Login,
        path: &["login"],
        operand: Some("<login code>"),
        flags: &[
            FlagSpec {
                name: "--context",
                value: Some("<name>"),
                default: "the first API address of the login code",
                help: "The context to log in to",
            },
            FlagSpec {
                name: "--issue-code",
                value: None,
                default: "off",
                help: "Print a new login code instead of logging in",
            },
            FlagSpec {
                name: "--user",
                value: Some("<name>"),
                default: "admin",
                help: "With --issue-code as root on a control node: the user",
            },
            FlagSpec {
                name: "--org",
                value: Some("<name>"),
                default: "default",
                help: "With --issue-code as root on a control node: the user's org",
            },
        ],
        summary: "Log in to a cluster, or print a new login code",
        description: "Exchanges a login code for an access key for this computer, \
            stores the key, and saves a context for the cluster that becomes the \
            current one. With --issue-code, prints a new login code instead: for the \
            current context's user, or, run as root on a control node with no \
            current context, through the local admin socket for the user that --user \
            and --org name.",
        examples: &["sudo alloy login --issue-code"],
    },
    CommandSpec {
        id: CommandId::NodeList,
        path: &["node", "list"],
        operand: None,
        flags: &[OUTPUT],
        summary: "List the nodes of the cluster",
        description: "Prints every node of the current context's cluster, one row per \
            node, sorted by name.",
        examples: &["alloy node list", "alloy node list --output json"],
    },
];

/// The words of the command line other than flags, and the flags given,
/// once each.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Invocation {
    /// The operands after the command's words.
    pub operands: Vec<String>,
    /// The flags given, by name, with their values; a switch's value is
    /// empty.
    pub flags: BTreeMap<&'static str, String>,
}

impl Invocation {
    /// The value of the flag `name`, when it was given.
    pub fn value(&self, name: &str) -> Option<&str> {
        self.flags.get(name).map(String::as_str)
    }

    /// Whether the flag `name` was given.
    pub fn has(&self, name: &str) -> bool {
        self.flags.contains_key(name)
    }
}

/// What a command line asks for.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Parsed<'a> {
    /// Run this command with these operands and flags.
    Run(&'a CommandSpec, Invocation),
    /// Print the help of this command, or of `alloy` itself when `None`.
    Help(Option<&'a CommandSpec>),
}

/// A command line that `alloy` does not accept (exit code 2, ARFC 0019
/// clause 3.1.7).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct UsageError {
    /// What is wrong, naming the one wrong argument or flag (principle T29),
    /// with control characters escaped.
    pub message: String,
    /// The usage line of the command, or of `alloy`.
    pub usage: String,
    /// The command for the `Try:` line, which runs as printed: it holds only
    /// words of the command table, never a word the user typed.
    pub try_command: Option<String>,
}

impl UsageError {
    fn new(message: String, usage: String, try_command: String) -> UsageError {
        UsageError {
            message,
            usage,
            try_command: Some(try_command),
        }
    }

    /// A usage error of `spec` that its own checks find after parsing, with
    /// `alloy <command> --help` on the `Try:` line.
    pub fn of(spec: &CommandSpec, message: String) -> UsageError {
        UsageError::new(
            message,
            spec.usage(),
            format!("alloy {} --help", spec.name()),
        )
    }

    /// The text for standard error: the message wrapped at 80 columns, the
    /// usage line, and the `Try:` line, its command bold when `style` says
    /// so (ARFC 0019 clause 3.1.8, principles T3, T9, T27, and T29).
    pub fn render(&self, style: Style) -> String {
        let mut text = format!("{}\n{}\n", wrap(&self.message, WIDTH), self.usage);
        if let Some(command) = &self.try_command {
            let _ = writeln!(text, "Try: {}", style.bold(command));
        }
        text
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
                    format!(
                        "The argument `{}` is not valid UTF-8.",
                        escape(&arg.to_string_lossy())
                    ),
                    TOP_USAGE.to_owned(),
                    "alloy --help".to_owned(),
                )
            })
        })
        .collect()
}

/// Parses `args`, the arguments after the program name, against `commands`.
///
/// Flags may stand anywhere; `--` ends them. A flag that takes a value takes
/// it as `--flag value` or `--flag=value`. The words that are not flags or
/// flag values name the command, and a command registered with a longer path
/// wins; the words after its path are its operands.
///
/// # Errors
///
/// A [`UsageError`] for no command, an unknown command or verb, an unknown
/// flag, a flag without its value or with a value it does not take, a flag
/// given twice, or an operand the command does not take, naming the first
/// one found in that order. The `Try:` line holds the nearest valid command
/// when one is within two edits and that command parses, else the command's
/// help.
pub fn parse<'a>(commands: &'a [CommandSpec], args: &[String]) -> Result<Parsed<'a>, UsageError> {
    parse_with(commands, args, true)
}

/// Whether `words` is a command line that parses to a command to run.
fn runs(commands: &[CommandSpec], words: &[&str]) -> bool {
    let args: Vec<String> = words.iter().map(|&word| word.to_owned()).collect();
    matches!(parse_with(commands, &args, false), Ok(Parsed::Run(..)))
}

/// A flag as given: its name, its value when given with `=` or after it,
/// and whether a value was given at all.
struct GivenFlag<'w> {
    name: &'w str,
    value: Option<&'w str>,
}

fn parse_with<'a>(
    commands: &'a [CommandSpec],
    args: &[String],
    suggest: bool,
) -> Result<Parsed<'a>, UsageError> {
    let mut words: Vec<&str> = Vec::new();
    let mut given: Vec<GivenFlag<'_>> = Vec::new();
    let mut help = false;
    let mut flags_end = false;
    let mut rest = args.iter().map(String::as_str);
    while let Some(arg) = rest.next() {
        if flags_end || arg == "-" || !arg.starts_with('-') {
            words.push(arg);
        } else if arg == "--" {
            flags_end = true;
        } else if HELP_FLAGS.contains(&arg) {
            help = true;
        } else if let Some((name, value)) = arg.split_once('=')
            && name.starts_with("--")
        {
            given.push(GivenFlag {
                name,
                value: Some(value),
            });
        } else if takes_value(commands, &words, arg) {
            given.push(GivenFlag {
                name: arg,
                value: rest.next(),
            });
        } else {
            given.push(GivenFlag {
                name: arg,
                value: None,
            });
        }
    }
    let found = resolve(commands, &words, suggest)?;
    let spec = found.map(|(spec, _)| spec);
    let mut invocation = Invocation::default();
    for flag in &given {
        let Some(known) = spec.and_then(|spec| spec.flag(flag.name)).or_else(|| {
            GLOBAL_FLAGS
                .iter()
                .find(|global| spec.is_none() && global.name == flag.name)
        }) else {
            return Err(flag_error(commands, spec, flag.name, suggest));
        };
        let value = match (known.value, flag.value) {
            (Some(_), Some(value)) => value.to_owned(),
            (None, None) => String::new(),
            (Some(placeholder), None) => {
                return Err(flag_usage(
                    spec,
                    format!("`{}` needs a value: {placeholder}.", known.name),
                ));
            }
            (None, Some(_)) => {
                return Err(flag_usage(
                    spec,
                    format!("`{}` takes no value.", known.name),
                ));
            }
        };
        if invocation.flags.insert(known.name, value).is_some() {
            return Err(flag_usage(
                spec,
                format!("`{}` is given more than once.", known.name),
            ));
        }
    }
    match found {
        _ if help => Ok(Parsed::Help(spec)),
        None => Err(UsageError::new(
            "alloy needs a command.".to_owned(),
            TOP_USAGE.to_owned(),
            "alloy --help".to_owned(),
        )),
        Some((spec, operands)) => {
            let allowed = usize::from(spec.operand.is_some());
            if operands.len() > allowed {
                let message = match spec.operand {
                    // The operand may be a secret, such as a login code, so
                    // it is not echoed (ARFC 0019 clause 3.1.13).
                    Some(operand) => format!(
                        "`alloy {}` takes one argument, {operand}, and {} were given.",
                        spec.name(),
                        operands.len()
                    ),
                    None => format!(
                        "`alloy {}` takes no arguments, and `{}` was given.",
                        spec.name(),
                        escape(operands[0])
                    ),
                };
                return Err(UsageError::new(
                    message,
                    spec.usage(),
                    format!("alloy {} --help", spec.name()),
                ));
            }
            invocation.operands = operands.iter().map(|&word| word.to_owned()).collect();
            Ok(Parsed::Run(spec, invocation))
        }
    }
}

/// Whether `flag`, given after `words`, takes a value in a command those
/// words may name, or is a global flag that does.
fn takes_value(commands: &[CommandSpec], words: &[&str], flag: &str) -> bool {
    let named = commands
        .iter()
        .filter(|spec| spec.path.starts_with(words) || words.starts_with(spec.path))
        .flat_map(|spec| spec.flags);
    named
        .chain(GLOBAL_FLAGS)
        .any(|known| known.name == flag && known.value.is_some())
}

/// The usage error `message` about a flag of `spec`, or of `alloy`.
fn flag_usage(spec: Option<&CommandSpec>, message: String) -> UsageError {
    match spec {
        Some(spec) => UsageError::of(spec, message),
        None => UsageError::new(message, TOP_USAGE.to_owned(), "alloy --help".to_owned()),
    }
}

/// The command `words` name and the words after its path, or `None` when
/// there are no words.
type Found<'a, 'w> = Option<(&'a CommandSpec, &'w [&'w str])>;

fn resolve<'a, 'w>(
    commands: &'a [CommandSpec],
    words: &'w [&'w str],
    suggest: bool,
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
    let shown = escape(word);
    let message = if depth == 0 {
        format!("`{shown}` is not an alloy command.")
    } else {
        format!("`{shown}` is not a verb of `alloy {lead}`.")
    };
    // The words with the wrong one replaced by the nearest valid one, cut to
    // the path of the command they then name. The words after that path are
    // the user's and are left out, so the line holds only words of the
    // table, and it is offered only when it parses (T-0091 review F2).
    let try_command = nearest(word, &next)
        .filter(|_| suggest)
        .and_then(|near| {
            let mut fixed = words.to_vec();
            fixed[depth] = near;
            commands
                .iter()
                .filter(|spec| fixed.starts_with(spec.path))
                .max_by_key(|spec| spec.path.len())
                .map(|spec| spec.path)
        })
        .filter(|path| runs(commands, path))
        .map_or_else(
            || "alloy --help".to_owned(),
            |path| format!("alloy {}", path.join(" ")),
        );
    Err(UsageError::new(message, TOP_USAGE.to_owned(), try_command))
}

fn flag_error(
    commands: &[CommandSpec],
    spec: Option<&CommandSpec>,
    flag: &str,
    suggest: bool,
) -> UsageError {
    let shown = escape(flag);
    let (message, usage, help) = match spec {
        Some(spec) => (
            format!("`{shown}` is not a flag of `alloy {}`.", spec.name()),
            spec.usage(),
            format!("alloy {} --help", spec.name()),
        ),
        None => (
            format!("`{shown}` is not an alloy flag."),
            TOP_USAGE.to_owned(),
            "alloy --help".to_owned(),
        ),
    };
    if !suggest {
        return UsageError::new(message, usage, help);
    }
    let mut names: Vec<&str> = HELP_FLAGS.to_vec();
    if let Some(spec) = spec {
        names.extend(spec.flags.iter().chain(GLOBAL_FLAGS).map(|f| f.name));
    }
    // The command with the nearest flag, offered only when it parses: a
    // flag that needs a value does not, since the line holds no word the
    // user typed (T-0091 review F2).
    let path: &[&str] = spec.map_or(&[], |spec| spec.path);
    let near = nearest(flag, &names).and_then(|near| {
        let mut line = path.to_vec();
        line.push(near);
        runs(commands, &line)
            .then(|| line.join(" "))
            .or_else(|| HELP_FLAGS.contains(&near).then(|| line.join(" ")))
    });
    // `--version` and the like name a top-level command.
    let bare = flag.trim_start_matches('-');
    let try_command = if let Some(line) = near {
        format!("alloy {line}")
    } else if spec.is_none() && commands.iter().any(|c| c.path == [bare]) && runs(commands, &[bare])
    {
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
    text.push_str("\nFlags:\n");
    let mut rows: Vec<(String, String)> = spec
        .map_or(&[][..], |spec| spec.flags)
        .iter()
        .chain(GLOBAL_FLAGS)
        .map(|flag| {
            let name = match flag.value {
                Some(value) => format!("{} {value}", flag.name),
                None => flag.name.to_owned(),
            };
            (name, format!("{}. Default: {}", flag.help, flag.default))
        })
        .collect();
    rows.push((
        "-h, --help".to_owned(),
        "Print this help. Default: off".to_owned(),
    ));
    let width = rows.iter().map(|(name, _)| name.len()).max().unwrap_or(0);
    for (name, about) in rows {
        let line = format!("  {name:<width$}  {about}");
        if line.chars().count() <= WIDTH {
            let _ = writeln!(text, "{line}");
        } else {
            // A long line continues under its description (principle T3).
            let indent = " ".repeat(width + 4);
            let wrapped = wrap(&about, WIDTH - width - 4);
            let mut lines = wrapped.lines();
            let _ = writeln!(text, "  {name:<width$}  {}", lines.next().unwrap_or(""));
            for rest in lines {
                let _ = writeln!(text, "{indent}{rest}");
            }
        }
    }
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

    /// A table with nouns, verbs, operands, and flags.
    const TABLE: &[CommandSpec] = &[
        CommandSpec {
            id: CommandId::Version,
            path: &["version"],
            operand: None,
            flags: &[],
            summary: "Print the release",
            description: "Prints the release.",
            examples: &["alloy version"],
        },
        CommandSpec {
            id: CommandId::NodeList,
            path: &["node", "list"],
            operand: None,
            flags: &[OUTPUT],
            summary: "List nodes",
            description: "Lists nodes.",
            examples: &["alloy node list"],
        },
        CommandSpec {
            id: CommandId::Version,
            path: &["node", "approve"],
            operand: Some("<name>"),
            flags: &[FlagSpec {
                name: "--yes",
                value: None,
                default: "off",
                help: "Do not ask",
            }],
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

    fn run_of(
        spec: &'static CommandSpec,
        operands: &[&str],
        flags: &[(&'static str, &str)],
    ) -> Parsed<'static> {
        Parsed::Run(
            spec,
            Invocation {
                operands: words(operands),
                flags: flags.iter().map(|&(k, v)| (k, v.to_owned())).collect(),
            },
        )
    }

    #[test]
    fn parses_top_level_and_noun_verb_commands() {
        assert_eq!(ok(&["version"]), run_of(&TABLE[0], &[], &[]));
        assert_eq!(ok(&["node", "list"]), run_of(&TABLE[1], &[], &[]));
        assert_eq!(ok(&["node", "approve"]), run_of(&TABLE[2], &[], &[]));
        assert_eq!(
            ok(&["node", "approve", "node2"]),
            run_of(&TABLE[2], &["node2"], &[])
        );
    }

    #[test]
    fn flags_take_values_in_either_form_and_stand_anywhere() {
        let json = run_of(&TABLE[1], &[], &[("--output", "json")]);
        assert_eq!(ok(&["node", "list", "--output", "json"]), json);
        assert_eq!(ok(&["node", "--output", "json", "list"]), json);
        assert_eq!(ok(&["--output=json", "node", "list"]), json);
        assert_eq!(
            ok(&["node", "approve", "--yes", "node2", "--color", "never"]),
            run_of(
                &TABLE[2],
                &["node2"],
                &[("--yes", ""), ("--color", "never")]
            )
        );
        // A value that starts with a dash is still the value.
        assert_eq!(
            ok(&["node", "list", "--output", "-"]),
            run_of(&TABLE[1], &[], &[("--output", "-")])
        );
        assert_eq!(
            ok(&["--color", "always", "version"]),
            run_of(&TABLE[0], &[], &[("--color", "always")])
        );
    }

    #[test]
    fn help_flags_stand_anywhere() {
        assert_eq!(ok(&["--help"]), Parsed::Help(None));
        assert_eq!(ok(&["-h"]), Parsed::Help(None));
        assert_eq!(ok(&["node", "-h", "list"]), Parsed::Help(Some(&TABLE[1])));
        assert_eq!(ok(&["version", "--help"]), Parsed::Help(Some(&TABLE[0])));
        assert_eq!(ok(&["--", "version"]), run_of(&TABLE[0], &[], &[]));
    }

    #[test]
    fn usage_errors_name_the_wrong_word_and_suggest_the_nearest() {
        let cases: [(&[&str], &str, &str); 19] = [
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
                "alloy version --help",
            ),
            (
                &["node", "approve", "a", "b"],
                "`alloy node approve` takes one argument, <name>, and 2 were given.",
                "alloy node approve --help",
            ),
            (
                &["node", "approve", "--ye"],
                "`--ye` is not a flag of `alloy node approve`.",
                "alloy node approve --yes",
            ),
            (
                &["node", "list", "--outptu", "json"],
                "`--outptu` is not a flag of `alloy node list`.",
                "alloy node list --help",
            ),
            (
                &["node", "list", "--output"],
                "`--output` needs a value: <format>.",
                "alloy node list --help",
            ),
            (
                &["node", "approve", "--yes=1"],
                "`--yes` takes no value.",
                "alloy node approve --help",
            ),
            (
                &["node", "list", "--output", "a", "--output=b"],
                "`--output` is given more than once.",
                "alloy node list --help",
            ),
            (
                &["--output", "json"],
                "`--output` is not an alloy flag.",
                "alloy --help",
            ),
        ];
        for (args, message, try_command) in cases {
            let e = fail(args);
            assert_eq!(e.message, message, "{args:?}");
            assert_eq!(e.try_command.as_deref(), Some(try_command), "{args:?}");
        }
        assert_eq!(fail(&["version", "now"]).usage, "Usage: alloy version");
        assert_eq!(
            fail(&["node", "approve", "a", "b"]).usage,
            "Usage: alloy node approve <name> [flags]"
        );
        assert_eq!(fail(&["verison"]).usage, TOP_USAGE);
    }

    /// T-0091 review F2: a `Try:` command parses when run as printed, with
    /// words after a mistyped command, and holds no word the user typed.
    #[test]
    fn every_try_command_runs_as_printed() {
        let typed: &[&[&str]] = &[
            &["verison", "now"],
            &["verison", "a b"],
            &["verison", "--", "-"],
            &["nod", "list", "extra"],
            &["nod", "approve", "x", "y", "z"],
            &["node", "lst", "--output"],
            &["--", "--version"],
            &["version", "--json"],
            &["node", "approve", "--ye", "x"],
            &["verison", "$(rm -rf /)"],
        ];
        for args in typed {
            let e = fail(args);
            let command = e.try_command.expect("a Try: line");
            let line: Vec<&str> = command.split(' ').collect();
            assert_eq!(line[0], "alloy", "{args:?}");
            let parsed = parse(TABLE, &words(&line[1..]));
            assert!(
                matches!(parsed, Ok(Parsed::Run(..) | Parsed::Help(_))),
                "{args:?} -> {command}: {parsed:?}"
            );
            for word in &line[1..] {
                assert!(
                    TABLE.iter().any(|spec| spec.path.contains(word)) || word.starts_with('-'),
                    "{args:?} -> {command}"
                );
            }
        }
        assert_eq!(
            fail(&["verison", "now"]).try_command.as_deref(),
            Some("alloy version")
        );
    }

    /// T-0091 review F3: control characters in an echoed argument are
    /// escaped.
    #[test]
    fn control_characters_in_echoed_arguments_are_escaped() {
        let e = fail(&["\u{1b}]0;pwned\u{7}"]);
        assert_eq!(
            e.message,
            "`\\u{1b}]0;pwned\\u{7}` is not an alloy command."
        );
        let e = fail(&["version", "--x\u{1b}[2J"]);
        assert!(!e.render(Style::default()).contains('\u{1b}'), "{e:?}");
        let e = fail(&["version", "a\nb"]);
        assert!(e.message.contains("`a\\nb`"), "{e:?}");
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
               version    Print the release of this alloy\n  \
               init       Make this machine the first node of a new cluster\n  \
               join       Make this machine a node of a cluster\n  \
               login      Log in to a cluster, or print a new login code\n  \
               node list  List the nodes of the cluster\n\
             \n\
             Flags:\n  \
               --color <when>  Color the output: auto, always, or never. Default: auto\n  \
               -h, --help      Print this help. Default: off\n\
             \n\
             Examples:\n  \
               alloy version\n  \
               sudo alloy init\n  \
               sudo alloy init --advertise 203.0.113.10\n  \
               sudo alloy join <join-token>\n  \
               sudo alloy login --issue-code\n  \
               alloy node list\n  \
               alloy node list --output json\n"
        );
        let version = help(COMMANDS, Some(&COMMANDS[0]));
        assert!(version.starts_with("Usage: alloy version\n\nPrints the release"));
        assert!(version.ends_with("Example:\n  alloy version\n"));
        let login = help(
            COMMANDS,
            COMMANDS.iter().find(|spec| spec.id == CommandId::Login),
        );
        assert!(
            login.starts_with("Usage: alloy login <login code> [flags]\n"),
            "{login}"
        );
        assert!(
            login.contains(
                "  --issue-code      Print a new login code instead of logging in. Default: off\n"
            ),
            "{login}"
        );
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
        let mut all = vec![top, table];
        all.extend(COMMANDS.iter().map(|spec| help(COMMANDS, Some(spec))));
        for text in all {
            assert!(
                text.lines().all(|line| line.chars().count() <= WIDTH),
                "{text}"
            );
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
                let example = example.strip_prefix("sudo ").unwrap_or(example);
                let args: Vec<&str> = example.split(' ').skip(1).collect();
                match parse(COMMANDS, &words(&args)).unwrap() {
                    Parsed::Run(found, _) => assert_eq!(found, spec, "{example}"),
                    Parsed::Help(_) => panic!("{example}"),
                }
            }
            for flag in spec.flags {
                assert!(flag.name.starts_with("--"), "{}", flag.name);
                assert!(!GLOBAL_FLAGS.iter().any(|g| g.name == flag.name));
            }
        }
    }
}
