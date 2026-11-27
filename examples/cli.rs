//! A command-line tool: it prints its results on standard output and its
//! log lines on standard error, so the results can be piped or saved
//! without log lines mixed in. It runs for a moment and exits, and on the
//! way out oxtel sends whatever is still pending, when the tool fails too.
//!
//! ```bash
//! cargo run -p oxtel-examples --example cli                  # the primes up to 100
//! cargo run -p oxtel-examples --example cli -- 20 2>/dev/null   # the results alone
//! cargo run -p oxtel-examples --example cli -- twenty        # fails, and says why
//! ```
//!
//! A tool of your own needs:
//!
//! ```toml
//! [dependencies]
//! oxtel = "0.4"
//! tracing = "0.1"
//! ```

use std::error::Error;
use std::io::Write;
use std::process::ExitCode;

fn main() -> Result<ExitCode, oxtel::Error> {
    // First in main, so it's dropped last, after everything that logs.
    // Log lines go to standard error.
    let _telemetry = oxtel::init()?;

    let up_to = std::env::args().nth(1).unwrap_or_else(|| "100".into());
    let code = match primes(&up_to) {
        Ok(_) => ExitCode::SUCCESS,
        // `primes` has already logged why, inside its span.
        Err(_) => ExitCode::FAILURE,
    };
    // Either way, main returns, which drops the guard, which sends what's
    // still pending. `std::process::exit` would skip that.
    Ok(code)
}

/// Writes the primes up to `up_to` on standard output, one per line, and
/// returns how many there were. The run is one span: its log lines carry
/// `up_to` and the run's trace id, and `err` logs a failure inside it.
#[tracing::instrument(err)]
fn primes(up_to: &str) -> Result<u64, Box<dyn Error>> {
    let up_to: u64 = up_to
        .parse()
        .map_err(|_| format!("{up_to} isn't a whole number"))?;
    let mut out = std::io::stdout().lock();
    let mut found = 0;
    for n in 2..=up_to {
        if (2..).take_while(|d| d * d <= n).all(|d| n % d != 0) {
            writeln!(out, "{n}")?;
            found += 1;
        }
    }
    tracing::info!(found, "done");
    Ok(found)
}
