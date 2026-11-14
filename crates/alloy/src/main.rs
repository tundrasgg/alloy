//! `alloy`, the Alloy command line interface (ARFC 0019 section 3.1).

#![forbid(unsafe_code)]

use std::io::IsTerminal;
use std::process::ExitCode;

use alloy::{ADMIN_SOCKET, ENV_VARS, Env};
use alloy_rt::{StreamAddr, UnixPath};

fn main() -> ExitCode {
    let stdout = std::io::stdout();
    let stderr = std::io::stderr();
    let vars = ENV_VARS
        .iter()
        .filter_map(|&name| std::env::var_os(name).map(|value| (name.to_owned(), value)))
        .collect();
    let admin_socket = if cfg!(unix) {
        UnixPath::new(ADMIN_SOCKET).ok().map(StreamAddr::Unix)
    } else {
        None
    };
    let env = Env {
        stdout_terminal: stdout.is_terminal(),
        stderr_terminal: stderr.is_terminal(),
        windows: cfg!(windows),
        vars,
        admin_socket,
    };
    let code = alloy::run(
        std::env::args_os().skip(1),
        &env,
        &mut stdout.lock(),
        &mut stderr.lock(),
    );
    ExitCode::from(code)
}
