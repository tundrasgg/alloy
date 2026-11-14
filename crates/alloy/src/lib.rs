//! The Alloy command line interface, `alloy` (ARFC 0019 section 3.1).
//!
//! Commands are `alloy <noun> <verb> [name] [flags]` and the top-level
//! commands of ARFC 0019 clause 3.1.2. A command exists once its task
//! registers it in [`COMMANDS`]; until then it is a usage error, like any
//! unknown command. This release registers the commands M11 rung 2 builds
//! (ARFC 0002 section 3.15): `version`, `init`, `join`, `login` (with a
//! login code, and `--issue-code`), and `node list`.
//!
//! [`run`] is the whole command line: the binary passes it the process's
//! arguments, an [`Env`] describing its streams and environment, and its
//! standard output and error, and returns the exit code it gives back.
//! `version`, help, and usage errors need nothing else. The other commands
//! start an `os` runtime of `alloy-rt` for the API connection and the
//! configuration files (`agent/eng/rust.md` rule 2); `init` and `join` also
//! build the `host` seam beside it (ADR 0071).

#![forbid(unsafe_code)]

mod api;
mod config;
mod grammar;
mod login;
mod machine;
mod node;
mod repr;
mod setup;
mod text;

use std::collections::BTreeMap;
use std::ffi::{OsStr, OsString};
use std::io::Write;

use alloy_api::Transport;
use alloy_rt::{Runtime, StreamAddr};

pub use grammar::{
    COMMANDS, CommandId, CommandSpec, FlagSpec, GLOBAL_FLAGS, Invocation, Parsed, UsageError, parse,
};

/// The release this `alloy` is: the package version at build time, which the
/// release workflow sets from the release's tag (ARFC 0017 clause 3.13).
pub const VERSION: &str = env!("CARGO_PKG_VERSION");

/// Exit code 0: success (ARFC 0019 clause 3.1.7).
pub const EXIT_SUCCESS: u8 = 0;
/// Exit code 1: failure (ARFC 0019 clause 3.1.7).
pub const EXIT_FAILURE: u8 = 1;
/// Exit code 2: usage error (ARFC 0019 clause 3.1.7).
pub const EXIT_USAGE: u8 = 2;
/// Exit code 3: not found (ARFC 0019 clause 3.1.7).
pub const EXIT_NOT_FOUND: u8 = 3;
/// Exit code 4: permission denied, not logged in included (ARFC 0019
/// clauses 3.1.7 and 3.1.11).
pub const EXIT_DENIED: u8 = 4;
/// Exit code 5: conflict (ARFC 0019 clause 3.1.7).
pub const EXIT_CONFLICT: u8 = 5;
/// Exit code 6: unavailable (ARFC 0019 clause 3.1.7).
pub const EXIT_UNAVAILABLE: u8 = 6;

/// The local admin socket on Linux (ARFC 0007 clause 3.10.2).
pub const ADMIN_SOCKET: &str = "/run/alloy/admin.sock";

/// The environment variables `alloy` reads: color (principle T12), the
/// configuration directory (ARFC 0019 clause 3.1.9), the hostname that
/// names a new access key (ARFC 0007 clause 3.9.3), and on Windows the
/// folder that holds the data directory (ARFC 0002 section 3.12).
pub const ENV_VARS: &[&str] = &[
    "NO_COLOR",
    "TERM",
    "XDG_CONFIG_HOME",
    "HOME",
    "APPDATA",
    "COMPUTERNAME",
    "HOSTNAME",
    "ProgramData",
];

/// What `alloy` knows of its process besides its arguments.
#[derive(Clone, Debug, Default)]
pub struct Env {
    /// Whether standard output is a terminal.
    pub stdout_terminal: bool,
    /// Whether standard error is a terminal.
    pub stderr_terminal: bool,
    /// Whether this is Windows, where this release colors nothing, since it
    /// does not check whether the console accepts ANSI sequences.
    pub windows: bool,
    /// The values of the variables of [`ENV_VARS`] that are set.
    pub vars: BTreeMap<String, OsString>,
    /// The local admin socket, where the platform has one.
    pub admin_socket: Option<StreamAddr>,
}

impl Env {
    /// The variable `name`, when it is set.
    fn var(&self, name: &str) -> Option<&OsStr> {
        self.vars.get(name).map(OsString::as_os_str)
    }

    /// The style of a stream that is a terminal when `terminal`, by
    /// principle T12 and `--color`.
    fn style(&self, terminal: bool, mode: ColorMode) -> Style {
        let auto = Style::of_stream(
            terminal,
            self.var("NO_COLOR"),
            self.var("TERM"),
            self.windows,
        );
        match mode {
            ColorMode::Auto => auto,
            ColorMode::Never => Style { on: false },
            ColorMode::Always => Style { on: !self.windows },
        }
    }
}

/// `--color` (principle T12).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ColorMode {
    Auto,
    Always,
    Never,
}

/// How text on one stream may be styled.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Style {
    /// Whether bold and the ANSI colors are used (principles T9 and T11).
    pub on: bool,
}

impl Style {
    /// The style of a stream by principle T12 with `--color` at `auto`:
    /// styled only on a terminal, with `NO_COLOR` unset or empty and `TERM`
    /// other than `dumb`. Never on Windows, where this release does not check
    /// whether the console accepts ANSI sequences.
    pub fn of_stream(
        is_terminal: bool,
        no_color: Option<&OsStr>,
        term: Option<&OsStr>,
        windows: bool,
    ) -> Style {
        let on = is_terminal
            && !windows
            && no_color.is_none_or(OsStr::is_empty)
            && term.is_none_or(|term| term != "dumb");
        Style { on }
    }

    fn paint(self, code: &str, text: &str) -> String {
        if self.on {
            format!("\x1b[{code}m{text}\x1b[0m")
        } else {
            text.to_owned()
        }
    }

    /// `text` in bold, when styled.
    pub fn bold(self, text: &str) -> String {
        self.paint("1", text)
    }

    /// `text` in red, when styled.
    pub fn red(self, text: &str) -> String {
        self.paint("31", text)
    }

    /// `text` in yellow, when styled.
    pub fn yellow(self, text: &str) -> String {
        self.paint("33", text)
    }

    /// `text` in green, when styled.
    pub fn green(self, text: &str) -> String {
        self.paint("32", text)
    }
}

/// The styles of both streams.
#[derive(Clone, Copy, Debug, Default)]
pub(crate) struct Styles {
    pub(crate) out: Style,
    pub(crate) err: Style,
}

/// Why a command failed, for standard error: the message, then a `Try:`
/// line when a command fixes it (ARFC 0019 clause 3.1.8, principle T27).
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Failure {
    /// The exit code (ARFC 0019 clause 3.1.7).
    pub(crate) code: u8,
    /// What happened and why, one paragraph, control characters escaped.
    pub(crate) message: String,
    /// The usage line, for a usage error.
    pub(crate) usage: Option<String>,
    /// The command that fixes it, which runs as printed (principle T5).
    pub(crate) try_command: Option<String>,
}

impl Failure {
    pub(crate) fn new(code: u8, message: String) -> Failure {
        Failure {
            code,
            message,
            usage: None,
            try_command: None,
        }
    }

    pub(crate) fn with_try(mut self, command: impl Into<String>) -> Failure {
        self.try_command = Some(command.into());
        self
    }

    fn render(&self, style: Style) -> String {
        let mut text = text::wrap(&self.message, 80);
        text.push('\n');
        if let Some(usage) = &self.usage {
            text.push_str(usage);
            text.push('\n');
        }
        if let Some(command) = &self.try_command {
            text.push_str("Try: ");
            text.push_str(&style.bold(command));
            text.push('\n');
        }
        text
    }
}

impl From<UsageError> for Failure {
    fn from(error: UsageError) -> Failure {
        Failure {
            code: EXIT_USAGE,
            message: error.message,
            usage: Some(error.usage),
            try_command: error.try_command,
        }
    }
}

/// What a command runs with.
pub(crate) struct Ctx<'a> {
    pub(crate) runtime: &'a Runtime,
    pub(crate) transport: Transport,
    pub(crate) env: &'a Env,
    pub(crate) styles: Styles,
}

/// Runs the command line `args`, the arguments after the program name, and
/// returns the exit code.
///
/// The result goes to `out` and nothing else does (principle T1). Step
/// lines, warnings, errors, and the `Next:` and `Try:` lines go to `err`. A
/// usage error goes to `err` as its message, the usage line, and a `Try:`
/// line, and returns [`EXIT_USAGE`]. A failed write to `out` returns
/// [`EXIT_FAILURE`].
pub fn run<I>(args: I, env: &Env, out: &mut dyn Write, err: &mut dyn Write) -> u8
where
    I: IntoIterator<Item = OsString>,
{
    let args: Vec<OsString> = args.into_iter().collect();
    let parsed = grammar::utf8_args(args.iter().cloned()).and_then(|args| parse(COMMANDS, &args));
    let (spec, invocation) = match parsed {
        Ok(Parsed::Help(spec)) => return write_out(out, &grammar::help(COMMANDS, spec)),
        Ok(Parsed::Run(spec, invocation)) => (spec, invocation),
        Err(e) => {
            let style = env.style(env.stderr_terminal, color_hint(&args));
            return fail(err, &e.into(), style);
        }
    };
    let mode = match invocation.value("--color") {
        None | Some("auto") => ColorMode::Auto,
        Some("always") => ColorMode::Always,
        Some("never") => ColorMode::Never,
        Some(other) => {
            let e = UsageError::of(
                spec,
                format!(
                    "`--color` takes auto, always, or never, and `{}` was given.",
                    text::escape(other)
                ),
            );
            let style = env.style(env.stderr_terminal, ColorMode::Auto);
            return fail(err, &e.into(), style);
        }
    };
    let styles = Styles {
        out: env.style(env.stdout_terminal, mode),
        err: env.style(env.stderr_terminal, mode),
    };
    // What the command line alone decides is a usage error before anything
    // is read or sent, and before the runtime starts.
    if let Err(e) = check(spec, &invocation, env.windows) {
        return fail(err, &e.into(), styles.err);
    }
    match spec.id {
        CommandId::Version => write_out(out, &format!("{VERSION}\n")),
        CommandId::Init | CommandId::Join => {
            match machine::run_setup(env, async |runtime, transport, host, data| {
                let ctx = Ctx {
                    runtime,
                    transport,
                    env,
                    styles,
                };
                let setup = setup::Setup {
                    ctx: &ctx,
                    host,
                    data,
                };
                let outcome = match spec.id {
                    CommandId::Init => match setup::init::check(spec, &invocation) {
                        Ok(args) => setup::init::init(&setup, spec, &args, out, err).await,
                        Err(e) => Err(e.into()),
                    },
                    _ => match setup::join::check(spec, &invocation, env.windows) {
                        Ok(args) => setup::join::join(&setup, spec, &args, out, err).await,
                        Err(e) => Err(e.into()),
                    },
                };
                match outcome {
                    Ok(()) => EXIT_SUCCESS,
                    Err(failure) => fail(err, &failure, styles.err),
                }
            }) {
                Ok(code) => code,
                Err(failure) => fail(err, &failure, styles.err),
            }
        }
        CommandId::Login | CommandId::NodeList => {
            match machine::run(env, async |runtime, transport| {
                let ctx = Ctx {
                    runtime,
                    transport,
                    env,
                    styles,
                };
                execute(&ctx, spec, &invocation, out, err).await
            }) {
                Ok(code) => code,
                Err(failure) => fail(err, &failure, styles.err),
            }
        }
    }
}

/// The `--color` of a command line that does not parse, so that a usage
/// error honors it too: the last `--color never` or `--color always`, in
/// either form, else `auto`.
fn color_hint(args: &[OsString]) -> ColorMode {
    let mut mode = ColorMode::Auto;
    let mut after_flag = false;
    for arg in args {
        let value = match arg.to_str() {
            Some("--") => break,
            Some("--color") => {
                after_flag = true;
                continue;
            }
            Some(text) if after_flag => Some(text),
            Some(text) => text.strip_prefix("--color="),
            None => None,
        };
        after_flag = false;
        match value {
            Some("never") => mode = ColorMode::Never,
            Some("always") => mode = ColorMode::Always,
            Some("auto") => mode = ColorMode::Auto,
            _ => {}
        }
    }
    mode
}

/// The checks of `spec` that need nothing but the command line.
fn check(spec: &CommandSpec, invocation: &Invocation, windows: bool) -> Result<(), UsageError> {
    match spec.id {
        CommandId::Version => Ok(()),
        CommandId::Init => setup::init::check(spec, invocation).map(|_| ()),
        CommandId::Join => setup::join::check(spec, invocation, windows).map(|_| ()),
        CommandId::Login => login::check(spec, invocation),
        CommandId::NodeList => node::check(spec, invocation).map(|_| ()),
    }
}

/// Runs a command that needs the runtime, and returns its exit code.
pub(crate) async fn execute(
    ctx: &Ctx<'_>,
    spec: &CommandSpec,
    invocation: &Invocation,
    out: &mut dyn Write,
    err: &mut dyn Write,
) -> u8 {
    let outcome = match spec.id {
        CommandId::Version | CommandId::Init | CommandId::Join => Ok(()),
        CommandId::Login => login::login(ctx, spec, invocation, out, err).await,
        CommandId::NodeList => node::list(ctx, spec, invocation, out).await,
    };
    match outcome {
        Ok(()) => EXIT_SUCCESS,
        Err(failure) => fail(err, &failure, ctx.styles.err),
    }
}

fn write_out(out: &mut dyn Write, text: &str) -> u8 {
    match out.write_all(text.as_bytes()).and_then(|()| out.flush()) {
        Ok(()) => EXIT_SUCCESS,
        Err(_) => EXIT_FAILURE,
    }
}

fn fail(err: &mut dyn Write, failure: &Failure, style: Style) -> u8 {
    // Nothing else can report a failed write to standard error.
    let _ = err.write_all(failure.render(style).as_bytes());
    let _ = err.flush();
    failure.code
}

/// Writes `text` to standard error. A failed write is not reported: nothing
/// else could report it.
pub(crate) fn note(err: &mut dyn Write, text: &str) {
    let _ = err.write_all(text.as_bytes());
    let _ = err.flush();
}

/// Writes `text` to standard output.
///
/// # Errors
///
/// A [`Failure`] with [`EXIT_FAILURE`] when the write fails.
pub(crate) fn emit(out: &mut dyn Write, text: &str) -> Result<(), Failure> {
    out.write_all(text.as_bytes())
        .and_then(|()| out.flush())
        .map_err(|e| {
            Failure::new(
                EXIT_FAILURE,
                format!("alloy could not write its result to standard output: {e}."),
            )
        })
}

#[cfg(test)]
mod tests;
