use mintcat_sdk::{ArtifactRef, ModRef};
use rusqlite::{params, OptionalExtension, TransactionBehavior};
use serde::{Deserialize, Serialize};
use uuid::Uuid;
use crate::{store::OwnerStore, LibraryAuthorization, LibraryError, CONTENT_WRITE_PERMISSION};
use crate::library_api::{authorize_profile, bump_content_revision, ensure_content_available, ensure_revision, hash_json, insert_application_event, load_profile, now_ms, record_mutation, replay_mutation};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct SourceAddRequest {
    pub profile_id: String,
    pub reference: ModRef,
    pub expected_content_revision: u64,
}
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct SourceAddTarget {
    pub profile_id: String,
    pub game_id: String,
    pub incarnation_id: String,
    pub content_revision: u64,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct SourceAddVersion {
    pub version_id: String,
    pub remote_file_id: Option<String>,
    pub file_size: u64,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct SourceAddMetadata {
    pub display_name: String,
    pub profile_url: Option<String>,
    pub tags: Vec<String>,
    pub versions: Vec<SourceAddVersion>,
    pub selected_version_id: Option<String>,
}
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct SourceAddResult {
    pub operation_id: String,
    pub profile_id: String,
    pub profile_mod_id: String,
    pub mod_id: String,
    pub content_revision: u64,
    pub changed: bool,
}

impl OwnerStore {
    pub fn source_add_result(&self, authorization:&LibraryAuthorization, idempotency_key:&str, request:&SourceAddRequest, local:bool)->Result<Option<SourceAddResult>,LibraryError> {
        authorization.require(CONTENT_WRITE_PERMISSION)?;
        let inputs_hash=hash_json(&serde_json::to_value(request)?)?;
        let mut connection=self.connection()?;
        let transaction=connection.transaction_with_behavior(TransactionBehavior::Deferred)?;
        let result=replay_mutation(&transaction,authorization,if local {"mods.importLocal"} else {"mods.add"},idempotency_key,&inputs_hash)?;
        transaction.commit()?;
        Ok(result)
    }

    pub fn source_add_target(&self, authorization: &LibraryAuthorization, request: &SourceAddRequest) -> Result<SourceAddTarget, LibraryError> {
        authorization.require(CONTENT_WRITE_PERMISSION)?;
        validate_request(request)?;
        let mut connection = self.connection()?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Deferred)?;
        let profile = load_profile(&transaction, &request.profile_id)?;
        authorize_profile(authorization, &profile)?;
        ensure_revision(request.expected_content_revision, profile.content_revision)?;
        ensure_content_available(&transaction, &request.profile_id)?;
        Ok(SourceAddTarget { profile_id: profile.profile_id, game_id: profile.game_id, incarnation_id: profile.incarnation_id, content_revision: profile.content_revision })
    }

    pub fn commit_source_add(&self, authorization: &LibraryAuthorization, idempotency_key: &str, request: &SourceAddRequest, target: &SourceAddTarget, metadata: &SourceAddMetadata) -> Result<SourceAddResult, LibraryError> {
        if request.reference.source_id == "local" { return Err(LibraryError::InvalidRecord("local addition requires a verified input grant".into())); }
        self.commit_source_add_inner(authorization, idempotency_key, request, target, metadata, None, None)
    }

    // The native facade authenticates the grant and constructs this ArtifactRef
    // from verified bytes, never from the caller's metadata.
    pub fn commit_local_source_add(&self, authorization: &LibraryAuthorization, idempotency_key: &str, request: &SourceAddRequest, target: &SourceAddTarget, metadata: &SourceAddMetadata, artifact: &ArtifactRef, owner_generation: &str) -> Result<SourceAddResult, LibraryError> {
        if !artifact.verified || owner_generation.trim().is_empty() || request.reference.source_id != "local" || request.reference.remote_mod_id != artifact.digest {
            return Err(LibraryError::InvalidRecord("local source identity must equal its verified snapshot digest".into()));
        }
        self.commit_source_add_inner(authorization, idempotency_key, request, target, metadata, Some(artifact), Some(owner_generation))
    }

    fn commit_source_add_inner(&self, authorization: &LibraryAuthorization, idempotency_key: &str, request: &SourceAddRequest, target: &SourceAddTarget, metadata: &SourceAddMetadata, artifact: Option<&ArtifactRef>, owner_generation: Option<&str>) -> Result<SourceAddResult, LibraryError> {
        authorization.require(CONTENT_WRITE_PERMISSION)?;
        validate_request(request)?;
        if metadata.display_name.trim().is_empty() || metadata.display_name.len() > 512 {
            return Err(LibraryError::InvalidRecord("source display name is invalid".into()));
        }
        let scope = if artifact.is_some() { "mods.importLocal" } else { "mods.add" };
        let inputs_hash = hash_json(&serde_json::to_value(request)?)?;
        let now = now_ms()?;
        let mut connection = self.connection()?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        if let Some(result) = replay_mutation(&transaction, authorization, scope, idempotency_key, &inputs_hash)? {
            transaction.commit()?;
            return Ok(result);
        }
        let profile = load_profile(&transaction, &request.profile_id)?;
        authorize_profile(authorization, &profile)?;
        if target.profile_id != profile.profile_id || target.game_id != profile.game_id || target.incarnation_id != profile.incarnation_id {
            return Err(LibraryError::InvalidRecord("source addition target incarnation or game changed".into()));
        }
        ensure_revision(request.expected_content_revision, profile.content_revision)?;
        ensure_revision(target.content_revision, profile.content_revision)?;
        ensure_content_available(&transaction, &request.profile_id)?;
        let reference = &request.reference;
        let existing = transaction.query_row("SELECT mod_id, game_id FROM library_mods WHERE source_id=?1 AND remote_game_key=?2 AND remote_mod_id=?3", params![reference.source_id, reference.remote_game_key, reference.remote_mod_id], |row| Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))).optional()?;
        let mod_id = if let Some((id, game_id)) = existing {
            if game_id != profile.game_id { return Err(LibraryError::InvalidRecord("source identity belongs to another game".into())); }
            id
        } else {
            let id = Uuid::new_v4().to_string();
            transaction.execute("INSERT INTO library_mods(mod_id,game_id,source_id,remote_game_key,remote_mod_id,name_id,display_name,original_name,url,tags_json,approval_status,last_update_at_ms,online_update_at_ms,online_available,local_missing,created_at_ms,updated_at_ms,revision,incarnation_id) VALUES(?1,?2,?3,?4,?5,?5,?6,?6,?7,?8,'unavailable',0,0,?9,?10,?11,?11,1,?12)", params![id, profile.game_id, reference.source_id, reference.remote_game_key, reference.remote_mod_id, metadata.display_name.trim(), metadata.profile_url.as_deref().unwrap_or(""), serde_json::to_string(&metadata.tags)?, artifact.is_none(), artifact.is_none(), now, Uuid::new_v4().to_string()])?;
            id
        };
        let mut version_ids = std::collections::HashSet::new();
        for version in &metadata.versions {
            if version.version_id.trim().is_empty() || !version_ids.insert(&version.version_id) { return Err(LibraryError::InvalidRecord("source returned invalid or duplicate versions".into())); }
            transaction.execute("INSERT INTO library_mod_versions(mod_id,version_id,remote_file_id,file_size,availability,current,created_at_ms,updated_at_ms) VALUES(?1,?2,?3,?4,?5,0,?6,?6) ON CONFLICT(mod_id,version_id) DO NOTHING", params![mod_id,version.version_id,version.remote_file_id,version.file_size,if version.remote_file_id.is_some() { "remote" } else { "metadata-only" },now])?;
        }
        if metadata.selected_version_id.as_ref().is_some_and(|id| !version_ids.contains(id)) { return Err(LibraryError::InvalidRecord("selected source version is absent".into())); }
        let existing_profile_mod = transaction.query_row("SELECT profile_mod_id FROM library_profile_mods WHERE profile_id=?1 AND mod_id=?2",params![profile.profile_id,mod_id],|row|row.get::<_,String>(0)).optional()?;
        let new_profile_mod = existing_profile_mod.is_none();
        let profile_mod_id = existing_profile_mod.unwrap_or_else(|| Uuid::new_v4().to_string());
        let binding_changed = if let Some(artifact) = artifact {
            !transaction.query_row("SELECT EXISTS(SELECT 1 FROM library_profile_content_bindings b JOIN library_profile_mods p ON p.profile_mod_id=b.profile_mod_id WHERE b.profile_mod_id=?1 AND b.artifact_digest=?2 AND b.version_id IS ?3 AND p.used_version IS ?3)",params![profile_mod_id,artifact.digest,metadata.selected_version_id],|row|row.get::<_,bool>(0))?
        } else { false };
        let changed = new_profile_mod || binding_changed;
        if new_profile_mod {
            transaction.execute("INSERT INTO library_profile_mods(profile_mod_id,profile_id,mod_id,incarnation_id,sort_order,enabled,created_at_ms,updated_at_ms,version_selection_kind,selected_version_id) VALUES(?1,?2,?3,?4,(SELECT COALESCE(MAX(sort_order)+1,0) FROM library_profile_mods WHERE profile_id=?2 AND group_id IS NULL),1,?5,?5,?6,?7)",params![profile_mod_id,profile.profile_id,mod_id,Uuid::new_v4().to_string(),now,if reference.version_id.is_some() {"pinned"} else {"auto"}, metadata.selected_version_id])?;
        }
        let operation_id = Uuid::new_v4().to_string();
        if let Some(artifact) = artifact {
            let version = metadata.selected_version_id.as_ref().ok_or_else(|| LibraryError::InvalidRecord("local snapshot must select a version".into()))?;
            if artifact.digest != reference.remote_mod_id || artifact.size != metadata.versions.iter().find(|v| &v.version_id == version).map(|v|v.file_size).unwrap_or(u64::MAX) { return Err(LibraryError::InvalidRecord("local snapshot content metadata mismatch".into())); }
            if binding_changed {
            // Immediate transaction holds the content fence throughout add+bind.
            transaction.execute("UPDATE library_profiles SET lease_epoch=lease_epoch+1 WHERE profile_id=?1",params![profile.profile_id])?;
            let epoch: u64 = transaction.query_row("SELECT lease_epoch FROM library_profiles WHERE profile_id=?1",params![profile.profile_id],|row|row.get(0))?;
            let generation = owner_generation.ok_or_else(|| LibraryError::InvalidRecord("local import generation missing".into()))?;
            transaction.execute("INSERT INTO library_download_operations(operation_id,owner_scope,idempotency_scope,idempotency_key,inputs_hash,profile_id,mode,expected_content_revision,target_incarnation_id,owner_generation,fence_epoch,state,created_at_ms,updated_at_ms,completed_at_ms) VALUES(?1,?2,?3,?4,?5,?6,'prepare-and-apply',?7,?8,?11,?9,'succeeded',?10,?10,?10)",params![operation_id,authorization.owner_scope(),scope,idempotency_key,inputs_hash,profile.profile_id,profile.content_revision,profile.incarnation_id,epoch,now,generation])?;
            transaction.execute("INSERT INTO library_operation_leases(resource_key,operation_id,fence_epoch,owner_generation,state,acquired_at_ms,heartbeat_at_ms,released_at_ms) VALUES(?1,?2,?3,?5,'released',?4,?4,?4) ON CONFLICT(resource_key) DO UPDATE SET operation_id=excluded.operation_id,fence_epoch=excluded.fence_epoch,owner_generation=excluded.owner_generation,state='released',acquired_at_ms=excluded.acquired_at_ms,heartbeat_at_ms=excluded.heartbeat_at_ms,released_at_ms=excluded.released_at_ms",params![format!("profile-content:{}",profile.profile_id),operation_id,epoch,now,generation])?;
            transaction.execute("INSERT INTO library_content_refs(digest,size,verified,grant_id,created_at_ms) VALUES(?1,?2,1,?1,?3) ON CONFLICT(digest) DO NOTHING",params![artifact.digest,artifact.size,now])?;
            transaction.execute("INSERT INTO library_profile_content_bindings(profile_mod_id,version_id,artifact_digest,operation_id,updated_at_ms) VALUES(?1,?2,?3,?4,?5) ON CONFLICT(profile_mod_id) DO UPDATE SET version_id=excluded.version_id,artifact_digest=excluded.artifact_digest,operation_id=excluded.operation_id,updated_at_ms=excluded.updated_at_ms",params![profile_mod_id,version,artifact.digest,operation_id,now])?;
            transaction.execute("UPDATE library_profile_mods SET used_version=?2,version_selection_kind='pinned',selected_version_id=?2,updated_at_ms=?3 WHERE profile_mod_id=?1",params![profile_mod_id,version,now])?;
            }
        }
        let content_revision = if changed { bump_content_revision(&transaction,&profile.profile_id,now)? } else { profile.content_revision };
        let result = SourceAddResult { operation_id: operation_id.clone(), profile_id:profile.profile_id, profile_mod_id, mod_id, content_revision, changed };
        record_mutation(&transaction,authorization,scope,idempotency_key,&inputs_hash,&operation_id,&result,now)?;
        if changed { insert_application_event(&transaction,"mod.added",&serde_json::to_value(&result)?,now)?; }
        transaction.commit()?;
        Ok(result)
    }
}
fn validate_request(request: &SourceAddRequest) -> Result<(), LibraryError> {
    if [&request.profile_id,&request.reference.source_id,&request.reference.remote_game_key,&request.reference.remote_mod_id].iter().any(|field| field.trim().is_empty()) { return Err(LibraryError::InvalidRecord("source addition identity cannot be empty".into())); }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use mintcat_runtime_kernel::{RuntimeEnvironment, RuntimePaths};

    fn fixture() -> (tempfile::TempDir, OwnerStore, LibraryAuthorization, SourceAddRequest) {
        let root = tempfile::tempdir().unwrap();
        let layout = RuntimePaths::new(root.path().join("data")).unwrap().initialize(RuntimeEnvironment::Test).unwrap();
        let store = OwnerStore::open(&layout).unwrap();
        store.connection().unwrap().execute_batch(
            "INSERT INTO library_games VALUES('game','game','Game','',1,0,0);
             INSERT INTO library_profiles(profile_id,game_id,owner_scope,name,display_name,active,description,revision,content_revision,incarnation_id,last_used_at_ms,created_at_ms,updated_at_ms)
             VALUES('profile','game','owner','default','Default',1,'',1,0,'incarnation',0,0,0);"
        ).unwrap();
        let auth = LibraryAuthorization::new("owner", [CONTENT_WRITE_PERMISSION]);
        let request = SourceAddRequest { profile_id:"profile".into(), reference:ModRef {source_id:"modio".into(),remote_game_key:"signed-key".into(),remote_mod_id:"42".into(),version_id:None,remote_file_id:None},expected_content_revision:0 };
        (root, store, auth, request)
    }
    fn metadata() -> SourceAddMetadata {
        SourceAddMetadata { display_name:"Remote mod".into(),profile_url:None,tags:vec![],versions:vec![SourceAddVersion {version_id:"v1".into(),remote_file_id:Some("file1".into()),file_size:42}],selected_version_id:Some("v1".into()) }
    }

    #[test]
    fn remote_addition_is_idempotent_and_never_binds_unverified_content() {
        let (_root, store, auth, mut request) = fixture();
        let target = store.source_add_target(&auth,&request).unwrap();
        let result = store.commit_source_add(&auth,"same",&request,&target,&metadata()).unwrap();
        assert!(result.changed);
        assert_eq!(result.content_revision,1);
        let after = store.snapshot().unwrap();
        assert_eq!(store.commit_source_add(&auth,"same",&request,&target,&metadata()).unwrap(),result);
        assert_eq!(store.snapshot().unwrap(),after);
        assert!(after.content_refs.is_empty());
        assert!(after.profile_content_bindings.is_empty());
        assert_eq!(after.versions[0].availability,"remote");
        assert_eq!(after.versions[0].artifact_digest,None);
        request.expected_content_revision=1;
        let target = store.source_add_target(&auth,&request).unwrap();
        let duplicate = store.commit_source_add(&auth,"different",&request,&target,&metadata()).unwrap();
        assert!(!duplicate.changed);
        assert_eq!(duplicate.profile_mod_id,result.profile_mod_id);
        assert_eq!(duplicate.content_revision,1);
    }

    #[test]
    fn stale_incarnation_wrong_game_and_active_guard_roll_back_every_source_row() {
        let (_root, store, auth, request) = fixture();
        let target = store.source_add_target(&auth,&request).unwrap();
        let before = store.snapshot().unwrap();
        for field in ["incarnation","game","revision"] {
            let mut invalid = target.clone();
            match field { "incarnation"=>invalid.incarnation_id="recreated".into(), "game"=>invalid.game_id="other".into(), _=>invalid.content_revision=99 }
            assert!(store.commit_source_add(&auth,field,&request,&invalid,&metadata()).is_err());
            assert_eq!(store.snapshot().unwrap(),before);
        }
        store.connection().unwrap().execute(
            "INSERT INTO library_profile_content_guards(resource_key,guard_id,profile_id,owner_scope,owner_generation,fence_epoch,expected_content_revision,target_incarnation_id,state,acquired_at_ms,heartbeat_at_ms) VALUES('profile-content:profile','guard','profile','owner','generation',1,0,'incarnation','active',0,0)",[]
        ).unwrap();
        let guarded = store.snapshot().unwrap();
        assert!(matches!(store.commit_source_add(&auth,"guarded",&request,&target,&metadata()),Err(LibraryError::ResourceBusy {..})));
        assert_eq!(store.snapshot().unwrap(),guarded);
    }

    #[test]
    fn local_snapshot_requires_verified_identity_and_commits_one_released_fence() {
        let (_root, store, auth, mut request) = fixture();
        let target = store.source_add_target(&auth,&request).unwrap();
        let digest = format!("sha256:{}", "a".repeat(64));
        request.reference.source_id="local".into();
        request.reference.remote_mod_id=digest.clone();
        let metadata=SourceAddMetadata {display_name:"Local directory".into(),profile_url:None,tags:vec![],versions:vec![SourceAddVersion {version_id:digest.clone(),remote_file_id:None,file_size:42}],selected_version_id:Some(digest.clone())};
        let mut artifact = ArtifactRef {digest:digest.clone(),size:42,verified:false,media_type:None};
        let before=store.snapshot().unwrap();
        assert!(store.commit_local_source_add(&auth,"local",&request,&target,&metadata,&artifact,"generation").is_err());
        assert_eq!(store.snapshot().unwrap(),before);
        artifact.verified=true;
        let added=store.commit_local_source_add(&auth,"local",&request,&target,&metadata,&artifact,"generation").unwrap();
        let snapshot=store.snapshot().unwrap();
        assert_eq!(snapshot.profile_content_bindings[0].artifact_digest,digest);
        assert_eq!(snapshot.profile_content_bindings[0].profile_mod_id,added.profile_mod_id);
        assert_eq!(snapshot.profile_mods[0].used_version.as_deref(),Some(digest.as_str()));
        assert_eq!(snapshot.profile_mods[0].selected_version_id.as_deref(),Some(digest.as_str()));
        assert_eq!(snapshot.profile_mods[0].version_selection_kind,"pinned");
        assert_eq!(snapshot.operation_leases[0].state,"released");
        assert_eq!(snapshot.operation_leases[0].owner_generation,"generation");
        assert_eq!(snapshot.operation_leases[0].fence_epoch,1);
        assert_eq!(snapshot.download_operations[0].state,"succeeded");
        assert_eq!(store.commit_local_source_add(&auth,"local",&request,&target,&metadata,&artifact,"generation").unwrap(),added);
        assert_eq!(store.snapshot().unwrap(),snapshot);
        request.expected_content_revision=added.content_revision;
        let target=store.source_add_target(&auth,&request).unwrap();
        let duplicate=store.commit_local_source_add(&auth,"new-request",&request,&target,&metadata,&artifact,"generation").unwrap();
        assert!(!duplicate.changed);
        assert_eq!(duplicate.content_revision,added.content_revision);
        let after=store.snapshot().unwrap();
        assert_eq!(after.operation_leases,snapshot.operation_leases);
        assert_eq!(after.download_operations,snapshot.download_operations);
        assert_eq!(after.profile_content_bindings,snapshot.profile_content_bindings);
    }
}
