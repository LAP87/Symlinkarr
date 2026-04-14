# Symlinkarr Web UI Charter

This document is the design brief for future UI work, including Stitch explorations.

## Goal

Make Symlinkarr feel immediately familiar to `*arr` users:

- fast to scan visually
- dense but calm
- dark-first and operational
- advanced when needed, not noisy by default
- explanatory without forcing long reading on every page

## Reference Pattern

Reference products:

- Sonarr
- Radarr
- other `*arr` operator panels

What to borrow:

- left navigation with predictable groups
- compact cards and tables
- restrained accent usage
- short, operator-first labels
- settings and diagnostics hidden behind an advanced posture

What not to borrow blindly:

- unnecessary chrome
- novelty components
- long top-of-page essays
- dense jargon with no escape hatch

## Layout Rules

- Keep the app shell stable across every page.
- Navigation groups should stay predictable: `Overview`, `Activity`, `Maintenance`, `System`.
- Page headers should answer only three things: where am I, what is this page for, what should I look at first.
- Metrics should be compact and comparable at a glance.
- Tables should stay dense and operational, not marketing-like.

## Copy Rules

- Prefer a one-line page summary over a paragraph.
- Prefer short section copy over prose blocks.
- Put edge cases, caveats, and theory behind `Advanced` or in the wiki.
- When a page needs explanation, add a `Learn more` link to the GitHub wiki instead of expanding the body text.
- Keep mutation wording direct: `Scan`, `Repair`, `Validate Config`, `Open Prune Preview`.

## Help-Link Rules

- Every complex page should expose at least one contextual wiki link near the page header.
- Link labels should describe intent, not just destination.
- Use the wiki for:
  - operator workflow
  - safety posture
  - media-server behavior
  - installation and first-run expectations
- Do not overload every section with links. One or two good links near the top is enough.

## Theme Rules

- `Dark` is the default and should remain closest to `*arr`.
- `Light` must feel intentionally light, not dark surfaces on a white canvas.
- `Matrix` is the only high-contrast alternative for now.
- Remove low-value theme variants unless they solve a real operator problem.

## Interaction Rules

- Expensive pages should render the shell first and load heavy content after.
- Advanced diagnostics should be hidden by default.
- Dangerous flows should stay review-first.
- Background work should be explicit and easy to understand from status surfaces.

## Current Wiki Targets

Use these URLs for contextual help until feature-specific wiki pages exist:

- `Scan`: `https://github.com/LAP87/Symlinkarr/wiki/Operations-and-Safety`
- `Discover`: `https://github.com/LAP87/Symlinkarr/wiki/Operations-and-Safety`
- `Cleanup`: `https://github.com/LAP87/Symlinkarr/wiki/Operations-and-Safety`
- `Dead Links / Repair`: `https://github.com/LAP87/Symlinkarr/wiki/Operations-and-Safety`
- `Doctor`: `https://github.com/LAP87/Symlinkarr/wiki/Operations-and-Safety`
- `Config`: `https://github.com/LAP87/Symlinkarr/wiki/Getting-Started`
- `Status / Media Refresh`: `https://github.com/LAP87/Symlinkarr/wiki/Media-Servers`

## Stitch Prompt Seed

Use this when exploring directions in Stitch:

> Design a self-hosted media operator dashboard that feels native beside Sonarr and Radarr. Use a left sidebar, compact dense tables, restrained cyan/blue accents, dark-first surfaces, and minimal prose. Hide advanced diagnostics behind a toggle. Keep page headers short and operational. Favor scanability, safety cues, and familiar `*arr` information hierarchy over novelty.
