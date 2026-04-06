# Symlinkarr v1.0 RC — Peripheral Code Audit

> Status note, 2026-04-06: this file is a point-in-time audit snapshot. Several implementation and release-hygiene items below were addressed during the later RC closeout pass. Use [RC_ROADMAP.md](./RC_ROADMAP.md) for the live blocker/task list.

**Audit Date:** 2026-04-05
**Audit Scope:** Error messages UX · API clients · Build/release pipeline · Upgrade/migration paths · Logging/observability · Backup/restore integrity
**Auditor:** Peripheral audit sweep
**Status:** RC-PREP — 18 issues identified across 6 peripheral domains

---

## Table of Contents

1. [Error Messages UX](#1-error-messages-ux)
2. [API Clients Deepdive](#2-api-clients-deepdive)
3. [Build / Release Pipeline](#3-build--release-pipeline)
4. [Upgrade / Migration Paths](#4-upgrade--migration-paths)
5. [Logging / Structured Observability](#5-logging--structured-observability)
6. [Backup / Restore Integrity](#6-backup--restore-integrity)
7. [Summary Matrix](#7-summary-matrix)

---

## 1. Error Messages UX

**Auditor note:** Operator-facing error messages are the most important documentation in production. Most issues found are MEDIUM-LOW severity but collectively represent significant UX debt.

### 1.1 HIGH — API Error Templates Leak Internal Enum Names

All external API clients use template-based errors that expose internal operation names:

```rust
// src/api/tmdb.rs:319
"TMDB {} error {}: {}"  // operation, status, body

// src/api/tvdb.rs:293
"TVDB {} error {}: {}"  // same pattern

// src/api/sonarr.rs:141
"Sonarr get_series error {}: {}"
```

The `{}` for operation is the method name (e.g., `"series_lookup"`). This is developer-facing, not operator-facing. Should be wrapped with context like "TMDB metadata lookup failed" before surfacing to operators.

---

### 1.2 HIGH — "Unknown" Errors Are Developer-Facing, Not Operator-Facing

```rust
// src/db.rs:515
"Unknown migration version {}"  // operator doesn't know what migration versions exist

// src/db.rs:1068
"Cannot migrate down unknown version {}"

// src/db.rs:125
"Unknown acquisition relink kind '{}'"  // what kinds exist?

// src/db.rs:166
"Unknown acquisition job status '{}'"  // what statuses exist?
```

These are assertion-style errors — they should never fire in production but will confuse operators if they do.

---

### 1.3 MEDIUM — Two `error!` Calls Should Be `warn!`

```rust
// commands/scan.rs:547
tracing::error!("Failed to sync Real-Debrid cache: {}. Using existing cache if available.");

// commands/scan.rs:604
tracing::error!("Failed to read cache for source {}: {}. Falling back to filesystem scan.");
```

Both operations **gracefully degrade** — the scan continues with fallback behavior. These should be `warn!` since they represent expected operational conditions, not application errors.

---

### 1.4 MEDIUM — Missing `with_context` Chains

Many `anyhow::bail!` calls would benefit from `context()` to explain WHY an operation was attempted:

```rust
// src/cleanup_audit.rs — prune validation errors are good, but:
// src/linker.rs:58
anyhow::bail!("Aborting {}: source path became unhealthy: {}");
// Missing: which operation, which source, why this was checked

// src/backup.rs:526
anyhow::bail!("Aborting backup restore: source target became unhealthy: {}");
// Missing: which target, which backup file
```

Compare with the well-crafted cleanup audit errors at `cleanup_audit.rs:1175-1212` which include recovery hints.

---

### 1.5 MEDIUM — Database Errors Lack Operation Context

```rust
// src/web/handlers.rs:225
tracing::error!("Failed to get scan history: {}");
// Missing: which user/request triggered this, what library filter was active
```

---

### 1.6 LOW — Error Consistency: Some With `{}`, Some With `{:?}`, Some Bare

| Style | Example | Files |
|-------|---------|-------|
| `anyhow::bail!("message {}", var)` | cleanup_audit.rs | Majority |
| `anyhow::bail!("message {:?}", path)` | repair.rs | Minority |
| `anyhow::bail!("{}", err)` | linker.rs | Some locations |
| `tracing::error!("message {}", err)` | commands/scan.rs | Throughout |

No enforced style guide for error message formatting.

---

### 1.7 GOOD — Well-Crafted Errors

These errors set a good standard that should be replicated:

```rust
// src/cleanup_audit.rs:1204
"Refusing prune apply: invalid or missing confirmation token. Re-run preview and pass --confirm-token {}"

// src/cleanup_audit.rs:1212
"Refusing prune apply: {} candidates exceeds delete cap {} (use --max-delete to override)"

// src/commands/mod.rs:238
"Refusing {}: library '{}' is not healthy: {}"  // includes which library

// src/config.rs:1817
"Plaintext secret is not allowed for {}. Use env:VAR or secretfile:/path/to/file"  // actionable fix
```

---

## 2. API Clients Deepdive

**Per-client ratings:**

| Client | Retry (1-5) | Error Quality (1-5) | Security (1-5) |
|--------|-------------|---------------------|-----------------|
| RealDebrid | 3 | 3 | 3 |
| Plex | 3 | 3 | 3 |
| Emby | 3 | 3 | 3 |
| Jellyfin | 3 | 3 | 3 |
| Sonarr | 3 | 3 | 3 |
| Radarr | 3 | 3 | 3 |
| Prowlarr | 3 | 4 | 3 |
| Bazarr | 4 | 4 | 3 |
| Tautulli | 3 | 3 | 3 |
| TMDB | 3 | 3 | 3 |
| TVDB | 4 | 4 | 4 |
| Decypharr | 3 | 4 | 3 |
| DMM | 3 | 3 | 2 |

---

### 2.1 HIGH — DMM Auth Salt Hardcoded in Source

```rust
// src/api/dmm.rs:12
const DMM_SALT: &str = "debridmediamanager.com%%fe7#td00rA3vHz%VmI";
```

Authentication salt is a hardcoded constant. If this leaks or the DMM API changes auth scheme, it cannot be rotated without a code change and redeploy.

**Fix:** Move to config: `dmm.auth_salt: "env:SYMLINKARR_DMM_SALT"` or `secretfile:`.

---

### 2.2 HIGH — No Idempotency Keys on POST Mutations

RealDebrid `add_magnet` and Prowlarr `grab` are POST operations. If the request times out after submission but before response, retrying adds the magnet again:

```rust
// src/api/realdebrid.rs:181-206
pub async fn add_magnet(&self, magnet: &str) -> Result<String> {
    let req = self.client.post(&url)...  // no idempotency key
    let resp = http::send_with_retry(req).await?;  // retry may dup
}

// src/api/prowlarr.rs:116-143
pub async fn grab(&self, guid: &str, indexer_id: i32) -> Result<()> {
    let req = self.client.post(&url)...  // no idempotency key
}
```

**Fix:** Add `Idempotency-Key: <stable-content-hash>` header on retries.

---

### 2.3 MEDIUM — API Key in Query String (Bazarr, Tautulli)

```rust
// src/api/bazarr.rs:74-79
fn with_query_api_key(&self, url: &str) -> String {
    format!("{}?apikey={}", url, self.api_key)  // key in URL
}

// src/api/tautulli.rs:153-155
fallback_params.push(("apikey", self.api_key.as_str()));
```

API keys appear in server access logs, browser history, and referrer headers. Header-based auth should be tried first. Currently Bazarr only falls back to query auth on 401 — acceptable but not ideal.

---

### 2.4 MEDIUM — No Circuit Breaker for Emby/Jellyfin

```rust
// src/media_servers/emby.rs:198-216
for (idx, batch) in batches.into_iter().enumerate() {
    match post_media_updates(...).await {
        Ok(()) => { ... }
        Err(err) => {
            telemetry.failed_batches += 1;
            // continues to next batch — hammers overwhelmed server
        }
    }
}
```

If Emby/Jellyfin is overwhelmed (429/503), the client continues hammering it. After enough consecutive failures, it should fail-fast for a cooldown period.

---

### 2.5 MEDIUM — TVDB Client Borrow Checker Issue

```rust
// src/api/tvdb.rs:175-185
if resp.status() == 401 {
    if retried {
        anyhow::bail!("TVDB authentication failed...");
    }
    self.authenticate().await?;  // requires &mut self
    return self.get_series_metadata_inner(tvdb_id, db, true).await;
}
```

`authenticate()` requires `&mut self` but TVDB client is stored behind `Arc<Mutex<TvdbClient>>`. The `&mut` forces sequential calls — concurrent TVDB metadata fetches are not possible.

---

### 2.6 MEDIUM — No Per-Request Timeout Override

```rust
// src/api/http.rs:10-11
const CONNECT_TIMEOUT_SECS: u64 = 5;
const REQUEST_TIMEOUT_SECS: u64 = 20;  // global only
```

Global 20s timeout for all operations. Large batch operations (like fetching 500 TMDB seasons) may need longer. Small operations (health checks) could be faster.

---

### 2.7 MEDIUM — DMM Auth Pair Regenerated Every Call

```rust
// src/api/dmm.rs:164
fn generate_dmm_auth_pair(&self) -> (String, String) { ... }
// Called on every fetch_torrents() call — CPU-intensive hash, not cached
```

Custom hash function called on every request. The auth pair should be cached and reused within a TTL window.

---

### 2.8 LOW — TMDB Season Fetch Failures Silently Warn

```rust
// src/api/tmdb.rs — season fetches run concurrently
// A single season failure logs a warning but produces incomplete metadata
```

One bad season in a 20-season show produces a warning but no error to the caller. The caller gets partial metadata without knowing it's incomplete.

---

### 2.9 GOOD — Retry Infrastructure Is Solid

The shared `http.rs` retry system is well-designed:
- Exponential backoff with jitter
- 429 respects `Retry-After` header (both seconds and HTTP-date formats)
- Separate backoff bases for rate-limit (2s) vs transport errors (250ms)
- Non-retryable statuses (401, 403, 404) are correctly not retried

---

## 3. Build / Release Pipeline

### 3.1 HIGH — `panic = "abort"` Not Set

```toml
# Cargo.toml:43-46
[profile.release]
lto = "thin"
strip = true
opt-level = 3
# panic = "abort"  ← MISSING
```

`panic = "abort"` removes stack-unwinding infrastructure. Without it, panic messages include backtrace frames. With it, the binary is ~50KB smaller and panics are instant aborts. Standard for production Rust binaries.

**Fix:** Add `panic = "abort"` to `[profile.release]`.

---

### 3.2 HIGH — No `cargo-audit` in CI

```yaml
# .github/workflows/ci.yml — no cargo-audit step
jobs:
  test: ...
  clippy: ...
  fmt: ...
  # NO: cargo audit for vulnerable dependencies
```

Known vulnerable dependencies could be deployed without detection. CVEs in transitive dependencies (especially `serde_yaml = "0.0.12"`) are not being scanned.

**Fix:** Add `cargo audit --locked` step after tests.

---

### 3.3 MEDIUM — Using `lto = "thin"` Instead of `lto = "fat"`

```toml
lto = "thin"  # Current
lto = "fat"   # Recommended
```

Thin LTO is faster to compile but produces 5-15% larger binaries and slightly slower runtime. For a production binary that's distributed and runs indefinitely, `fat` LTO is worth the compile time cost.

---

### 3.4 MEDIUM — No Binary Size Regression Check

CI builds the release binary but does not track or fail on size increases. A dependency change that adds 5MB would go unnoticed.

**Fix:** Add a `check-binary-size` job that asserts `binary_size < N MB`.

---

### 3.5 MEDIUM — `tokio` with `"full"` Features

```toml
# Cargo.toml:9
tokio = { version = "1", features = ["full"] }
```

The `"full"` feature flag enables all tokio features (sync, fs, io-driver, etc.). Symlinkarr only uses `sync`, `time`, and `rt` (inferred from existing code). Trimming to specific features could reduce binary size.

---

### 3.6 MEDIUM — No SBOM Generated

No software bill of materials is generated for release artifacts. This is increasingly required for supply chain compliance.

**Fix:** Add `cargo sbom --locked` or `cargo-spdx` to release workflow.

---

### 3.7 LOW — No Reproducible Build Guarantee

Docker layer caching (`type=gha`) can produce non-deterministic layers if build context changes. The `Cargo.lock` is committed and `--locked` is used everywhere — good foundation, but not formally reproducible.

---

### 3.8 GOOD — Docker Multi-Stage Build Is Correct

```dockerfile
# Dockerfile:22
COPY --from=builder /app/target/release/symlinkarr /usr/local/bin/symlinkarr
COPY --from=builder /app/src/web/static /usr/local/share/symlinkarr/static
```

Binary at `/usr/local/bin/`, static files at `/usr/local/share/symlinkarr/static`. The `static_dir()` fallback chain in `src/web/mod.rs:1344-1361` correctly resolves the Docker path.

---

### 3.9 GOOD — Release Produces Binary Tarballs + SHA256 + Multi-Arch Docker

Release workflow at `.github/workflows/release.yml` produces:
- `symlinkarr-{version}-linux-amd64.tar.gz` + `.sha256`
- `symlinkarr-{version}-linux-arm64.tar.gz` + `.sha256`
- Docker image (amd64 + arm64) on GHCR

Comprehensive artifact set.

---

## 4. Upgrade / Migration Paths

### 4.1 MEDIUM — Down-Migrations Exist But Are Test-Only

```rust
// src/db.rs:898  #[cfg(test)]
pub async fn migrate_down_one(&mut self) -> Result<()> {
```

Production has no downgrade path. If a bad migration is deployed, the only recovery is a manual DB fix or backup restore. This is acceptable for an RC but should be documented.

---

### 4.2 MEDIUM — No Distributed Lock for Concurrent Migration

```rust
// src/db.rs:365-367
sqlx::query("PRAGMA busy_timeout = 5000").execute(&pool).await?;  // 5s only
```

Two processes starting simultaneously on the same DB: if migration takes >5s, the second process gets `SQLITE_BUSY` and crashes. Unlikely but possible with large migrations.

---

### 4.3 MEDIUM — Backup Version Exists But Is Not Enforced

```rust
// src/backup.rs:43
pub struct BackupManifest {
    pub version: u32,  // set to 1, never checked on restore
}

// src/backup.rs:262
let manifest: BackupManifest = serde_json::from_str(&json)?;  // no version gate
```

If a future version changes the backup manifest schema, restore will silently use defaults for missing fields or fail with a serde error. The `version` field is informational only.

---

### 4.4 MEDIUM — Database Itself Is Not Backed Up

```rust
// src/backup.rs:113-151
// Only LinkStatus::Active links are backed up — NOT the SQLite DB
```

If the SQLite database file is corrupted, no backup exists. The backup captures symlink mapping state but not the full DB (including scan history, acquisition jobs, link events, etc.).

**Fix:** Add an option to also backup the `.db` file in `backup create`.

---

### 4.5 LOW — `infer_legacy_schema_version` Fragile Detection Order

```rust
// src/db.rs:436-471
// Checks tables in hardcoded order: scan_history → links → scan_runs → ...
```

If a future migration inserts a table between existing ones, the inference could mis-detect the version on legacy databases. The version marker should be written to legacy DBs during the first upgrade.

---

### 4.6 GOOD — Migration Atomicity Is Well-Designed

Each migration + version bump is atomic. A crash mid-migration leaves `schema_version` unchanged, so the migration re-runs cleanly on next startup. Idempotent column additions handle retry correctly.

---

### 4.7 GOOD — Config BC Is Solid

All new config fields use `#[serde(default)]` and have `Default` implementations. An old config file with missing fields parses correctly with safe defaults.

---

## 5. Logging / Structured Observability

### 5.1 HIGH — No Correlation ID for Scan Operations

There is no request/scan/operation ID that chains log lines together. A single scan's lifecycle cannot be traced through logs — you need timestamp + library name heuristics.

The `run_token` exists (`commands/scan.rs:82`) but is never logged. It should be: `tracing::info!(run_token = %run_token, scan_scope = %scope, "Starting scan")`.

---

### 5.2 HIGH — Extensive Operational Detail Hidden Behind `debug!`

At default `info!` level, operators cannot diagnose:
- Why a specific file was/wasn't matched
- Why a symlink was skipped
- Cache hit/miss decisions
- Which API calls were made and their outcomes

All of this exists at `debug!` level but is invisible in production. For a tool that manages irreplaceable symlinks, the inability to diagnose failures at default log level is significant.

---

### 5.3 MEDIUM — No Structured Logging

```rust
// Majority of logs use string interpolation:
tracing::info!("Background scan completed (scope={}, dry_run={}, search_missing={}): added_or_updated={}, removed={}", ...);

// Instead of:
tracing::info!(scope = %scope, dry_run = %dry_run, search_missing = %search_missing, added_or_updated = %added_or_updated, removed = %removed, "Background scan completed");
```

Zero field-based structured logging. Logs cannot be filtered/queryable by field in observability backends. All logs are opaque strings.

---

### 5.4 MEDIUM — Source Paths Logged at `info!` Level

```rust
// src/source_scanner.rs:84
tracing::info!("Scanning source via cache: {} at {:?}", source.name, source.path);
// Logs: "Scanning source via cache: RD at /mnt/seedbox/downloads/ Anime /Sword Art Online/"
// Full source path with media names visible in logs at info level
```

---

### 5.5 MEDIUM — Two `error!` Calls Should Be `warn!`

(See Section 1.3 — same finding, relevant to observability)

---

### 5.6 LOW — `user_println` and `tracing` Are Two Separate Output Streams

```rust
// utils.rs:231
pub fn user_println(message: impl AsRef<str>) {
    if stdout_text_enabled() {
        println!("{}", message.as_ref());
    }
}
```

Operator text output (`user_println`) goes to stdout as plain text. `tracing` goes to the configured subscriber (structured). These cannot be interleaved or correlated in production observability pipelines.

**Fix:** Consider routing `user_println` through `tracing` with `target = "user"` so all output is unified.

---

### 5.7 LOW — Sensitive Config Paths in Logs

```rust
// config.rs:979
tracing::info!("Configuration loaded from {:?}", path);
// config.rs:1563
tracing::info!("Loaded {} env var(s) from {:?}", loaded, path);
```

Config file paths and `.env` file paths are visible in logs at info level.

---

## 6. Backup / Restore Integrity

### 6.1 HIGH — No Database Backup

Only symlink records are backed up — the SQLite database itself is not included. If the DB is corrupted, there is no recovery path from backups.

```rust
// src/backup.rs:113-151 — only queries LinkStatus::Active
```

---

### 6.2 HIGH — No Backup Integrity Verification

No checksum or hash of backup contents. Corrupted backup files will not be detected until restore is attempted.

---

### 6.3 MEDIUM — Partial Restore Has No Atomicity

```rust
// src/backup.rs:376-402
// If DB insert fails after symlink creation: symlink is rolled back
// If process crashes mid-restore: indeterminate state
```

If the process crashes after some symlinks are created and some DB records are inserted, the system is left in a partially restored state. There is no batch transaction wrapper.

---

### 6.4 MEDIUM — Backup Manifest Version Not Enforced

```rust
// src/backup.rs:262
let manifest: BackupManifest = serde_json::from_str(&json)?;
```

Future schema changes could silently produce wrong defaults or confusing serde errors. The `version: 1` field exists but is never read or acted upon.

---

### 6.5 MEDIUM — Moved Source Files Cause Silent Skip

```rust
// src/backup.rs:298-310
if !restore_target_available(entry).await? {
    warn!("Skipping restore for {:?}: source target missing: {:?}", ...);
    skipped += 1;
}
```

If a source file has moved since backup, the entry is silently skipped. There is no target remapping — the backup cannot redirect to a new source location.

---

### 6.6 MEDIUM — No Compression

```rust
// src/backup.rs:142
serde_json::to_string_pretty(&manifest)?  // pretty-printed JSON, no compression
```

Backups for large libraries are uncompressed JSON. For 10,000 symlinks, a backup could be 2-5MB of raw JSON. gzip would reduce this significantly.

---

### 6.7 LOW — Quarantine Not Related to Backup

Quarantine files live under `backup.path/quarantine/` but are not tracked in backup manifests. Cannot restore quarantine entries from backup.

---

### 6.8 GOOD — Restore Dry-Run Works Correctly

`backup restore --dry-run` correctly simulates restore without making changes.

---

### 6.9 GOOD — Source Health Check Before Restore

```rust
// src/backup.rs:519-532
fn restore_target_available(entry: &BackupEntry) -> Result<bool> {
    let health = cached_source_health(&entry.target_path, ...);
    Ok(health.is_healthy())
}
```

Restore checks source target health before attempting to restore, preventing restore of symlinks pointing to unavailable sources.

---

## 7. Summary Matrix

| Domain | # | Blocking | Should Fix | Nice to Have |
|--------|---|----------|-----------|--------------|
| Error Messages UX | 7 | 2 | 4 | 1 |
| API Clients | 8 | 2 | 5 | 1 |
| Build/Release | 7 | 2 | 4 | 1 |
| Migration Paths | 6 | 0 | 4 | 2 |
| Logging/Observability | 7 | 2 | 4 | 1 |
| Backup/Restore | 7 | 2 | 4 | 1 |
| **TOTAL** | **42** | **10** | **25** | **7** |

---

### Top 10 Blocking Issues

| # | Domain | Issue |
|---|--------|-------|
| 1 | Build | `panic = "abort"` not set in release profile |
| 2 | Build | No `cargo-audit` in CI |
| 3 | API Clients | DMM auth salt hardcoded in source |
| 4 | API Clients | No idempotency keys on POST mutations (RealDebrid, Prowlarr) |
| 5 | Logging | No correlation ID for scan operations |
| 6 | Logging | Extensive diagnostic detail hidden behind `debug!` level |
| 7 | Backup | No database backup — only symlink records |
| 8 | Backup | No backup integrity verification |
| 9 | Error UX | "Unknown" errors leak internal enum names to operators |
| 10 | Error UX | API error templates expose internal operation names |

---

### Per-Domain Quick Reference

**Error Messages UX:**
- Well-crafted prune/cleanup errors (best in codebase)
- API client errors need wrapping for operators
- Two `error!` calls should be `warn!` (scan cache failures)

**API Clients:**
- Retry infrastructure is solid (backoff, jitter, Retry-After)
- DMM auth salt is the main security concern
- Emby/Jellyfin need circuit breakers

**Build/Release:**
- Docker build is correct and follows best practices
- `panic = "abort"` and `cargo-audit` are the main gaps
- Binary size (24MB) is acceptable for this dependency set

**Migration:**
- Forward migration is safe and atomic
- No production downgrade path
- Config BC is solid

**Logging:**
- No structured logging anywhere
- No correlation IDs
- `user_println` and `tracing` are separate output streams

**Backup:**
- Dry-run works correctly
- Source health checks before restore
- No DB backup is the main gap
- No compression, no integrity verification

---

*Generated: 2026-04-05*
*Auditor: Peripheral audit sweep — error messages · API clients · build/release · migrations · logging · backup integrity*
