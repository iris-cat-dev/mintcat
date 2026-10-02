# Proxy feature module

Status: **retained experiment; further product integration cancelled on 2026-10-01**. Preserve the existing implementation and historical evidence, but exclude it from the current signed UI validation baseline and acceptance dependencies. System certificates, hosts, high-privilege cleanup and Proxy redistribution licensing are not prerequisites for the current UI/game-data work. This scope decision grants no redistribution license; any future resumption requires explicit authorization.

The existing Rust proxy implementation now lives in this signed module. It remains a real rustls/hyper HTTPS reverse proxy with Watt API rules, cached offline rules, custom rules, and DoH upstream resolution. It decrypts HTTPS traffic; use only on devices and traffic for which you have authorization. The migrated implementation preserves the original raw-IP acceleration upstream certificate-validation bypass; upstream TLS authenticity is therefore not guaranteed. Signed artifact verification is independent of that network policy.

## Source, license, toolchain

This repository's former `src-tauri/crates/mintcat-proxy` described itself as an independent implementation, using `https://api.steampp.net/api/Accelerate/All` for rules, not the SteamTools GUI, WinDivert or PAC implementation. That source was moved, not replaced by a fake service. Its README explicitly left LICENSE undefined and neither the source crate nor repository root supplied one. Production redistribution of this proxy therefore requires the project owner's license/provenance decision; do not infer an upstream license.

Observed host toolchain: rustc/cargo 1.99.0-nightly. Build from workspace root:

```sh
cargo build -p mintcat-module-feature-proxy --bin mintcat-proxy
```

Run the actual offline service smoke (isolated Test trust, real signed executable, approved local CA creation; no hosts or system trust mutation):

```sh
cargo run -p mintcat-module-feature-proxy --example offline -- target/debug/mintcat-proxy
cargo test -p mintcat-module-feature-proxy
```

Signed packaging uses `tools/release` and the existing Ed25519 ArtifactStore/release trust, not MD5 update manifests or arbitrary executables. Manifest id is `mintcat.feature.proxy`; the service component has `kind: service`, `runtime: mintcat.proxy-service/1`, `stateModel: daemon-owned`, and a signed executable entry. Include the built local ESM UI and `ui/contributions.json` in the exact signed file list.

## Daemon API and ownership

The daemon owns `ProxyServiceManager`, including Child and ModuleLease. `proxy.start` and `proxy.stop` are durable jobs; `proxy.status` reports real process state and health. Startup checks the signed component and matches ready/health responses to module digest, version, protocol and unique instance. Failed ready or health kills and reaps only the owned child. Stop never resolves an active newer binary and still works after revocation. Revocation rejects new starts. Closing or unloading the UI only disposes polling; it cannot delete the service or job. Loss of the daemon control pipe makes the service exit.

A service invocation takes only daemon-owned `--data-dir`, `--bind`, `--port`, `--offline`. Control uses bounded JSON-lines stdin/stdout; diagnostics are stderr. No PID kill, arbitrary executable, general shell, or caller-controlled certificate/hosts path API exists.

## Independent approvals and platform prerequisites

`proxy.privilege` creates a Required-confirmation JobStore action for exactly one operation: create local CA credentials, install certificate, remove certificate, apply hosts, cleanup hosts, or authorize a privileged listen port. Approval is bound to artifact and current credential/rule hashes. Only the separate trusted approval surface can approve; the feature UI cannot approve its own request. The restricted broker consumes that durable grant before acting.

Create credentials before start. Unprivileged offline operation needs `accelerate_cache.json` (an empty list is valid if custom rules exist) and `custom_rules.json` in the daemon-owned data directory. Private keys are 0600 and the directory 0700 on Unix. Startup never installs trust or rewrites hosts implicitly. Actual Steam hosts acceleration needs exact-domain hosts integration and port 443; approve hosts cleanup independently before applying hosts so daemon-owned cleanup remains authorized. Certificate trust removal is separately approved and targets the actual certificate, not every certificate sharing a name.

macOS authorized system integration requires a dedicated test device/session permitted to modify `/Library/Keychains/System.keychain` via `/usr/bin/security`, write `/etc/hosts`, and bind the requested port. Windows requires an administrator-authorized daemon/test account permitted to modify LocalMachine Root via fixed `C:\Windows\System32\certutil.exe` and write `C:\Windows\System32\drivers\etc\hosts`. No UAC/sudo arbitrary-shell fallback is provided; denial is a real error and partial system mutation leaves a durable recovery-required job. Linux uses fixed system CA paths and `/usr/sbin/update-ca-certificates` with equivalent root prerequisites.

Historical isolated proof: actual Application `proxy.privilege` → durable human approval → local CA creation → `proxy.start`/`proxy.status` → CA-verified HTTPS forwarding to an owned loopback HTTP upstream. Disabling the signed UI retained the same running service identity; `proxy.stop` closed its listener, and an occupied port produced a failed job with no running service. Evidence: `/tmp/mintcat-proxy-application.aK8GEy/evidence.json`. Offline rules were a legal controlled fixture; no system trust or hosts were modified. The documented unverified system/licensing scenarios are conditions for an explicitly resumed experiment, not blockers for current product acceptance.
