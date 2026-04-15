# Design Council — Theme Portfolio — 2026-04-15

Source audit: exact upstream theme tokens pulled from official *arr frontend theme files on 2026-04-15.

## Decision

Symlinkarr should ship the exact upstream *arr light/dark palettes as first-class built-in themes, not approximate homages.

That means:

- `sonarr-dark`
- `sonarr-light`
- `radarr-dark`
- `radarr-light`
- `prowlarr-dark`
- `prowlarr-light`
- `lidarr-dark`
- `lidarr-light`
- `readarr-dark`
- `readarr-light`

These should be the default theme portfolio presented in settings and available without any import/custom theme work.

## Why

- Operators already know these palettes and associate them with the surrounding media stack.
- They are visually distinct without needing a full UI rewrite.
- They give us a stable semantic baseline before we add broader “fun” theme packs.
- Exact upstream parity is better than “inspired by” drift.

## Exact Upstream Baselines

Shared family conventions observed across the audited apps:

- dark surface base is usually `pageBackground #202020`, header/sidebar `#2a2a2a`, toolbar `#262626`, card `#333333`, text `#ccc`
- light surface base is usually `pageBackground #f5f7fa`, card `#fff`, text `#515253`
- status colors are largely shared: `primaryColor #5d9cec`, `dangerColor #f05050`, `warningColor #ffa500`

Per-app brand tokens:

| App | Mode | Brand token(s) | Key shell colors |
|---|---|---|---|
| Sonarr | Dark | `themeBlue #35c5f4`, `themeAlternateBlue #2193b5` | `themeDarkColor #494949`, `themeLightColor #595959`, `pageBackground #202020`, `cardBackgroundColor #333333` |
| Sonarr | Light | `themeBlue #35c5f4`, `themeAlternateBlue #2193b5` | `themeDarkColor #3a3f51`, `themeLightColor #4f566f`, `pageHeaderBackgroundColor #2193b5`, `sidebarBackgroundColor #3a3f51`, `toolbarBackgroundColor #4f566f` |
| Radarr | Dark | `themeBlue #ffc230` | `themeDarkColor #494949`, `themeLightColor #595959`, `pageBackground #202020`, `cardBackgroundColor #333333` |
| Radarr | Light | `themeBlue #ffc230` | `themeDarkColor #595959`, `themeLightColor #707070`, `pageHeaderBackgroundColor #464b51`, `sidebarBackgroundColor #595959`, `toolbarBackgroundColor #707070` |
| Prowlarr | Dark | `themeBlue #e66000` | `themeDarkColor #595959`, `themeLightColor #e66000`, `pageBackground #202020`, `cardBackgroundColor #333333` |
| Prowlarr | Light | `themeBlue #e66000` | `themeDarkColor #595959`, `themeLightColor #707070`, `pageHeaderBackgroundColor #e66000`, `sidebarBackgroundColor #595959`, `toolbarBackgroundColor #707070` |
| Lidarr | Dark | `themeBlue #00A65B`, `themeAlternateBlue #00a65b` | `themeDarkColor #494949`, `themeLightColor #595959`, `pageBackground #202020`, `cardBackgroundColor #333333` |
| Lidarr | Light | `themeBlue #00A65B`, `themeAlternateBlue #00a65b` | `themeDarkColor #353535`, `themeLightColor #1d563d`, `pageHeaderBackgroundColor #00A65B`, `sidebarBackgroundColor #353535`, `toolbarBackgroundColor #1d563d` |
| Readarr | Dark | `themeRed #ca302d`, `themeAlternateRed #a41726`, `themeDarkRed #66001a` | `themeDarkColor #494949`, `themeLightColor #595959`, `pageBackground #202020`, `cardBackgroundColor #333333` |
| Readarr | Light | `themeRed #ca302d`, `themeAlternateRed #a41726`, `themeDarkRed #66001a` | `themeDarkColor #353535`, `themeLightColor #810020`, `pageHeaderBackgroundColor #ca302d`, `sidebarBackgroundColor #353535`, `toolbarBackgroundColor #810020` |

## Source Files

- Sonarr dark: <https://github.com/Sonarr/Sonarr/blob/1449b8152545171a6f628a0e2ce6292e4c420da8/frontend/src/Styles/Themes/dark.js>
- Sonarr light: <https://github.com/Sonarr/Sonarr/blob/1449b8152545171a6f628a0e2ce6292e4c420da8/frontend/src/Styles/Themes/light.js>
- Radarr dark: <https://github.com/Radarr/Radarr/blob/4b85fab05bc37a51c2e673673d9cabd4113fedd8/frontend/src/Styles/Themes/dark.js>
- Radarr light: <https://github.com/Radarr/Radarr/blob/4b85fab05bc37a51c2e673673d9cabd4113fedd8/frontend/src/Styles/Themes/light.js>
- Prowlarr dark: <https://github.com/Prowlarr/Prowlarr/blob/46ce8e270138e757b14cc1b42b259419a2fac979/frontend/src/Styles/Themes/dark.js>
- Prowlarr light: <https://github.com/Prowlarr/Prowlarr/blob/46ce8e270138e757b14cc1b42b259419a2fac979/frontend/src/Styles/Themes/light.js>
- Lidarr dark: <https://github.com/Lidarr/Lidarr/blob/fd6f97640cb75af1143a64c917376108f14f6d69/frontend/src/Styles/Themes/dark.js>
- Lidarr light: <https://github.com/Lidarr/Lidarr/blob/fd6f97640cb75af1143a64c917376108f14f6d69/frontend/src/Styles/Themes/light.js>
- Readarr dark: <https://github.com/Readarr/Readarr/blob/0b79d3000d4e5f8425f499970b0190e2c421fceb/frontend/src/Styles/Themes/dark.js>
- Readarr light: <https://github.com/Readarr/Readarr/blob/0b79d3000d4e5f8425f499970b0190e2c421fceb/frontend/src/Styles/Themes/light.js>

## Product Recommendation

Implement theme support in two layers:

1. Semantic UI tokens for Symlinkarr components.
2. Vendor theme packs that map those semantic tokens to exact upstream *arr values.

That keeps component code stable while allowing exact theme parity.

## Default Portfolio Order

Recommended order in the UI:

1. `Auto`
2. `Sonarr Light`
3. `Sonarr Dark`
4. `Radarr Light`
5. `Radarr Dark`
6. `Prowlarr Light`
7. `Prowlarr Dark`
8. `Lidarr Light`
9. `Lidarr Dark`
10. `Readarr Light`
11. `Readarr Dark`

Rationale:

- Sonarr is the most neutral baseline pair.
- The rest of the family then reads as deliberate brand variants, not random skins.

## Fun Theme Pack After Baseline

Once the exact *arr set is landed, the next curated pack should be:

- Catppuccin Latte
- Catppuccin Frappe
- Catppuccin Macchiato
- Catppuccin Mocha
- Matrix
- 2–4 terminal/TUI-inspired themes in the spirit of Nanocoder

These should come after, not before, the exact upstream parity work.

## Implementation Notes

- Do not hardcode theme colors in templates once this work starts.
- Prefer a checked-in theme manifest or Rust/JSON token table over ad hoc CSS fragments.
- Keep dark/light paired for every built-in family theme.
- Preserve status semantics across themes: success, warning, danger, selected, focus states must remain legible and consistent.
- Avoid a free-form theme editor in the first pass; curated presets are enough.

## Non-Goals

- Full theme marketplace
- Per-widget custom colors
- Runtime CSS authoring by users
- “AI generated” themes without a curated token spec
