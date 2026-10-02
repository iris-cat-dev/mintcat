mod project;
mod stage;
mod surface;
use std::{future::Future, path::PathBuf};
use clap::{Parser, Subcommand};

#[derive(Parser)]
#[command(name = "mintcat-module", about = "Isolated signed module development and verified offline release packaging")]
struct Cli { #[command(subcommand)] command: Command }
#[derive(Subcommand)]
enum Command {
    New { directory: PathBuf, #[arg(long)] id: String, #[arg(long)] target: String },
    Check { directory: PathBuf },
    Pack { directory: PathBuf, #[arg(long)] key: PathBuf, #[arg(long)] output: PathBuf },
    Dev {
        directory: PathBuf,
        #[arg(long)] dev_root: PathBuf,
        #[arg(long)] key: PathBuf,
        /// Verified signed dependency packages; no unsigned directory overrides.
        #[arg(long, requires = "trust_root")] dependency: Vec<PathBuf>,
        #[arg(long)] trust_root: Option<PathBuf>,
        #[arg(long, default_value = "example")] method: String,
        /// Actual initial invocation parameters; the generated example accepts arbitrary JSON.
        #[arg(long)] params: Option<PathBuf>,
        /// Run one generation and a real invoke then framed drain; useful for CI/local smoke.
        #[arg(long)] once: bool,
    },
    #[command(subcommand)]
    Release(mintcat_release::Command),
    /// Verify every scope lock and byte of the complete offline baseline before staging binaries.
    Stage { #[arg(long)] manifest: PathBuf },
    /// Reauthenticate a pinned existing stage; refuses changed bytes, permissions or scope pins.
    VerifyStage {
        #[arg(long)] directory: PathBuf,
        #[arg(long)] target: String,
        #[arg(long)] variant: String,
        #[arg(long)] manifest_sha256: String,
    },
}
fn shutdown_signal() -> std::io::Result<impl Future<Output = std::io::Result<()>>> {
    #[cfg(unix)]
    {
        // Keep both subscriptions alive through synchronous generation builds and swaps.
        let mut interrupt = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::interrupt())?;
        let mut terminate = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())?;
        Ok(async move {
            let received = tokio::select! { event = interrupt.recv() => event, event = terminate.recv() => event };
            received.ok_or_else(|| std::io::Error::other("dev shutdown signal stream closed"))
        })
    }
    #[cfg(not(unix))]
    {
        Ok(tokio::signal::ctrl_c())
    }
}
#[tokio::main]
async fn main() {
    let result: Result<(), Box<dyn std::error::Error>> = match Cli::parse().command {
        Command::New { directory, id, target } => project::new(&directory, &id, &target),
        Command::Check { directory } => project::check(&directory).map(|_| ()),
        Command::Pack { directory, key, output } => project::pack(&directory, &key, &output),
        Command::Dev { directory, dev_root, key, dependency, trust_root, method, params, once } => project::dev(&directory, &dev_root, &key, &dependency, trust_root.as_deref(), &method, params.as_deref(), once).await,
        Command::Release(command) => mintcat_release::execute(command).map_err(Into::into),
        Command::Stage { manifest } => stage::stage(&manifest),
        Command::VerifyStage { directory, target, variant, manifest_sha256 } => stage::verify_stage(&directory, &target, &variant, &manifest_sha256),
    };
    if let Err(error) = result { eprintln!("{error}"); std::process::exit(1); }
}
