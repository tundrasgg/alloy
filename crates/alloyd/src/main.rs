//! `alloyd`, the Alloy node daemon (ARFC 0000 section 2.3).
//!
//! Exit codes follow ARFC 0019 clause 3.1.7: 0 success, 1 failure, 2 usage
//! error.

#![forbid(unsafe_code)]

use std::io::Write;
use std::process::ExitCode;

use alloyd::{Command, DaemonError, HELP, LineLog, RunOptions, VERSION, default_data_dir};

const SUCCESS: u8 = 0;
const FAILURE: u8 = 1;
const USAGE: u8 = 2;

fn main() -> ExitCode {
    ExitCode::from(alloyd_main())
}

fn alloyd_main() -> u8 {
    let command = match alloyd::parse_args(std::env::args_os().skip(1)) {
        Ok(command) => command,
        Err(e) => {
            eprintln!("{e}");
            return USAGE;
        }
    };
    match command {
        Command::Version => print(&format!("{VERSION}\n")),
        Command::Help => print(HELP),
        Command::Run => match daemon() {
            Ok(()) => SUCCESS,
            Err(e) => {
                eprintln!("{e}");
                FAILURE
            }
        },
    }
}

/// Writes `text` to standard output: 0 when it was written, 1 when not, as on
/// a closed pipe.
fn print(text: &str) -> u8 {
    let mut out = std::io::stdout().lock();
    match out.write_all(text.as_bytes()).and_then(|()| out.flush()) {
        Ok(()) => SUCCESS,
        Err(_) => FAILURE,
    }
}

/// Runs the daemon: as the Windows service `alloyd` when the service control
/// manager started this process, otherwise until a stop signal.
fn daemon() -> Result<(), DaemonError> {
    // A second subscriber cannot have been set: this is the only call, and
    // nothing ran before it.
    let _ = tracing::subscriber::set_global_default(LineLog::new(std::io::stderr()));
    #[cfg(windows)]
    if alloyd::service::dispatch()? {
        return Ok(());
    }
    let program_data = std::env::var_os("ProgramData");
    let data_dir = default_data_dir(program_data.as_deref())?;
    alloyd::run(&RunOptions { data_dir }, alloyd::os_signals).map(|_| ())
}
