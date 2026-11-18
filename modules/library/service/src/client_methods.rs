impl LibraryStore {
    pub fn snapshot(&self) -> Result<LibrarySnapshot, LibraryError> {
        self.owner.snapshot()
    }
    pub fn set_global_setting(&self, name: &str, value: &str) -> Result<(), LibraryError> {
        let _writer = self.writer.lock().map_err(|_| LibraryError::Poisoned)?;
        self.advance_epoch()?;
        self.owner.set_global_setting(name, value)
    }
    pub fn set_game_setting(
        &self,
        game_id: &str,
        name: &str,
        value: &str,
    ) -> Result<(), LibraryError> {
        let _writer = self.writer.lock().map_err(|_| LibraryError::Poisoned)?;
        self.advance_epoch()?;
        self.owner.set_game_setting(game_id, name, value)
    }
    pub fn get_global_setting(&self, name: &str) -> Result<Option<String>, LibraryError> {
        self.owner.get_global_setting(name)
    }
    pub fn get_game_setting(
        &self,
        game_id: &str,
        name: &str,
    ) -> Result<Option<String>, LibraryError> {
        self.owner.get_game_setting(game_id, name)
    }
    pub fn get_settings(&self, authorization: &LibraryAuthorization, request: &SettingsGetParams) -> Result<SettingsResult, LibraryError> {
        self.owner.get_settings(authorization, request)
    }
    pub fn update_settings(&self, authorization: &LibraryAuthorization, key: &str, request: &SettingsUpdateParams, schema: Option<&Value>) -> Result<SettingsResult, LibraryError> {
        let _writer = self.writer.lock().map_err(|_| LibraryError::Poisoned)?;
        self.advance_epoch()?;
        self.owner.update_settings(authorization, key, request, schema)
    }
    pub fn list_games(
    &self,
    authorization: &LibraryAuthorization,
    request: &GamesListParams,
) -> Result<GamesListResult, LibraryError> {
        self.owner.list_games(authorization, request)
    }
    /// Host-only: descriptor and validation must come from the authenticated game adapter.
    pub fn register_game(
        &self,
        authorization: &LibraryAuthorization,
        idempotency_key: &str,
        request: &GameEnrollmentRequest,
    ) -> Result<GameEnrollmentResult, LibraryError> {
        let _writer = self.writer.lock().map_err(|_| LibraryError::Poisoned)?;
        self.advance_epoch()?;
        self.owner.register_game(authorization, idempotency_key, request)
    }
    pub fn registered_installation(
        &self,
        authorization: &LibraryAuthorization,
        installation_id: &str,
    ) -> Result<mintcat_game_api::ValidatedGameInstallation, LibraryError> {
        self.owner.registered_installation(authorization, installation_id)
    }
    pub fn list_profiles(
    &self,
    authorization: &LibraryAuthorization,
    request: &ProfilesListParams,
) -> Result<ProfilesListResult, LibraryError> {
        self.owner.list_profiles(authorization, request)
    }
    pub fn create_profile(
    &self,
    authorization: &LibraryAuthorization,
    idempotency_key: &str,
    request: &ProfilesCreateParams,
) -> Result<ProfileSummary, LibraryError> {
        let _writer = self.writer.lock().map_err(|_| LibraryError::Poisoned)?;
        self.advance_epoch()?;
        self.owner.create_profile(authorization, idempotency_key, request)
    }
    pub fn update_profile(
    &self,
    authorization: &LibraryAuthorization,
    idempotency_key: &str,
    request: &ProfilesUpdateParams,
) -> Result<ProfileSummary, LibraryError> {
        let _writer = self.writer.lock().map_err(|_| LibraryError::Poisoned)?;
        self.advance_epoch()?;
        self.owner.update_profile(authorization, idempotency_key, request)
    }
    pub fn activate_profile(
    &self,
    authorization: &LibraryAuthorization,
    idempotency_key: &str,
    request: &ProfilesActivateParams,
) -> Result<ProfileSummary, LibraryError> {
        let _writer = self.writer.lock().map_err(|_| LibraryError::Poisoned)?;
        self.advance_epoch()?;
        self.owner.activate_profile(authorization, idempotency_key, request)
    }
    pub fn delete_profile_command(
    &self,
    authorization: &LibraryAuthorization,
    idempotency_key: &str,
    request: &ProfilesDeleteParams,
) -> Result<LibraryMutationResult, LibraryError> {
        let _writer = self.writer.lock().map_err(|_| LibraryError::Poisoned)?;
        self.advance_epoch()?;
        self.owner.delete_profile_command(authorization, idempotency_key, request)
    }
    pub fn clone_profile(
    &self,
    authorization: &LibraryAuthorization,
    idempotency_key: &str,
    request: &ProfilesCloneParams,
) -> Result<ProfileSummary, LibraryError> {
        let _writer = self.writer.lock().map_err(|_| LibraryError::Poisoned)?;
        self.advance_epoch()?;
        self.owner.clone_profile(authorization, idempotency_key, request)
    }
    pub fn list_groups(
    &self,
    authorization: &LibraryAuthorization,
    request: &GroupsListParams,
) -> Result<GroupsListResult, LibraryError> {
        self.owner.list_groups(authorization, request)
    }
    pub fn create_group(
    &self,
    authorization: &LibraryAuthorization,
    idempotency_key: &str,
    request: &GroupsCreateParams,
) -> Result<GroupSummary, LibraryError> {
        let _writer = self.writer.lock().map_err(|_| LibraryError::Poisoned)?;
        self.advance_epoch()?;
        self.owner.create_group(authorization, idempotency_key, request)
    }
    pub fn update_group(
    &self,
    authorization: &LibraryAuthorization,
    idempotency_key: &str,
    request: &GroupsUpdateParams,
) -> Result<GroupSummary, LibraryError> {
        let _writer = self.writer.lock().map_err(|_| LibraryError::Poisoned)?;
        self.advance_epoch()?;
        self.owner.update_group(authorization, idempotency_key, request)
    }
    pub fn move_group(
    &self,
    authorization: &LibraryAuthorization,
    idempotency_key: &str,
    request: &GroupsMoveParams,
) -> Result<LibraryMutationResult, LibraryError> {
        let _writer = self.writer.lock().map_err(|_| LibraryError::Poisoned)?;
        self.advance_epoch()?;
        self.owner.move_group(authorization, idempotency_key, request)
    }
    pub fn delete_group_command(
    &self,
    authorization: &LibraryAuthorization,
    idempotency_key: &str,
    request: &GroupsDeleteParams,
) -> Result<LibraryMutationResult, LibraryError> {
        let _writer = self.writer.lock().map_err(|_| LibraryError::Poisoned)?;
        self.advance_epoch()?;
        self.owner.delete_group_command(authorization, idempotency_key, request)
    }
    pub fn list_mods(
    &self,
    authorization: &LibraryAuthorization,
    request: &ModsListParams,
) -> Result<ModsListResult, LibraryError> {
        self.owner.list_mods(authorization, request)
    }
    pub fn get_mod(
    &self,
    authorization: &LibraryAuthorization,
    request: &ModsGetParams,
) -> Result<ModDetails, LibraryError> {
        self.owner.get_mod(authorization, request)
    }
    pub fn rename_mod(
    &self,
    authorization: &LibraryAuthorization,
    idempotency_key: &str,
    request: &ModsRenameParams,
) -> Result<ModEntityResult, LibraryError> {
        let _writer = self.writer.lock().map_err(|_| LibraryError::Poisoned)?;
        self.advance_epoch()?;
        self.owner.rename_mod(authorization, idempotency_key, request)
    }
    pub fn set_mod_enabled(
    &self,
    authorization: &LibraryAuthorization,
    idempotency_key: &str,
    request: &ModsSetEnabledParams,
) -> Result<LibraryMutationResult, LibraryError> {
        let _writer = self.writer.lock().map_err(|_| LibraryError::Poisoned)?;
        self.advance_epoch()?;
        self.owner.set_mod_enabled(authorization, idempotency_key, request)
    }
    pub fn set_mods_enabled_batch(
    &self,
    authorization: &LibraryAuthorization,
    idempotency_key: &str,
    request: &ModsSetEnabledBatchParams,
) -> Result<LibraryMutationResult, LibraryError> {
        let _writer = self.writer.lock().map_err(|_| LibraryError::Poisoned)?;
        self.advance_epoch()?;
        self.owner.set_mods_enabled_batch(authorization, idempotency_key, request)
    }
    pub fn move_mod(
    &self,
    authorization: &LibraryAuthorization,
    idempotency_key: &str,
    request: &ModsMoveParams,
) -> Result<LibraryMutationResult, LibraryError> {
        let _writer = self.writer.lock().map_err(|_| LibraryError::Poisoned)?;
        self.advance_epoch()?;
        self.owner.move_mod(authorization, idempotency_key, request)
    }
    pub fn remove_mod(
    &self,
    authorization: &LibraryAuthorization,
    idempotency_key: &str,
    request: &ModsRemoveParams,
) -> Result<LibraryMutationResult, LibraryError> {
        let _writer = self.writer.lock().map_err(|_| LibraryError::Poisoned)?;
        self.advance_epoch()?;
        self.owner.remove_mod(authorization, idempotency_key, request)
    }
    pub fn cleanup_missing_mods(
    &self,
    authorization: &LibraryAuthorization,
    idempotency_key: &str,
    request: &ModsCleanupMissingParams,
) -> Result<LibraryMutationResult, LibraryError> {
        let _writer = self.writer.lock().map_err(|_| LibraryError::Poisoned)?;
        self.advance_epoch()?;
        self.owner.cleanup_missing_mods(authorization, idempotency_key, request)
    }
    pub fn acquire_profile_content_guard(
    &self,
    authorization: &LibraryAuthorization,
    guard_id: &str,
    profile_id: &str,
    expected_content_revision: u64,
    target_incarnation_id: &str,
    owner_generation: &str,
) -> Result<ProfileContentGuard, LibraryError> {
        let _writer = self.writer.lock().map_err(|_| LibraryError::Poisoned)?;
        self.advance_epoch()?;
        self.owner.acquire_profile_content_guard(authorization, guard_id, profile_id, expected_content_revision, target_incarnation_id, owner_generation)
    }
    pub fn validate_profile_content_guard(
    &self,
    guard: &ProfileContentGuard,
) -> Result<(), LibraryError> {
        self.owner.validate_profile_content_guard(guard)
    }
    pub fn release_profile_content_guard(
    &self,
    guard: &ProfileContentGuard,
) -> Result<(), LibraryError> {
        let _writer = self.writer.lock().map_err(|_| LibraryError::Poisoned)?;
        self.advance_epoch()?;
        self.owner.release_profile_content_guard(guard)
    }
    pub fn start_download_operation(
    &self,
    authorization: &LibraryAuthorization,
    request: &StartDownloadOperation,
) -> Result<StartedDownloadOperation, LibraryError> {
        let _writer = self.writer.lock().map_err(|_| LibraryError::Poisoned)?;
        self.advance_epoch()?;
        self.owner.start_download_operation(authorization, request)
    }
    pub fn download_operation(
    &self,
    authorization: &LibraryAuthorization,
    operation_id: &str,
) -> Result<DownloadOperationSnapshot, LibraryError> {
        self.owner.download_operation(authorization, operation_id)
    }
    pub fn start_version_download(
    &self,
    authorization: &LibraryAuthorization,
    idempotency_key: &str,
    owner_generation: &str,
    module_lock_digest: &str,
    request: &ModsSetVersionParams,
) -> Result<StartedDownloadOperation, LibraryError> {
        let _writer = self.writer.lock().map_err(|_| LibraryError::Poisoned)?;
        self.advance_epoch()?;
        self.owner.start_version_download(authorization, idempotency_key, owner_generation, module_lock_digest, request)
    }
    pub fn download_execution_plan(
    &self,
    authorization: &LibraryAuthorization,
    operation_id: &str,
) -> Result<DownloadExecutionPlan, LibraryError> {
        self.owner.download_execution_plan(authorization, operation_id)
    }
    pub fn profile_download_state(
    &self,
    authorization: &LibraryAuthorization,
    profile_id: &str,
) -> Result<ProfileDownloadState, LibraryError> {
        self.owner.profile_download_state(authorization, profile_id)
    }
    pub fn set_profile_mod_enabled(
    &self,
    authorization: &LibraryAuthorization,
    request: &SetProfileModEnabled,
) -> Result<MutationResult, LibraryError> {
        let _writer = self.writer.lock().map_err(|_| LibraryError::Poisoned)?;
        self.advance_epoch()?;
        self.owner.set_profile_mod_enabled(authorization, request)
    }
    pub fn delete_profile_mod(
    &self,
    authorization: &LibraryAuthorization,
    request: &DeleteProfileMod,
) -> Result<MutationResult, LibraryError> {
        let _writer = self.writer.lock().map_err(|_| LibraryError::Poisoned)?;
        self.advance_epoch()?;
        self.owner.delete_profile_mod(authorization, request)
    }
    pub fn mark_download_running(
    &self,
    operation_id: &str,
    fence_epoch: u64,
    owner_generation: &str,
) -> Result<(), LibraryError> {
        let _writer = self.writer.lock().map_err(|_| LibraryError::Poisoned)?;
        self.advance_epoch()?;
        self.owner.mark_download_running(operation_id, fence_epoch, owner_generation)
    }
    pub fn heartbeat_download_operation(
    &self,
    operation_id: &str,
    fence_epoch: u64,
    owner_generation: &str,
) -> Result<(), LibraryError> {
        let _writer = self.writer.lock().map_err(|_| LibraryError::Poisoned)?;
        self.advance_epoch()?;
        self.owner.heartbeat_download_operation(operation_id, fence_epoch, owner_generation)
    }
    pub fn record_download_artifact(
    &self,
    authorization: &LibraryAuthorization,
    operation_id: &str,
    fence_epoch: u64,
    owner_generation: &str,
    item_id: &str,
    grant: &ArtifactGrant,
) -> Result<RecordedArtifact, LibraryError> {
        let _writer = self.writer.lock().map_err(|_| LibraryError::Poisoned)?;
        self.advance_epoch()?;
        self.owner.record_download_artifact(authorization, operation_id, fence_epoch, owner_generation, item_id, grant)
    }
    pub fn apply_download_operation(
    &self,
    authorization: &LibraryAuthorization,
    operation_id: &str,
    fence_epoch: u64,
    owner_generation: &str,
    artifacts: &[AppliedArtifact<'_>],
) -> Result<ApplyOperationResult, LibraryError> {
        let _writer = self.writer.lock().map_err(|_| LibraryError::Poisoned)?;
        self.advance_epoch()?;
        self.owner.apply_download_operation(authorization, operation_id, fence_epoch, owner_generation, artifacts)
    }
    pub fn fail_download_operation(
    &self,
    authorization: &LibraryAuthorization,
    operation_id: &str,
    fence_epoch: u64,
    owner_generation: &str,
    error_code: &str,
    details: Value,
) -> Result<FailedDownloadOperation, LibraryError> {
        let _writer = self.writer.lock().map_err(|_| LibraryError::Poisoned)?;
        self.advance_epoch()?;
        self.owner.fail_download_operation(authorization, operation_id, fence_epoch, owner_generation, error_code, details)
    }
    pub fn cancel_download_operation(
    &self,
    authorization: &LibraryAuthorization,
    operation_id: &str,
) -> Result<CancelOperationResult, LibraryError> {
        let _writer = self.writer.lock().map_err(|_| LibraryError::Poisoned)?;
        self.advance_epoch()?;
        self.owner.cancel_download_operation(authorization, operation_id)
    }
    pub fn mark_expired_download_leases_suspect(
    &self,
    heartbeat_before_ms: i64,
) -> Result<usize, LibraryError> {
        let _writer = self.writer.lock().map_err(|_| LibraryError::Poisoned)?;
        self.advance_epoch()?;
        self.owner.mark_expired_download_leases_suspect(heartbeat_before_ms)
    }
    pub fn recover_download_operations(
    &self,
    current_generation: &str,
) -> Result<usize, LibraryError> {
        let _writer = self.writer.lock().map_err(|_| LibraryError::Poisoned)?;
        self.advance_epoch()?;
        self.owner.recover_download_operations(current_generation)
    }
    pub fn resume_download_operation(
    &self,
    authorization: &LibraryAuthorization,
    operation_id: &str,
    expected_content_revision: u64,
    owner_generation: &str,
) -> Result<DownloadOperationSnapshot, LibraryError> {
        let _writer = self.writer.lock().map_err(|_| LibraryError::Poisoned)?;
        self.advance_epoch()?;
        self.owner.resume_download_operation(authorization, operation_id, expected_content_revision, owner_generation)
    }
    pub fn retained_artifact_digests(
    &self,
    authorization: &LibraryAuthorization,
) -> Result<Vec<String>, LibraryError> {
        self.owner.retained_artifact_digests(authorization)
    }
    pub fn profile_content_capability(
    &self,
    authorization: &LibraryAuthorization,
    profile_id: &str,
) -> Result<ProfileContentCapability, LibraryError> {
        self.owner.profile_content_capability(authorization, profile_id)
    }
    pub fn pending_outbox(
    &self,
    destination: &str,
    limit: usize,
) -> Result<Vec<OutboxEvent>, LibraryError> {
        self.owner.pending_outbox(destination, limit)
    }
    pub fn mark_outbox_delivered(
    &self,
    event_id: &str,
    delivery_ref: &str,
) -> Result<bool, LibraryError> {
        let _writer = self.writer.lock().map_err(|_| LibraryError::Poisoned)?;
        self.advance_epoch()?;
        self.owner.mark_outbox_delivered(event_id, delivery_ref)
    }
    pub fn recover_profile_content_guards(
    &self,
    generation: &str,
    unresolved_profiles: &BTreeSet<String>,
) -> Result<usize, LibraryError> {
        let _writer = self.writer.lock().map_err(|_| LibraryError::Poisoned)?;
        self.advance_epoch()?;
        self.owner.recover_profile_content_guards(generation, unresolved_profiles)
    }
    pub fn backup_snapshot(&self) -> Result<LibrarySnapshot, LibraryError> {
        self.owner.backup_snapshot()
    }
    pub fn ensure_backup_quiescent(&self) -> Result<(), LibraryError> {
        self.owner.ensure_backup_quiescent()
    }
    pub fn legacy_import_outcome(&self, source_digest: &str) -> Result<Option<LegacyImportOutcome>, LibraryError> {
        self.owner.legacy_import_outcome(source_digest)
    }
    pub fn import_empty_snapshot(&self, snapshot: &LibrarySnapshot) -> Result<(), LibraryError> {
        let _writer = self.writer.lock().map_err(|_| LibraryError::Poisoned)?;
        self.advance_epoch()?;
        self.owner.import_empty_snapshot(snapshot)
    }
    pub fn source_add_target(&self, authorization: &LibraryAuthorization, request: &SourceAddRequest) -> Result<SourceAddTarget, LibraryError> {
        self.owner.source_add_target(authorization, request)
    }
    pub fn source_add_result(&self, authorization: &LibraryAuthorization, key: &str, request: &SourceAddRequest, local: bool) -> Result<Option<SourceAddResult>, LibraryError> {
        self.owner.source_add_result(authorization, key, request, local)
    }
    pub fn commit_source_add(&self, authorization: &LibraryAuthorization, idempotency_key: &str, request: &SourceAddRequest, target: &SourceAddTarget, metadata: &SourceAddMetadata) -> Result<SourceAddResult, LibraryError> {
        let _writer = self.writer.lock().map_err(|_| LibraryError::Poisoned)?;
        self.advance_epoch()?;
        self.owner.commit_source_add(authorization, idempotency_key, request, target, metadata)
    }
    pub fn commit_local_source_add(&self, authorization: &LibraryAuthorization, idempotency_key: &str, request: &SourceAddRequest, target: &SourceAddTarget, metadata: &SourceAddMetadata, artifact: &ArtifactRef, owner_generation: &str) -> Result<SourceAddResult, LibraryError> {
        let _writer = self.writer.lock().map_err(|_| LibraryError::Poisoned)?;
        self.advance_epoch()?;
        self.owner.commit_local_source_add(authorization, idempotency_key, request, target, metadata, artifact, owner_generation)
    }
}
