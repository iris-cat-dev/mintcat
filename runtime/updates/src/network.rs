use super::*;
use mintcat_runtime_broker::{NetworkBroker, NetworkRequest, Origin};
use mintcat_runtime_transfers::{TransferFileKey, TransferManager, TransferRequest, TransferSharing, TransferValidation};
use mintcat_source_api::DownloadDescriptor;
use mintcat_sdk::ModRef;
use std::fs::{self, File};
use url::Url;

impl UpdatesCoordinator {
    /// Fetches the metadata bundle through the approved broker. No direct reqwest/unsigned URL loader.
    pub async fn fetch_repository(&self, broker: &NetworkBroker, base: &Url, origins: Vec<Origin>) -> Result<RepositoryBundle> {
        let mut url = base.clone();
        url.path_segments_mut().map_err(|_| UpdateError::Blocked("repository URL cannot be a base".into()))?.pop_if_empty().push("current.json");
        let response = broker.execute(&NetworkRequest::get("mintcat.updates", url, origins), &mintcat_runtime_broker::CancellationToken::default()).await?;
        Ok(serde_json::from_slice(&response.body)?)
    }
    /// HTTP repository layout is identical to the local signed repository. Each payload file
    /// is a durable Transfer with its exact signed SHA-256 and length; no archive fallback.
    pub async fn stage_network(&self, bundle: RepositoryBundle, base: &Url, origins: Vec<Origin>, transfers: &TransferManager, scope: &str, channel: &str, expected_scope_revision: u64, now: u64) -> Result<UpdateOperation> {
        let verified = self.select(&bundle, scope, channel, now)?;
        let operation = self.begin(bundle, verified, expected_scope_revision, now)?;
        let stage = self.inner.layout.artifacts_dir.join("update-staging").join(operation.operation_id.replace(':', "-"));
        let result: Result<UpdateOperation> = async {
            self.phase(&operation.operation_id, UpdatePhase::Resolved)?;
            self.phase(&operation.operation_id, UpdatePhase::Downloading)?;
            fs::create_dir_all(&stage)?;
            let root = mintcat_runtime_release::trust_pinned_root(operation.generation.verified_root.clone(), now)?;
            let artifacts = self.generation_store(&operation.generation)?;
            for record in operation.generation.targets.signed.packages.values() {
                let package_dir = stage.join(&record.digest[7..]);
                fs::create_dir_all(package_dir.join("payload"))?;
                // Canonical metadata is already authenticated by the signed targets record.
                fs::write(package_dir.join("module.json"), serde_json::to_vec(&serde_json::to_value(&record.manifest)?)?)?;
                fs::write(package_dir.join("signature.json"), serde_json::to_vec(&serde_json::to_value(&record.signature)?)?)?;
                for file in &record.manifest.files {
                    let mut url = base.clone();
                    { let mut segments = url.path_segments_mut().map_err(|_| UpdateError::Blocked("repository URL cannot be a base".into()))?;
                      segments.pop_if_empty().push("artifacts").push("sha256").push(&record.digest[7..]).push("payload");
                      for part in file.path.split('/') { segments.push(part); }
                    }
                    let consumer = format!("{}:{}:{}", operation.operation_id, record.digest, file.path);
                    let request = TransferRequest {
                        file_key: TransferFileKey { source_id: "mintcat.signed-repository".into(), remote_game_key: scope.into(), remote_mod_id: record.manifest.id.clone(), resolved_version_id: record.digest.clone(), remote_file_id: file.path.clone(), byte_variant: file.sha256.clone() },
                        sharing: TransferSharing::StableFileIdentity,
                        descriptor: DownloadDescriptor { mod_ref: ModRef { source_id: "mintcat.signed-repository".into(), remote_game_key: scope.into(), remote_mod_id: record.manifest.id.clone(), version_id: Some(record.digest.clone()), remote_file_id: Some(file.path.clone()) }, url, allowed_origins: origins.clone(), headers: BTreeMap::new(), credential: None, expires_at: None, expected_size: file.size, expected_md5: None },
                        validation: TransferValidation { expected_sha256: Some(file.sha256.clone()), required_prefix: Vec::new(), media_type: None },
                    };
                    let transfer = transfers.attach(&consumer, &request)?;
                    transfers.run(&transfer.transfer_id, &request).await?;
                    let grant = transfers.artifact_for_consumer(&transfer.transfer_id, &consumer)?;
                    let destination = package_dir.join("payload").join(&file.path);
                    fs::create_dir_all(destination.parent().expect("signed file has parent"))?;
                    let mut input = grant.open_read()?;
                    let mut output = File::create(&destination)?;
                    std::io::copy(&mut input, &mut output)?;
                    #[cfg(unix)] { use std::os::unix::fs::PermissionsExt; fs::set_permissions(&destination, fs::Permissions::from_mode(if file.executable { 0o700 } else { 0o600 }))?; }
                    File::open(&destination)?.sync_all()?;
                    drop(grant);
                    transfers.detach_consumer(&transfer.transfer_id, &consumer)?;
                }
                require(mintcat_runtime_release::verify_package(&package_dir, &root, &self.inner.policy)? == *record, "downloaded package differs from signed exact closure")?;
                let installed = artifacts.publish_module(&package_dir.join("payload"), &record.manifest, &record.signature)?;
                require(installed.digest == record.digest, "installed package digest mismatch")?;
            }
            self.phase(&operation.operation_id, UpdatePhase::Staged)
        }.await;
        if let Err(error) = &result { self.reject(&operation.operation_id, error)?; }
        // Only this operation's transient tree; ArtifactStore remains execution/deletion authority.
        if stage.exists() { fs::remove_dir_all(stage)?; }
        result
    }
}
