# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

@ARCHITECTURE.md

## Keeping ARCHITECTURE.md current

- A change to process boundaries, the Node↔supervisor interface, the ffmpeg pipeline shape, the update/launch mechanism or an external integration updates `ARCHITECTURE.md` in the same commit.
- When a 3.0 part moves from `spikes/` to `src/`, update its proven/design marking there.
- Decisions and their reasoning go on the Notion pages, not in the repo; `ARCHITECTURE.md` names those pages, never links to them.

## Project Overview

StreamLoop restreams a YouTube playlist to Twitch around the clock from one Windows box. This branch holds 3.0: a Node.js/TypeScript backend (`src/server/`) drives a Rust supervisor (`supervisor/`), which plays the playlist through ffmpeg and pushes RTMP itself. There is no mpv or OBS. Node owns the config, the dashboard, Discord alerts, the Twitch checks and the updater. 2.x (mpv + OBS) lives on the `2.x` branch.

R1 is porting the Node backend to Rust: `streamloop.exe [--root DIR]` runs the control process (`crates/control/`) behind the same REST API, and `START.bat` now starts that exe. The Node sections below still describe the Node backend until R1 removes it, except where they name `START.bat`: that file is the one described under "START.bat (R1)".

## Setup

```bash
npm install
cp config.example.json config.json   # Then edit config.json with your settings
```

`config.json` is gitignored to prevent committing credentials. `config.example.json` is the tracked template.

**Dev setup with the supervisor.**

1. `cargo build --release` at the repo root (the Cargo workspace; the exe is `target\release\streamloop.exe`).
2. `node supervisor/scripts/dev-root.mjs` builds a dev root in the install layout (default `%TEMP%\streamloop-devroot`), with `target\release\streamloop.exe` hard-linked as `ROOT\streamloop.exe`. After a rebuild, the next `dev-root.mjs` run (every session starts with one) links the new exe.
3. Run `ROOT\streamloop.exe`. With `app\` beside it, its folder is the root, as in an install: the updater may stage there, so turn `autoUpdateCheck` off in a dev config. `target\release\streamloop.exe --root ROOT` runs the same root from anywhere; a root given by `--root` or `STREAMLOOP_ROOT` is dev mode, where the updater only checks. Either way it reads `ROOT\app\config.json`, writes its log and timeline to `ROOT\app\logs\`, and its launcher starts `ROOT\streamloop.exe launch`.

At every start the control process removes the 3.0 leftovers, `ROOT\supervisor\streamloop-supervisor.exe` among them. The Node backend, until R1 removes it, still runs that file: to run Node against a dev root, copy the exe there and start Node with cwd `ROOT\app` and `STREAMLOOP_ROOT=ROOT` through the repo's tsx. `supervisor/scripts/node-session.mjs` runs the end-to-end sessions against `ROOT\streamloop.exe`, started from a launcher console the way `START.bat` starts it.

Test-only environment variables, never set by `config.json` or a release:
- `STREAMLOOP_DEBUG_FAULTS=1`: the launcher starts the supervisor with `--debug-faults` (opens `POST /debug/fault`). The flag belongs to the supervisor process, not to Node: a Node restarted without the variable attaches to the running supervisor, and `/debug/fault` stays open until that supervisor is restarted.
- `STREAMLOOP_TEST_HOOKS=1`: adds `POST /api/test/exit-75`, the update restart without an update.
- `STREAMLOOP_TEST_HLS_URL`: the Twitch read-back reads this HLS instead of the channel's, and the channel counts as live, so no request reaches Twitch and Helix credentials are not needed.
- `STREAMLOOP_TEST_RELEASES=<file>`: the updater reads its release list from this JSON file (GitHub's shape) instead of GitHub, and takes asset URLs only from `http://127.0.0.1:<port>/`.
- `STREAMLOOP_TEST_BREAK_AT_S=<seconds>` (control process only): a staged update lands at this encoder uptime instead of 47 h 45 min.

Test-only builds, never released: the `streamloop` crate's features `test-supervisor-fails` and `test-control-fails` make supervisor mode or control mode exit 1 at start, for the rollback runs of `install-session.mjs`. A test in `release_layout.rs` fails if the release build turns either on. The updater stages only an exe that prints the release's `X.Y.Z`, so test builds take a plain version (`STREAMLOOP_BUILD_VERSION=3.1.1`), not a `-dev` one.

## Commands

```bash
npm start            # Run server (tsx)
npm run dev          # Run with file watching (tsx watch)
npm test             # Run tests once (vitest run)
npm run test:watch   # Run tests in watch mode (vitest)
npm run build:release       # dist/: streamloop-vX.Y.Z.zip (the install), streamloop-vX.Y.Z.exe, a .sha256 for each, node-version.txt
node build/verify-release.mjs <zip>   # check a ZIP against build/release-layout.js (both builds run it)
cargo build --release       # at the repo root: target\release\streamloop.exe (the control process, and the supervisor, launch, protect and check modes)
cargo test --workspace      # all Rust tests: supervisor, common, control, the exe's CLI
node supervisor/scripts/api-parity.mjs record --server rust --out FILE   # the dashboard's requests against target\release\streamloop.exe and a fake supervisor (ports 4417, 7654, 8791 free)
node supervisor/scripts/api-parity.mjs compare supervisor/scripts/api-parity/node.json FILE   # exit 0 when the answers match Node's, apart from the differences it names
node supervisor/scripts/node-session.mjs A|B|C|treekill [--port N]   # end-to-end runs of streamloop.exe (control process and supervisor) against local MediaMTX; each ends with its verdict; --port when 7654 is taken
node supervisor/scripts/install-session.mjs all|<run>[,<run>...]|build   # dist/'s full ZIP in a temp folder against local MediaMTX and a local release list; runs: install, update (planned break, Update Now, a landing while stopped), rollback (by the control process), rollback-bat (by START.bat), move (from v3.0.5), faults (the gate's fault checks); build only makes the test exes
node supervisor/scripts/update-session.mjs all|<run>[,<run>...]|build   # full update, rollback and bootstrap proof (runs: full-on, full-off, app-only, locked, broken, bootstrap, two-windows) on N and N+1 (the dist version's patch + 1 and + 2) repacked from the dist/ ZIPs; build only repacks them
cargo build --release       # in ui/: the app, ui/target/release/streamloop-ui.exe
node ui/scripts/ui-session.mjs dev|full|full-fail|update   # a real install, local MediaMTX and a Discord catcher, for driving the app through every dashboard action
```

No linter is configured. TypeScript strict mode is enabled via `tsconfig.json`.

## Release Checklist (3.x)

Before pushing and building a 3.x release (2.x releases come from the `2.x` branch and follow its own CLAUDE.md):

1. Bump the version in `package.json`. The version is read at startup and used by the updater to compare against GitHub Releases.
2. Update `RELEASE_NOTES.md` with the new version's changelog. This file is used when creating GitHub Releases.
3. **Build** after pushing: `npm run build:release`. It builds `streamloop.exe` with `cargo build --release -p streamloop` and writes `dist/streamloop-vX.Y.Z.zip`, `dist/streamloop-vX.Y.Z.exe`, `dist/streamloop-vX.Y.Z.sha256`, `dist/streamloop-vX.Y.Z.exe.sha256` (each `<hash>  <filename>`) and `dist/node-version.txt`, then runs `verify-release.mjs`, which fails on a missing layout path or an exe that does not print the ZIP's version. It needs `cargo` and `spikes/buffer/media/slate.mp4`.
4. The ZIP is the install: `streamloop.exe`, `node\`, `app\config.example.json`, `supervisor\slate.mp4`, `ffmpeg\ffmpeg.exe`, `yt-dlp\` (yt-dlp, deno, `plugins\bgutil-ytdlp-pot-provider.zip`), `pot\` (the token server, built under the bundled Node so `canvas` matches its ABI), `data\` and `START.bat`. It holds no `app\config.json`: the control process creates it from `app\config.example.json` on first start, so a reinstall keeps the config and the protected stream key. The loose `.exe` is what the updater downloads. There is no update ZIP and no `components.json`; a new Node, ffmpeg or token server means the full ZIP by hand.
5. Create the GitHub release with `gh release create ... --prerelease --latest=false` and upload the ZIP, the exe, both `.sha256` files and `node-version.txt`. The updater takes the exe's checksum from `streamloop-vX.Y.Z.exe.sha256`; `node-version.txt` tells an install its Node is out of date.
6. **3.x releases are never marked latest**, and 3.x is `--prerelease` while it is on trial: 2.x installs older than 2.3.7 still read GitHub's "latest release" and would otherwise swap 3.0 into a 2.x folder; 2.3.7 and later only take releases with their own major version.
7. **Existing installs.** The updater delivers `streamloop.exe` only. yt-dlp keeps itself current through `yt-dlp -U` on start (`autoUpdateYtdlp`). A release that changes Node, ffmpeg or the token server says in `RELEASE_NOTES.md` that installs need the full ZIP: Stop, close the window, extract the ZIP into the folder it was first extracted into (`C:\StreamLoop3`, so its top folder `streamloop\` lands on `C:\StreamLoop3\streamloop`, never into the install itself, which would make `streamloop\streamloop\`), start, press Start. Config, key and `data\` survive because the ZIP ships no `config.json`.
8. Prove a release candidate with `node build/verify-release.mjs dist/streamloop-vX.Y.Z.zip` and `node supervisor/scripts/install-session.mjs all`, started from PowerShell. It installs the ZIP into `%TEMP%\sl3 install Ö`, takes it live, lands test builds 3.1.1 to 3.1.3 at a planned break moved to 300 s, by Update Now and while stopped, proves both rollbacks with broken 3.1.1 builds, moves a v3.0.5 install onto it (with Stop, and with its supervisor still streaming), and runs the gate's fault checks. Each run prints PASS or FAIL and writes `supervisor\results\install\<run>\results.json`.

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

**Control process** (`crates/control/`, R1): one Rust module per Node module (`app` for `index.ts`, `api/` for `api.ts`, `launcher`, `events`, `timeline`, `discord`, `twitch`, `readback`, `playlists`, `setup_check`, `supervisor_client`, `updater` and `landing`, `ytdlp_update`, `logging`), with the same behaviour, timings and texts, plus `leftovers`, which removes what 3.0 left in an install. `app.rs` keeps `index.ts`'s start order and exit codes: 0 for Ctrl+C, a closed console window or `/api/shutdown`, 1 with no install root or a config that does not load, 2 when the port is taken, 64 for an unknown argument, 75 after a landed update, a rollback or the test hook. Its tests live in `crates/control/tests/`, one file per module; `tests/app.rs` drives the wired process on a real socket with fakes behind it. `ARCHITECTURE.md` (Control mode (R1)) has the start order, the 3.0 leftovers it removes and the exe update.

## Testing

Tests live in `src/server/__tests__/` using Vitest. Tests use a `.test-tmp` directory for file fixtures. All server modules use dependency injection for mock-friendly testing.

## Platform Notes

Windows-specific: The `/api/autostart` endpoint manages a VBS script in the Windows Startup folder. The release build (`build/prepare-release.js`) lays out the install that `supervisor/src/paths.rs` expects; `build/release-layout.js` lists the required paths and `crates/streamloop/tests/release_layout.rs` checks them against `Layout` in `crates/common/src/paths.rs` so the two cannot drift. It bundles portable Node.js v22.23.4 win-x64, yt-dlp nightly and deno (latest), and pinned inputs whose SHA-256 must match: the Node zip, ffmpeg 8.0 essentials (gyan.dev, only `ffmpeg.exe`), and bgutil-ytdlp-pot-provider 2.0.0 (server source and yt-dlp plugin). The full build runs npm under the bundled Node.

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
1. `download`: a full update does not start while a second `START.bat` window of this install runs (`countStartBatWindows`: `cmd.exe` processes whose command line names `ROOT\START.bat`, ignoring case, and that run a program besides their console host), since that window's heal would run beside the swap and put old parts back under it. An idle prompt does not count: the trampoline's `start` opens `cmd /K`, so a launcher that hands off or stops cleanly leaves a prompt that still names `START.bat`. A window found idle is looked at again a second later, since a running launcher has no child for a moment between two commands. The listing reads command lines for `cmd.exe` only, so the encoder's, which carries the stream key, never reaches Node. `POST /api/update/apply` then answers 409 "Close the other StreamLoop window first.", and nothing is downloaded or written. An app-only update does not count windows. Then copies an earlier update left behind (`_update_old`, `_update_old_<folder>`, `_update_bad_<folder>`) are removed, but one whose live part is not whole is kept and the update is called off. A `data\update-checking` an earlier update could not remove goes too; one that still cannot be removed calls the update off. A 2 s test encode with `supervisor.encoder` on the installed ffmpeg is the baseline. Then the full ZIP is downloaded, verified, extracted and staged, all while the stream stays on air. The download counts its bytes against the content-length, and the dashboard shows "Downloading, 42%, 1 min 10 s" (an app-only download shows its percentage in the banner).
2. `stop`: the windows are counted again first, so one opened during the download counts too; a second window fails the update at `stop` with "another StreamLoop window is open" before anything is stopped. Then Node's own timers stop (update check, Twitch checks, launcher, event tail), Discord gets "Updating to {version}, the stream is off air until it is done.", and `stopForSwap` runs. Try 1 sends `POST /shutdown` (no `stopped-by-user` marker) and waits until `/status` stops answering and nothing runs from `supervisor\`, try 2 sends it again, try 3 ends those processes. Then nothing may run from the folders being replaced: try 1 waits, tries 2 and 3 end what runs there. A try looks up to 11 times, a second apart, and each look lists the running programs through PowerShell (about 0.4 s a listing on the dev machine), so a try that does not succeed takes well over 10 s. Node's own process and the tsx process that started it are never ended. When the stop fails, the update is `failed` at `stop` with the program still running as the reason, `_update_tmp\` is removed and Node's services start again, so the launcher brings the old supervisor back. Every failure this Node records (at `download`, `stop` or `swap`) is posted to Discord at level `warn` with the dashboard's failure copy, "The update to {version} failed at {step}: {reason}. Still on {old}."
3. `swap`: Node exits 75 (master's updater; the R1 exe updates itself, see Exe update below).
4. `check`: an `update` start, or any start that finds `data\update-checking`, holds the launcher and runs `runHealthCheck`, unless the update is at `download` or `stop`: then nothing was swapped, and the marker is one an earlier update left. Each item gets 3 tries, 3 s apart. The items are every install file `checkDependencies` lists (not the overlay font in `C:\Windows\Fonts`: no update changes it, so, like an encoder that already failed, it is not the update's fault), the configured encoder still working on the new ffmpeg when it worked in the baseline (never `pickEncoder`, whose fallback would hide a broken NVENC), and `streamloop-supervisor.exe check --root ROOT`, which gives the token server it starts up to 15 s to answer. A check try that runs over 60 s is ended with `taskkill /T`, so the token server it started does not keep port 4418 for the next try. Pass: the result `ok` and step `start` are written, then `data\update-checking` is removed, then the `_update_old*` folders, in that order so that later crashes cannot roll a passed update back onto the copies still left. With `autoStartStream` on and no Stop pressed before the update, `launcher.release()` starts the stream, and the step becomes `done` once `/status` reports `playing` with `encoder.alive`, with Discord "Back on air on {version}.". Otherwise Discord gets "Updated to {version}. Ready to stream." and the dashboard waits for Start. The banner says "Updated to {version}. Ready to stream." until the stream is live and "Back on air on {version}." at `done`, for up to 10 minutes. Fail: the result `failed` and the reason are written, `data\update-checking` stays (`START.bat`'s rollback removes it, and a Node that dies before its exit counts as a crash during the check) and Node exits **76** with no Discord post, since the old version posts the failure on its rollback start.
5. A `rollback` start marks the update `rolled-back`, keeping the reason or taking the last `failed` line of the swap log. A `first` or `restart` start that finds a `running` state was cut off: at `download` or `stop` the old version never left (`failed`, "StreamLoop was restarted during the update."), at `swap` or `check` with no `data\update-checking` heal put the old version back (`rolled-back`, "The update was cut off, so the old version was put back."). Both remove a leftover `_update_tmp\`. Each of these results is posted to Discord with the failure copy once it is written, so an update that does not go through is posted once; a version from before full updates (3.0.5) as the rollback target posts nothing.

While a full update runs (updater status `downloading`, `extracting` or `ready`, or a state whose result is `running`), `POST /stream/stop`, `/stream/start`, `/player/*`, `/playlist/switch`, `/config`, `/stream-key`, `/update/apply`, `/yt-dlp/update` and `/setup-check/run` answer 409 "An update is running. Try again when it is done.", and an update check during the download, the extraction or the stop changes nothing. `/update/apply` also answers 409 while an update check runs: a check clears the last offer (`updateAvailable` and the asset URLs) when it starts, so an apply then would install from an offer half replaced.

**START.bat (R1).** `scripts/START.bat` is a short launcher around `streamloop.exe`. The exe does the updating (see Exe update below), so the launcher swaps nothing and replaces nothing, itself included. It checks that `streamloop.exe`, `ffmpeg\ffmpeg.exe`, `node\node.exe` and `pot\build\main.js` exist and says "Reinstall StreamLoop from the full ZIP." when one does not. An update cut off between its two renames leaves only `streamloop.exe.old`, which it renames back before that check. It then runs `streamloop.exe` in a loop with `STREAMLOOP_START` set to `first` for the first run and `restart` for every later one. Exit codes: 0 stays down; 2 means another StreamLoop holds the port, so it says so and stops, with no restart and no rollback; 75 is an exit on purpose (a landed update, a rollback, the test hook) and restarts after the same 10 s wait without the rollback check; anything else is a crash and restarts after 10 s. After a crash, when `_update\landed` and `streamloop.exe.old` both exist, the new exe stopped before it confirmed itself, so `:rollback` puts the previous exe back (`streamloop.exe` becomes `streamloop.exe.bad`, `streamloop.exe.old` becomes `streamloop.exe`, `_update\landed` moves to `data\update-rolled-back`) and the next run starts with `STREAMLOOP_START=rollback`. When `streamloop.exe` is already gone, the update was cut off between its two renames and the new exe never ran: only the old exe is renamed back, `_update\landed` stays and nothing is written to `data\update-rolled-back`. Batch rules, all held by `crates/streamloop/tests/start_bat.rs`: CRLF line endings only (`.gitattributes` marks the file `-text`, so the committed blob is CRLF too), `ping -n 11 127.0.0.1 >nul` for the wait and never `timeout` (it fails without a console), and inside a parenthesised block only `rem` comments without parentheses, never `::`, since cmd parses the whole block at once and a `::` line aborts the launcher.
**Safety guards:**
- Concurrent `downloadAndApply()` calls are rejected (prevents double-click corruption)
- A full update takes the stream off air only once its ZIP is verified and staged, and anything that fails before the swap leaves the old version running
- A full update does not start, and fails at `stop` with nothing stopped, while a second `START.bat` window of the install runs
- New parts are checked before they stream: a failed health check, a state write that throws during the check, and a start that finds its own failed check still marked all exit 76. The one gap is a rollback that cannot put a part back because a program from outside the install holds it: that part stays new beside the old version until heal puts it back on a later start, and the swap log says so (`<folder> failed the new version stayed in use and could not be put back`)
- `_update_tmp/` is cleaned up on download/extraction failure
- A leftover from an earlier update that cannot be removed calls the swap off before any rename, so a rollback can never put a stale copy over a live part
- Every process the server starts gets `cwd: SPAWN_CWD` (`spawn-cwd.ts`, the temp folder). A child that inherits `cwd=app\` and outlives the server (browser, yt-dlp) blocks the `rename app _update_old` step. `spawn-cwd.test.ts` fails when a spawn site lacks a `cwd`.
- Dashboard `waitForRestart()` runs one wait at a time and polls every 2 s with no time limit, since the server is down for as long as `START.bat`'s part swap or rollback takes; meanwhile it shows the last step it saw
- Checksum verification uses streaming hash (no full ZIP in memory); a ZIP with no matching `.sha256` is refused before anything is downloaded
- GitHub API and `node-version.txt` fetches time out after 10s

**Dev mode:** When the `node/` sibling directory doesn't exist (i.e., running via `npm start`), the updater operates in check-only mode — it can detect available updates but won't download or apply them.

**Config options:** `autoUpdateCheck` (boolean, default true) and `updateCheckIntervalMs` (number, default 21600000 = 6 hours).

**Exe update (the R1 control process).** Everything above is Node's updater, which ships. The control process updates only `streamloop.exe`: a check stages `_update\streamloop.exe` (SHA-256 and `--version` checked). With `autoUpdateCheck` on, the update lands by itself at the planned break before Twitch's 48 h cut, once the stream has been off air for 5 min, or at once when it was stopped; with it off, only Update Now lands it. A landing stops the supervisor, writes `_update\landed`, renames `streamloop.exe` to `streamloop.exe.old`, moves the staged exe in, starts the supervisor from the new exe unless the stream was held (a first run counts as held), and exits 75; the next start confirms it within 180 s or puts the old exe back and exits 75 again. There are no part swaps, no exit 76, no `components.json` and no update ZIP. `ARCHITECTURE.md` (Control mode (R1)) has the details.

**Start the stream when StreamLoop starts:** `autoStartStream` (boolean, default true). When it is off, the first Node start after `START.bat` opens, and the first after a rollback, come up without launching the supervisor and the dashboard shows "Ready to stream. Press Start." (`START.bat` sets `STREAMLOOP_START` to `first`, `restart` or `rollback`; the control process derives `update` itself from `_update\landed`; `data\waiting-for-start` carries a wait across a restart). A supervisor that is already running is attached, never stopped, and a crashed one is still relaunched; a dashboard Stop wins over everything.
