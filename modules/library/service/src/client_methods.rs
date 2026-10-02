impl LibraryStore {
    pub fn snapshot(&self) -> Result<LibrarySnapshot, LibraryError> {
        #[cfg(any(test, feature = "fixtures"))] if let Some(local)=&self.local { return local.snapshot(); }
        self.call("snapshot", None, json!({}), &[], None)
    }
    pub fn set_global_setting(&self, name: &str, value: &str) -> Result<(), LibraryError> {
        #[cfg(any(test, feature = "fixtures"))] if let Some(local)=&self.local { return local.set_global_setting(name, value); }
        self.call("set_global_setting", None, json!({"name":name, "value":value}), &[], None)
    }
    pub fn set_game_setting(
        &self,
        game_id: &str,
        name: &str,
        value: &str,
    ) -> Result<(), LibraryError> {
        #[cfg(any(test, feature = "fixtures"))] if let Some(local)=&self.local { return local.set_game_setting(game_id, name, value); }
        self.call("set_game_setting", None, json!({"game_id":game_id, "name":name, "value":value}), &[], None)
    }
    pub fn get_global_setting(&self, name: &str) -> Result<Option<String>, LibraryError> {
        #[cfg(any(test, feature = "fixtures"))] if let Some(local)=&self.local { return local.get_global_setting(name); }
        self.call("get_global_setting", None, json!({"name":name}), &[], None)
    }
    pub fn get_game_setting(
        &self,
        game_id: &str,
        name: &str,
    ) -> Result<Option<String>, LibraryError> {
        #[cfg(any(test, feature = "fixtures"))] if let Some(local)=&self.local { return local.get_game_setting(game_id, name); }
        self.call("get_game_setting", None, json!({"game_id":game_id, "name":name}), &[], None)
    }
    pub fn get_settings(&self, authorization: &LibraryAuthorization, request: &SettingsGetParams) -> Result<SettingsResult, LibraryError> {
        #[cfg(any(test, feature = "fixtures"))] if let Some(owner) = &self.local { return owner.get_settings(authorization, request); }
        self.call("get_settings", Some(authorization), json!({"request":request}), &[], None)
    }
    pub fn update_settings(&self, authorization: &LibraryAuthorization, key: &str, request: &SettingsUpdateParams, schema: Option<&Value>) -> Result<SettingsResult, LibraryError> {
        #[cfg(any(test, feature = "fixtures"))] if let Some(owner) = &self.local {
            crate::import_outcome::advance(self.database_path())?;
            return owner.update_settings(authorization, key, request, schema);
        }
        self.call("update_settings", Some(authorization), json!({"key":key,"request":request,"schema":schema}), &[], None)
    }
    pub fn list_games(
    &self,
    authorization: &LibraryAuthorization,
    request: &GamesListParams,
) -> Result<GamesListResult, LibraryError> {
        #[cfg(any(test, feature = "fixtures"))] if let Some(local)=&self.local { return local.list_games(authorization, request); }
        self.call("list_games", Some(authorization), json!({"request":request}), &[], None)
    }
    /// Host-only: descriptor and validation must come from the authenticated game adapter.
    pub fn register_game(
        &self,
        authorization: &LibraryAuthorization,
        idempotency_key: &str,
        request: &GameEnrollmentRequest,
    ) -> Result<GameEnrollmentResult, LibraryError> {
        #[cfg(any(test, feature = "fixtures"))]
        if let Some(local) = &self.local {
            crate::import_outcome::advance(self.database_path())?;
            return local.register_game(authorization, idempotency_key, request);
        }
        self.call("register_game", Some(authorization), json!({"idempotency_key":idempotency_key,"request":request}), &[], None)
    }
    pub fn registered_installation(
        &self,
        authorization: &LibraryAuthorization,
        installation_id: &str,
    ) -> Result<mintcat_game_api::ValidatedGameInstallation, LibraryError> {
        #[cfg(any(test, feature = "fixtures"))]
        if let Some(local) = &self.local { return local.registered_installation(authorization, installation_id); }
        self.call("registered_installation", Some(authorization), json!({"installation_id":installation_id}), &[], None)
    }
    pub fn list_profiles(
    &self,
    authorization: &LibraryAuthorization,
    request: &ProfilesListParams,
) -> Result<ProfilesListResult, LibraryError> {
        #[cfg(any(test, feature = "fixtures"))] if let Some(local)=&self.local { return local.list_profiles(authorization, request); }
        self.call("list_profiles", Some(authorization), json!({"request":request}), &[], None)
    }
    pub fn create_profile(
    &self,
    authorization: &LibraryAuthorization,
    idempotency_key: &str,
    request: &ProfilesCreateParams,
) -> Result<ProfileSummary, LibraryError> {
        #[cfg(any(test, feature = "fixtures"))] if let Some(local)=&self.local { return local.create_profile(authorization, idempotency_key, request); }
        self.call("create_profile", Some(authorization), json!({"idempotency_key":idempotency_key, "request":request}), &[], None)
    }
    pub fn update_profile(
    &self,
    authorization: &LibraryAuthorization,
    idempotency_key: &str,
    request: &ProfilesUpdateParams,
) -> Result<ProfileSummary, LibraryError> {
        #[cfg(any(test, feature = "fixtures"))] if let Some(local)=&self.local { return local.update_profile(authorization, idempotency_key, request); }
        self.call("update_profile", Some(authorization), json!({"idempotency_key":idempotency_key, "request":request}), &[], None)
    }
    pub fn activate_profile(
    &self,
    authorization: &LibraryAuthorization,
    idempotency_key: &str,
    request: &ProfilesActivateParams,
) -> Result<ProfileSummary, LibraryError> {
        #[cfg(any(test, feature = "fixtures"))] if let Some(local)=&self.local { return local.activate_profile(authorization, idempotency_key, request); }
        self.call("activate_profile", Some(authorization), json!({"idempotency_key":idempotency_key, "request":request}), &[], None)
    }
    pub fn delete_profile_command(
    &self,
    authorization: &LibraryAuthorization,
    idempotency_key: &str,
    request: &ProfilesDeleteParams,
) -> Result<LibraryMutationResult, LibraryError> {
        #[cfg(any(test, feature = "fixtures"))] if let Some(local)=&self.local { return local.delete_profile_command(authorization, idempotency_key, request); }
        self.call("delete_profile_command", Some(authorization), json!({"idempotency_key":idempotency_key, "request":request}), &[], None)
    }
    pub fn clone_profile(
    &self,
    authorization: &LibraryAuthorization,
    idempotency_key: &str,
    request: &ProfilesCloneParams,
) -> Result<ProfileSummary, LibraryError> {
        #[cfg(any(test, feature = "fixtures"))] if let Some(local)=&self.local { return local.clone_profile(authorization, idempotency_key, request); }
        self.call("clone_profile", Some(authorization), json!({"idempotency_key":idempotency_key, "request":request}), &[], None)
    }
    pub fn list_groups(
    &self,
    authorization: &LibraryAuthorization,
    request: &GroupsListParams,
) -> Result<GroupsListResult, LibraryError> {
        #[cfg(any(test, feature = "fixtures"))] if let Some(local)=&self.local { return local.list_groups(authorization, request); }
        self.call("list_groups", Some(authorization), json!({"request":request}), &[], None)
    }
    pub fn create_group(
    &self,
    authorization: &LibraryAuthorization,
    idempotency_key: &str,
    request: &GroupsCreateParams,
) -> Result<GroupSummary, LibraryError> {
        #[cfg(any(test, feature = "fixtures"))] if let Some(local)=&self.local { return local.create_group(authorization, idempotency_key, request); }
        self.call("create_group", Some(authorization), json!({"idempotency_key":idempotency_key, "request":request}), &[], None)
    }
    pub fn update_group(
    &self,
    authorization: &LibraryAuthorization,
    idempotency_key: &str,
    request: &GroupsUpdateParams,
) -> Result<GroupSummary, LibraryError> {
        #[cfg(any(test, feature = "fixtures"))] if let Some(local)=&self.local { return local.update_group(authorization, idempotency_key, request); }
        self.call("update_group", Some(authorization), json!({"idempotency_key":idempotency_key, "request":request}), &[], None)
    }
    pub fn move_group(
    &self,
    authorization: &LibraryAuthorization,
    idempotency_key: &str,
    request: &GroupsMoveParams,
) -> Result<LibraryMutationResult, LibraryError> {
        #[cfg(any(test, feature = "fixtures"))] if let Some(local)=&self.local { return local.move_group(authorization, idempotency_key, request); }
        self.call("move_group", Some(authorization), json!({"idempotency_key":idempotency_key, "request":request}), &[], None)
    }
    pub fn delete_group_command(
    &self,
    authorization: &LibraryAuthorization,
    idempotency_key: &str,
    request: &GroupsDeleteParams,
) -> Result<LibraryMutationResult, LibraryError> {
        #[cfg(any(test, feature = "fixtures"))] if let Some(local)=&self.local { return local.delete_group_command(authorization, idempotency_key, request); }
        self.call("delete_group_command", Some(authorization), json!({"idempotency_key":idempotency_key, "request":request}), &[], None)
    }
    pub fn list_mods(
    &self,
    authorization: &LibraryAuthorization,
    request: &ModsListParams,
) -> Result<ModsListResult, LibraryError> {
        #[cfg(any(test, feature = "fixtures"))] if let Some(local)=&self.local { return local.list_mods(authorization, request); }
        self.call("list_mods", Some(authorization), json!({"request":request}), &[], None)
    }
    pub fn get_mod(
    &self,
    authorization: &LibraryAuthorization,
    request: &ModsGetParams,
) -> Result<ModDetails, LibraryError> {
        #[cfg(any(test, feature = "fixtures"))] if let Some(local)=&self.local { return local.get_mod(authorization, request); }
        self.call("get_mod", Some(authorization), json!({"request":request}), &[], None)
    }
    pub fn rename_mod(
    &self,
    authorization: &LibraryAuthorization,
    idempotency_key: &str,
    request: &ModsRenameParams,
) -> Result<ModEntityResult, LibraryError> {
        #[cfg(any(test, feature = "fixtures"))] if let Some(local)=&self.local { return local.rename_mod(authorization, idempotency_key, request); }
        self.call("rename_mod", Some(authorization), json!({"idempotency_key":idempotency_key, "request":request}), &[], None)
    }
    pub fn set_mod_enabled(
    &self,
    authorization: &LibraryAuthorization,
    idempotency_key: &str,
    request: &ModsSetEnabledParams,
) -> Result<LibraryMutationResult, LibraryError> {
        #[cfg(any(test, feature = "fixtures"))] if let Some(local)=&self.local { return local.set_mod_enabled(authorization, idempotency_key, request); }
        self.call("set_mod_enabled", Some(authorization), json!({"idempotency_key":idempotency_key, "request":request}), &[], None)
    }
    pub fn set_mods_enabled_batch(
    &self,
    authorization: &LibraryAuthorization,
    idempotency_key: &str,
    request: &ModsSetEnabledBatchParams,
) -> Result<LibraryMutationResult, LibraryError> {
        #[cfg(any(test, feature = "fixtures"))] if let Some(local)=&self.local { return local.set_mods_enabled_batch(authorization, idempotency_key, request); }
        self.call("set_mods_enabled_batch", Some(authorization), json!({"idempotency_key":idempotency_key, "request":request}), &[], None)
    }
    pub fn move_mod(
    &self,
    authorization: &LibraryAuthorization,
    idempotency_key: &str,
    request: &ModsMoveParams,
) -> Result<LibraryMutationResult, LibraryError> {
        #[cfg(any(test, feature = "fixtures"))] if let Some(local)=&self.local { return local.move_mod(authorization, idempotency_key, request); }
        self.call("move_mod", Some(authorization), json!({"idempotency_key":idempotency_key, "request":request}), &[], None)
    }
    pub fn remove_mod(
    &self,
    authorization: &LibraryAuthorization,
    idempotency_key: &str,
    request: &ModsRemoveParams,
) -> Result<LibraryMutationResult, LibraryError> {
        #[cfg(any(test, feature = "fixtures"))] if let Some(local)=&self.local { return local.remove_mod(authorization, idempotency_key, request); }
        self.call("remove_mod", Some(authorization), json!({"idempotency_key":idempotency_key, "request":request}), &[], None)
    }
    pub fn cleanup_missing_mods(
    &self,
    authorization: &LibraryAuthorization,
    idempotency_key: &str,
    request: &ModsCleanupMissingParams,
) -> Result<LibraryMutationResult, LibraryError> {
        #[cfg(any(test, feature = "fixtures"))] if let Some(local)=&self.local { return local.cleanup_missing_mods(authorization, idempotency_key, request); }
        self.call("cleanup_missing_mods", Some(authorization), json!({"idempotency_key":idempotency_key, "request":request}), &[], None)
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
        #[cfg(any(test, feature = "fixtures"))] if let Some(local)=&self.local { return local.acquire_profile_content_guard(authorization, guard_id, profile_id, expected_content_revision, target_incarnation_id, owner_generation); }
        self.call("acquire_profile_content_guard", Some(authorization), json!({"guard_id":guard_id, "profile_id":profile_id, "expected_content_revision":expected_content_revision, "target_incarnation_id":target_incarnation_id, "owner_generation":owner_generation}), &[], None)
    }
    pub fn validate_profile_content_guard(
    &self,
    guard: &ProfileContentGuard,
) -> Result<(), LibraryError> {
        #[cfg(any(test, feature = "fixtures"))] if let Some(local)=&self.local { return local.validate_profile_content_guard(guard); }
        self.call("validate_profile_content_guard", None, json!({"guard":guard}), &[], None)
    }
    pub fn release_profile_content_guard(
    &self,
    guard: &ProfileContentGuard,
) -> Result<(), LibraryError> {
        #[cfg(any(test, feature = "fixtures"))] if let Some(local)=&self.local { return local.release_profile_content_guard(guard); }
        self.call("release_profile_content_guard", None, json!({"guard":guard}), &[], None)
    }
    pub fn start_download_operation(
    &self,
    authorization: &LibraryAuthorization,
    request: &StartDownloadOperation,
) -> Result<StartedDownloadOperation, LibraryError> {
        #[cfg(any(test, feature = "fixtures"))] if let Some(local)=&self.local { return local.start_download_operation(authorization, request); }
        self.call("start_download_operation", Some(authorization), json!({"request":request}), &[], None)
    }
    pub fn download_operation(
    &self,
    authorization: &LibraryAuthorization,
    operation_id: &str,
) -> Result<DownloadOperationSnapshot, LibraryError> {
        #[cfg(any(test, feature = "fixtures"))] if let Some(local)=&self.local { return local.download_operation(authorization, operation_id); }
        self.call("download_operation", Some(authorization), json!({"operation_id":operation_id}), &[], None)
    }
    pub fn start_version_download(
    &self,
    authorization: &LibraryAuthorization,
    idempotency_key: &str,
    owner_generation: &str,
    module_lock_digest: &str,
    request: &ModsSetVersionParams,
) -> Result<StartedDownloadOperation, LibraryError> {
        #[cfg(any(test, feature = "fixtures"))] if let Some(local)=&self.local { return local.start_version_download(authorization, idempotency_key, owner_generation, module_lock_digest, request); }
        self.call("start_version_download", Some(authorization), json!({"idempotency_key":idempotency_key, "owner_generation":owner_generation, "module_lock_digest":module_lock_digest, "request":request}), &[], None)
    }
    pub fn download_execution_plan(
    &self,
    authorization: &LibraryAuthorization,
    operation_id: &str,
) -> Result<DownloadExecutionPlan, LibraryError> {
        #[cfg(any(test, feature = "fixtures"))] if let Some(local)=&self.local { return local.download_execution_plan(authorization, operation_id); }
        self.call("download_execution_plan", Some(authorization), json!({"operation_id":operation_id}), &[], None)
    }
    pub fn profile_download_state(
    &self,
    authorization: &LibraryAuthorization,
    profile_id: &str,
) -> Result<ProfileDownloadState, LibraryError> {
        #[cfg(any(test, feature = "fixtures"))] if let Some(local)=&self.local { return local.profile_download_state(authorization, profile_id); }
        self.call("profile_download_state", Some(authorization), json!({"profile_id":profile_id}), &[], None)
    }
    pub fn set_profile_mod_enabled(
    &self,
    authorization: &LibraryAuthorization,
    request: &SetProfileModEnabled,
) -> Result<MutationResult, LibraryError> {
        #[cfg(any(test, feature = "fixtures"))] if let Some(local)=&self.local { return local.set_profile_mod_enabled(authorization, request); }
        self.call("set_profile_mod_enabled", Some(authorization), json!({"request":request}), &[], None)
    }
    pub fn delete_profile_mod(
    &self,
    authorization: &LibraryAuthorization,
    request: &DeleteProfileMod,
) -> Result<MutationResult, LibraryError> {
        #[cfg(any(test, feature = "fixtures"))] if let Some(local)=&self.local { return local.delete_profile_mod(authorization, request); }
        self.call("delete_profile_mod", Some(authorization), json!({"request":request}), &[], None)
    }
    pub fn mark_download_running(
    &self,
    operation_id: &str,
    fence_epoch: u64,
    owner_generation: &str,
) -> Result<(), LibraryError> {
        #[cfg(any(test, feature = "fixtures"))] if let Some(local)=&self.local { return local.mark_download_running(operation_id, fence_epoch, owner_generation); }
        self.call("mark_download_running", None, json!({"operation_id":operation_id, "fence_epoch":fence_epoch, "owner_generation":owner_generation}), &[], None)
    }
    pub fn heartbeat_download_operation(
    &self,
    operation_id: &str,
    fence_epoch: u64,
    owner_generation: &str,
) -> Result<(), LibraryError> {
        #[cfg(any(test, feature = "fixtures"))] if let Some(local)=&self.local { return local.heartbeat_download_operation(operation_id, fence_epoch, owner_generation); }
        self.call("heartbeat_download_operation", None, json!({"operation_id":operation_id, "fence_epoch":fence_epoch, "owner_generation":owner_generation}), &[], None)
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
        #[cfg(any(test, feature = "fixtures"))] if let Some(local)=&self.local { return local.record_download_artifact(authorization, operation_id, fence_epoch, owner_generation, item_id, grant); }
        self.call("record_download_artifact", Some(authorization), json!({"operation_id":operation_id, "fence_epoch":fence_epoch, "owner_generation":owner_generation, "item_id":item_id, "grant":grant.descriptor.grant_id}), &[grant], None)
    }
    pub fn apply_download_operation(
    &self,
    authorization: &LibraryAuthorization,
    operation_id: &str,
    fence_epoch: u64,
    owner_generation: &str,
    artifacts: &[AppliedArtifact<'_>],
) -> Result<ApplyOperationResult, LibraryError> {
        #[cfg(any(test, feature = "fixtures"))] if let Some(local)=&self.local { return local.apply_download_operation(authorization, operation_id, fence_epoch, owner_generation, artifacts); }
        self.call("apply_download_operation", Some(authorization), json!({"operation_id":operation_id, "fence_epoch":fence_epoch, "owner_generation":owner_generation, "artifacts":artifacts.iter().map(|a| json!({"itemId":a.item_id,"grantId":a.grant.descriptor.grant_id})).collect::<Vec<_>>()}), &artifacts.iter().map(|a|a.grant).collect::<Vec<_>>(), None)
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
        #[cfg(any(test, feature = "fixtures"))] if let Some(local)=&self.local { return local.fail_download_operation(authorization, operation_id, fence_epoch, owner_generation, error_code, details); }
        self.call("fail_download_operation", Some(authorization), json!({"operation_id":operation_id, "fence_epoch":fence_epoch, "owner_generation":owner_generation, "error_code":error_code, "details":details}), &[], None)
    }
    pub fn cancel_download_operation(
    &self,
    authorization: &LibraryAuthorization,
    operation_id: &str,
) -> Result<CancelOperationResult, LibraryError> {
        #[cfg(any(test, feature = "fixtures"))] if let Some(local)=&self.local { return local.cancel_download_operation(authorization, operation_id); }
        self.call("cancel_download_operation", Some(authorization), json!({"operation_id":operation_id}), &[], None)
    }
    pub fn mark_expired_download_leases_suspect(
    &self,
    heartbeat_before_ms: i64,
) -> Result<usize, LibraryError> {
        #[cfg(any(test, feature = "fixtures"))] if let Some(local)=&self.local { return local.mark_expired_download_leases_suspect(heartbeat_before_ms); }
        self.call("mark_expired_download_leases_suspect", None, json!({"heartbeat_before_ms":heartbeat_before_ms}), &[], None)
    }
    pub fn recover_download_operations(
    &self,
    current_generation: &str,
) -> Result<usize, LibraryError> {
        #[cfg(any(test, feature = "fixtures"))] if let Some(local)=&self.local { return local.recover_download_operations(current_generation); }
        self.call("recover_download_operations", None, json!({"current_generation":current_generation}), &[], None)
    }
    pub fn resume_download_operation(
    &self,
    authorization: &LibraryAuthorization,
    operation_id: &str,
    expected_content_revision: u64,
    owner_generation: &str,
) -> Result<DownloadOperationSnapshot, LibraryError> {
        #[cfg(any(test, feature = "fixtures"))] if let Some(local)=&self.local { return local.resume_download_operation(authorization, operation_id, expected_content_revision, owner_generation); }
        self.call("resume_download_operation", Some(authorization), json!({"operation_id":operation_id, "expected_content_revision":expected_content_revision, "owner_generation":owner_generation}), &[], None)
    }
    pub fn retained_artifact_digests(
    &self,
    authorization: &LibraryAuthorization,
) -> Result<Vec<String>, LibraryError> {
        #[cfg(any(test, feature = "fixtures"))] if let Some(local)=&self.local { return local.retained_artifact_digests(authorization); }
        self.call("retained_artifact_digests", Some(authorization), json!({}), &[], None)
    }
    pub fn profile_content_capability(
    &self,
    authorization: &LibraryAuthorization,
    profile_id: &str,
) -> Result<ProfileContentCapability, LibraryError> {
        #[cfg(any(test, feature = "fixtures"))] if let Some(local)=&self.local { return local.profile_content_capability(authorization, profile_id); }
        self.call("profile_content_capability", Some(authorization), json!({"profile_id":profile_id}), &[], None)
    }
    pub fn pending_outbox(
    &self,
    destination: &str,
    limit: usize,
) -> Result<Vec<OutboxEvent>, LibraryError> {
        #[cfg(any(test, feature = "fixtures"))] if let Some(local)=&self.local { return local.pending_outbox(destination, limit); }
        self.call("pending_outbox", None, json!({"destination":destination, "limit":limit}), &[], None)
    }
    pub fn mark_outbox_delivered(
    &self,
    event_id: &str,
    delivery_ref: &str,
) -> Result<bool, LibraryError> {
        #[cfg(any(test, feature = "fixtures"))] if let Some(local)=&self.local { return local.mark_outbox_delivered(event_id, delivery_ref); }
        self.call("mark_outbox_delivered", None, json!({"event_id":event_id, "delivery_ref":delivery_ref}), &[], None)
    }
    pub fn recover_profile_content_guards(
    &self,
    generation: &str,
    unresolved_profiles: &BTreeSet<String>,
) -> Result<usize, LibraryError> {
        #[cfg(any(test, feature = "fixtures"))] if let Some(local)=&self.local { return local.recover_profile_content_guards(generation, unresolved_profiles); }
        self.call("recover_profile_content_guards", None, json!({"generation":generation, "unresolved_profiles":unresolved_profiles}), &[], None)
    }
    pub fn backup_snapshot(&self) -> Result<LibrarySnapshot, LibraryError> {
        #[cfg(any(test, feature = "fixtures"))] if let Some(local)=&self.local { return local.backup_snapshot(); }
        self.call("backup_snapshot", None, json!({}), &[], None)
    }
    pub fn ensure_backup_quiescent(&self) -> Result<(), LibraryError> {
        #[cfg(any(test, feature = "fixtures"))] if let Some(local)=&self.local { return local.ensure_backup_quiescent(); }
        self.call("ensure_backup_quiescent", None, json!({}), &[], None)
    }
    pub fn legacy_import_outcome(&self, source_digest: &str) -> Result<Option<LegacyImportOutcome>, LibraryError> {
        #[cfg(any(test, feature = "fixtures"))] if let Some(local)=&self.local { return local.legacy_import_outcome(source_digest); }
        self.call("legacy_import_outcome", None, json!({"source_digest":source_digest}), &[], None)
    }
    pub fn import_empty_snapshot(&self, snapshot: &LibrarySnapshot) -> Result<(), LibraryError> {
        #[cfg(any(test, feature = "fixtures"))] if let Some(local)=&self.local { return local.import_empty_snapshot(snapshot); }
        self.call("import_empty_snapshot", None, json!({"snapshot":snapshot}), &[], None)
    }
    pub fn source_add_target(&self, authorization: &LibraryAuthorization, request: &SourceAddRequest) -> Result<SourceAddTarget, LibraryError> {
        #[cfg(any(test, feature = "fixtures"))] if let Some(local)=&self.local { return local.source_add_target(authorization, request); }
        self.call("source_add_target", Some(authorization), json!({"request":request}), &[], None)
    }
    pub fn source_add_result(&self, authorization: &LibraryAuthorization, key: &str, request: &SourceAddRequest, local: bool) -> Result<Option<SourceAddResult>, LibraryError> {
        #[cfg(any(test, feature = "fixtures"))] if let Some(owner)=&self.local { return owner.source_add_result(authorization, key, request, local); }
        self.call("source_add_result", Some(authorization), json!({"key":key, "request":request, "local":local}), &[], None)
    }
    pub fn commit_source_add(&self, authorization: &LibraryAuthorization, idempotency_key: &str, request: &SourceAddRequest, target: &SourceAddTarget, metadata: &SourceAddMetadata) -> Result<SourceAddResult, LibraryError> {
        #[cfg(any(test, feature = "fixtures"))] if let Some(local)=&self.local { return local.commit_source_add(authorization, idempotency_key, request, target, metadata); }
        self.call("commit_source_add", Some(authorization), json!({"idempotency_key":idempotency_key, "request":request, "target":target, "metadata":metadata}), &[], None)
    }
    pub fn commit_local_source_add(&self, authorization: &LibraryAuthorization, idempotency_key: &str, request: &SourceAddRequest, target: &SourceAddTarget, metadata: &SourceAddMetadata, artifact: &ArtifactRef, owner_generation: &str) -> Result<SourceAddResult, LibraryError> {
        #[cfg(any(test, feature = "fixtures"))] if let Some(local)=&self.local { return local.commit_local_source_add(authorization, idempotency_key, request, target, metadata, artifact, owner_generation); }
        let grant=self.artifacts()?.grant_blob(artifact).map_err(|_| LibraryError::ArtifactNotVerified(artifact.digest.clone()))?;
        self.call("commit_local_source_add", Some(authorization), json!({"idempotency_key":idempotency_key, "request":request, "target":target, "metadata":metadata, "artifact":grant.descriptor.grant_id, "owner_generation":owner_generation}), &[&grant], None)
    }
}
