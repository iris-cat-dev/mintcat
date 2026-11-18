# MINTCAT
![Logo](apps/desktop/src/assets/icon.ico)

# Introduction
MintCat manages Deep Rock Galactic and Rogue Core Mods with a standalone Rust daemon, a typed CLI and a Tauri Workbench. UI, Library, sources, Agent and backup ship with the application; only game adapters are independently signed plugins.


# Features
## 1. Mod Management:
- Add mods from mod.io
- Add mods from local files
- Update mods
- Mod rename

2. mod.io Search, Download, Update


# Architecture
- `apps/desktop`: statically bundled official Workbench and independent rescue controls; no renderer SQL or game-file writes.
- `apps/cli`: `mintcat`, generated domain commands and one canonical Application API.
- `runtime/daemon`: builtin business services, authenticated IPC, durable jobs/actions, network/credential/file brokers and atomic installation commits.
- `modules`: ordinary Rust business libraries, shared Unreal PAK/UE4SS code, and independently packaged DRG/Rogue Core workers with their resources. Proxy is opt-in through `experimental-proxy`.
- `runtime/release` / `runtime/updates`: game-only signed packages, independent game scopes, approval, probation, recovery, rollback and retained historical closures.
- `packages/contracts`: one CommandSpec/schema source for the Rust and TypeScript SDKs.
- The active desktop sources are `apps/desktop/src` and `apps/desktop/src-tauri`. Retired root-level `src`, `src-tauri` and `public` directories have been removed; do not recreate them as compatibility entrypoints. Native bundle icons live under `apps/desktop/src-tauri/icons`; the frontend logo lives under `apps/desktop/src/assets` and is imported through Vite's asset pipeline, with no shared public directory.
- Artifact GC retains Library content, transfer references, prepared/applying install plans, active receipts, frozen historical game packages and live task/service/rollback leases; unused expired plans release their artifacts.
- The Workbench follows the supplied dark/light glass UI references: 172px labeled sidebar, 72px game/search/Profile toolbar, compact source-grouped mod table, pink actions and switches, and translucent rounded surfaces over a native window material—not a simulated wallpaper or whole-window opacity. Light uses a milky tint; Dark uses blue-gray. Sticky table headers and elevated menus remain opaque enough to read. Local import and download management are real Library routes; missing category/compatibility/certification metadata stays “—”. Game settings, Proxy and backup remain under Settings; Agent retains its 680px drawer.
- Native appearance follows the saved UI theme. macOS uses an Overlay title bar with real traffic lights and a public behind-window `NSVisualEffectView` using `UnderWindowBackground`, not a dense whole-window Sidebar fill. CSS tint layers stay light: window/panel alpha is 18%/16% for Light and 32%/14% for Dark; these are separate from the system material and are not whole-window opacity. Transparent WKWebView still requires Tauri's `macos-private-api` feature and `macOSPrivateApi`, so this configuration is not Mac App Store compatible. Windows 11 22H2+ uses public DWM Acrylic with checked application results; older Windows, Linux, unavailable effects and reduced transparency keep a solid surface. macOS also observes system accessibility changes. Browser preview always uses the solid fallback.
- Official pages and styles share the host React/ReactDOM/Ant Design runtime and are bundled by Vite. Page drafts remain local; mutations go through the Application API and durable jobs survive navigation. Expected API errors stay within the page.

This README describes the current architecture. [The earlier modular design](docs/design/modular-runtime-architecture.md) and [its execution record](docs/design/modular-refactor-todo.md) remain historical migration evidence.


# Build & Dev

```shell
pnpm install
cargo build              # Default desktop, CLI and builtin daemon; no game builds.
pnpm check
pnpm dev                 # Actual native Tauri host; builds its daemon first.
pnpm --filter @mintcat/desktop-vnext dev  # Browser-only preview; no native RPC.
```

The old desktop, renderer task executor, CLI bridge and DLL installation entrypoints are retired. Every CLI/daemon development entry requires an explicit data directory through `--data-dir` or `MINTCAT_DATA_DIR`; it never infers the legacy `com.mint.cat` database path.

Local development bundles use `com.mint.cat.vnext.local`. On macOS, a release-mode local app defaults to `~/Library/Application Support/com.mint.cat.vnext.local/vnext`; a debug host uses `isolated-vnext-development` under the same application directory. Earlier `com.mint.cat.vnext.dev` data and production data are not migrated implicitly. Keep local module signing seeds outside shipped artifacts and build-cleanup directories, with mode `0600`; retain them for subsequent signed updates. Replacing a pinned root is not a repair for missing Library data and requires an explicit trust migration or a separate data directory.

Without game plugins, builtin Library, Agent and backup remain available. Existing games, Profiles and Mods can still be browsed and edited. Discovery, game-specific import/install and source bindings require the corresponding verified game adapter. Search offers configured sources intersecting that adapter's bindings; a supported but unenrolled game can be browsed before a target Profile is created.

Agent sessions require an explicit frozen target: `gameId`, `profileId`, `profileRevision`, `contentRevision`, `installationId` and `installationRevision`. All six fields are required; a credential reference is optional. A model cannot grant approval or replace that target with another game, Profile or installation.

```shell
target/debug/mintcatd --data-dir ./target/dev-data --environment development init
target/debug/mintcat --data-dir ./target/dev-data --json status

# Builtin services start without any signed baseline or game configuration.
target/debug/mintcat --data-dir ./target/dev-data --environment development --json doctor
target/debug/mintcat --data-dir ./target/dev-data --environment development --json api games.list --params '{"pageSize":100}'
```

`status` does not initialize storage or start a daemon. Native API commands can start the builtin daemon on demand. Explicit game configuration adds verified game packages, not business services. A directory marker prevents an unmarked legacy database directory from being initialized as runtime storage. Each data directory owns one daemon and an authenticated mode-`0600` Unix socket or owner-only Windows named pipe. On Unix, keep fixture paths short enough for the local socket path limit.

### Independently signed game plugins

Each game scope selects one self-contained game package: its worker, descriptor and required resources. UI, remote sources, Library, Agent, backup, engine libraries and standalone resource packages are not independently activated.

Packages live under `artifacts/modules/sha256/<digest>` and are never modified in place. `module.json` declares the exact target, host API, worker contract, permissions and complete file identities; `signature.json` authenticates the canonical package digest. Trusted signing keys remain required for every game package. Development-only keys are accepted only in explicitly isolated development runtimes.

Workers use framed inherited stdin/stdout and private stderr logs. The host checks the signed handshake and health, admits the exact game provider, and keeps old packages/artifacts pinned by pending tasks and installation receipts. The worker computes verified outputs and a deployment plan; the host owns approval, atomic file commits, recovery and uninstall.

### vNext durable jobs and actions

`runtime/jobs` owns durable operation registration in the isolated kernel SQLite database. A new mutation and its idempotency mapping commit before its `jobId` is returned. Per-job event sequences are monotonic; `jobs.watch` reads retained events after a cursor and returns the authoritative snapshot plus `resetRequired` when the cursor predates bounded retention. `jobs.get` remains available after a client disconnect or daemon restart.

Cancellation is a persisted request, not a worker kill: `jobs.cancel` moves active work to `cancelling`, and only `cancel_at_safe_point` records the terminal `cancelled` state. Commit entry is persisted separately; an interrupted or unknown commit becomes `recovery-required` instead of restarting from the beginning. Workers may persist resumable checkpoints, which startup recovery validates before requeueing.

Approval is also durable. `actions.get` and `actions.approve` preserve the action revision and response across reconnects. Consumption verifies the exact target, target revision, plan digest, module-lock digest, expiry, and response schema before work resumes.

```shell
# Exercise registration replay, event retention, cancellation safe points,
# approval bindings, checkpoints, unknown commits, IPC reconnect, and restart.
cargo test -p mintcat-runtime-jobs -p mintcat-daemon
```

### vNext network, credential, and interaction broker

`runtime/broker` is the transport boundary for builtin source providers. Every request declares exact HTTP(S) origins, redirect limits, response bounds, cancellation, and a direct/forward route policy. Credentials are opaque keychain references injected only on declared origins; caller-supplied authorization, cookie, proxy-authorization, and API-key headers are rejected. Redirects strip credentials outside their scope, forwarding requests never carry provider credentials, and diagnostics remove query strings and secrets.

Browser authorization, browser-mediated downloads, and file selection are durable `ActionRequired` transitions, not renderer callbacks. The host validates every browser navigation and callback against declared origins or an explicit custom callback scheme. File paths and browser callback URLs stay in the protected broker grant table; command consumers receive one-time opaque grants, and the broker wipes the protected resource when consumed. A host can explicitly reject or close an interaction, which rejects the action and fails the bound job with `INTERACTION_REJECTED`. Headless jobs remain queryable in `awaiting-approval` and resume after a host completes the same bound action.

```shell
# Exercise local HTTP redirect/proxy/cancellation policy, credential isolation,
# browser navigation policy, and durable one-time file/callback grants.
cargo test -p mintcat-runtime-broker
```

### vNext source providers

`modules/source/api` defines the only source-provider interface used by vNext search, detail, version, dependency, and download-resolution workflows. Source results use stable `ModRef` identities; a short-lived URL is returned only in a `DownloadDescriptor`, never persisted as a version identity.

Configured mod.io, ModCat and Nexus providers run inside the daemon with the real `NetworkBroker`; no source workers are installed. `sources.list` reports configured providers, while verified game descriptors supply bindings. Local input uses a host-authorized grant and a real game binding, not a pretend remote provider.

`modules/source-modio` maps an explicit local game binding to the numeric mod.io game ID. It provides live cursor pagination, sort mapping, detail/review/tag normalization, version and dependency enumeration, URL parsing, and exact remote-file resolution through `runtime/broker`. Stable versions use `file:<remoteFileId>`; selecting an old version cannot silently resolve the latest file. Provider credentials remain broker-owned, and download URLs must stay within the configured CDN origin set.

`modules/source-modcat` keeps ModCat’s `ModId`, `VersionId`, and `FilesId` semantics inside the provider. Game descriptors supply separate ModCat IDs (for example `drg` and `drgrc`), so search/detail results cannot cross DRG and Rogue Core bindings. Every version retains its exact file identity; authenticated download headers and credential references travel only in the bounded `DownloadDescriptor`, not in Library metadata. Empty or blank optional `PicUrl` values are normalized to missing images in search and detail metadata, so absent thumbnails cannot invalidate a result page.

`modules/source-nexusmods` keeps Nexus game-domain, mod ID, and file ID semantics inside the provider. GraphQL search uses game-bound live cursors and retains partial GraphQL failures as structured warnings. REST detail/file lookup never substitutes `latest` for an explicit file ID. API keys remain broker-owned; premium or browser-gated downloads return a durable `browser-download` action whose navigation and callback scopes are host-enforced. A captured `nxm:` callback must match the bound game/mod/file and be unexpired before its short-lived key can resolve a constrained CDN descriptor.

`modules/source-local` imports only host-authorized file or directory grants. Files are streamed into the content-addressed artifact store; directories are copied through a no-follow, bounded, two-pass snapshot and encoded as a deterministic directory artifact. PAK, ZIP, unpacked `Content`, JavaScript, and native payloads are classified without persisting the original path. ZIP inspection rejects traversal, absolute/drive/ADS names, links and special files, case-insensitive collisions, excessive depth/count/size/expansion, and compression bombs. Script/native counts remain explicit approval risks.

Each source task freezes the application's exact executable digest, source configuration and verified game descriptors. After the executable changes, an interrupted remote download enters `recovery-required` and must be cancelled and resubmitted. Legacy source-worker generations are recovery inputs only; builtin code does not silently resume their network work. Authorized local input and already-committed Library journals retain their original recovery semantics.

```shell
cargo test -p mintcat-source-modio -p mintcat-source-modcat -p mintcat-source-nexusmods -p mintcat-source-local
```

### vNext shared transfers

`runtime/transfers` separates immutable file identity, physical attempts, and operation/Profile consumers. Stable sharing keys include source, game, Mod, resolved version, remote file, and byte variant; signed download URLs remain transient broker inputs. Sources without a trustworthy fixed file identity use isolated transfers and deduplicate only when `ArtifactStore` publishes the final SHA-256 artifact.

Every attempt writes a new private `.part` file. A checkpoint is resumable only with a persisted strong ETag or `Last-Modified` validator, an exact byte offset, and a matching `206 Content-Range`; a changed validator or full `200` response truncates the new attempt before consuming current bytes. Transfer events keep monotonic per-transfer sequences and separate `downloading`, `verifying`, `publishing`, and `succeeded`, so receiving all bytes is not completion.

Consumers are durable rows. Detaching one consumer never cancels a transfer still needed by another; detaching the last consumer cancels active network I/O. Successful publication atomically records the immutable artifact and persistent consumer references before exposing a read grant. Restart marks in-flight attempts interrupted, preserves only validator-bound checkpoints for a later fresh descriptor, and never exposes staging paths.

```shell
# Exercise shared cancellation, concurrent versions, validator-bound resume,
# changed remotes, phase ordering, crash recovery, persistent references, and digest deduplication.
cargo test -p mintcat-runtime-transfers
```

### vNext download-and-apply transactions

`runtime/downloads` is the single coordinator between source providers, shared transfers, the artifact store, durable jobs, and the Library owner. `mods.setVersion` resolves one exact version/file plan and commits the operation plus outbox before returning its `jobId`; it does not change the requested or effective Profile version early. `cache-only` records verified content without taking a Profile-content lease, while `prepare-and-apply` holds the Profile lease and switches every declared binding and version selection in one fenced Library transaction only after every item has a live verified grant.

Verified items from a failed batch remain cache entries, but the Profile keeps all prior bindings and its prior content revision. Cancellation revokes Library commit authority before detaching transfer consumers. Provider interactions remain durable actions bound to the operation plan and its frozen authority. Safe checkpoints resume with a fresh fence only while source identity still matches; a crash after the Library commit is reconciled from the recorded operation result instead of applying a second revision.

```shell
# Exercise cache/apply separation, batch rollback, shared multi-Profile files,
# durable interactions, cancellation races, restart, and commit reconciliation.
cargo test -p mintcat-runtime-downloads
```

### vNext Profile list and download overlay

The desktop keeps revision-keyed Library rows separate from `profiles.contentState`. That read returns one transactionally consistent content capability plus active operation, job, item, transfer, attempt, sequence, phase, and byte overlays. Transfer data can update only the pending-version/progress surface for an existing `profileModId`/incarnation; it cannot replace display names, enabled state, order, applied version, or create a row.

Checkboxes, batch actions, drag/drop, action menus, keyboard moves/removal, and version selection consume the same backend-owned `writable` capability. Profile switching, search, task inspection, cancellation, and shared-name editing remain available while content is busy. A completed content revision invalidates the query cache; the UI never promotes a pending version before the fenced Library commit.

```shell
pnpm test
VITE_MINTCAT_VNEXT_DATA_DIR=./target/dev-data pnpm --filter @mintcat/desktop-vnext build
```

### vNext game-file transactions

`runtime/file-transactions` commits verified artifact grants without giving game modules raw artifact paths or independent write protocols. It locks the installation by physical root identity, rejects unsafe or aliased target paths, checks the expected old digest, target process, free space, and same-filesystem staging, then persists each backup and replacement step in the kernel journal. A synced `COMMITTED` marker is the commit decision; the receipt index can be rebuilt from the root-local receipt after a process exit.

Recovery rolls an uncommitted apply back only when observed digests still match the journal. Unknown or externally changed bytes enter `recovery-required` instead of being overwritten. Cancellation before apply returns `cancelled`; cancellation after the first filesystem mutation is deferred and the returned result states whether commit completed. Uninstall restores or removes only receipt-owned, unchanged files, preserves modified and foreign files, and reports each category.

```shell
# Exercise physical-root exclusion, grant/path/process checks, every apply mutation,
# marker/index recovery, cancellation truthfulness, and receipt-owned uninstall.
cargo test -p mintcat-runtime-file-transactions
```

### vNext Unreal PAK engine

`modules/engine-unreal-pak` accepts immutable `ArtifactGrant` inputs in PAK, single-PAK ZIP, or versioned directory-snapshot form. It inspects index metadata, normalizes portable paths, reports first-wins conflicts in caller order, and emits a verified content-addressed PAK artifact plus a complete file/digest/winner manifest. It never receives a game installation path. Entry payloads stream into a bounded temporary file and are memory-mapped only for repak's contiguous writer API; an entire input archive is not copied into process memory.

`algorithms/unreal-pak` owns PAK parsing, path/case normalization, deterministic version-11 writing, duplicate rejection, and explicit codec metadata. Output supports uncompressed or Zlib PAKs. Inputs using repak's built-in uncompressed/Zlib/Gzip/Zstd/LZ4 readers are accepted; encrypted indexes and Oodle builds are rejected until a keyed/approved codec artifact contract exists. The same ordered inputs, reference casing, target, and plan hash produce the same artifact digest.

```shell
# Exercise baseline PAK/ZIP fixtures, directory artifacts, first-wins conflicts,
# deterministic compressed output, verification, traversal, corruption, and unsupported targets.
cargo test -p mintcat-unreal-pak -p mintcat-engine-unreal-pak
```

### vNext UE4SS loader and signed resources

`modules/loader-ue4ss` is a shared Rust library that converts controlled artifacts into a restricted `DeploymentManifest`; it never receives an installation root or extracts an archive into a game. Game packages bind resource files to explicit targets, lock resource ID/version/package digest/publisher/signing key, and include those locks in executable-file approvals. Rogue Core settings add UE 5.6 and both required hooks; DRG keeps its own template. UE4SS is not a separately activated plugin.

JS ZIPs and directory snapshots publish only the single selected `js/` tree, native inputs must be PE images marked as DLLs, and audio ZIPs must contain exactly one safety-checked PAK. Portable paths, case and parent/file collisions, links, compression ratios, entry sizes, and aggregate sizes are checked before a target is emitted. Manifests contain exact owned files only; receipt-driven file transactions perform uninstall, so shared `ue4ss` or `mods` directories are never deletion targets.

```shell
# Exercise signed resource locks, DRG/RC settings, JS/DLL/audio target scopes,
# archive/path rejection, approval binding, and cross-plan collision handling.
cargo test -p mintcat-loader-ue4ss
```

### vNext Deep Rock Galactic adapter

`modules/game-drg` is an independently buildable `mintcat.game-adapter@2` worker. Its descriptor owns Steam/Xbox layouts, exact process names, source game keys, DRG settings schema, and signed patch/optional UE4SS resource requirements. Installation validation fingerprints the exact main PAK and rejects unknown layouts; build plans bind that fingerprint, Profile revision, module/resource locks, ordered artifact inputs, compression, and UE4SS selection.

The builder stages immutable grants away from the selected game root, produces the legacy-compatible DRG mod PAK, verifies every planned output through the Unreal engine, and publishes content-addressed artifacts. Deployment requires the signed DRG patch plan, exact per-target preconditions, and native-code approval for the hook DLL. Legacy detection reports only recognized takeover files; audio-name matches and shared loader directories remain foreign.

DRG installation-path algorithms, Blueprint patches, raw assets, and compatibility PAK integration live in `modules/game-drg/src/integrator`. Rogue Core owns its separate `modules/game-rogue-core/src/integrator`, including the UE 5.6 UE4SS policy. `algorithms/mintcat-integrator-core` contains only shared `common`, `uasset_utils`, progress and validation helpers; it has no game-builder features or game-specific exports. Each game's normal dependency graph excludes the other game.

```shell
# Exercise descriptor/discovery, settings isolation, representative PAK content,
# signed-patch boundaries, deploy planning, and non-destructive legacy takeover.
cargo test -p mintcat-game-drg
```

### vNext Rogue Core adapter

`modules/game-rogue-core` is a separate `mintcat.game-adapter@2` worker for Steam release and Playtest installations. Both variants use the explicit `RogueCore/Content/Paks/RogueCore-Windows.pak` layout, Rogue Core process names, Nexus/ModCat/local source keys, a UE 5.6 compatibility declaration, and independent signed patch/UE4SS resource locks. Validation accepts only supported unencrypted V11 PAKs whose `RogueCore/AssetRegistry.bin` can actually be read and parsed; it never falls back to a plausible empty result.

Its artifact-only builder creates `RogueCore-Windows_Mods.pak` in isolated staging, verifies published PAK outputs, and emits a clean zero-input build when previously selected Mods disappear. Compression and UE4SS options are bound to `game:rogue-core`; a DRG snapshot is rejected. Running-game checks consume the validated game descriptor's exact process names rather than a shared `FSD.exe` default.

```shell
# Exercise release/Playtest discovery, version rejection, representative and
# zero-input builds, process checks, settings isolation, and takeover safety.
cargo test -p mintcat-game-rogue-core
```

### vNext unified pack, install, takeover, and uninstall

`modules/install` resolves one immutable Profile snapshot into ordered, verified game inputs and one pinned module closure. Pack and install call the same game-adapter BuildPlan; pack returns content-addressed outputs without writing the selected game, while install persists an expiring, hashed DeployPlan whose approval binds the exact output digests, target preconditions, and risks. A Library content guard prevents Profile edits from crossing the final revision/order check, and the file-transaction runtime supplies physical-root exclusion, process checks, journaled replacement, recovery, and receipts.

`WorkerGameAdapter` routes to the selected game's signed worker. `build.prepare` performs engine inspection/conflict reporting and loader planning using shared Rust code. Prepared output grants are authenticated and adopted by the host before approval and installation; file transactions, journals and receipts remain shared.

The worker contract is version 2. New releases use one immutable game package per scope, without a separate engine/resource/UI closure. Historical tasks and receipts retain their exact original packages and resource bindings for recovery and uninstall; they are never redirected to a newer adapter. Licensed game data and authentic required archives remain necessary for real installation.

Known legacy outputs without a receipt require an exact-digest takeover plan and explicit approval. Updating an installation never deletes the last known-good output first. Each successful replacement adds a receipt, and unified uninstall replays the active receipt chain newest-first to restore the original pre-Mintcat bytes or remove files that were originally absent; modified and foreign files remain untouched. Results are returned by plan/apply calls and can be replayed from the persisted plan/receipt instead of inferred from global UI events.

```shell
# Exercise real DRG/RC test builders through one no-GUI workflow, first-wins
# summaries, pack isolation, approval/takeover, replacement receipts, cancellation,
# stale inputs/targets, client disconnect, physical-root exclusion, and uninstall.
cargo test -p mintcat-install
```

### vNext Library owner, operation gate, and legacy import

The daemon owns one Library writer at `state/mintcat.library/library.sqlite`; GUI, CLI, workers, and other modules do not receive a SQL connection. Its schema separates game definitions from installation instances, profiles/groups/memberships from source identities, version metadata from immutable content, and global settings from per-game settings. Profiles and memberships carry revision, content revision, incarnation, and monotonic lease-epoch fields.

`LibraryStore::start_download_operation` checks backend authorization, owner scope, target incarnation, expected content revision, and immutable source/version inputs in one immediate transaction. For `prepare-and-apply`, the same commit advances the Profile fence, acquires its content lease, registers idempotency, pins every item, and appends the durable outbox event. `cache-only` instead owns an operation-scoped lease and remains valid if the originating Profile is later edited or deleted. Normal content edits and deletes check the Profile lease in their own write transaction. Apply requires a live `ArtifactGrant`; cancellation revokes the matching lease before old generations, fences, or late callbacks can commit.

The daemon replays Library outbox rows into `runtime/jobs` with the Library operation ID as the kernel idempotency key, then `runtime/downloads` executes that fixed plan. It marks an event delivered only after durable job registration. Restart or heartbeat expiry changes an active lease to `suspect`/`recovery-required`; safe job checkpoints requeue and resume under a fresh fence. Internal owner recovery snapshots retain operational state; portable user backup format2 excludes executable outbox, mutation receipts and leases rather than transplanting task authority.

`LibraryStore::import_legacy` opens an explicitly selected legacy database read-only and reads one SQLite transaction, so committed WAL rows are included without copying or modifying the old database. The importer records every legacy ID mapping, preserves order/enabled/`usedVersion`, and retains old cache paths and `installedHash` only as unverified migration hints—not artifacts or installation receipts. Non-empty OAuth values must be moved through a `CredentialVault`; only its opaque keychain reference can enter Library storage.

The legacy `rc` game key becomes canonical `rogue-core` / `game:rogue-core` during import, including linked profiles, Mods, installations, hints and per-game settings. Source-specific remote namespaces remain migration metadata; this is not a permanent game API alias.

Exports are deterministic logical snapshots. A synced temporary file is atomically linked to a new backup path; exports never overwrite a backup. Portable format2/Library4 archives restore into current Library5, including data-owner settings, without requiring retired stateless UI/source packages. Empty-owner admission, content identity, schema compatibility, credential exclusion and human approval remain mandatory; execution authority is never transplanted.

Cloud backup additionally requires a configured HTTPS service origin implementing `/v1/backups`, allowed origins for external download URLs, and a daemon-owned credential reference. The default local application has no cloud endpoint. Missing service configuration is reported as non-retryable `CLOUD_PREREQUISITE` with a safe UI message; offline export/import remains available. Backup and Agent Application API errors use the shared UI presentation rather than raw error JSON.

Library query and edit commands use the same owner and gate. `games.list`, Profile and group CRUD, paged Mod reads, shared rename, association-only remove, first-wins moves, batch enable/disable, scoped missing-local cleanup, and version downloads all require explicit game/Profile/member IDs. Mutations require idempotency keys and expected revisions; reads never create or activate a Profile. The native desktop bridge, product CLI, and Agent command catalog consume these generated contracts rather than opening Library storage.

```shell
# Exercise the checked-in v0.6 database copy, committed WAL visibility,
# identity/order/version migration, operation-gate races, fencing,
# credential references, durable outbox replay, and export/restore.
cargo test -p mintcat-library-service -p mintcat-daemon
```

```shell
# Call the typed Application API without a GUI. Mutations additionally require
# --idempotency-key and their contract-specific expected revision.
pnpm cli --data-dir ./target/dev-data --environment development api games.list --params '{"pageSize":100}'
```

## Product CLI and game development

`mintcat` connects to standalone `mintcatd`; the desktop need not be running. Builtin services require no signed baseline. Game plugins use an explicit schemaVersion=2 `runtime.json` containing game scopes, repository/trust/policy and source configuration.

```shell
pnpm build:cli
pnpm cli --help
pnpm cli --data-dir ./target/dev-data --environment development --json api games.list --params '{"pageSize":100}'
pnpm cli --data-dir ./target/dev-data --json jobs wait --job-id <job-id> --wait-timeout 30
```

Commands, help, pagination and JSON validation share the Application CommandSpec. Mutations require idempotency keys and expected revisions. `jobs.watch` returns retained events and the authoritative snapshot; jobs and approval bindings survive disconnects. `--yes` declares a human request, not approval of a durable action or permission to bypass a stale target.

```shell
pnpm game check modules/game-drg --target aarch64-apple-darwin
pnpm game pack modules/game-drg --target aarch64-apple-darwin \
  --key /path/to/game-signing.seed --output ./target/drg-package
pnpm game release --help
```

`check` and `pack` build only the selected game and validate its required signed payload. The official DRG and Rogue Core projects currently lack authentic `resources/drg-patch.zip` and `resources/rogue-core-patch.zip`; packaging deliberately refuses until those inputs are supplied. Generic `new/dev` scaffolding and a standalone UI host are retired.

For mechanism-only verification, `node tools/module-dev/smoke-games.mjs --help` describes isolated TEST-ONLY fixtures using real workers. It exercises check/pack/stage and actual worker authorization, and rejects marker-only archives before deployment. Those fixtures are not installable game resources.

# Packaging and runtime data

### Local macOS app build

```shell
pnpm package:app
# Optionally include explicitly verified games; never rebuild them here:
pnpm package:app --game-config /path/to/runtime.json --output target/my-app-build
pnpm package:app --help
```

Requires macOS and installed JavaScript/Rust toolchains. The default is a fresh builtin-only application with no games, no baseline and no publisher signing key. `--game-config` accepts schemaVersion=2 game-only scopes and reuses their verified packages, trust, policy and source configuration.

The builder compiles static UI plus desktop, CLI, daemon and verifier for the explicit host target using the shared Cargo cache. It pins binaries with streamed SHA-256, verifies an immutable stage, assembles and checks an ad-hoc-signed `.app`. Output is `target/app-builds/<timestamp>-<pid>/assembled/bundles/macos/MintCat.app`; existing outputs are not replaced.

Business updates arrive with the application; game updates remain independently signed. Assembly does not change product versions, reset data, read publisher keys, publish pointers or launch the GUI. Native startup and interaction are separate from signing and embedded-asset checks.

### Verified release assembly

```shell
pnpm build
target/debug/mintcat-module stage --manifest ./release-input.json
MINTCAT_MODULE_BIN=./target/debug/mintcat-module node scripts/package-cli.mjs --manifest ./assembly.json
# GUI flavors: scripts/build-release.sh, build-portable.mjs, build-fixed-webview2.mjs.
```

Release inputs and runtime manifests use schemaVersion=2. Assembly inputs require an explicit signing mode and stage-manifest SHA-256 pin. Exact target, builtin binaries and selected game package/scope identities are verified. Builtin-only inputs omit game trust/repository resources. Portable and fixed WebView2 flavors are Windows MSVC-only; fixed WebView2 is a pinned native resource and forbids system-runtime fallback. Production signing/notarization requires explicit credentials.

A complete GUI bundle contains static Workbench assets, `runtime.json`, its immutable release manifest and sibling `mintcat`, `mintcatd` and `mintcat-module`. Game configurations additionally include verified baseline/trust resources. Do not ship a bare desktop `tauri bundle`. The assembler checks embedded assets; acceptance also requires native launch, daemon health and changed interactions.

Shipped CLI/daemon discover the same resource configuration as the desktop: executable siblings for standalone/Windows, `Contents/Resources` in a macOS app, and product resources on Linux. A partial sibling release is an error.

Bundled game pins initialize missing scopes, but replacing the app does not advance existing scopes. Updates use `updates.stage` → `updates.prepare` → `updates.activate` → `updates.confirm`, with expected revision, durable idempotency and human activation approval. Game routes publish atomically after confirmation; historical tasks and receipts keep their old closures. There is no platform/UI activation barrier.

Source browsing is separate from binary-download authority. mod.io and Nexus can read with empty `downloadOrigins`, but authenticated operations still need broker-managed credentials. Missing authorization is explicit; users may select another configured, game-bound source. UI diagnostics keep credentials and provider response text out of default messages.

`kernel.sqlite3` stores durable runtime state, `runtime/` owns transport/logs, `state/` owns business data and maintenance evidence, and `artifacts/` owns immutable content/packages. Legacy databases are read-only import inputs, never a second active writer. Use `--data-dir` for isolated validation.

TEST-ONLY local staging and worker execution do not establish licensed-game installation or a clean Windows machine matrix. Authentic patch/optional UE4SS resources and an authorized Windows host remain prerequisites for those checks.

