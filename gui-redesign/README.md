# GUI redesign mockup

Static, self-contained proposal for a cleaner Symlinkarr web UI. **Nothing here is wired to
the app** — it touches no file under `src/web/`. It exists so the new direction can be reviewed
and accepted (or discarded) before any real template/CSS work begins.

## View it
```bash
python3 -m http.server 8750 --directory gui-redesign
# open http://localhost:8750
```
Open `index.html` directly in a browser also works. Click the sidebar, the row `⋯` menus,
the "Run scan"/"Actions" dropdowns, the Settings sub-tabs, and the theme toggle (top right).

## Rollback
Delete the folder:
```bash
rm -rf gui-redesign
```
No other change is needed — production UI is untouched.

## What this changes vs. the current UI
- **No self-linking panel sprawl.** The old Dashboard/Config stack several "what this page is
  for" / "best follow-up" / wiki-link cards that mostly point back at the app. Removed.
- **Settings is one tidy page** with a section rail (General / Libraries / Matching / Services /
  Backup) + grouped form rows, instead of a column of open `<details>` disclosure cards.
- **Actions live in dropdown menus** (Run scan ▾, Actions ▾, per-row `⋯`) rather than scattered
  button rows and duplicated quick-links.
- **Real line icons** instead of the `OV`/`SC`/`ST` letter-pair placeholders.
- **Calmer visual system:** one restrained accent, hairline borders, tabular numbers with
  thousands separators (`77,177`), a single-color sparkline — no neon gradient/glow.
- **Theme-able** via CSS variables (dark + light included; toggle top-right).

## Themes are moddable files (old-school)
No in-app colour picker — the schemes live in plain, editable files:

- **`themes.css`** — one `[data-theme="id"]{ … }` block per scheme. A block only sets
  6 primitives (`--bg --text --accent --good --warn --bad`); surfaces, borders, dim text and
  soft badges are all *derived* via `color-mix` in the shared block at the top.
- **`themes.js`** — a manifest array the picker reads (`id`, `name`, `group`, 3 preview swatches).

**Add a theme = paste a 6-value CSS block + one manifest line.** That's the whole mod.
19 schemes ship by default (Core, Catppuccin, editor picks); the picker is the paint-splash
icon top-right and the choice persists in `localStorage`.

## If accepted
The design tokens at the top of `index.html` (`:root`) map onto the existing
`src/web/static/style.css` variable names, so migration is: port the token block, rebuild
`base.html` nav with the SVG icons, and convert `config.html` + dashboard partials to the
grouped/section-rail layout shown here.
