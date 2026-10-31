//! Formflux command-line entry point.

mod args;
mod commands;
mod output;

use std::process::ExitCode;

use clap::Parser;

use args::Cli;
use output::{clean_text, load_config, Context};

#[tokio::main]
async fn main() -> ExitCode {
    let cli = Cli::parse();
    let config = match load_config(cli.config.as_deref(), cli.jobs) {
        Ok(config) => config,
        Err(error) => {
            eprintln!("error: {error:#}");
            return ExitCode::FAILURE;
        }
    };
    if let Err(error) =
        formflux_shared::logging::init(&config.log_level, cli.verbose > 0, cli.trace)
    {
        eprintln!("warning: could not initialise logging: {error}");
    }
    let jobs = cli.jobs.unwrap_or_else(|| config.effective_jobs());
    let ctx = Context {
        json: cli.json,
        quiet: cli.quiet,
        dry_run: cli.dry_run,
        force: cli.force,
        yes: cli.yes,
        jobs,
    };
    match commands::run(cli.command, &config, &ctx).await {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            if ctx.json {
                let payload = serde_json::json!({
                    "error": clean_text(&error.to_string()),
                    "causes": error.chain().skip(1).map(|cause| clean_text(&cause.to_string())).collect::<Vec<_>>(),
                });
                eprintln!(
                    "{}",
                    serde_json::to_string_pretty(&payload).unwrap_or_default()
                );
            } else {
                eprintln!("error: {}", clean_text(&format!("{error:#}")));
            }
            ExitCode::FAILURE
        }
    }
}
