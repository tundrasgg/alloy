//! A binary crate: `main` returns its exit code, which both Clippy steps
//! accept.

use std::process::ExitCode;

fn main() -> ExitCode {
    let usage_error: u8 = 2;
    if usage_error == 0 {
        ExitCode::SUCCESS
    } else {
        ExitCode::from(usage_error)
    }
}
