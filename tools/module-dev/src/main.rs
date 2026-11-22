mod project;
mod stage;
use std::path::PathBuf;
use clap::{Parser, Subcommand};

#[derive(Parser)]
#[command(name = "mintcat-module", about = "Signed game package development and verified native app assembly")]
struct Cli { #[command(subcommand)] command: Command }
#[derive(Subcommand)]
enum Command {
    /// Build and validate only this game's worker and bundled resources.
    Check { directory: PathBuf, #[arg(long)] target: Option<String> },
    /// Build and sign a game package with an explicitly provisioned publisher key.
    Pack {
        directory: PathBuf,
        #[arg(long)] target: Option<String>,
        #[arg(long)] key: PathBuf,
        #[arg(long)] output: PathBuf,
        /// Supply the project's declared patch ZIP without modifying its payload directory.
        #[arg(long)] patch_archive: Option<PathBuf>,
    },
    #[command(subcommand)]
    Release(mintcat_release::Command),
    /// Authenticate configured game packages and pin all app binaries/resources in a fresh stage.
    Stage { #[arg(long)] manifest: PathBuf },
    /// Reauthenticate a pinned stage, including exact bytes, permissions and game scope pins.
    VerifyStage {
        #[arg(long)] directory: PathBuf,
        #[arg(long)] target: String,
        #[arg(long)] variant: String,
        #[arg(long)] manifest_sha256: String,
    },
}
fn main() {
    let result: Result<(), Box<dyn std::error::Error>> = match Cli::parse().command {
        Command::Check { directory, target } => project::check(&directory, target.as_deref()),
        Command::Pack { directory, target, key, output, patch_archive } => project::pack(&directory, target.as_deref(), &key, &output, patch_archive.as_deref()),
        Command::Release(command) => mintcat_release::execute(command).map_err(Into::into),
        Command::Stage { manifest } => stage::stage(&manifest),
        Command::VerifyStage { directory, target, variant, manifest_sha256 } => stage::verify_stage(&directory, &target, &variant, &manifest_sha256),
    };
    if let Err(error) = result { eprintln!("{error}"); std::process::exit(1); }
}
