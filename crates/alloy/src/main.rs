//! `alloy`, the Alloy command line interface (ARFC 0019 section 3.1).

#![forbid(unsafe_code)]

use std::io::IsTerminal;
use std::process::ExitCode;

use alloy::Style;

fn main() -> ExitCode {
    let stderr = std::io::stderr();
    let style = Style::of_stream(
        stderr.is_terminal(),
        std::env::var_os("NO_COLOR").as_deref(),
        std::env::var_os("TERM").as_deref(),
        cfg!(windows),
    );
    let code = alloy::run(
        std::env::args_os().skip(1),
        &mut std::io::stdout().lock(),
        &mut stderr.lock(),
        style,
    );
    ExitCode::from(code)
}
