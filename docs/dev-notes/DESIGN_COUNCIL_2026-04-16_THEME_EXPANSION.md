# Design Council — Theme Expansion — 2026-04-16

Source: follow-up after landing exact upstream *arr light/dark themes and a checked-in theme manifest in the web UI.

## Decision

The next curated theme pack after the exact *arr baseline should be:

1. `Catppuccin Latte`
2. `Catppuccin Frappe`
3. `Catppuccin Macchiato`
4. `Catppuccin Mocha`
5. `Matrix`
6. `Amber Terminal`
7. `Cyan Terminal`
8. `Phosphor`

This is intentionally a small curated set, not a theme marketplace.

## Why This Pack

- Catppuccin gives four widely recognized palettes with good dark/light coverage and strong operator appeal.
- Matrix stays because it is the most distinctive high-contrast novelty theme already associated with the app.
- Terminal/TUI-inspired themes capture the Nanocoder-style appeal without turning the app into a random color dump.
- A capped pack keeps the picker usable and reviewable.

## Product Rules

- Keep the exact *arr family as the default built-in portfolio.
- Do not let novelty themes displace the *arr family in the top of the picker.
- Keep every new theme inside the checked-in manifest; do not add one-off picker entries.
- Preserve semantic status colors and focus states even when the shell palette gets playful.
- Prefer themes that feel intentional and operator-friendly over “maximalist RGB”.

## Recommended Picker Order

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
12. `Matrix`
13. `Catppuccin Latte`
14. `Catppuccin Frappe`
15. `Catppuccin Macchiato`
16. `Catppuccin Mocha`
17. `Amber Terminal`
18. `Cyan Terminal`
19. `Phosphor`

## Implementation Notes

- Continue using the theme manifest as the single source of truth for ids, labels, files, and picker swatches.
- Group future themes visually in the picker only if the list becomes unwieldy; do not add UI grouping before it is needed.
- Add each new fun theme as paired shell/content token maps, not template-specific overrides.
- Reuse the shell/content split introduced for the *arr themes so light themes can keep a strong shell without dirtying the main content surface.

## Non-Goals

- User-authored themes
- Theme import/export
- Per-widget color customization
- Shipping dozens of near-duplicate terminal palettes
