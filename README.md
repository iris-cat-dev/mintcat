# MINTCAT
![Logo](apps/desktop/src/assets/icon.ico)

# Introduction
MintCat manages Deep Rock Galactic and Rogue Core Mods through a standalone Rust daemon, a typed product CLI, independently signed workers/resources/UI modules, and a Tauri Workbench host.


# Features
## 1. Mod Management:
- Add mods from mod.io
- Add mods from local files
- Update mods
- Mod rename

2. mod.io Search, Download, Update


# Architecture
- `apps/desktop`: contribution-based Workbench and independent rescue controls; no business SQL or game installer in the renderer.
- `apps/cli`: `mintcat`, generated domain commands and a canonical Application API escape hatch.
- `runtime/daemon`: `mintcatd`, authenticated local IPC, durable jobs/actions, controlled brokers and scoped module admission.
- `modules`: the Library single writer, source providers, Unreal PAK engine, independent game adapters, installation orchestration, Agent, Proxy and backup.
- `runtime/release` / `runtime/updates`: verified packages and complete per-scope closures, activation barriers, probation, recovery, rollback and leases.
- `packages/contracts`: one CommandSpec/schema source for the Rust and TypeScript SDKs.
- The active desktop sources are `apps/desktop/src` and `apps/desktop/src-tauri`. Retired root-level `src`, `src-tauri` and `public` directories have been removed; do not recreate them as compatibility entrypoints. Native bundle icons live under `apps/desktop/src-tauri/icons`; the frontend logo lives under `apps/desktop/src/assets` and is imported through Vite's asset pipeline, with no shared public directory.
- Artifact GC combines durable Library content, transfer references, unexpired prepared/applying install plans, active receipts and module/task/service/UI/rollback leases; expired unused plans release their artifacts.
- The Workbench follows the supplied dark/light glass UI references: 172px labeled sidebar, 72px game/search/Profile toolbar, compact source-grouped mod table, pink actions and switches, and translucent rounded surfaces over a native window material—not a simulated wallpaper or whole-window opacity. Light uses a milky tint; Dark uses blue-gray. Sticky table headers and elevated menus remain opaque enough to read. Local import and download management are real Library routes; missing category/compatibility/certification metadata stays “—”. Game settings, Proxy and backup remain under Settings; Agent retains its 680px drawer.
- Native appearance follows the saved UI theme. macOS uses an Overlay title bar with real traffic lights and a public behind-window `NSVisualEffectView` using `UnderWindowBackground`, not a dense whole-window Sidebar fill. CSS tint layers stay light: window/panel alpha is 18%/16% for Light and 32%/14% for Dark; these are separate from the system material and are not whole-window opacity. Transparent WKWebView still requires Tauri's `macos-private-api` feature and `macOSPrivateApi`, so this configuration is not Mac App Store compatible. Windows 11 22H2+ uses public DWM Acrylic with checked application results; older Windows, Linux, unavailable effects and reduced transparency keep a solid surface. macOS also observes system accessibility changes. Browser preview always uses the solid fallback.
- Signed UI modules share one host React/ReactDOM/Ant Design runtime. Initial React commit must succeed before a page is admitted; later render failures remain isolated from the frame. Business requests still go through the Application API, and expected API errors do not tear down the page.

Current design and acceptance limits: [modular runtime architecture](docs/design/modular-runtime-architecture.md) and [execution record](docs/design/modular-refactor-todo.md).


# Build & Dev

```shell
pnpm install
cargo build --workspace --all-targets
pnpm check
pnpm dev                 # Browser preview only; no native module/data authority.
pnpm desktop:native      # Actual Tauri host.
```

The old desktop, renderer task executor, CLI bridge and DLL installation entrypoints are retired. Every CLI/daemon development entry requires an explicit data directory through `--data-dir` or `MINTCAT_DATA_DIR`; it never infers the legacy `com.mint.cat` database path.

Local development bundles use `com.mint.cat.vnext.local`. On macOS, a release-mode local app defaults to `~/Library/Application Support/com.mint.cat.vnext.local/vnext`; a debug host uses `isolated-vnext-development` under the same application directory. Earlier `com.mint.cat.vnext.dev` data and production data are not migrated implicitly. Keep local module signing seeds outside shipped artifacts and build-cleanup directories, with mode `0600`; retain them for subsequent signed updates. Replacing a pinned root is not a repair for missing Library data and requires an explicit trust migration or a separate data directory.

Signed game support does not imply an enrolled Library game. Until an installation is registered, the Library page shows setup guidance, and neither Library nor Search requests that game's profiles. Online Search can still browse signed sources before enrollment; adding mods and choosing a target Profile remain disabled until a Profile exists. Existing enrolled games still load their profiles and content through the normal Application API.

```shell
target/debug/mintcatd --data-dir ./target/dev-data --environment development init
target/debug/mintcat --data-dir ./target/dev-data --json status

# Business modules require an explicitly authenticated baseline configuration.
MINTCAT_RUNTIME_CONFIG=./runtime.json target/debug/mintcat --data-dir ./target/dev-data --environment development --json doctor
MINTCAT_RUNTIME_CONFIG=./runtime.json target/debug/mintcat --data-dir ./target/dev-data --environment development --json api games.list --params '{"pageSize":100}'
```

`status` does not initialize storage or start a daemon. An unconfigured daemon offers diagnostics/rescue, not an unsigned fallback Library or game installer. The browser preview runs on port 1430 and explicitly lacks native authority. A directory marker prevents an unmarked legacy database directory from being initialized as modular-runtime storage. Each data directory owns one daemon and an authenticated local endpoint: a mode-`0600` Unix socket or an owner-only Windows named pipe.

### vNext module runtime

Module payloads are published under `artifacts/modules/sha256/<digest>` and are never updated in place. `module.json` owns the target, host API, worker entry, contracts, permissions, and complete file digest list; `signature.json` is an Ed25519 signature over the canonical package digest. Every environment requires a trusted key. Keys registered as development-only are accepted only by an explicitly isolated `development` runtime and never by `production`.

Service workers use inherited stdin/stdout pipes for the framed protocol and reserve stderr for logs under `runtime/module-logs`. The host verifies the worker handshake against the signed manifest, performs a health request before registration, binds providers by scope/contract/version, drains active requests before shutdown, and retains artifact leases until the worker or large-file grant is released.

```shell
# Build and exercise the real example worker, signature/target checks,
# multi-version scoped providers, crash isolation, grants, leases, and GC.
cargo test -p mintcat-runtime-artifacts -p mintcat-runtime-supervisor
```

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

`runtime/broker` is the only vNext transport boundary for source modules. Every request declares exact HTTP(S) origins, redirect limits, response bounds, cancellation, and a direct/forward route policy. Credentials are opaque keychain references and are injected only on their declared origins; caller-supplied authorization, cookie, proxy-authorization, and API-key headers are rejected. Redirects are followed manually, credentials are stripped outside their credential scope, forwarding proxy requests never carry provider credentials, and diagnostics remove query strings and secret values.

Browser authorization, browser-mediated downloads, and file selection are durable `ActionRequired` transitions, not renderer callbacks. The host validates every browser navigation and callback against declared origins or an explicit custom callback scheme. File paths and browser callback URLs stay in the protected broker grant table; command consumers receive one-time opaque grants, and the broker wipes the protected resource when consumed. A host can explicitly reject or close an interaction, which rejects the action and fails the bound job with `INTERACTION_REJECTED`. Headless jobs remain queryable in `awaiting-approval` and resume after a host completes the same bound action.

```shell
# Exercise local HTTP redirect/proxy/cancellation policy, credential isolation,
# browser navigation policy, and durable one-time file/callback grants.
cargo test -p mintcat-runtime-broker
```

### vNext source providers

`modules/source/api` defines the only source-provider interface used by vNext search, detail, version, dependency, and download-resolution workflows. Source results use stable `ModRef` identities; a short-lived URL is returned only in a `DownloadDescriptor`, never persisted as a version identity.

`modules/source-modio` maps an explicit local game binding to the numeric mod.io game ID. It provides live cursor pagination, sort mapping, detail/review/tag normalization, version and dependency enumeration, URL parsing, and exact remote-file resolution through `runtime/broker`. Stable versions use `file:<remoteFileId>`; selecting an old version cannot silently resolve the latest file. Provider credentials remain broker-owned, and download URLs must stay within the configured CDN origin set.

`modules/source-modcat` keeps ModCat’s `ModId`, `VersionId`, and `FilesId` semantics inside the provider. Game descriptors supply separate ModCat IDs (for example `drg` and `drgrc`), so search/detail results cannot cross DRG and Rogue Core bindings. Every version retains its exact file identity; authenticated download headers and credential references travel only in the bounded `DownloadDescriptor`, not in Library metadata. Empty or blank optional `PicUrl` values are normalized to missing images in search and detail metadata, so absent thumbnails cannot invalidate a result page.

`modules/source-nexusmods` keeps Nexus game-domain, mod ID, and file ID semantics inside the provider. GraphQL search uses game-bound live cursors and retains partial GraphQL failures as structured warnings. REST detail/file lookup never substitutes `latest` for an explicit file ID. API keys remain broker-owned; premium or browser-gated downloads return a durable `browser-download` action whose navigation and callback scopes are host-enforced. A captured `nxm:` callback must match the bound game/mod/file and be unexpired before its short-lived key can resolve a constrained CDN descriptor.

`modules/source-local` imports only host-authorized file or directory grants. Files are streamed into the content-addressed artifact store; directories are copied through a no-follow, bounded, two-pass snapshot and encoded as a deterministic directory artifact. PAK, ZIP, unpacked `Content`, JavaScript, and native payloads are classified without persisting the original path. ZIP inspection rejects traversal, absolute/drive/ADS names, links and special files, case-insensitive collisions, excessive depth/count/size/expansion, and compression bombs. Script/native counts remain explicit approval risks.

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

Verified items from a failed batch remain cache entries, but the Profile keeps all prior bindings and its prior content revision. Cancellation revokes Library commit authority before detaching transfer consumers. Provider interactions remain durable actions bound to the operation plan and module digest. Checkpoints resume with a fresh fence after restart; a crash after the Library commit is reconciled from the recorded operation result instead of applying a second revision.

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

`modules/loader-ue4ss` converts controlled artifacts into a restricted `DeploymentManifest`; it never receives an installation root or extracts an archive into a game. Signed module packages bind every resource file to one explicit target, lock resource ID/version/package digest/publisher/signing key per game, and attach that lock to approval requirements for executable files. Rogue Core settings deterministically add UE 5.6 and both required hooks while Deep Rock Galactic keeps the signed template unchanged.

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

`WorkerGameAdapter` selects providers from the signed game's declared requirements inside the exact closure, rather than requiring every game to bind Unreal. The game worker's `build.prepare` performs engine inspection/conflict reporting and loader planning; both current games explicitly reuse `modules/engine-unreal-pak/src/game_inputs.rs` and the shared UE4SS loader. Prepared deployment files carry authenticated output grants and are adopted into the host artifact store before approval and installation planning. File transactions, approval bindings, journals and receipts remain shared.

This worker wire contract is version 2, not an alias for version 1. Publish newly signed DRG/RC service/UI packages under new immutable package versions and regenerate their scope locks before starting the new host; do not mix an `@1` baseline with `@2` workers or mutate admitted package bytes. Drain `@1` in-flight work under its original generation rather than redirecting its frozen closure. The existing plans still describe the two supported Unreal games; removing implicit engine binding does not provide a Unity installer or establish a Windows real-game compatibility matrix.

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

Exports are deterministic logical snapshots from one read transaction. A synced same-directory temporary file is atomically linked to a new backup path; exports never overwrite an existing backup. Restore requires an empty Library database.

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
pnpm cli -- --data-dir ./target/dev-data --environment development api games.list --params '{"pageSize":100}'
```

## Product CLI and module development

`mintcat` connects to the standalone `mintcatd`; the desktop does not need to be running. The daemon loads an explicitly configured, authenticated offline baseline. An unconfigured daemon exposes diagnostics/rescue, not a hidden unsigned Library or game implementation.

```shell
pnpm build:cli
pnpm cli -- --help
MINTCAT_RUNTIME_CONFIG=./runtime.json pnpm cli -- --data-dir ./target/dev-data --environment development --json api games.list --params '{"pageSize":100}'
pnpm cli -- --data-dir ./target/dev-data --json jobs wait --job-id <job-id> --wait-timeout 30
```

Domain help, pagination, completion and JSON validation come from the same CommandSpec as the Application API. Mutations require an idempotency key and their expected revisions. Durable jobs and actions survive client disconnects; an approval is bound to its frozen target and revision. `--yes` does not let an Agent or a stale action bypass human-only approval. `api jobs.watch` returns one canonical result envelope; `jobs watch`, `jobs wait`, or an explicit `--wait`/`--events` request follow jobs using JSON-line events and authoritative snapshots. Diagnostics go to stderr.

Module development and release use one executable:

```shell
target/debug/mintcat-module new ./target/example-module --id local.example --target aarch64-apple-darwin
target/debug/mintcat-module dev ./target/example-module --dev-root ./target/example-dev --key /path/to/test-only.seed
target/debug/mintcat-module check ./target/example-module
target/debug/mintcat-module pack ./target/example-module --key /path/to/signing.seed --output ./target/example.mcpkg
target/debug/mintcat-module release --help
```

`mintcat-module` incrementally builds a signed worker/UI generation, invokes the real framed worker protocol, serves the owning UI package with the shared SDK, and drains the old process before replacement. `release compose/verify/publish` creates and checks exact scope bindings, signatures, target triples and immutable package bytes in a selected local repository; it never edits an online release pointer implicitly. The runnable `new` example is not an implementation of an existing game.
The isolated UI host mounts one authenticated `pages` contribution at a time. Its page links select the route in the URL hash and reload with the same scoped session state; additional Library pages no longer create multiple renderers in one container. The standalone worker host does not grant Application mutation authority.

The local workflow was exercised through `new/dev/check/pack/compose/verify/publish` and pinned standalone-CLI staging on `aarch64-apple-darwin`. This does not establish Windows installation, production signing, real-game compatibility or native Workbench visual acceptance.


# Packaging and runtime data

### Local macOS app build

```shell
pnpm package:app
# Explicit signed baseline and fresh output directory:
pnpm package:app --baseline /path/to/MintCat.app --output target/my-app-build
pnpm package:app --help
```

Requires macOS, the installed JavaScript/Rust build toolchains, and a signed baseline for the host architecture. By default, the script reuses `target/release/bundle/macos/MintCat.app`; `--baseline` also accepts a complete immutable stage or an app's `Contents/Resources` directory. A missing/incomplete baseline fails before building rather than producing a desktop-only bundle.

`scripts/build-app.mjs` rebuilds the frontend and all four native executables, hashes the binaries, creates and verifies an immutable stage, and invokes the existing assembler for the `.app` target only. It verifies the final bundle signature and embedded UI. Output defaults to a fresh `target/app-builds/<timestamp>-<pid>/assembled/bundles/macos/MintCat.app`; existing outputs and the input baseline are never overwritten.

This is a local ad-hoc-signed build, not a production release. It reuses signed module versions, trust and source configuration without reading a signing seed, resetting databases, advancing active scopes or launching the GUI. Changes to business modules require a newly signed baseline supplied with `--baseline`; the script does not silently ship unsigned replacements. Actual native GUI startup remains an acceptance check separate from packaging.

### Verified release assembly

All release flavors use one verified immutable stage and assembler:

```shell
target/debug/mintcat-module stage --manifest ./release-input.json
MINTCAT_MODULE_BIN=./target/debug/mintcat-module node scripts/package-cli.mjs --manifest ./assembly.json
# GUI flavors: scripts/build-release.sh, build-portable.mjs, build-fixed-webview2.mjs.
```

GUI artifacts must embed the offline Workbench rather than require the development server:

```shell
pnpm build
cargo build -p mintcat-desktop-vnext --features custom-protocol
target/debug/mintcat-desktop-vnext --verify-embedded-ui
```

The assembler invokes that native self-check for GUI flavors and rejects a dev-only desktop before creating an output. This checks embedded assets, not native UI visual acceptance.

The embedded-UI self-check does not validate a runnable application. Do not distribute a bare `tauri bundle` of the desktop executable: release-mode startup also requires `runtime.json`, the immutable release manifest, the authenticated module baseline/trust resources, and sibling `mintcat`, `mintcatd`, and `mintcat-module` executables. Build a complete stage and use the assembler above, including for an ad-hoc-signed local `.app`. Verify the actual native launch, signed Workbench page admission, `modules.list`, and daemon diagnostics—not just CodeSign and embedded assets.

The repaired local application is `target/release/bundle/macos/MintCat.app`; its verified assembly inputs and complete bundle are under `target/mac-app-startup-fixed`. This rebuild reuses the retained 13-module signed baseline, existing trust root and confirmed scope releases without clearing or migrating the local data directory.

Inputs must pin the exact target, binaries, signed worker/resource/UI closure, scope locks and stage-manifest SHA-256. Portable and fixed WebView2 are Windows MSVC-only; fixed WebView2 verifies native files and forbids system-runtime fallback. Production signing/notarization requires explicit credentials. No release assembler silently builds its verifier or uses a developer's Cargo at runtime.

Shipped CLI/daemon discover the same relative runtime configuration as the desktop: executable siblings for standalone/Windows, `Contents/Resources` for a macOS app, and the compiled product resource directory on Linux. A partial sibling release is an error, not permission to choose another baseline.

Bundled scope pins bootstrap missing scopes; replacing the application does not silently advance scopes already registered in the data directory. A retained-data update uses the signed repository and explicit `updates.stage` → `updates.prepare` → `updates.activate` → `updates.confirm`, with an expected scope revision, durable idempotency keys and human approval for activation. Platform probation keeps the state-owner migration gate closed; native routing is published after confirmation, not while that gate is held.

The current local macOS source/error repack under `target/mac-source-errors-fixed/assembled-final/bundles` preserves the `com.mint.cat.vnext.local` data identity and existing signing root. Quit the old desktop before running `node target/mac-source-errors-fixed/activate-local-platform.mjs --yes`; the helper gracefully restarts only the daemon holding that data directory's lock when its executable differs, and explicitly confirms the signed platform update without clearing Library state. If a UI barrier blocks preparation, it rolls back its own preparation to release maintenance rather than fabricate snapshots or disposal acknowledgements. That failed candidate remains quarantined and requires a new signed release; replacing `.app` or deleting quarantine state is not an upgrade. `--data-dir` and `--app` select an explicit isolated fixture or installed application.

Source API browsing is independent of binary download authority: mod.io and Nexus accept an empty `downloadOrigins` for read operations, while provider resolution and host verification still deny unapproved downloads. The local source configuration has no mod.io/Nexus `credentialRef`; these authenticated providers require real broker-managed credentials, and missing/invalid authorization is reported clearly rather than hidden or replaced with an invented key. Users can explicitly switch to an enabled, game-bound alternative such as ModCat. Shared SDK UI error presentation keeps the Chinese cause/action visible and technical identifiers in collapsed diagnostics; provider response text, secrets and credential-bearing URLs are not exposed as default error messages.

CLI/daemon use the declared data directory; the desktop uses its declared isolated runtime directory. `kernel.sqlite3` lives at its root, `runtime/` contains local transport/logs, `state/` contains module-owned state and maintenance evidence, and `artifacts/` contains immutable content/packages. Legacy database paths are import inputs only, never a second active writer.

Local Darwin fixture staging does not establish Windows installation, production signing, real-game compatibility or native Workbench visual acceptance. External prerequisites remain explicit in the execution record.
