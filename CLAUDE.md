# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

@ARCHITECTURE.md

## Keeping ARCHITECTURE.md current

- A change to process boundaries, the Node↔supervisor interface, the ffmpeg pipeline shape, the update/launch mechanism or an external integration updates `ARCHITECTURE.md` in the same commit.
- When a 3.0 part moves from `spikes/` to `src/`, update its proven/design marking there.
- Decisions and their reasoning go on the Notion pages, not in the repo; `ARCHITECTURE.md` names those pages, never links to them.

## Project Overview

StreamLoop restreams a YouTube playlist to Twitch around the clock from one Windows box. This branch holds 3.0: a Node.js/TypeScript backend (`src/server/`) drives a Rust supervisor (`supervisor/`), which plays the playlist through ffmpeg and pushes RTMP itself. There is no mpv or OBS. Node owns the config, the dashboard, Discord alerts, the Twitch checks and the updater. 2.x (mpv + OBS) lives on the `2.x` branch.

## Setup

```bash
npm install
cp config.example.json config.json   # Then edit config.json with your settings
```

`config.json` is gitignored to prevent committing credentials. `config.example.json` is the tracked template.

**Dev setup with the supervisor.** In dev, Node reads `config.json` from its working directory, and the supervisor reads `ROOT\app\config.json`. They must be the same file, so run Node from `ROOT\app`:

1. `cargo build --release` in `supervisor/`.
2. `node supervisor/scripts/dev-root.mjs` builds a dev root in the install layout (default `%TEMP%\streamloop-devroot`); put `supervisor\target\release\streamloop-supervisor.exe` in `ROOT\supervisor\`.
3. Start Node with cwd `ROOT\app` and `STREAMLOOP_ROOT=ROOT`, running the repo's tsx on the repo's `src/server/index.ts` by absolute path. `installRoot()` uses `STREAMLOOP_ROOT` when there is no `node\` folder next to the app.

Node's own logs and timeline still go to the repo's `logs/` in dev, since `logger.ts` and `index.ts` resolve them from the source folder. `supervisor/scripts/node-session.mjs` does all of this for the end-to-end runs.

Test-only environment variables, never set by `config.json` or a release:
- `STREAMLOOP_DEBUG_FAULTS=1`: the launcher starts the supervisor with `--debug-faults` (opens `POST /debug/fault`). The flag belongs to the supervisor process, not to Node: a Node restarted without the variable attaches to the running supervisor, and `/debug/fault` stays open until that supervisor is restarted.
- `STREAMLOOP_TEST_HOOKS=1`: adds `POST /api/test/exit-75`, the update restart without an update.
- `STREAMLOOP_TEST_HLS_URL`: the Twitch read-back reads this HLS instead of the channel's, and the channel counts as live, so no request reaches Twitch and Helix credentials are not needed.
- `STREAMLOOP_TEST_RELEASES=<file>`: the updater reads its release list from this JSON file (GitHub's shape) instead of GitHub, and takes asset URLs only from `http://127.0.0.1:<port>/`.

## Commands

```bash
npm start            # Run server (tsx)
npm run dev          # Run with file watching (tsx watch)
npm test             # Run tests once (vitest run)
npm run test:watch   # Run tests in watch mode (vitest)
npm run build:release       # full ZIP in the install layout (node, app, supervisor, ffmpeg, yt-dlp, pot, data, START.bat), plus components.json
npm run build:release:slim  # update ZIP: app + START.bat only (streamloop-vX.Y.Z-update.zip)
node build/verify-release.mjs <zip>   # check a ZIP against build/release-layout.js (both builds run it)
cargo build --release       # in supervisor/: the supervisor binary
node supervisor/scripts/node-session.mjs A|B|C|treekill   # end-to-end runs of Node + supervisor against local MediaMTX
node supervisor/scripts/install-session.mjs              # install, update and switch-back proof from the dist/ ZIPs
node supervisor/scripts/update-session.mjs all|<run>[,<run>...]|build   # full update, rollback and bootstrap proof (runs: full-on, full-off, app-only, locked, broken, bootstrap) on N and N+1 repacked from the dist/ ZIPs; build only repacks them
```

No linter is configured. TypeScript strict mode is enabled via `tsconfig.json`.

## Release Checklist (3.x)

Before pushing and building a 3.x release (2.x releases come from the `2.x` branch and follow its own CLAUDE.md):

1. Bump the version in `package.json`. The version is read at startup and used by the updater to compare against GitHub Releases.
2. Update `RELEASE_NOTES.md` with the new version's changelog. This file is used when creating GitHub Releases.
3. **Build both ZIPs** after pushing: `npm run build:release` (full, `streamloop-vX.Y.Z.zip`) and `npm run build:release:slim` (update, `streamloop-vX.Y.Z-update.zip`). Each writes a matching `.sha256` (`<hash>  <filename>`) and `dist/node-version.txt`; the full build also writes `dist/components.json` (the version, the Node version and a hash per part, which the updater compares with the install). Each fails if `verify-release.mjs` finds a layout path missing. The full build needs `cargo` and `spikes/buffer/media/slate.mp4`.
4. The full ZIP is the install: `node\`, `app\`, `supervisor\` (exe + `slate.mp4`), `ffmpeg\ffmpeg.exe`, `yt-dlp\` (yt-dlp, deno, `plugins\bgutil-ytdlp-pot-provider.zip`), `pot\` (the token server, built under the bundled Node so `canvas` matches its ABI), `data\` and `START.bat`. Neither ZIP holds `app\config.json`: `loadConfig` creates it from `app\config.example.json` on first start, so a full-ZIP reinstall keeps the config and the protected stream key. The update ZIP is `app\` + `START.bat` only, what an app-only update swaps. When `components.json` names a part that differs from the install's, the updater takes the full ZIP instead and swaps those parts too, so every release ships both; the update ZIP is never a standalone release.
5. Create the GitHub release with `gh release create` and upload **both ZIPs, both `.sha256` files, `node-version.txt` and `components.json`**. The updater matches each checksum to its ZIP by name and compares `components.json` with the install's parts to choose an app-only or a full update. An install on 3.0.5 or older, or a release without `components.json`, reads `node-version.txt` instead, to say the install's Node is out of date.
6. **3.x releases are never marked latest.** Create them with `gh release create ... --latest=false`, and also `--prerelease` while 3.0 is on trial. 2.x installs older than 2.3.7 still read GitHub's "latest release" and would otherwise swap 3.0 into a 2.x folder; 2.3.7 and later only take releases with their own major version.
7. **Changed parts for existing installs.** yt-dlp keeps itself current through Node's `yt-dlp -U` on start (`autoUpdateYtdlp`). A release that changes Node, the supervisor, ffmpeg, the token server or the yt-dlp plugin reaches an install newer than 3.0.5 through the dashboard's update button, as a full update that takes the stream off air while the parts are swapped. An install on 3.0.5 or older takes two steps: the app-only update to the new version, then "Update parts to {version}" in the dashboard. The full ZIP by hand still works for any install: Stop, close the window, extract the ZIP into the folder it was first extracted into (`C:\StreamLoop3`, so its top folder `streamloop\` lands on `C:\StreamLoop3\streamloop`, never into the install itself, which would make `streamloop\streamloop\`), start, press Start. Config, key and `data\` survive because no ZIP ships `config.json`. `RELEASE_NOTES.md` names the parts a release changes and which of these paths an install takes.
8. Prove a release candidate with `node supervisor/scripts/install-session.mjs`: it installs the full ZIP into a temp folder, updates it with the update ZIP through an exit-75 swap, checks that the installed updater offers a prerelease one patch above it, switches to v2.3.8 and back, and removes both installs.

## Switching back to 2.x

3.0 installs beside 2.x in its own folder. Both use port 7654, so one Node runs at a time; 3.0 also refuses to launch its supervisor while `obs64.exe` runs, and 2.x (from 2.3.7) refuses to start its OBS stream while `127.0.0.1:4417/status` reports `encoder.alive: true`.

1. In the 3.0 dashboard press Stop and wait for Stopped (the supervisor exits; `data\stopped-by-user` is written).
2. Close the 3.0 `START.bat` window.
3. Start 2.x's `START.bat` and OBS.

Back to 3.0: close 2.x and OBS, start 3.0's `START.bat`, press Start. Until Start, the stopped-by-user marker keeps the supervisor down, also across restarts. Autostart points at one install; turning it on in one moves it there.

## Architecture

**Node backend + supervisor + admin dashboard.** `ARCHITECTURE.md` has the diagram, the supervisor's interface and the main flows.

- **Server** (`src/server/`): Express app on 127.0.0.1:7654. `index.ts` wires the components by dependency injection.
- **Supervisor** (`supervisor/`, Rust): launched by Node through `streamloop-supervisor.exe launch`, a short-lived intermediate, so it is a sibling with a dead parent: a Node crash, restart, app-only update or tree kill leaves the stream up. Node talks to it over HTTP on 127.0.0.1:4417 and reads its events from `ROOT\data\logs\supervisor-events-DATE.jsonl` (`events-DATE.jsonl` up to 3.0.4; Node reads both).
- **Admin Dashboard** (`src/admin/`): first-run wizard, status, timeline, transport (next, previous, restart, jump), Stop and Start stream, settings, webhooks, logs, update button.

**Server modules:**

| Module | Responsibility |
|--------|---------------|
| `config.ts` / `types.ts` | Loads and saves `config.json` with Zod validation and defaults, including the `supervisor` section the supervisor reads |
| `supervisor-client.ts` | HTTP client for the supervisor (`/status`, `/control`, `/reload`, `/shutdown`); `protectStreamKey` runs `streamloop-supervisor.exe protect` so the key is stored only as a DPAPI blob |
| `supervisor-launcher.ts` | `installRoot()`; polls `/status` every 2 s, launches the supervisor after 3 misses (backoff 10 s, 30 s, 60 s, 5 min), stays down after Stop stream until Start, across Node restarts (`ROOT\data\stopped-by-user`); `holdForStart` holds the launch until Start, `release()` or a supervisor that answers (the `autoStartStream` setting, an update's check) |
| `supervisor-events.ts` | Tails the supervisor's events file; `describe()` maps each event to timeline text and an optional Discord alert, `AlertGate` sends one alert per outage |
| `event-log.ts` / `event-store.ts` | The 100-entry dashboard timeline, persisted as daily JSONL in `logs/` |
| `twitch.ts` | Helix check that the channel is live while the encoder runs; restarts the encoder at most once per 30 min, then only alerts |
| `twitch-readback.ts` | Every 5 min, two frames of the channel's own HLS 5 s apart; an overlay clock that stands still twice asks for `restart-encoder` |
| `discord.ts` | Webhook notifications with templates, batched over 5 s, criticals sent at once |
| `playlist-metadata.ts` | Playlist video list for the dashboard, fetched with Node's own yt-dlp and cached |
| `api.ts` | REST endpoints (`/api/status`, `/api/state`, `/api/events`, `/api/config`, `/api/stream-key`, `/api/player/*`, `/api/stream/stop`, `/api/stream/start`, `/api/playlist/*`, `/api/logs`, `/api/update/*`, `/api/autostart`, `/api/discord/*`) |
| `updater.ts` / `ytdlp-updater.ts` | GitHub Releases updater (app-only or full update, download, stage, stop for the swap); yt-dlp self-update |
| `full-update.ts` | Node's side of a full update: `stopForSwap`, `runHealthCheck`, and `decideStartup` / `runStartup` for what a start does about an update in progress |
| `update-state.ts` | `data\update-state.json` (a full update's step and result) and `START.bat`'s `data\update-swap.log` |
| `spawn-cwd.ts` | The temp folder every child process starts in |
| `logger.ts` | Pino structured logging |

**Configuration hot-reload:** `POST /api/config` saves `config.json`, rebuilds Discord and the Twitch checks, and asks the supervisor to `POST /reload`. The reply says when a change (ingest, key, encoder, freeze or silence duration) needs a supervisor restart.

## Testing

Tests live in `src/server/__tests__/` using Vitest. Tests use a `.test-tmp` directory for file fixtures. All server modules use dependency injection for mock-friendly testing.

## Platform Notes

Windows-specific: The `/api/autostart` endpoint manages a VBS script in the Windows Startup folder. The release build (`build/prepare-release.js`) lays out the install that `supervisor/src/paths.rs` expects; `build/release-layout.js` lists the required paths and `release-layout.test.ts` reads `paths.rs` so the two cannot drift. It bundles portable Node.js v22.23.4 win-x64, yt-dlp nightly and deno (latest), and pinned inputs whose SHA-256 must match: the Node zip, ffmpeg 8.0 essentials (gyan.dev, only `ffmpeg.exe`), and bgutil-ytdlp-pot-provider 2.0.0 (server source and yt-dlp plugin). The full build runs npm under the bundled Node; `--slim` uses the build machine's `npm`, since nothing in `app\` is compiled against a Node ABI.

ES modules throughout (`"type": "module"` in package.json, `"module": "ES2022"` in tsconfig).

## Auto-Updater

The updater checks GitHub Releases for `Sponsorn/streamloop` and supports one-click updates from the admin dashboard.

**API endpoints:**
- `GET /api/update/status` — current update status (version info, availability, kind and parts, and `downloadPercent` while a ZIP downloads), plus `progress` (`data\update-state.json`, or null) and `swapLog` (the lines of `data\update-swap.log`)
- `POST /api/update/check` — manually trigger a version check (60s cooldown)
- `POST /api/update/apply` — download, extract, swap, and restart

**Release selection:** `pickRelease()` takes only releases with the running major version. 2.x takes stable releases; 3.x also takes prereleases, since 3.x ships as `--prerelease` while on trial. A release without both the full and the `-update.zip` is skipped, so one still uploading never wins. `compareVersions()` sorts a suffixed version such as `3.0.0-dev` below its release.

**Update kind and asset selection:** Node hashes its own install parts once at start (`hashParts`). A check compares them with the release's `components.json`: when every part matches, the update is app-only; when one differs, it is a full update of the parts that differ (`updateKind()`, status `kind` and `parts`). A release with no `components.json`, or a malformed one, stays app-only. When nothing newer exists but the install's parts differ from the running version's own release, that release is offered as a same-version full update (`sameVersion`), which is how an install that took this feature as an app-only update gets its new binaries. `selectReleaseAssets(assets, kind)` picks the slim `-update.zip` for an app-only update and the full ZIP for a full one. The matching `.sha256` is resolved by name (accepts `<file>.zip.sha256` or `<file>.sha256`) so a release carrying both bundles can't cross checksums. `stageUpdate()` stages `app/` and `START.bat`, and for a full update the folders of the changed parts (`foldersFor(parts)`, which always adds `yt-dlp\`, since the PO-token plugin lives in it). An app-only update never stages a part folder, because the supervisor holds `yt-dlp\` and its token server holds `node\`. When a release has no `components.json` and its `node-version.txt` names another Node than the running one, the status carries `nodeNotice`, which points at the full ZIP.

**Full update, Node's side** (`updater.ts`, `full-update.ts`). Each step is written to `data\update-state.json` as it starts (`from`, `to`, `parts`, `folders`, `step`, `result`, `reason`, `baseline`, and `downloadPercent`, written at most once a second while the ZIP downloads); a state write that throws fails its step.
1. `download`: a full update does not start while a second `START.bat` window of this install runs (`countStartBatWindows`: `cmd.exe` processes whose command line names `ROOT\START.bat`, ignoring case), since that window's heal would run beside the swap and put old parts back under it. `POST /api/update/apply` then answers 409 "Close the other StreamLoop window first.", and nothing is downloaded or written. An app-only update does not count windows. Then copies an earlier update left behind (`_update_old`, `_update_old_<folder>`, `_update_bad_<folder>`) are removed, but one whose live part is not whole is kept and the update is called off. A `data\update-checking` an earlier update could not remove goes too; one that still cannot be removed calls the update off. A 2 s test encode with `supervisor.encoder` on the installed ffmpeg is the baseline. Then the full ZIP is downloaded, verified, extracted and staged, all while the stream stays on air. The download counts its bytes against the content-length, and the dashboard shows "Downloading, 42%, 1 min 10 s" (an app-only download shows its percentage in the banner).
2. `stop`: the windows are counted again first, so one opened during the download counts too; a second window fails the update at `stop` with "another StreamLoop window is open" before anything is stopped. Then Node's own timers stop (update check, Twitch checks, launcher, event tail), Discord gets "Updating to {version}, the stream is off air until it is done.", and `stopForSwap` runs. Try 1 sends `POST /shutdown` (no `stopped-by-user` marker) and waits until `/status` stops answering and nothing runs from `supervisor\`, try 2 sends it again, try 3 ends those processes. Then nothing may run from the folders being replaced: try 1 waits, tries 2 and 3 end what runs there. A try looks up to 11 times, a second apart, and each look lists the running programs through PowerShell (about 0.4 s on the dev machine), so a try that does not succeed takes about 15 s, longer on a slower box. Node's own process and the tsx process that started it are never ended. When the stop fails, the update is `failed` at `stop` with the program still running as the reason, `_update_tmp\` is removed and Node's services start again, so the launcher brings the old supervisor back. Every failure this Node records (at `download`, `stop` or `swap`) is posted to Discord at level `warn` with the dashboard's failure copy, "The update to {version} failed at {step}: {reason}. Still on {old}."
3. `swap`: Node exits 75 and `START.bat` swaps (see below).
4. `check`: an `update` start, or any start that finds `data\update-checking`, holds the launcher and runs `runHealthCheck`, unless the update is at `download` or `stop`: then nothing was swapped, and the marker is one an earlier update left. Each item gets 3 tries, 3 s apart. The items are every install file `checkDependencies` lists (not the overlay font in `C:\Windows\Fonts`: no update changes it, so, like an encoder that already failed, it is not the update's fault), the configured encoder still working on the new ffmpeg when it worked in the baseline (never `pickEncoder`, whose fallback would hide a broken NVENC), and `streamloop-supervisor.exe check --root ROOT`, which gives the token server it starts up to 15 s to answer. A check try that runs over 60 s is ended with `taskkill /T`, so the token server it started does not keep port 4418 for the next try. Pass: the result `ok` and step `start` are written, then `data\update-checking` is removed, then the `_update_old*` folders, in that order so that later crashes cannot roll a passed update back onto the copies still left. With `autoStartStream` on and no Stop pressed before the update, `launcher.release()` starts the stream, and the step becomes `done` once `/status` reports `playing` with `encoder.alive`, with Discord "Back on air on {version}.". Otherwise Discord gets "Updated to {version}. Ready to stream." and the dashboard waits for Start. The banner says "Updated to {version}. Ready to stream." until the stream is live and "Back on air on {version}." at `done`, for up to 10 minutes. Fail: the result `failed` and the reason are written, `data\update-checking` stays (`START.bat`'s rollback removes it, and a Node that dies before its exit counts as a crash during the check) and Node exits **76** with no Discord post, since the old version posts the failure on its rollback start.
5. A `rollback` start marks the update `rolled-back`, keeping the reason or taking the last `failed` line of the swap log. A `first` or `restart` start that finds a `running` state was cut off: at `download` or `stop` the old version never left (`failed`, "StreamLoop was restarted during the update."), at `swap` or `check` with no `data\update-checking` heal put the old version back (`rolled-back`, "The update was cut off, so the old version was put back."). Both remove a leftover `_update_tmp\`. Each of these results is posted to Discord with the failure copy once it is written, so an update that does not go through is posted once; a version from before full updates (3.0.5) as the rollback target posts nothing.

While a full update runs (updater status `downloading`, `extracting` or `ready`, or a state whose result is `running`), `POST /stream/stop`, `/stream/start`, `/player/*`, `/playlist/switch`, `/config`, `/stream-key`, `/update/apply`, `/yt-dlp/update` and `/setup-check/run` answer 409 "An update is running. Try again when it is done.", and an update check during the download, the extraction or the stop changes nothing. `/update/apply` also answers 409 while an update check runs: a check clears the last offer (`updateAvailable` and the asset URLs) when it starts, so an apply then would install from an offer half replaced.

**Restart mechanism:** After applying an update, the server exits with **code 75** (`process.exit(75)` in `index.ts` after a graceful shutdown — stop the update check, the Twitch checks, the launcher and the event tail, close the HTTP server; after an app-only update the supervisor and the stream keep running, while a full update has stopped them first). 75 is a sentinel, not a failure: `START.bat` reads `%ERRORLEVEL%` after the server process ends and branches on it — `75` = "exited on purpose to apply a staged update", `76` = "the new version failed its health check, roll back", `0` = clean shutdown (stay down), anything else = crash (auto-restart after 10s). The number is arbitrary-but-conventional (75 = `EX_TEMPFAIL` in BSD sysexits.h); what matters is node never emits it on a normal or crashing exit, so it can't be confused with either. On code 75, `START.bat` first swaps the staged install parts (see below), then swaps `app/` → `_update_old/` and `_update_tmp/app/` → `app/`, copies `config.json` and `logs/` from the old app (playback state lives in `data\`, outside `app\`), verifies the new app is valid, and re-launches the server. On swap failure, it rolls back by renaming `_update_old` back to `app`; if `app/` cannot be renamed at all, it deletes `_update_tmp/` so a later exit 75 cannot swap in that stale app.

**Part swap and rollback.** A full update stages each changed part folder in `_update_tmp\<folder>`. `START.bat` swaps them in the order `node`, `supervisor`, `ffmpeg`, `pot`, `yt-dlp` (its `PARTS` and `KEY_<folder>` lines, which `launcher.test.ts` holds to `SWAP_FOLDERS` and `PART_KEY_FILES` in `components.js`). Leftovers from an earlier update (`_update_old*`, `_update_bad_*`) are removed first. If one cannot be removed, the swap is called off before anything is renamed: `_update_tmp/` goes and the current version starts with `STREAMLOOP_START=rollback`. Before the first rename it ends every process whose executable sits under a staged folder (PowerShell `Get-CimInstance Win32_Process`), whether or not it holds the folder. Each folder is then renamed to `_update_old_<folder>` in up to 3 tries, ending processes again and waiting about 3 s before tries 2 and 3. Then the staged copy moves in and must hold its key file, and the new `node.exe --version` must run. The new `yt-dlp\` gets the install's own `yt-dlp.exe`, which keeps itself current. Each event goes to `data\update-swap.log` as `<folder> <status> <detail>`, status `retry`, `killed`, `ok`, `failed` or `restored`. When a part cannot be swapped, every part already swapped goes back, `app/` is left alone, `_update_tmp/` is removed and the old version starts with `STREAMLOOP_START=rollback`. A failed `app/` swap after a part swap puts the parts back the same way. After a part swap, the old parts stay in `_update_old_<folder>` and the old app in `_update_old/` until the new version passes its health check. Right after the part swap, before it removes `data\update-swapping`, `START.bat` writes `data\update-checking`, so new parts always carry one of the two markers, and it starts the new Node with `STREAMLOOP_START=update`. Putting parts back removes `data\update-checking` before `data\update-swapping`. Exit 76, or the third crash in a row while `data\update-checking` exists, rolls back. Processes running from the folders are ended, each `<folder>` becomes `_update_bad_<folder>` and `_update_old_<folder>` goes back to `<folder>`. When the old copy does not come back whole, the current one goes back in its place rather than leaving none. `app/` goes back the same way, with the newer `config.json` and `logs/` carried into the restored app. The `_update_bad_*` copies and `data\update-checking` are removed, and the old version starts with `STREAMLOOP_START=rollback`. A rollback writes `data\update-swapping` too and removes it when every folder is back; when a folder stays in use, the marker stays and heal tries again on the next start. The rollback needs nothing from the old app, so a version from before full updates (3.0.5) starts as usual.

**Launcher self-update (trampoline).** `START.bat` must never overwrite itself in place: cmd.exe reads a running batch file from disk by byte offset, so replacing it mid-run desyncs the read position and usually closes the window before the relaunch (the classic "update applied, terminal never reappears" hang). Instead, when the update ships a `START.bat` that differs from the running one (`fc /b` compare), the new launcher is staged to `START.bat.new`. At the end of the code-75 block, if `START.bat.new` exists, `START.bat` writes a detached one-shot `_apply_launcher.bat`, `start`s it in a **separate** process, and `exit /b 0`s. That trampoline waits ~2s for the original window to close, `move`s `START.bat.new` over `START.bat` while nothing holds it, relaunches in a fresh "StreamLoop 3" window (so preflight re-runs), then self-deletes. It finds the root from its own path (`%~dp0`), never from a path echoed into it, because echo writes in the console codepage and a non-ASCII install path would miss. The trampoline hands the start that was due (`update` after a part swap, else `restart`) to the new `START.bat` as its first argument, which it takes as `STREAMLOOP_START` instead of `first`. Updates that don't change the launcher take the plain `goto loop` in-place restart. Note the release build ships `START.bat` in **every** ZIP (full and slim), so the `fc /b` content check — not "did the ZIP include a launcher" — is what gates the trampoline.

**Half-swap self-heal.** Each swap is non-atomic (`rename app _update_old`, then `move _update_tmp\app app`, and the same for each part folder). If the process dies between those steps the folder is missing, which would brick every future launch at preflight. A `:heal` routine in `START.bat` runs before preflight and again before every start under `:loop`: if `app/src/server/index.ts` is missing, it restores `app/` from `_update_old/`, or promotes `_update_tmp\app` if that's all that survived. A part folder whose key file is missing is restored from `_update_old_<folder>`, promoted from `_update_tmp\<folder>`, or taken back from `_update_bad_<folder>`; `app/` falls back to `_update_bad_app` last as well. A part swap or a rollback cut off part-way (window closed, power cut) would leave old and new parts side by side, so `START.bat` keeps `data\update-swapping` from the first part rename until the app swap is done, and through a rollback. Heal finding it first puts back every part and the app, then removes `data\update-checking`.

**Safety guards:**
- Concurrent `downloadAndApply()` calls are rejected (prevents double-click corruption)
- A full update takes the stream off air only once its ZIP is verified and staged, and anything that fails before the swap leaves the old version running
- A full update does not start, and fails at `stop` with nothing stopped, while a second `START.bat` window of the install runs
- New parts are checked before they stream: a failed health check, a state write that throws during the check, and a start that finds its own failed check still marked all exit 76. The one gap is a rollback that cannot put a part back because a program from outside the install holds it: that part stays new beside the old version until heal puts it back on a later start, and the swap log says so (`<folder> failed the new version stayed in use and could not be put back`)
- `_update_tmp/` is cleaned up on download/extraction failure
- `START.bat` verifies swap success before cleanup; rolls back on failure, and a part folder still held after 3 tries puts back every part already swapped
- `START.bat` never replaces itself in place — launcher updates hand off to a detached trampoline (see above)
- `START.bat` self-heals a half-swapped `app/` or part folder, and puts back a part swap or finishes a rollback that was cut off, before preflight and before every restart
- A leftover from an earlier update that cannot be removed calls the swap off before any rename, so a rollback can never put a stale copy over a live part
- `START.bat` passes paths to PowerShell only through environment variables, never in the `-Command` text, where a quote or a non-ASCII letter in the install path would break it (`launcher.test.ts`)
- `start-bat.test.ts` runs the real `START.bat` (Windows only, about 2 min) in temp roots with a space and an `Ö` in their path: a full part swap, processes running from a part folder ended before the first rename, a folder held through all 3 tries, `app\` held after a part swap, a leftover that cannot be removed, the trampoline, exit 76, an incomplete old copy, three crashes during the check, heal after a cut-off swap or rollback, also one cut off between the app's rename and its move, and a rollback to an app from before full updates
- Inside `START.bat`'s parenthesized blocks, comments are `rem` without parentheses, never `::`. cmd parses the whole exit-code block on every server exit, and a `::` line in it aborts the launcher (window closes, no crash restart). `launcher.test.ts` enforces this.
- Every process the server starts gets `cwd: SPAWN_CWD` (`spawn-cwd.ts`, the temp folder). A child that inherits `cwd=app\` and outlives the server (browser, yt-dlp) blocks the `rename app _update_old` step. `spawn-cwd.test.ts` fails when a spawn site lacks a `cwd`.
- Dashboard `waitForRestart()` runs one wait at a time and polls every 2 s with no time limit, since the server is down for as long as `START.bat`'s part swap or rollback takes; meanwhile it shows the last step it saw
- Checksum verification uses streaming hash (no full ZIP in memory); a ZIP with no matching `.sha256` is refused before anything is downloaded
- GitHub API and `node-version.txt` fetches time out after 10s

**Dev mode:** When the `node/` sibling directory doesn't exist (i.e., running via `npm start`), the updater operates in check-only mode — it can detect available updates but won't download or apply them.

**Config options:** `autoUpdateCheck` (boolean, default true) and `updateCheckIntervalMs` (number, default 21600000 = 6 hours).

**Start the stream when StreamLoop starts:** `autoStartStream` (boolean, default true). When it is off, the first Node start after `START.bat` opens, and the first after a rollback, come up without launching the supervisor and the dashboard shows "Ready to stream. Press Start." (`START.bat` sets `STREAMLOOP_START`: `first`, `restart` for a crash restart or an app-only update, `update` for the first start after a part swap and for a crash restart during its check, `rollback` for the first start after a rollback; `data\waiting-for-start` carries a wait across a restart). A supervisor that is already running is attached, never stopped, and a crashed one is still relaunched; a dashboard Stop wins over everything.
