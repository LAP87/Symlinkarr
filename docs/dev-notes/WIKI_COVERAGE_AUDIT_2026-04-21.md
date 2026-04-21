# Wiki Coverage Audit - 2026-04-21

Purpose: capture the current mismatch between web UI help-link coverage and the actual wiki information architecture.

This is not a feature plan. It is an operator-docs audit so future UI polish and wiki cleanup can converge on the same map.

## Current UI Coverage

Pages with explicit wiki/help links today:

- `scan`
- `cleanup`
- `status`
- `doctor`
- `dead_links`
- `config`
- `discover`
- `noconfig`

Pages without direct wiki/help links today:

- `dashboard`
- `links`
- `backup`
- `scan_history`
- `scan_run`
- `prune_preview`
- `anime_remediation`
- `discover_content`
- result pages such as `scan_result`, `cleanup_result`, `repair_result`, `backup_result`, `anime_remediation_result`

## Current Wiki Structure

The current intended wiki structure is:

- `Home`
- `Getting Started`
- `User Guide`
- `Operations and Safety`
- `Media Servers`
- `Troubleshooting`
- `Roadmap and Remaining Work`

This is coherent at the top level, but several pages are too broad to serve as precise landings from a focused UI surface.

## Main Problem

There are really two separate issues:

1. Some UI pages still have no corresponding help link at all.
2. Several existing wiki targets are too dense and multi-purpose, so even a present link does not reliably land the operator on the exact answer they need.

In practice, this means the UI can contain a valid-looking help link that still behaves like a weak destination.

## Where Current Wiki Pages Are Too Broad

### `User Guide`

Too broad for:

- `discover`
- `scan_history`
- `scan_run`
- `anime_remediation`

Why:

- these pages are operationally different
- they each need their own intent, glossary, and "what to do next" framing

### `Operations and Safety`

Too broad for:

- `cleanup`
- `prune_preview`
- `dead_links`
- `doctor`
- `backup`

Why:

- it mixes mutation safety, recovery posture, and review workflow into one landing page
- the operator often needs a much narrower answer, for example:
  - "When should I repair vs prune?"
  - "What does a blocked prune finding mean?"
  - "What exactly can restore overwrite?"

### `Getting Started`

Too broad for:

- `config`
- `noconfig`
- `bootstrap`
- `restore`

Why:

- first install, fresh recovery, config inspection, and restore semantics are not the same user intent

## Recommended Wiki Split

Short version: the wiki should be split by operator task, not by broad system area.

Recommended target pages:

- `Dashboard and Daily Operations`
- `Scan, History, and Why-Not Signals`
- `Repair and Dead Links`
- `Cleanup, Audit, and Prune Preview`
- `Backup and Restore`
- `Configuration and Doctor`
- `Discover and Queue`
- `Anime Remediation`
- `Media Server Refresh and Deferred Work`

The existing broad pages can still exist, but they should become index pages that route deeper rather than carrying every detail themselves.

## Recommended UI Mapping

### High-priority pages missing direct help links

- `dashboard` -> `Dashboard and Daily Operations`
- `backup` -> `Backup and Restore`
- `scan_history` -> `Scan, History, and Why-Not Signals`
- `scan_run` -> `Scan, History, and Why-Not Signals`
- `prune_preview` -> `Cleanup, Audit, and Prune Preview`
- `anime_remediation` -> `Anime Remediation`

### Medium-priority pages missing direct help links

- `links` -> `Repair and Dead Links`
- `discover_content` -> `Discover and Queue`
- `backup_result` -> `Backup and Restore`
- `cleanup_result` -> `Cleanup, Audit, and Prune Preview`
- `repair_result` -> `Repair and Dead Links`

### Lower-priority / likely unnecessary

- `scan_result`
- `anime_remediation_result`

These are transitional outcome pages and may not need dedicated help links if their parent pages are well covered.

## Short-Term Action

Add missing help links only where we can point to a page that is already good enough:

- `dashboard` -> `Home` or a future dashboard page
- `backup` -> current restore/backup page, even if imperfect
- `scan_history` and `scan_run` -> current `User Guide` only as a temporary stopgap

This is only worth doing if we also label the destination work as temporary.

## Better Action

Do the docs work in this order:

1. Define the future wiki page map.
2. Split the most overloaded wiki pages into narrower operator-task pages.
3. Update UI help links once those destinations actually deserve traffic.

## Recommendation

Do not treat this as "add more links everywhere."

The right goal is:

- every major operator page gets a contextual help target
- every help target answers the exact job implied by the page
- broad wiki pages become navigation hubs, not dumping grounds
