//! Structured logging to stderr.  stdout is reserved for command output, so
//! pipelines and `--json` consumers never see log noise.

use std::io::IsTerminal;
use std::sync::Once;

use tracing_subscriber::EnvFilter;

use crate::error::Result;

static INIT: Once = Once::new();

/// Initialise the global tracing subscriber exactly once.
///
/// The filter resolves in this order: `FORMFLUX_LOG`, `RUST_LOG`, explicit
/// `trace`, explicit `verbose`, then `default_level`.  Valid levels are
/// `error`, `warn`, `info`, `debug` and `trace`; an invalid default falls back
/// to `info`.
pub fn init(default_level: &str, verbose: bool, trace: bool) -> Result<()> {
    INIT.call_once(|| {
        let level = if trace {
            "trace"
        } else if verbose {
            "debug"
        } else if is_valid_level(default_level) {
            default_level
        } else {
            "info"
        };
        let filter = EnvFilter::try_from_env("FORMFLUX_LOG")
            .or_else(|_| EnvFilter::try_from_default_env())
            .unwrap_or_else(|_| EnvFilter::new(level));
        let _ = tracing_subscriber::fmt()
            .with_env_filter(filter)
            .with_writer(std::io::stderr)
            .with_ansi(std::io::stderr().is_terminal())
            .with_target(false)
            .with_thread_ids(false)
            .with_thread_names(false)
            .compact()
            .try_init();
    });
    Ok(())
}

/// True when `level` is a tracing level name.
pub fn is_valid_level(level: &str) -> bool {
    matches!(
        level.to_ascii_lowercase().as_str(),
        "error" | "warn" | "info" | "debug" | "trace"
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn level_validation() {
        assert!(is_valid_level("INFO"));
        assert!(is_valid_level("trace"));
        assert!(!is_valid_level("loud"));
    }

    #[test]
    fn init_is_idempotent() {
        init("warn", false, false).unwrap();
        init("debug", true, false).unwrap();
    }
}
