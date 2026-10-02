use std::{
    io::{self, Write},
    path::PathBuf,
    process::ExitCode,
};

use anyhow::{Context, Result};
use clap::{Parser, Subcommand, ValueEnum};
use mintcat_daemon::BoundDaemon;
use mintcat_runtime_kernel::{RuntimeEnvironment, RuntimePaths, DATA_DIR_ENV};

#[derive(Debug, Parser)]
#[command(name = "mintcatd", version, about = "MintCat vNext standalone daemon")]
struct Cli {
    /// Explicit vNext data directory. The legacy desktop database is never used implicitly.
    #[arg(long, env = DATA_DIR_ENV)]
    data_dir: PathBuf,

    /// Emit machine-readable JSON.
    #[arg(long, global = true)]
    json: bool,

    /// Runtime trust and isolation environment.
    #[arg(long, global = true, value_enum, default_value_t = EnvironmentArg::Development)]
    environment: EnvironmentArg,

    #[command(subcommand)]
    command: Command,
}

#[derive(Debug, Clone, Copy, ValueEnum)]
enum EnvironmentArg {
    Development,
    Test,
    Production,
}

#[derive(Debug, Subcommand)]
enum Command {
    /// Initialize and validate the isolated runtime layout, then exit.
    Init,
    /// Print the resolved runtime paths without mutating the filesystem.
    Paths,
    /// Serve authenticated local IPC until an operating-system shutdown signal.
    Serve,
}

#[tokio::main]
async fn main() -> ExitCode {
    match run(Cli::parse()).await {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("mintcatd: {error:#}");
            ExitCode::from(1)
        }
    }
}

async fn run(cli: Cli) -> Result<()> {
    let paths = RuntimePaths::new(cli.data_dir).context("invalid runtime data directory")?;
    match cli.command {
        Command::Init => {
            let layout = paths
                .initialize(cli.environment.into())
                .context("could not initialize isolated vNext runtime")?;
            print_value(&layout, cli.json)?;
        }
        Command::Paths => print_value(&paths.layout(), cli.json)?,
        Command::Serve => {
            let daemon = BoundDaemon::bind(&paths, cli.environment.into())
                .context("could not bind standalone daemon")?;
            let ready = daemon.ready();
            print_value(&ready, cli.json)?;
            io::stdout()
                .flush()
                .context("could not flush readiness output")?;
            eprintln!(
                "mintcatd: ready generation={} endpoint={}",
                ready.generation_id,
                ready.endpoint.display()
            );
            daemon
                .run(shutdown_signal())
                .await
                .context("daemon runtime failed")?;
        }
    }
    Ok(())
}

fn print_value(value: &impl serde::Serialize, json: bool) -> Result<()> {
    if json {
        println!(
            "{}",
            serde_json::to_string(value).context("could not encode daemon output")?
        );
    } else {
        println!(
            "{}",
            serde_json::to_string_pretty(value).context("could not encode daemon output")?
        );
    }
    Ok(())
}

async fn shutdown_signal() {
    #[cfg(unix)]
    {
        let mut terminate =
            tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
                .expect("SIGTERM handler must install");
        tokio::select! {
            result = tokio::signal::ctrl_c() => {
                if let Err(error) = result {
                    eprintln!("mintcatd: Ctrl-C handler failed: {error}");
                }
            }
            _ = terminate.recv() => {}
        }
    }
    #[cfg(not(unix))]
    if let Err(error) = tokio::signal::ctrl_c().await {
        eprintln!("mintcatd: Ctrl-C handler failed: {error}");
    }
}

impl From<EnvironmentArg> for RuntimeEnvironment {
    fn from(value: EnvironmentArg) -> Self {
        match value {
            EnvironmentArg::Development => Self::Development,
            EnvironmentArg::Test => Self::Test,
            EnvironmentArg::Production => Self::Production,
        }
    }
}
