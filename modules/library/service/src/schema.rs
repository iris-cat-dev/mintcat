pub(crate) const MIGRATION_1: &str = r#"
CREATE TABLE library_games (
  game_id TEXT PRIMARY KEY,
  game_key TEXT NOT NULL UNIQUE,
  display_name TEXT NOT NULL,
  icon TEXT NOT NULL,
  active INTEGER NOT NULL CHECK (active IN (0, 1)),
  created_at_ms INTEGER NOT NULL,
  updated_at_ms INTEGER NOT NULL
);

CREATE TABLE library_installations (
  installation_id TEXT PRIMARY KEY,
  game_id TEXT NOT NULL REFERENCES library_games(game_id),
  root_path TEXT,
  state TEXT NOT NULL CHECK (state IN ('configured', 'unresolved')),
  incarnation_id TEXT NOT NULL,
  created_at_ms INTEGER NOT NULL,
  updated_at_ms INTEGER NOT NULL
);
CREATE INDEX library_installations_game ON library_installations(game_id);

CREATE TABLE library_profiles (
  profile_id TEXT PRIMARY KEY,
  game_id TEXT NOT NULL REFERENCES library_games(game_id),
  owner_scope TEXT NOT NULL,
  name TEXT NOT NULL,
  display_name TEXT NOT NULL,
  active INTEGER NOT NULL CHECK (active IN (0, 1)),
  description TEXT NOT NULL,
  revision INTEGER NOT NULL CHECK (revision >= 0),
  content_revision INTEGER NOT NULL CHECK (content_revision >= 0),
  incarnation_id TEXT NOT NULL,
  last_used_at_ms INTEGER NOT NULL,
  created_at_ms INTEGER NOT NULL,
  updated_at_ms INTEGER NOT NULL,
  UNIQUE(owner_scope, game_id, name)
);
CREATE INDEX library_profiles_game_owner ON library_profiles(game_id, owner_scope, active);

CREATE TABLE library_groups (
  group_id TEXT PRIMARY KEY,
  profile_id TEXT NOT NULL REFERENCES library_profiles(profile_id) ON DELETE CASCADE,
  parent_group_id TEXT REFERENCES library_groups(group_id) ON DELETE CASCADE DEFERRABLE INITIALLY DEFERRED,
  name TEXT NOT NULL,
  group_type TEXT NOT NULL,
  sort_order INTEGER NOT NULL CHECK (sort_order >= 0),
  expanded INTEGER NOT NULL CHECK (expanded IN (0, 1)),
  created_at_ms INTEGER NOT NULL,
  updated_at_ms INTEGER NOT NULL
);
CREATE INDEX library_groups_profile_order ON library_groups(profile_id, sort_order, group_id);

CREATE TABLE library_mods (
  mod_id TEXT PRIMARY KEY,
  game_id TEXT NOT NULL REFERENCES library_games(game_id),
  source_id TEXT NOT NULL,
  remote_game_key TEXT NOT NULL,
  remote_mod_id TEXT NOT NULL,
  name_id TEXT NOT NULL,
  display_name TEXT NOT NULL,
  original_name TEXT NOT NULL,
  url TEXT NOT NULL,
  tags_json TEXT NOT NULL,
  approval_status TEXT NOT NULL,
  last_update_at_ms INTEGER NOT NULL,
  online_update_at_ms INTEGER NOT NULL,
  online_available INTEGER NOT NULL CHECK (online_available IN (0, 1)),
  local_missing INTEGER NOT NULL CHECK (local_missing IN (0, 1)),
  dependency_mod_id TEXT REFERENCES library_mods(mod_id) DEFERRABLE INITIALLY DEFERRED,
  created_at_ms INTEGER NOT NULL,
  updated_at_ms INTEGER NOT NULL,
  UNIQUE(source_id, remote_game_key, remote_mod_id)
);
CREATE INDEX library_mods_game_name ON library_mods(game_id, display_name, mod_id);

CREATE TABLE library_mod_versions (
  mod_id TEXT NOT NULL REFERENCES library_mods(mod_id) ON DELETE CASCADE,
  version_id TEXT NOT NULL,
  remote_file_id TEXT,
  artifact_digest TEXT,
  legacy_location TEXT,
  source_url TEXT,
  file_size INTEGER NOT NULL CHECK (file_size >= 0),
  legacy_download_status TEXT,
  availability TEXT NOT NULL CHECK (availability IN ('remote', 'legacy-unverified', 'metadata-only')),
  current INTEGER NOT NULL CHECK (current IN (0, 1)),
  created_at_ms INTEGER NOT NULL,
  updated_at_ms INTEGER NOT NULL,
  PRIMARY KEY(mod_id, version_id)
);

CREATE TABLE library_profile_mods (
  profile_mod_id TEXT PRIMARY KEY,
  profile_id TEXT NOT NULL REFERENCES library_profiles(profile_id) ON DELETE CASCADE,
  mod_id TEXT NOT NULL REFERENCES library_mods(mod_id),
  group_id TEXT REFERENCES library_groups(group_id) ON DELETE SET NULL,
  incarnation_id TEXT NOT NULL,
  sort_order INTEGER NOT NULL CHECK (sort_order >= 0),
  enabled INTEGER NOT NULL CHECK (enabled IN (0, 1)),
  used_version TEXT,
  created_at_ms INTEGER NOT NULL,
  updated_at_ms INTEGER NOT NULL,
  UNIQUE(profile_id, mod_id)
);
CREATE INDEX library_profile_mods_order ON library_profile_mods(profile_id, sort_order, profile_mod_id);

CREATE TABLE library_settings (
  scope_id TEXT NOT NULL,
  name TEXT NOT NULL,
  value TEXT NOT NULL,
  created_at_ms INTEGER NOT NULL,
  updated_at_ms INTEGER NOT NULL,
  PRIMARY KEY(scope_id, name)
);

CREATE TABLE library_credentials (
  credential_id TEXT PRIMARY KEY,
  owner_scope TEXT NOT NULL,
  provider TEXT NOT NULL,
  credential_ref TEXT,
  created_at_ms INTEGER NOT NULL,
  updated_at_ms INTEGER NOT NULL,
  UNIQUE(owner_scope, provider)
);

CREATE TABLE library_legacy_install_hints (
  game_id TEXT PRIMARY KEY REFERENCES library_games(game_id),
  installed_hash TEXT NOT NULL,
  claim_status TEXT NOT NULL CHECK (claim_status = 'unverified')
);

CREATE TABLE library_legacy_mappings (
  source_digest TEXT NOT NULL,
  entity_kind TEXT NOT NULL,
  legacy_id TEXT NOT NULL,
  new_id TEXT NOT NULL,
  PRIMARY KEY(source_digest, entity_kind, legacy_id),
  UNIQUE(source_digest, entity_kind, new_id)
);

CREATE TABLE library_import_runs (
  source_digest TEXT PRIMARY KEY,
  source_label TEXT NOT NULL,
  imported_at_ms INTEGER NOT NULL,
  row_counts_json TEXT NOT NULL
);
"#;

pub(crate) const MIGRATION_2: &str = r#"
ALTER TABLE library_profiles ADD COLUMN lease_epoch INTEGER NOT NULL DEFAULT 0;

CREATE TABLE library_download_operations (
  operation_id TEXT PRIMARY KEY,
  owner_scope TEXT NOT NULL,
  idempotency_scope TEXT NOT NULL,
  idempotency_key TEXT NOT NULL,
  inputs_hash TEXT NOT NULL,
  profile_id TEXT NOT NULL REFERENCES library_profiles(profile_id),
  mode TEXT NOT NULL CHECK (mode IN ('cache-only', 'prepare-and-apply')),
  expected_content_revision INTEGER NOT NULL,
  target_incarnation_id TEXT NOT NULL,
  owner_generation TEXT NOT NULL,
  fence_epoch INTEGER NOT NULL,
  state TEXT NOT NULL CHECK (state IN ('queued', 'running', 'recovery-required', 'succeeded', 'cancelled', 'failed')),
  result_json TEXT,
  error_code TEXT,
  created_at_ms INTEGER NOT NULL,
  updated_at_ms INTEGER NOT NULL,
  completed_at_ms INTEGER,
  UNIQUE(owner_scope, idempotency_scope, idempotency_key)
);
CREATE INDEX library_operations_profile_state ON library_download_operations(profile_id, state);

CREATE TABLE library_operation_leases (
  resource_key TEXT PRIMARY KEY,
  operation_id TEXT NOT NULL UNIQUE REFERENCES library_download_operations(operation_id),
  fence_epoch INTEGER NOT NULL,
  owner_generation TEXT NOT NULL,
  state TEXT NOT NULL CHECK (state IN ('active', 'suspect', 'released', 'revoked')),
  acquired_at_ms INTEGER NOT NULL,
  heartbeat_at_ms INTEGER NOT NULL,
  released_at_ms INTEGER
);

CREATE TABLE library_download_operation_items (
  operation_id TEXT NOT NULL REFERENCES library_download_operations(operation_id) ON DELETE CASCADE,
  item_id TEXT NOT NULL,
  profile_mod_id TEXT NOT NULL,
  profile_mod_incarnation_id TEXT NOT NULL,
  mod_id TEXT NOT NULL,
  source_id TEXT NOT NULL,
  source_mod_id TEXT NOT NULL,
  source_file_id TEXT,
  resolved_version TEXT NOT NULL,
  expected_digest TEXT,
  validator TEXT,
  artifact_digest TEXT,
  state TEXT NOT NULL CHECK (state IN ('pending', 'verified')),
  PRIMARY KEY(operation_id, item_id)
);

CREATE TABLE library_content_refs (
  digest TEXT PRIMARY KEY,
  size INTEGER NOT NULL,
  verified INTEGER NOT NULL CHECK (verified = 1),
  grant_id TEXT NOT NULL,
  created_at_ms INTEGER NOT NULL
);

CREATE TABLE library_profile_content_bindings (
  profile_mod_id TEXT PRIMARY KEY REFERENCES library_profile_mods(profile_mod_id) ON DELETE CASCADE,
  version_id TEXT NOT NULL,
  artifact_digest TEXT NOT NULL REFERENCES library_content_refs(digest),
  operation_id TEXT NOT NULL REFERENCES library_download_operations(operation_id),
  updated_at_ms INTEGER NOT NULL
);

CREATE TABLE library_mutation_results (
  owner_scope TEXT NOT NULL,
  idempotency_scope TEXT NOT NULL,
  idempotency_key TEXT NOT NULL,
  inputs_hash TEXT NOT NULL,
  result_json TEXT NOT NULL,
  operation_id TEXT NOT NULL UNIQUE,
  created_at_ms INTEGER NOT NULL,
  PRIMARY KEY(owner_scope, idempotency_scope, idempotency_key)
);

CREATE TABLE library_outbox (
  event_seq INTEGER PRIMARY KEY AUTOINCREMENT,
  event_id TEXT NOT NULL UNIQUE,
  operation_id TEXT REFERENCES library_download_operations(operation_id),
  event_type TEXT NOT NULL,
  destination TEXT NOT NULL,
  payload_json TEXT NOT NULL,
  delivery_state TEXT NOT NULL CHECK (delivery_state IN ('pending', 'delivered', 'cancelled')),
  delivery_ref TEXT,
  created_at_ms INTEGER NOT NULL,
  delivered_at_ms INTEGER
);
CREATE INDEX library_outbox_delivery ON library_outbox(destination, delivery_state, event_seq);
"#;

pub(crate) const MIGRATION_3: &str = r#"
ALTER TABLE library_profiles ADD COLUMN deleted_at_ms INTEGER;

ALTER TABLE library_mods ADD COLUMN revision INTEGER NOT NULL DEFAULT 1;
ALTER TABLE library_mods ADD COLUMN incarnation_id TEXT NOT NULL DEFAULT '';
UPDATE library_mods SET incarnation_id = 'incarnation:mod:' || mod_id
WHERE incarnation_id = '';

ALTER TABLE library_profile_mods
  ADD COLUMN version_selection_kind TEXT NOT NULL DEFAULT 'auto'
  CHECK (version_selection_kind IN ('auto', 'pinned'));
ALTER TABLE library_profile_mods ADD COLUMN selected_version_id TEXT;
UPDATE library_profile_mods
SET version_selection_kind = 'pinned', selected_version_id = used_version
WHERE used_version IS NOT NULL AND used_version <> '';
"#;

pub(crate) const MIGRATION_4: &str = r#"
ALTER TABLE library_download_operation_items
  ADD COLUMN remote_game_key TEXT NOT NULL DEFAULT '';
UPDATE library_download_operation_items
SET remote_game_key = COALESCE(
  (SELECT remote_game_key FROM library_mods
   WHERE library_mods.mod_id = library_download_operation_items.mod_id),
  ''
);
ALTER TABLE library_download_operation_items
  ADD COLUMN apply_selection_kind TEXT
  CHECK (apply_selection_kind IS NULL OR apply_selection_kind IN ('auto', 'pinned'));
ALTER TABLE library_download_operation_items
  ADD COLUMN apply_selected_version_id TEXT;
"#;

pub(crate) const MIGRATION_5: &str = r#"
CREATE TABLE library_profile_content_guards (
  resource_key TEXT PRIMARY KEY,
  guard_id TEXT NOT NULL UNIQUE,
  profile_id TEXT NOT NULL REFERENCES library_profiles(profile_id),
  owner_scope TEXT NOT NULL,
  owner_generation TEXT NOT NULL,
  fence_epoch INTEGER NOT NULL,
  expected_content_revision INTEGER NOT NULL,
  target_incarnation_id TEXT NOT NULL,
  state TEXT NOT NULL CHECK (state IN ('active', 'released')),
  acquired_at_ms INTEGER NOT NULL,
  heartbeat_at_ms INTEGER NOT NULL,
  released_at_ms INTEGER
);
CREATE INDEX library_profile_content_guards_profile
  ON library_profile_content_guards(profile_id, state);
"#;
