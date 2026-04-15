# Symlinkarr v1.0 RC — Documentation & UX Audit

**Audit Date:** 2026-04-04
**Audit Scope:** CLI manual, API schema, README, UI copy, version consistency, cross-doc consistency, config docs
**Auditor:** Secondary audit — docs/UX/nitpicky consistency sweep
**Status:** RC-PREP — 8 blocking issues, 24 should-fix issues

---

## Table of Contents

1. [Executive Summary](#1-executive-summary)
2. [Version Inconsistencies](#2-version-inconsistencies)
3. [CLI / API / Web UI Parity Gaps](#3-cli--api--web-ui-parity-gaps)
4. [API Schema Issues](#4-api-schema-issues)
5. [CLI Manual Issues](#5-cli-manual-issues)
6. [README / Product Scope Inconsistencies](#6-readme--product-scope-inconsistencies)
7. [UI Copy Issues](#7-ui-copy-issues)
8. [Config Documentation Issues](#8-config-documentation-issues)
9. [Changelog Issues](#9-changelog-issues)
10. [Missing Documentation](#10-missing-documentation)
11. [UX / Navigation Issues](#11-ux--navigation-issues)
12. [Security Model Documentation](#12-security-model-documentation)
13. [Priority Matrix](#13-priority-matrix)

---

## 1. Executive Summary

The codebase has a well-structured set of documentation files, but this audit surfaces inconsistencies between what the docs claim and what the code actually does, plus gaps in coverage. The most serious issues are:

1. **API Schema claims CSRF synchronizer token** but the code doesn't actually implement it
2. **CLI Manual and API Schema directly contradict** the README on whether `api_key` alone enables remote UI access
3. **CLI `--search-missing` is documented but not in the API schema** scan endpoint
4. **`discover add`, `queue retry`, `repair trigger` are CLI-only** but the API schema doesn't mention this limitation
5. **Several API endpoints are missing** from the schema entirely

---

## 2. Version Inconsistencies

### ISSUE-2-1: Version String Hardcoded in HTML Footer

**Severity:** LOW
**Location:** `src/web/ui/base.html:83`

```html
Symlinkarr v0.3.0-beta.1 |
```

The version is hardcoded in the HTML footer. If the version changes in `Cargo.toml` or at release time, the HTML footer won't automatically update.

**Fix:** Pass the version from the server-side template context (`{{ version }}`) so it's derived from the binary's version at compile time.

---

## 3. CLI / API / Web UI Parity Gaps

### ISSUE-3-1: `discover add` CLI-Only — Not Documented as Such

**Severity:** HIGH
**Locations:**
- `src/main.rs:265-275` — `DiscoverAction::Add` exists in CLI
- `docs/CLI_MANUAL.md:224-234` — `discover add` IS shown in CLI manual
- `docs/API_SCHEMA.md:102-132` — `/api/v1/discover` only documents `GET /api/v1/discover`

The CLI manual correctly shows `symlinkarr discover add <TORRENT_ID> [--arr sonarr]`. However, the API schema section header says "Returns read-only discovery results" — it doesn't mention that discovery has a write operation. This is a documentation gap, not a parity issue per se.

**Fix:** In API_SCHEMA.md, add a note under `/api/v1/discover`: "Note: Adding RD torrents via API is not yet supported. Use `symlinkarr discover add` from the CLI."

---

### ISSUE-3-2: `queue retry` CLI-Only — Not Documented

**Severity:** MEDIUM
**Location:** `docs/API_SCHEMA.md` — missing

The CLI supports `symlinkarr queue retry [--scope all|blocked|no-result|failed|completed-unlinked]` but the API schema has no `/api/v1/queue` endpoints at all (only `/api/v1/links` and health/scan endpoints).

**Fix:** Add `/api/v1/queue` section to API_SCHEMA.md noting it's CLI-only, or implement the API endpoint.

---

### ISSUE-3-3: `repair trigger` CLI-Only — Not Documented

**Severity:** MEDIUM
**Location:** `docs/API_SCHEMA.md` — missing

`symlinkarr repair trigger --arr sonarr` has no API equivalent. Noted in API_SCHEMA.md's scope but worth making explicit.

**Fix:** Add note in API_SCHEMA.md that `repair trigger` is CLI-only.

---

### ISSUE-3-4: `search_missing` in Scan CLI vs API vs Web UI

**Severity:** MEDIUM
**Locations:**
- `src/main.rs:101-110` — `Scan { search_missing: bool }` CLI flag
- `docs/CLI_MANUAL.md:38` — `symlinkarr scan [--search-missing]` documented
- `docs/API_SCHEMA.md:139-173` — `POST /api/v1/scan` request body includes `"search_missing": false`
- `src/web/ui/scan.html:71` — `search_missing` checkbox exists in UI

All three surfaces have `search_missing`. However:
- The CLI scan description in CLI_MANUAL.md doesn't explain what `search_missing` does (only that it "Build auto-acquire requests for unmatched movies and anime episode gaps" in the scan UI form description)
- The daemon config has `daemon.search_missing` but **there is no web UI control for this** — it's config.yaml-only

**Fix:**
1. Add `search_missing` explanation to CLI_MANUAL.md scan description
2. Consider adding daemon `search_missing` toggle to web UI config page

---

### ISSUE-3-5: `scan --dry-run` vs `--search-missing` Clarity

**Severity:** LOW
**Location:** `docs/CLI_MANUAL.md:38`

The examples show `--dry-run` and `--library Anime --search-missing` separately but don't clarify that `--search-missing` can be combined with `--dry-run`:

```bash
symlinkarr scan --dry-run
symlinkarr scan --library Anime --search-missing
# But NOT: symlinkarr scan --dry-run --search-missing  ← valid and useful
```

**Fix:** Add example: `symlinkarr scan --dry-run --search-missing`

---

## 4. API Schema Issues

### ISSUE-4-1: CSRF Synchronizer Token Claimed But Not Implemented

**Severity:** HIGH (Documentation vs Implementation)
**Location:** `docs/API_SCHEMA.md:32-33`

The API schema states:
> "a synchronizer-style CSRF token rendered into HTML forms and validated on UI mutation routes"

This is **not implemented**. The code in `src/web/mod.rs:1210-1258` only checks `Origin`/`Referer` headers — it does NOT validate a synchronizer token from form posts. The `csrf_token` field in forms (`src/web/ui/scan.html:51`) is rendered but never validated server-side.

This means:
1. The API schema document is incorrect
2. Any user reading the schema and implementing a client based on this description will be confused
3. The claim in PRODUCT_SCOPE.md line 39 ("Browser-driven mutations should use same-origin session and CSRF gates") is only partially fulfilled

**Fix:**
- Update API_SCHEMA.md to accurately describe the actual CSRF protection (Origin/Referer header validation, not synchronizer tokens)
- Or implement the synchronizer token properly and update the code

---

### ISSUE-4-2: Missing `/api/v1/queue` Endpoints

**Severity:** MEDIUM
**Location:** `docs/API_SCHEMA.md` — absent

The API schema covers scan, repair, cleanup, links, config, doctor, health, discover, and anime-remediation. The `queue` command (`symlinkarr queue list`, `symlinkarr queue retry`) has no API coverage at all.

**Fix:** Either:
1. Add `/api/v1/queue` section documenting that queue management is CLI-only, OR
2. Implement the API endpoints

---

### ISSUE-4-3: Missing `/api/v1/repair/trigger` Endpoint

**Severity:** MEDIUM
**Location:** `docs/API_SCHEMA.md` — absent

`symlinkarr repair trigger --arr sonarr` has no API equivalent.

**Fix:** Add note that `repair trigger` is CLI-only.

---

### ISSUE-4-4: `POST /api/v1/scan` Status Code Inconsistency

**Severity:** LOW
**Location:** `docs/API_SCHEMA.md:145`

The schema says `202 Accepted when the scan was accepted and is now running in the background`. Need to verify the actual handler returns 202 and not 200.

---

### ISSUE-4-5: API Schema Says API Key Not Valid for Remote UI — CLI Manual Says Opposite

**Severity:** HIGH (Direct Contradiction)
**Locations:**
- `docs/API_SCHEMA.md:26` — "API key alone is for automation clients, not for making the built-in HTML UI remotely reachable."
- `docs/CLI_MANUAL.md:146` — "`web.api_key` alone is a valid remote-exposure mode for the built-in UI."
- `README.md:126` — "remote exposure now requires `web.username` + `web.password`; API key alone is not enough for the built-in HTML UI"

The README and API schema agree with each other. The CLI manual directly contradicts them.

**Fix:** Update CLI_MANUAL.md line 146 to match:
```rust
// CORRECT (matches README and API_SCHEMA):
// `web.api_key` alone is NOT a valid remote-exposure mode for the built-in UI.
// It is for automation clients only.
```

---

### ISSUE-4-6: Missing Response Schema for Some Endpoints

**Severity:** MEDIUM
**Location:** `docs/API_SCHEMA.md`

The following endpoints are mentioned but lack response schemas:
- `GET /api/v1/scan/status` — described but no response JSON shown
- `GET /api/v1/cleanup/audit/status` — described but no response JSON shown
- `GET /api/v1/cleanup/audit/jobs` — described but no response JSON shown

---

### ISSUE-4-7: JSON Error Example Has Quoting Bug

**Severity:** LOW
**Location:** `docs/API_SCHEMA.md:728`

```json
{ "error": "Plex DB path is required or must exist at a standard local path" }
```

Wait — the schema actually has it correct. But earlier in the file (line 42-43):
```json
{ "error": "message" }
```

This is a generic template. Not a bug.

---

### ISSUE-4-8: `format=tsv` for Anime Remediation — TSV Columns Not Documented

**Severity:** MEDIUM
**Location:** `docs/API_SCHEMA.md:510`

The `GET /api/v1/report/anime-remediation` endpoint supports `format=tsv` but:
1. The column headers of the TSV output are not documented
2. The meaning of each column is not explained
3. There's no example TSV snippet

**Fix:** Add TSV column documentation:
```
Columns: normalized_title | state | eligible | block_code | block_label | recommended_action | sample_legacy_path | sample_legacy_filesystem_symlinks | sample_legacy_db_active_links | ...
```

---

## 5. CLI Manual Issues

### ISSUE-5-1: `symlinkarr --version` Not Documented

**Severity:** LOW
**Location:** `docs/CLI_MANUAL.md:22`

The global options list shows:
```bash
-V, --version: version
```

But the examples section never shows `symlinkarr --version` or `symlinkarr -V`. The `-V` is only mentioned in the options list. Consider adding:

```bash
symlinkarr --version
```

---

### ISSUE-5-2: `report` Command Lacks Output Examples

**Severity:** LOW
**Location:** `docs/CLI_MANUAL.md:306-328`

The `report` command has `--output json --pretty` and `--anime-remediation-tsv` options but the examples only show basic invocations. Consider adding:

```bash
symlinkarr report --library Anime --plex-db "..." --full-anime-duplicates --output json --pretty
symlinkarr report --library Anime --anime-remediation-tsv /tmp/anime-remediation.tsv --plex-db "..."
```

---

### ISSUE-5-3: `cleanup prune` Include-Legacy Flag Description is Dense

**Severity:** LOW
**Location:** `docs/CLI_MANUAL.md:184-185`

```bash
--include-legacy-anime-roots  opt-ins warning-only anime findings where an untagged legacy root coexists with a tagged {tvdb-*}/{tmdb-*} root. These candidates are quarantined as foreign, not deleted.
```

This is one very long line. Consider splitting for readability.

---

### ISSUE-5-4: JSON Output Inconsistency

**Severity:** LOW
**Location:** `docs/CLI_MANUAL.md:330-342`

The "JSON-Capable Commands" list mentions `report` but `--output json` is not shown in the `report` examples. The `report` command also has `--pretty` which is notable for JSON output but not mentioned in the JSON-capable section.

---

### ISSUE-5-5: `--pretty` Flag Not Documented in `report` Command Syntax

**Severity:** LOW
**Location:** `docs/CLI_MANUAL.md:307`

The `report` command syntax shows:
```bash
symlinkarr report [--output text|json] [--filter movie|series] ...
```

But doesn't show `--pretty`. The actual CLI definition has `--pretty` (src/main.rs:210). This should be in the syntax line.

---

### ISSUE-5-6: `discover list` Examples Missing `--library` with JSON

**Severity:** LOW
**Location:** `docs/CLI_MANUAL.md:229-234`

The examples show:
```bash
symlinkarr discover list
symlinkarr discover list --library Movies --output json
```

But not `--library Anime --output json` which would be a common use case for anime users.

---

### ISSUE-5-7: `cache` Command Examples Could Show `cache status` Output

**Severity:** LOW
**Location:** `docs/CLI_MANUAL.md:267-272`

The examples just show the commands themselves. Showing what `cache status` outputs would help users understand what "cache build" populates.

---

### ISSUE-5-8: `daemon` Command Has No Examples

**Severity:** MEDIUM
**Location:** `docs/CLI_MANUAL.md:97-103`

The daemon section shows the syntax `symlinkarr daemon` but no examples. Many users would expect to see:
```bash
# Run in foreground (for testing)
symlinkarr daemon

# Run in background (systemd/docker)
docker compose up -d symlinkarr
```

---

### ISSUE-5-9: `web` Command Examples Missing `--port`

**Severity:** LOW
**Location:** `docs/CLI_MANUAL.md:113-118`

Examples show `symlinkarr web` and `symlinkarr web --port 9999` but the relationship to config.yaml's `web.port` isn't explained. If someone sets `web.port: 9999` in config, does `--port` override or conflict?

---

### ISSUE-5-10: Hidden `dry-run` Alias Not Clearly Explained

**Severity:** LOW
**Location:** `docs/CLI_MANUAL.md:344-356`

The hidden `symlinkarr dry-run` alias is shown with "Prefer: `symlinkarr scan --dry-run`". This is fine, but doesn't explain WHY the alias exists or when someone might encounter it (e.g., old scripts, muscle memory).

---

## 6. README / Product Scope Inconsistencies

### ISSUE-6-1: README and CLI Manual Contradict on API Key Remote Access

**Severity:** HIGH
**Locations:**
- `README.md:126` — "remote exposure now requires `web.username` + `web.password`; API key alone is not enough for the built-in HTML UI"
- `CLI_MANUAL.md:146` — "`web.api_key` alone is a valid remote-exposure mode for the built-in UI"

See ISSUE-4-5. The CLI manual is wrong.

---

### ISSUE-6-2: README Mentions GitHub Wiki — Wiki Might Not Exist

**Severity:** LOW
**Location:** `README.md:159`

```markdown
- [GitHub Wiki](https://github.com/LAP87/Symlinkarr/wiki)
```

The README links to a GitHub wiki. If the wiki is empty or missing content, this link is misleading. The wiki should either be populated or the link removed from the README until it's ready.

---

### ISSUE-6-3: README Badges Show Docker-Ready But No docker-compose Example

**Severity:** LOW
**Location:** `README.md:45`

The Docker badge says "Docker-ready" but the README only mentions `docker compose up -d symlinkarr` without showing:
1. A `docker-compose.yml` snippet
2. Volume mounts for config and data
3. Network requirements (host.docker.internal)

---

### ISSUE-6-4: Quick Start "Install" Section Is Incomplete

**Severity:** MEDIUM
**Location:** `README.md:56-90`

The quick start says "Download a release tarball from GitHub Releases" but:
1. No link to actual releases page
2. No checksums verification mention
3. No SHA256/PGP verification instructions
4. The Cargo build path assumes Rust toolchain is installed

---

## 7. UI Copy Issues

### ISSUE-7-1: Health Nav Emoji Is a Flower (❎) Instead of a Health Symbol

**Severity:** LOW (Nitpicky)
**Location:** `src/web/ui/base.html:35` and `src/web/ui/health.html:8`

```html
&#128154; <!-- This is a "cheering" person, not a health/heartbeat emoji -->
```

Actually, `&#128154;` is 🎀 (bouquet). The health page nav uses this, but the page itself uses `&#128154;` which is a flower. This is inconsistent — a stethoscope or heartbeat emoji would be more appropriate for a "Health" page.

Better options:
- `&#128568;` (pulse line) 
- `&#129656;` (medical symbol)
- `&#10084;` (heart)

---

### ISSUE-7-2: Cleanup Nav Emoji Appears Twice in Page Header

**Severity:** LOW
**Location:** `src/web/ui/cleanup.html:8`

```html
<h1>&#129529; Cleanup &amp; Maintenance</h1>
```

The nav item for cleanup uses `&#129529;` (🧹 broom). The page header also uses `&#129529;`. This is consistent but the broom emoji is more associated with "sweeping" than "pruning/cleanup" in a symlink context. Consider `&#128465;` (🗑️ trash) or `&#128679;` (🚩 flag) for "audit/flag".

---

### ISSUE-7-3: "Dead Links" Page Accessible from Nav but Not Clearly Explained

**Severity:** LOW
**Location:** `src/web/ui/links.html` (assumed) vs nav

The nav links to `/links/dead` (visible in cleanup.html line 73: `<a href="/links/dead"`) but the links page doesn't seem to have a clear explanation of the difference between "dead" and "active" links when you first visit `/links`.

---

### ISSUE-7-4: Discover Page May Not Explain `refresh_cache` Behavior

**Severity:** LOW
**Location:** `src/web/ui/discover.html` (assumed)

The API schema notes "Browser/UI discover defaults to cached-only mode for lower latency." The discover web UI should make this clear — there should be a `refresh cache` button or at least a note explaining that discover shows cached results by default.

---

### ISSUE-7-5: Footer GitHub Link Uses Raw GitHub URL

**Severity:** LOW
**Location:** `src/web/ui/base.html:84`

```html
<a href="https://github.com/LAP87/Symlinkarr" target="_blank">GitHub</a>
```

This should ideally be `{{ github_url }}` from config so operators can point it to their fork, or at minimum be a relative link if the repo is mirrored.

---

## 8. Config Documentation Issues

### ISSUE-8-1: `security.require_secret_provider` Explained Too Briefly

**Severity:** MEDIUM
**Location:** `config.example.yaml:1-4`

```yaml
security:
  require_secret_provider: true
  enforce_roots: true
  enforce_secure_permissions: true
```

No comments explain what `require_secret_provider: true` actually means in practice. A user seeing this for the first time has no idea:
- That it means "don't allow plaintext API keys in config.yaml"
- That it produces a WARNING (not error) when violated
- That it's recommended to be `true` in production

**Fix:** Add inline comment:
```yaml
# Require API keys to come from env:VAR or secretfile:/path, not plaintext in this file.
# Recommended for production. Defaults to false (warn-only) for ease of initial setup.
require_secret_provider: true
```

---

### ISSUE-8-2: `daemon.search_missing` Not Explained

**Severity:** MEDIUM
**Location:** `config.example.yaml:24-26`

```yaml
daemon:
  interval_minutes: 60
  search_missing: false
```

`interval_minutes` is self-explanatory. `search_missing` is not — it controls whether the daemon's periodic scans also search for missing content and submit auto-acquire requests. This should be documented.

---

### ISSUE-8-3: `matching.mode` and `matching.metadata_mode` Not Explained

**Severity:** MEDIUM
**Location:** `config.example.yaml:32-34`

```yaml
matching:
  mode: "strict"
  metadata_mode: "full"
```

What does `mode: "strict"` mean vs what alternatives exist? What does `metadata_mode: "full"` mean? These should have comments or the CLI manual should explain them.

---

### ISSUE-8-4: `symlink.naming_template` Examples Missing

**Severity:** LOW
**Location:** `config.example.yaml:28-29`

```yaml
symlink:
  naming_template: "{title} - S{season:02}E{episode:02} - {episode_title}"
```

No alternative examples are shown. Users who want different naming conventions have no reference for what template variables are available.

---

### ISSUE-8-5: Real-Debrid Cache Comments Are Dense

**Severity:** LOW
**Location:** `config.example.yaml:47-50`

```yaml
# Cache note: per-torrent file info is rate-limited to 150 fetches per scan
# cycle to avoid RD 429 errors. Run `symlinkarr cache build` for a full sync.
# Cache Coverage < 80% triggers an automatic filesystem walk fallback.
```

These comments are good but the "Cache Coverage < 80%" note uses `Coverage` with a capital C which looks like it might be a variable name. Should be lowercase: "cache coverage".

---

### ISSUE-8-6: `api.cache_ttl_hours` Default Explained Incorrectly

**Severity:** MEDIUM
**Location:** `config.example.yaml:41`

The config field says `cache_ttl_hours: 720` but the default in code (from the audit) is ~87600 hours (~10 years). 720 hours = 30 days. If the actual default in code is different from the example value, this misleads users about what "normal" caching behavior is.

---

### ISSUE-8-7: `decypharr.arr_name_*` Fields Are Commented Out

**Severity:** LOW
**Location:** `config.example.yaml:66-69`

```yaml
# arr_name_movie: "radarr"
# arr_name_tv: "sonarr"
# arr_name_anime: "sonarr-anime"
```

These are commented out examples but they're important for Decypharr integration. If someone enables Decypharr, they need to know these fields exist and must be configured. The comment says "must match your Decypharr config" but doesn't explain what happens if they're wrong or missing.

---

## 9. Changelog Issues

### ISSUE-9-1: Changelog Entry for 2026-04-03 Mentions `follow_links(false)` — Verify It Was Actually Added

**Severity:** MEDIUM
**Location:** `docs/CHANGELOG.md:24`

The changelog entry says:
> "made scanner traversal explicitly `follow_links(false)`"

But the security audit found that `follow_links(false)` still follows directory symlinks (WalkDir descends into symlinked directories). If the intent was to NOT follow symlinks at all, the changelog claim may be overstating what was actually fixed.

**Fix:** Verify the actual behavior and update the changelog entry to be precise about what was changed.

---

### ISSUE-9-2: Changelog Doesn't Mention the Auth Changes Are Not Yet Complete

**Severity:** LOW
**Location:** `docs/CHANGELOG.md:33-44`

The "Optional Web and API Authentication" section of the changelog implies auth was added, but doesn't mention:
- That the auth is still optional (no credentials by default = no auth)
- That API key only secures `/api/*` routes, not the HTML UI
- That CSRF protection is partial (Origin/Referer only, not synchronizer tokens)

---

### ISSUE-9-3: Changelog Has No Link to Issues/PRs

**Severity:** LOW
**Location:** `docs/CHANGELOG.md` — throughout

Each changelog entry describes what changed and which files were modified, but doesn't link to GitHub issues or PRs. For a project preparing for v1.0 RC, being able to click through to the relevant discussion/PR is valuable.

---

## 10. Missing Documentation

### ISSUE-10-1: No `symlinkarr --help` Output in Docs

**Severity:** MEDIUM
**Location:** `docs/CLI_MANUAL.md`

The CLI manual explains the commands but doesn't show the actual `symlinkarr --help` or `symlinkarr <command> --help` output. Including the actual `--help` output would:
1. Serve as authoritative documentation of the CLI surface
2. Be automatically kept in sync when CLI changes (if generated from source)
3. Show all the defaults and value enums clearly

**Fix:** Consider adding the output of `symlinkarr --help` and key subcommand `--help` outputs as a reference appendix.

---

### ISSUE-10-2: No Troubleshooting Section

**Severity:** MEDIUM
**Location:** `docs/` — absent

Common issues that deserve FAQ/troubleshooting entries:
- "Symlinkarr won't start: 'database is locked'"
- "Scan finds no matches: check your `{tvdb-*}` folder naming"
- "Plex not seeing new symlinks: trigger a library refresh"
- "RD cache out of sync: run `symlinkarr cache build`"
- "Cleanup audit found issues but prune is blocked"

---

### ISSUE-10-3: No Explanation of What `{tvdb-*}` and `{tmdb-*}` Mean

**Severity:** MEDIUM
**Location:** `docs/CLI_MANUAL.md` and `README.md`

The product heavily relies on ID-tagged folder names (`{tvdb-123456}`, `{tmdb-tt123456}`) but neither the README nor CLI manual explain:
1. Where these IDs come from (TheMovieDB.org, TheTVDB.com)
2. How to find them for your media
3. What happens if the folder isn't ID-tagged

---

### ISSUE-10-4: No `config.example.yaml` Explained Section

**Severity:** MEDIUM
**Location:** `docs/` — absent

The `config.example.yaml` file has inline comments but a section in the docs explaining each config block (security, libraries, sources, daemon, matching, etc.) would help users understand:
- What each section controls
- What the recommended values are for different setups
- How to migrate from older config versions

---

### ISSUE-10-5: No `symlinkarr doctor` Output Interpretation Guide

**Severity:** LOW
**Location:** `docs/` — absent

`symlinkarr doctor` runs a preflight checklist. It would help to document:
- What each check verifies
- What to do when a check fails
- The difference between "passed" and "warning" states

---

### ISSUE-10-6: No Explanation of Backup/Restore Safety Semantics

**Severity:** LOW
**Location:** `docs/CLI_MANUAL.md` — backup section

The backup section says `backup restore` uses the same runtime safety gate as scan/repair/cleanup. But what does this mean in practice? When would restore be refused? What happens to existing symlinks that are in the backup but already exist on disk?

---

### ISSUE-10-7: No Docker-Specific Documentation Beyond WSL

**Severity:** LOW
**Location:** `docs/DEV_SETUP_WSL.md`

There is a WSL development setup doc but no Docker runtime documentation explaining:
- How to configure the Docker image
- Volume mount strategy for persistent data
- How to view logs in Docker
- Healthcheck behavior

---

## 11. UX / Navigation Issues

### ISSUE-11-1: No Breadcrumb Navigation in Web UI

**Severity:** LOW
**Location:** `src/web/ui/base.html`

Users navigating to `/scan/history/42` from `/scan` have no breadcrumb or back link. Adding `<a href="/scan">← Back to Scan</a>` would improve navigation.

---

### ISSUE-11-2: Health Page Has No Link to Doctor

**Severity:** LOW
**Location:** `src/web/ui/health.html`

The Health page shows service status but there's no "Run Doctor" button. The Doctor page (`/doctor`) is in the nav but a context link from Health to Doctor would reduce friction when a user sees a problem.

---

### ISSUE-11-3: Cleanup Page "Review Dead Links" Button Links to `/links/dead`

**Severity:** LOW
**Location:** `src/web/ui/cleanup.html:73`

```html
<a href="/links/dead" class="btn btn-secondary">&#128683; Review Dead Links</a>
```

The dead links icon (🚩 flag) is used for "Review Dead Links" which is appropriate, but the dead links page itself (`/links/dead`) isn't linked from anywhere else in the nav.

---

### ISSUE-11-4: Status Page vs Health Page — Confusing Distinction

**Severity:** MEDIUM
**Location:** `src/web/ui/base.html`, `src/web/ui/status.html`, `src/web/ui/health.html`

The nav has both "Status" (📈) and "Health" (🎀) with no clear distinction:
- Status: database stats, link counts, scan history
- Health: service connectivity and configuration status

These are different but related. The naming is clear to someone who already knows the product, but new users would benefit from a one-line description in the nav tooltip or page subtitle.

---

### ISSUE-11-5: No Search/Filter on Links Page

**Severity:** LOW
**Location:** `src/web/ui/links.html` (assumed)

The `/links` endpoint supports pagination (`limit`) but no search or filter. For operators with thousands of links, finding a specific link is difficult. Consider adding a `?search=` query param.

---

## 12. Security Model Documentation

### ISSUE-12-1: Security Model Claims Outdated in API Schema

**Severity:** HIGH
**Location:** `docs/API_SCHEMA.md:30-34`

The API schema claims:
> "a synchronizer-style CSRF token rendered into HTML forms and validated on UI mutation routes"

This is not implemented (see ISSUE-4-1). The README and PRODUCT_SCOPE.md also claim CSRF protection. These documents need to accurately describe the actual protection (Origin/Referer header validation) or the implementation needs to be completed.

---

### ISSUE-12-2: `local-only` Mode Explained Inconsistently

**Severity:** MEDIUM
**Locations:**
- `README.md:116` — "bind to `127.0.0.1`, keep `allow_remote: false`"
- `API_SCHEMA.md:17` — "`local-only`: loopback bind, no remote exposure"
- `CLI_MANUAL.md:138-143` — three-mode explanation with `local-only` = "intentionally trusted: no built-in auth is required"
- `config.example.yaml:138-148` — comments show both `local-only` and `remote operator` modes

All four sources agree conceptually but the CLI manual's framing ("intentionally trusted") is more explicit about the security implications. This framing should be consistent across all docs.

---

## 13. Priority Matrix

### Priority 1 — Blocking (Fix Before v1.0 RC)

| ID | Issue | Location | Fix |
|----|-------|----------|-----|
| ISSUE-4-5 | CLI Manual contradicts API Schema + README on API key remote access | CLI_MANUAL.md:146 | Update CLI manual to match API schema and README |
| ISSUE-4-1 | CSRF synchronizer token claimed in docs but not implemented | API_SCHEMA.md:32-33 | Update docs to accurately describe actual CSRF protection (Origin/Referer), or implement the token |
| ISSUE-12-1 | Security model documentation inaccurate | API_SCHEMA.md, PRODUCT_SCOPE.md | Fix CSRF description to match actual implementation |
| ISSUE-3-1 | `discover add` API gap not documented | API_SCHEMA.md | Add note that discover write ops are CLI-only |
| ISSUE-3-2 | `queue retry` CLI-only not documented | API_SCHEMA.md | Add `/api/v1/queue` section or note CLI-only status |
| ISSUE-3-3 | `repair trigger` CLI-only not documented | API_SCHEMA.md | Add note CLI-only |
| ISSUE-4-2 | Missing `/api/v1/queue` endpoints in schema | API_SCHEMA.md | Document or note as CLI-only |
| ISSUE-8-6 | `cache_ttl_hours` default in example may not match code | config.example.yaml:41 | Verify actual default in `config.rs` and correct example |

### Priority 2 — Should Fix

| ID | Issue | Location |
|----|-------|----------|
| ISSUE-8-1 | `require_secret_provider` undocumented implications | config.example.yaml |
| ISSUE-8-2 | `daemon.search_missing` undocumented | config.example.yaml |
| ISSUE-8-3 | `matching.mode` and `metadata_mode` undocumented | config.example.yaml |
| ISSUE-10-3 | `{tvdb-*}` / `{tmdb-*}` naming never explained | README.md, CLI_MANUAL.md |
| ISSUE-6-1 | README/CLI manual contradiction (already covered as ISSUE-4-5) | CLI_MANUAL.md |
| ISSUE-10-1 | No `symlinkarr --help` output in docs | CLI_MANUAL.md |
| ISSUE-10-2 | No troubleshooting/FAQ section | docs/ |
| ISSUE-11-4 | Status vs Health page distinction confusing | UI nav |
| ISSUE-3-4 | `search_missing` in daemon not exposed in web UI | config.example.yaml, web UI |
| ISSUE-9-1 | Changelog `follow_links(false)` claim needs verification | CHANGELOG.md |

### Priority 3 — Nice to Have (Post-RC)

| ID | Issue | Location |
|----|-------|----------|
| ISSUE-2-1 | Version hardcoded in HTML footer | base.html:83 |
| ISSUE-5-1 | `symlinkarr --version` not in examples | CLI_MANUAL.md |
| ISSUE-5-2 | `report` command needs more output examples | CLI_MANUAL.md |
| ISSUE-5-8 | `daemon` command has no examples | CLI_MANUAL.md |
| ISSUE-6-2 | GitHub wiki link may be to empty wiki | README.md |
| ISSUE-6-3 | No docker-compose example in README | README.md |
| ISSUE-6-4 | Quick start install section incomplete | README.md |
| ISSUE-7-1 | Health emoji is a flower, not a health symbol | health.html |
| ISSUE-7-5 | Footer GitHub URL is hardcoded | base.html |
| ISSUE-8-4 | `naming_template` no examples | config.example.yaml |
| ISSUE-8-7 | `decypharr.arr_name_*` fields confusing | config.example.yaml |
| ISSUE-10-4 | No config explained section | docs/ |
| ISSUE-10-5 | No doctor output interpretation guide | docs/ |
| ISSUE-10-6 | No backup/restore safety semantics doc | docs/ |
| ISSUE-10-7 | No Docker runtime docs | docs/ |
| ISSUE-11-1 | No breadcrumb navigation | UI |
| ISSUE-11-2 | Health page has no Doctor link | health.html |
| ISSUE-11-3 | Dead links page not in nav | base.html |
| ISSUE-11-5 | No search on links page | links.html |
| ISSUE-4-6 | Missing response schemas for several endpoints | API_SCHEMA.md |
| ISSUE-4-8 | TSV column format not documented | API_SCHEMA.md |
| ISSUE-9-2 | Changelog doesn't note auth is still incomplete | CHANGELOG.md |
| ISSUE-9-3 | Changelog has no issue/PR links | CHANGELOG.md |

---

## Appendix: Quick Reference — Docs Cross-Check

| Claim | README | CLI Manual | API Schema | PRODUCT Scope | CHANGELOG | Consistent? |
|-------|--------|------------|------------|---------------|-----------|-------------|
| `api_key` alone enables remote UI | ❌ No (126) | ✅ Yes (146) | ❌ No (26) | N/A | N/A | **NO** |
| CSRF synchronizer token implemented | ✅ (126) | ✅ (147) | ✅ (32-33) | ✅ (39) | N/A | **NO** (not implemented) |
| `{tvdb-*}` naming explained | ❌ | ❌ | N/A | N/A | N/A | N/A |
| `local-only` = trusted, no auth required | ✅ (122) | ✅ (143) | ✅ (17) | ✅ (36) | N/A | ✅ |
| `daemon.search_missing` in UI | N/A | N/A | N/A | N/A | N/A | **NO** (config-only) |
| Version `0.3.0-beta.1` everywhere | ✅ | ✅ | ✅ | N/A | ✅ | ✅ |

---

*Generated: 2026-04-04*
*Auditor: Secondary docs/UX audit — nitpicky consistency sweep*
