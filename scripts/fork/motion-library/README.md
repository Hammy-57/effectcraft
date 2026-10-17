# Motion Library on every machine (fork)

The **Motion Library** panel (Window ▸ Motion Library) shows Elias's card library wherever
EffectCraft runs: the Mac, a rented vast.ai GPU, the browser build. The code ships with the app
(`crates/motionlib`, `crates/engine/src/commands/motion_library.rs`,
`crates/ui-egui/src/panels/motion_library.rs`); the cards live in a private Cloudflare R2 bucket.

## How it fits together

```
Mac: ~/Documents/Elias Motion Library/effectcraft/library.json + projects/*.ecproj
      │  publish (hash diff: only new files go up; then manifest, then head.json version+1)
      ▼
Worker effectcraft-motion-library (bearer token) ──► R2 bucket motion-library (private)
      ▲      head.json · blobs/<sha256>[.gz]
      │  panel open: head.json · scroll: thumbnails · hover: preview · apply: .ecproj + its files
EffectCraft anywhere ──► local cache by content hash (~/Library/Caches/EffectCraft/Motion Library,
                         ~/.cache/effectcraft/motion-library on Linux)
```

- **Footage stays home.** Video files are never uploaded, nor files over the size limits
  (Elias's pick, 17 Oct 2026: previews ≤ 2 MB, SceneYard files ≤ 20 MB, everything else ≤ 90 MB;
  about 1.77 GB in the bucket, 58 of 442 cards need footage). The card's
  project still names them, so applying the card imports them as *missing* footage with their
  name, size, duration and rate; the card shows **needs footage**. Relink with
  `footage.relink {item, path}` (or File ▸ Replace Footage). On the Mac the original path still
  exists, so the footage is just there.
- **Update chip.** Opening the panel reads `head.json`. A newer version than the cached one shows
  "Library updated: 3 new · 2 changed"; clicking it switches (`library.update`).
- **Offline** the cached catalog and every downloaded card still work; the panel says it's offline.

## Commands (also MCP `command_run`)

| Command | What |
| --- | --- |
| `library.status` | configured?, remote, version, cards, needsFootage, update, error |
| `library.sync {wait?, adopt?}` | check the remote (first catalog adopted at once) |
| `library.update` | switch to the newer catalog (the chip) |
| `library.catalog {folder?, query?, limit?, offset?}` | browse (`folders` in the library's order, with counts, depth, icon) |
| `library.search {query, source?}` | Motion Library cards (`source: "motion"`) first, then folder libraries |
| `library.card {id}` | a card with its files and placeholders |
| `library.apply {id, comp?, time?, wait?}` | download + import + nest at the playhead, one undo step; `needsFootage` lists items to relink |
| `library.configure {remote?, token?, cache?}` | writes `motion-library.json` in the settings folder |
| `library.publish {library, folders?, maxFileMB?, includeVideo?, catalogAll?, dryRun?, force?}` | desktop: publish |

The panel's view is the UI command `motionLibrary.view {folder?, query?, scale?, cardSize?, expand?,
collapse?, kind?, hereOnly?, showFolders?, showTools?, popover?, details?}` (interface scale 80–140 %, card size 88–1200, as the web panel's Display
settings). The manifest carries the web panel's collection order and icons (`collections`), each
card's type label and location (`Comp · Devin Jatho/UI`) and thumbnail size; videos without a
thumbnail get a poster of their middle frame (`catalogAll` / `--catalog-all`, the Mac script's default:
every other library entry is listed too, thumbnail only, badged **on the Mac** and never applied
here; `library.search` leaves those out) (`effectcraft/posters/`, made by
`poster.swift` next to the exporter).

The panel has the web panel's left rail (Assets, Curves, Search, Filters, Collections, SFX, Tools,
the label strip → `edit.label`, the library state dot, refresh, Display settings, + Save), card
details (ⓘ), drag a card onto the Timeline or the viewer (`motionLibrary.apply {id, time?}`), and
the quick actions on the right, each an engine command: `layer.smartPrecompose`,
`layer.decompose`, `layer.multiPrecompose`, `project.deepDuplicate`, `layer.splitMasks`,
`shape.gradientLock` (and `keys.easyEase`).

Search ranks cards like the Python panel (`retrieval.py`; parity test in
`crates/motionlib/src/search.rs`).

## Configuration

First found wins:

1. Environment: `EFFECTCRAFT_LIBRARY_REMOTE` (Worker URL or a folder), `EFFECTCRAFT_LIBRARY_TOKEN`,
   `EFFECTCRAFT_LIBRARY_CACHE`.
2. `motion-library.json` in the settings folder (`~/Library/Application Support/EffectCraft` on the
   Mac; the platform config folder elsewhere): `{"remote": "https://…", "token": "…"}`.

### On a rented GPU (cloud preview)

`scripts/fork/cloud.sh connect` installs the read token on the machine (ssh stdin, `motion-library.json`,
mode 600, for root and `user`), so the browser desktop app there has the panel too. When Elias applies a
card in the **Mac** app while the cloud preview is connected, the mirror sees the card's files by their path
in the Mac's cache (`blobs/<sha256>/<name>`) and the server downloads them from the Worker itself
(`effectcraft-cli library-pull`, sha256 + size checked), so they cross the home connection once (R2 → Mac),
not twice. If the server can't fetch them, the Mac uploads them as before. Details: `docs/fork/cloud.md`.

Tokens never go in the repo, a project or a command reply. The Worker has two: `READ_TOKEN` for
every machine (read-only) and `PUBLISH_TOKEN` for the Mac.

## Setting up Cloudflare (once, on the Mac)

```sh
CLOUDFLARE_ACCOUNT_ID=<account id> scripts/fork/motion-library/setup.sh
```

Creates the private bucket, deploys `worker.js`, sets both tokens as Worker secrets and writes
`motion-library.json` (publish token) and `motion-library.read.json` (read token, for rented
machines) to the settings folder. Free-plan limits that matter: 10 GB stored, uploads up to 100 MB per file (hence the 90 MB
limit). Live since 17 Oct 2026: bucket `motion-library` on Hakifx91's account (r2.dev URL off, no
custom domain), Worker `effectcraft-motion-library`.

## Publishing from the Mac

`publish-motion-library.sh` (next to the Python tools in
`~/Developer/AE Motion graphics/integrations/motion-library/`) refreshes `library.json` and
publishes. Or directly:

```sh
cargo run --release -p effectcraft-motionlib --example publish -- \
    --library "$HOME/Documents/Elias Motion Library/effectcraft" --remote "$EFFECTCRAFT_LIBRARY_REMOTE" \
    --folders "Devin Jatho,SceneYard" --max-preview-mb 2 --folder-max "SceneYard=20" [--dry-run]
```

The folder list is the `--folders` option (`MOTION_LIBRARY_FOLDERS` in the script); the limits are
`--max-mb`, `--max-preview-mb` and `--folder-max "Folder=MB,…"`. A dry run prints what would go up;
blobs the bucket already has (also from an interrupted publish) are skipped. Hashes are
cached in `.motion-library-hashes.json` next to `library.json`, so a re-publish only reads changed
files and uploads only new blobs. A local folder works as the remote too (the local backend used
for testing).
