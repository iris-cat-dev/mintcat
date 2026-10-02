use std::{fs::OpenOptions, io::Write, path::PathBuf, time::{SystemTime, UNIX_EPOCH}};
use clap::Subcommand;
use ed25519_dalek::SigningKey;
use mintcat_runtime_release::{compose_bundle, encode_public_key, key_id, load_root_rotation_anchor, load_trust_root, pack, read_json, read_signing_key, sign, trust_pinned_root, verify_package, LocalRepository, ReleaseError, RepositoryBundle, RepositoryVerifier, Result, RootMetadata, ScopePlan, VerificationPolicy};
use mintcat_sdk::ModuleManifest;
use serde::Serialize;

#[derive(Subcommand)]
pub enum Command {
    /// Print the public identity of an explicitly provisioned 0600 base64 seed.
    KeyInfo { #[arg(long)] key: PathBuf },
    /// Sign a root spec. Rotation requires both previous and new root thresholds.
    SignRoot {
        #[arg(long)] spec: PathBuf,
        #[arg(long, required = true)] key: Vec<PathBuf>,
        #[arg(long)] previous_root: Option<PathBuf>,
        #[arg(long)] output: PathBuf,
    },
    Pack {
        #[arg(long)] source: PathBuf,
        #[arg(long)] manifest: PathBuf,
        #[arg(long)] policy: PathBuf,
        #[arg(long)] key: PathBuf,
        #[arg(long)] key_id: String,
        #[arg(long)] output: PathBuf,
    },
    Compose {
        #[arg(long)] root: PathBuf,
        #[arg(long)] policy: PathBuf,
        #[arg(long)] plan: PathBuf,
        #[arg(long, required = true)] package: Vec<PathBuf>,
        #[arg(long)] previous: Option<PathBuf>,
        /// Ordered sequential roots, each authorized by both old and new thresholds.
        #[arg(long)] root_rotation: Vec<PathBuf>,
        #[arg(long, required = true)] key: Vec<PathBuf>,
        #[arg(long)] metadata_version: u64,
        #[arg(long)] expires: u64,
        #[arg(long)] output: PathBuf,
    },
    Verify {
        #[arg(long)] root: PathBuf,
        #[arg(long)] policy: PathBuf,
        #[arg(long, conflicts_with = "bundle", required_unless_present = "bundle")] package: Option<PathBuf>,
        #[arg(long, conflicts_with = "package")] bundle: Option<PathBuf>,
        /// Required for metadata: protected persistent trust/rollback storage.
        #[arg(long, requires = "bundle")] state: Option<PathBuf>,
        #[arg(long, requires_all = ["bundle", "channel"])] scope: Option<String>,
        #[arg(long, requires = "scope")] channel: Option<String>,
    },
    Publish {
        #[arg(long)] repository: PathBuf,
        #[arg(long)] root: PathBuf,
        #[arg(long)] policy: PathBuf,
        #[arg(long)] package: Vec<PathBuf>,
        #[arg(long)] bundle: Option<PathBuf>,
    },
}
fn now() -> Result<u64> {
    SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_secs()).map_err(|e| ReleaseError::Rejected(e.to_string()))
}
fn keys(paths: &[PathBuf]) -> Result<Vec<SigningKey>> { paths.iter().map(|p| read_signing_key(p)).collect() }
fn print<T: Serialize>(value: &T) -> Result<()> { println!("{}", serde_json::to_string_pretty(value)?); Ok(()) }
fn write_new<T: Serialize>(path: &PathBuf, value: &T) -> Result<()> {
    let bytes = serde_json::to_vec(&serde_json::to_value(value)?)?;
    let mut options = OpenOptions::new();
    options.create_new(true).write(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o644);
    }
    let mut file = options.open(path)?;
    file.write_all(&bytes)?;
    file.sync_all()?;
    Ok(())
}
pub fn execute(command: Command) -> Result<()> {
    let time = now()?;
    match command {
        Command::KeyInfo { key } => {
            let key = read_signing_key(&key)?.verifying_key();
            print(&serde_json::json!({"keyId": key_id(&key), "publicKey": encode_public_key(&key)}))
        }
        Command::SignRoot { spec, key, previous_root, output } => {
            let spec: RootMetadata = read_json(&spec)?;
            let signed = sign(spec, &keys(&key)?)?;
            if let Some(previous) = previous_root { load_root_rotation_anchor(&previous)?.rotate(signed.clone(), time)?; }
            else { trust_pinned_root(signed.clone(), time)?; }
            write_new(&output, &signed)?;
            print(&signed)
        }
        Command::Pack { source, manifest, policy, key, key_id, output } => {
            let manifest: ModuleManifest = read_json(&manifest)?;
            let policy: VerificationPolicy = read_json(&policy)?;
            print(&pack(&source, manifest, &key_id, &read_signing_key(&key)?, &policy, &output)?)
        }
        Command::Compose { root, policy, plan, package, previous, root_rotation, key, metadata_version, expires, output } => {
            let mut root = if root_rotation.is_empty() { load_trust_root(&root, time)? } else { load_root_rotation_anchor(&root)? };
            let mut rotations = Vec::new();
            for path in root_rotation {
                let rotation: mintcat_runtime_release::Signed<RootMetadata> = read_json(&path)?;
                root = root.rotate(rotation.clone(), time)?;
                rotations.push(rotation);
            }
            let policy: VerificationPolicy = read_json(&policy)?;
            let previous: Option<RepositoryBundle> = previous.as_ref().map(|path| read_json(path)).transpose()?;
            let packages = package.iter().map(|p| verify_package(p, &root, &policy)).collect::<Result<Vec<_>>>()?;
            let mut bundle = compose_bundle(previous.as_ref(), read_json::<ScopePlan>(&plan)?, packages, &policy, &root, &keys(&key)?, metadata_version, expires, time)?;
            bundle.root_rotations.extend(rotations);
            write_new(&output, &bundle)?;
            print(&bundle)
        }
        Command::Verify { root, policy, package, bundle, state, scope, channel } => {
            let policy: VerificationPolicy = read_json(&policy)?;
            if let Some(package) = package {
                let root = load_trust_root(&root, time)?;
                print(&verify_package(&package, &root, &policy)?)
            }
            else {
                let state = state.ok_or_else(|| ReleaseError::Rejected("metadata verification requires --state; ephemeral verification would permit rollback".into()))?;
                let bundle: RepositoryBundle = read_json(&bundle.expect("clap enforces one input"))?;
                let verifier = RepositoryVerifier::from_pinned_file(state, &root)?;
                if let Some(scope) = scope {
                    let verified = verifier.verify_scope(&bundle, &scope, &channel.expect("clap requires channel"), &policy, time)?;
                    print(verified.release())
                } else {
                    let verified = verifier.verify(&bundle, &policy, time)?;
                    print(&verified.iter().map(|scope| scope.release()).collect::<Vec<_>>())
                }
            }
        }
        Command::Publish { repository, root, policy, package, bundle } => {
            let policy: VerificationPolicy = read_json(&policy)?;
            let repository = LocalRepository::open(&repository)?;
            if let Some(bundle) = bundle {
                let root = load_root_rotation_anchor(&root)?;
                let bundle: RepositoryBundle = read_json(&bundle)?;
                let verified = repository.publish_bundle(&bundle, &package, &root, &policy, time)?;
                print(&verified.iter().map(|scope| scope.release()).collect::<Vec<_>>())
            } else {
                let root = load_trust_root(&root, time)?;
                if package.is_empty() { return Err(ReleaseError::Rejected("publish requires --package or --bundle".into())); }
                let published = package.iter().map(|path| repository.publish_package(path, &root, &policy)).collect::<Result<Vec<_>>>()?;
                print(&published)
            }
        }
    }
}
