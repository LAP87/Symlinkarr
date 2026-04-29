# Provider Source Import

`symlinkarr import` is for bootstrap and recovery cases:

- you already have a provider/RD mount with media
- the matching Arr/media-library folders do not exist yet
- you rebuilt hardware and do not have the old Symlinkarr database
- you want a quick first pass, then normal scan/daemon can clean up the details

It is not a destructive cleanup tool. It does not remove provider, RD, or Usenet content.

## Common Runs

Preview one movie folder:

```bash
symlinkarr import \
  --source /mnt/rd/movies \
  --destination /library/movies \
  --content-type movie \
  --mode preview
```

Broad provider-root bootstrap:

```bash
symlinkarr import \
  --source /mnt/rd \
  --movie-destination /library/movies \
  --tv-destination /library/tv \
  --anime-destination /library/anime \
  --content-type auto \
  --mode aggressive \
  --yes
```

Use `--report-path /config/reports/import.json` when automation needs a known report location.

## Web Import

The Web UI has an `Import` page for preview and apply. It builds the plan first so you can check:

- source shape detection
- destination routing
- warnings before forced runs
- candidate confidence and review rows

The candidate table can be filtered by confidence so low or ambiguous rows are easy to inspect before applying. Web import defaults to safe writes. Enable `Force` when you want the bootstrap behavior that tries to include low-confidence or unresolved candidates. Applied Web imports write a JSON report under the configured backup path in `import-reports/`.

## Source Shapes

Import accepts both narrow and broad source paths:

- direct item: one movie/show folder or one video file is treated as one candidate.
- multi-item folder: a folder such as `/mnt/rd/movies` is split into child items.
- provider root: a mixed root such as `/mnt/rd` is allowed intentionally. Common category folders such as `Movies`, `Shows`, `Series`, `Anime`, `4K`, and `Downloads` are expanded one level before candidates are classified and routed.

File-only folders are classified conservatively. A folder with several loose movie-like files is treated as a multi-item source, while sibling episode files for the same show such as `Show.S01E01.mkv` and `Show.S01E02.mkv` stay grouped as one direct show item.

## Modes

- `preview`: report only.
- `safe`: high-confidence writes only; skips destination conflicts.
- `aggressive`: fast bootstrap; can create unresolved links and replace existing symlink targets, but never overwrites real files.

In the Web UI, `Force` is the friendly label for the CLI's `aggressive` bootstrap behavior. It means "try to include low-confidence or unresolved candidates"; it does not mean Symlinkarr will overwrite real files or remove provider content.

Default write behavior creates provider symlinks. Use `--folders-only` when you only want real ID-tagged folders and prefer normal scan/daemon to create file links later. `--create-links` is available as an explicit spelling for the default and cannot be combined with `--folders-only`.

Default writes create symlinks from the destination library back to the selected provider item. When a Symlinkarr DB is available and the candidate has an ID, Symlinkarr also records the new link in its database. `--folders-only` creates real ID-tagged folders instead.

## Rebuild From Provider Mount

Use this when new hardware or a lost DB left you with provider/RD content but no local library surface:

1. Restore or create `config.yaml`.
2. Open Web Import or run `symlinkarr import --mode preview`.
3. Review the report, especially low-confidence and unresolved rows.
4. Apply from Web Import or rerun CLI with `--mode safe --yes`.
5. Enable `Force` or use `--mode aggressive --yes` only when you want the broader bootstrap pass.
6. Run `symlinkarr scan` or leave the daemon running so normal matching and repair can take over.
7. Let media-server scheduled scans/folder watchers catch up, or drain deferred refresh work if you queued it.

## Routing Example

Rules are evaluated top-down, first match wins inside each content bucket:

```xml
<importRules>
  <destinations>
    <movies default="/library/movies">
      <route resolution="2160p" to="/library/movies-4k" />
      <route hdr="dv-p8" to="/library/movies-dolby-vision" />
      <route sourcePathContains="/Kids/" to="/library/kids" />
    </movies>
    <tv default="/library/tv" />
    <anime default="/library/anime">
      <route audio="jpn" subtitles="eng" to="/library/anime-subbed" />
    </anime>
  </destinations>
</importRules>
```

## Lookup And Rules

Default lookup is cache-first. Use `--offline` for explicit IDs only. Use `--lookup-mode remote` only when you want bounded TMDB/TVDB searches during the run. Add `--refresh-metadata` when that remote run should bypass existing metadata cache. Remote lookup stops at `--max-lookups` and leaves the rest unresolved for cache, explicit IDs, or a later run.

Successful remote matches are written to both the normal metadata cache and a direct import-resolution cache keyed from content type, normalized title, and year. A later run can reuse that result before making another TMDB/TVDB request.

Anime-Lists is cache-backed as well. Import should reuse cached anime identity hints where available, but it should not fetch Anime-Lists fresh for every run. If a mapping looks stale, refresh it explicitly:

```bash
symlinkarr cache invalidate anime-lists
```

`--rules /config/import-rules.xml` can route by path/title hints and, when probing is enabled, by stream metadata such as resolution, codec, HDR, audio language, and subtitle language. Dolby Vision profile labels are exposed as HDR tokens when the probe tool reports them, for example `dv-p8`.

Keep `--metadata-mode fast` unless rules need stream metadata. `probe` and `strict` use `ffprobe` or `mediainfo` and can cause provider-backed reads. When a Symlinkarr DB is available, successful probe results are cached by source path, size, mtime, and selected probe tool so repeated import runs avoid re-reading the same provider file.

## After Import

After import:

- review the JSON report
- Web reports live under `backup.path/import-reports/`
- CLI reports live at `--report-path` when set, otherwise in the current working directory
- run `symlinkarr scan` or let the daemon take over
- import itself does not force Plex, Jellyfin, or Emby to scan immediately
- low-confidence and unresolved rows are expected in Force/bootstrap runs and should be checked before relying on them
- if Plex, Jellyfin, or Emby already have folder watchers or scheduled scans enabled, avoid stacking extra manual scans at the same time
- if you use deferred media refresh and have queued work, run `symlinkarr refresh drain`
