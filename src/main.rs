//! `ccm` binary entry point.
//!
//! Deliberately thin: parse arguments, run the pipeline, map any error to its
//! documented exit code (see `error.rs`), and exit. The one subtlety here is the
//! explicit stdout flush below — `std::process::exit` skips destructors, so without it
//! a `--dry-run` message written to a piped/captured stdout could be lost.

use ccm::cli::Cli;
use ccm::env::RealEnvironment;
use ccm::error::ExitCode;
use ccm::pipeline;
use std::io::Write as _;
use std::process::ExitCode as ProcessExitCode;

fn main() -> ProcessExitCode {
    let cli = Cli::parse_args();
    let env = RealEnvironment;
    let mut stdin = std::io::stdin().lock();
    let mut stdout = std::io::stdout();
    let mut stderr = std::io::stderr();

    let code = match pipeline::run(&cli, &env, &mut stdin, &mut stdout, &mut stderr) {
        Ok(()) => ExitCode::SUCCESS,
        Err(err) => {
            eprintln!("ccm: {err}");
            err.exit_code()
        }
    };
    // `main`'s own stdout writes (if any) are line-buffered by default; flush
    // explicitly so nothing is lost when stdout is piped/captured, since the process
    // exit path below does not run destructors.
    let _ = stdout.flush();
    ProcessExitCode::from(code.as_u8())
}
