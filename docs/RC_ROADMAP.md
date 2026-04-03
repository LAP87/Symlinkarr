# Symlinkarr RC Roadmap

This is the current high-signal list of what still remains before Symlinkarr should be called a real `1.0 RC`.

## Current Position

Symlinkarr is now past “rough beta” territory and into “power-user beta / RC-prep”.

Already working in real use:

- scan, match, link, repair, cleanup, and cache flows
- web UI and JSON API
- guarded cleanup and guarded anime remediation preview/apply
- guarded anime remediation is now reachable from the web UI, not only CLI/API
- Plex, Emby, and Jellyfin invalidation adapters
- multi-backend refresh fan-out
- persisted scan telemetry and per-backend refresh history
- media-server-free usage when no Plex/Emby/Jellyfin backend is configured

Still true in live data:

- anime legacy cleanup is the messiest remaining operational area
- Plex overload and refresh pressure still need continued hardening
- some of the most important operator questions are finally visible in UI/API, but remediation still needs to become safer and easier to trust at larger scale

## What Must Be Finished Before `1.0 RC`

### 1. Remediation Trust

This is the biggest remaining category.

Must finish:

- increase safe eligibility where possible without lowering cleanup safety
- keep every remediation path quarantine-first for foreign/legacy material
- keep the new web preview/apply remediation flow as safe and informative as the CLI path

Why:

- users need to trust that Symlinkarr will not silently make legacy-folder situations worse
- right now only a small fraction of correlated anime groups are auto-eligible

### 2. Scan and Link Observability

The next trust problem is “why did this not link?”

Must finish:

- persist and surface meaningful skip reasons for scan/link outcomes
- make “ambiguous”, “source missing before link”, and similar cases visible in UI/API
- keep overview screens useful without forcing the operator into log files
- continue improving per-backend refresh visibility

Why:

- a stable product cannot feel random when it skips something

### 3. Media-Server Hardening

The adapter layer is real now, but it still needs more operational polish.

Must finish:

- continue tuning refresh pacing, cap guards, and abort behavior against real library load
- keep partial-failure semantics honest across scan, cleanup, repair, and remediation
- validate the new Emby/Jellyfin library-root fallback and refresh-lock semantics under real concurrent load
- avoid media-server overload during cleanup/remediation runs

Why:

- a working invalidation layer is not enough if it can still destabilize the media server

### 4. Runtime Safety Parity

Safety rules need to stay uniform across every mutation surface.

Must finish:

- ensure every destructive or semi-destructive path respects mount/runtime health gates
- keep CLI, web UI, and API behavior aligned
- keep blocking behavior explicit instead of success-shaped no-ops

Why:

- RD mount failures are common enough that this cannot be inconsistent

### 5. Docs and Operator Onboarding

The project has grown past “the README is enough”.

Must finish:

- keep README slim and useful
- keep wiki, CLI manual, and API schema aligned with reality
- maintain a clear “what is stable vs what is still RC work” story
- keep the roadmap current when work lands

Why:

- broader users will judge stability partly from how predictable the docs and behavior are

## Important, But Not Required For `1.0 RC`

These are good next steps, but they should not block RC if the trust/safety work above is done.

- Emby/Jellyfin DB compare adapters
- Emby/Jellyfin duplicate-correlation or remediation helpers
- item-ID-based refresh/invalidation for Emby or Jellyfin
- more aggressive missing-search acquisition strategies
- broader non-anime duplicate remediation
- richer dashboards and cosmetic UI work

## Explicitly Not Ready To Ship

Do not ship these as “safe defaults” yet:

- blind permanent deletion of duplicate groups
- automatic remediation of the whole correlated anime backlog
- broad non-anime cleanup based on anime heuristics
- large matcher rewrites without tight regression coverage
- event-driven/watchdog mode as a default runtime model

## Immediate Next Slices

If work resumes right now, the best next slices are:

1. real concurrent-load validation for Plex/Emby/Jellyfin refresh, especially around the new refresh-lock and Emby/Jellyfin root-fallback guard
2. raise safe anime remediation eligibility without weakening quarantine-first guarantees
3. extend the same remediation trust model into broader legacy/foreign cleanup cases outside anime

## Supporting Docs

- [README.md](../README.md)
- [CLI manual](CLI_MANUAL.md)
- [API schema](API_SCHEMA.md)
- [Media-server adapter plan](MEDIA_SERVER_ADAPTER_PLAN.md)
