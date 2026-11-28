# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

@ARCHITECTURE.md

## Keeping ARCHITECTURE.md current

- A change to process boundaries, the control process↔supervisor interface, the ffmpeg pipeline shape, the update/launch mechanism, the release layout or an external integration updates `ARCHITECTURE.md` in the same commit.
- When a part moves from `spikes/` into the workspace, update its proven/design marking there.
- Decisions and their reasoning go on the Notion pages, not in the repo; `ARCHITECTURE.md` names those pages, never links to them.

## Project Overview

StreamLoop restreams a YouTube playlist to Twitch around the clock from one Windows box. This branch holds 3.1: one Rust `streamloop.exe`, built from the Cargo workspace at the repo root, runs as three processes: the supervisor, the control process and the app. `streamloop.exe supervisor` (`supervisor/`) plays the playlist through ffmpeg and pushes RTMP itself; there is no mpv or OBS. `streamloop.exe control`, or plain `streamloop.exe` under `START.bat` (which sets `STREAMLOOP_START`), is the control process (`crates/control/`): the config, the REST API the app talks to, Discord alerts, the Twitch checks, the launcher and the updater. Plain `streamloop.exe` anywhere else opens the app, `streamloop.exe ui` (`crates/ui/`): the window and the tray icon, one per install, which starts `START.bat` hidden when nothing answers. The exe opens no console window, and there is no web dashboard. The PO-token server is `streamloop-pot.exe` (built in `tokenserver/`), run by the supervisor; the full ZIP ships no Node (an install updated only through the updater keeps `node\` and `pot\`, bgutil's TypeScript server, as its fallback until it gets the full ZIP). 3.0 (a Node backend in `src/server/` driving the supervisor) lives on the `3.0` branch, 2.x (mpv + OBS) on the `2.x` branch.

## Setup

```bash
cargo build --release   # at the repo root: target\release\streamloop.exe
npm install             # vitest, for the harness's tests and the repo guards
```

Rust stable with the MSVC toolchain. `config.json` is gitignored to prevent committing credentials. `config.example.json` is the tracked template, and the release ships it as `app\config.example.json`.

**Dev setup.**

1. `cargo build --release` at the repo root, and `cargo build --release` in `tokenserver/` for `streamloop-pot.exe` (a separate Cargo project; its first build fetches rusty_v8's prebuilt library).
2. `node supervisor/scripts/dev-root.mjs` builds a dev root in the install layout (default `%TEMP%\streamloop-devroot`), with `target\release\streamloop.exe` hard-linked as `ROOT\streamloop.exe`. After a rebuild, the next `dev-root.mjs` run (every session starts with one) links the new exe. It also places `tokenserver\target\release\streamloop-pot.exe` as `supervisor\streamloop-pot.exe` and takes the PO-token plugin from `dist\pot-ts\bgutil-ytdlp-pot-provider.zip` (nothing creates that folder: copy the file from `yt-dlp\plugins\` of a release ZIP, or from `dist\streamloop\yt-dlp\plugins\` after `npm run build:release`; its SHA-256 is `bce874dfa25896c2798e0f4f8147b7b22e785479eb1e459ab232bf2506c95016`, which `pair-session.mjs` checks), and leaves no `node\` or `pot\`, so the Rust token server runs. It leaves `app\` and `data\` to you: copy `config.example.json` to `ROOT\app\config.json` and edit it.
3. Run the control process with `ROOT\streamloop.exe control` (plain `streamloop.exe` outside `START.bat` opens the app). With `app\` beside it, its folder is the root, as in an install: the updater may stage there, so turn `autoUpdateCheck` off in a dev config. `target\release\streamloop.exe control --root ROOT` runs the same root from anywhere; a root given by `--root` or `STREAMLOOP_ROOT` is dev mode, where the updater only checks. Either way it reads `ROOT\app\config.json`, writes its log and timeline to `ROOT\app\logs\`, and its launcher starts `ROOT\streamloop.exe launch`. `streamloop.exe ui --root ROOT` opens the app on the same root. The exe is a GUI program, and a cmd or PowerShell prompt does not wait for one: run the control process with `start /wait "" ROOT\streamloop.exe control --root ROOT` from cmd, or from a batch file, so its lines show in that console and Ctrl+C ends it; check the version with `streamloop.exe --version | more` (cmd) or `streamloop.exe --version | Out-Host` (PowerShell).

`supervisor/scripts/node-session.mjs` runs the end-to-end sessions against `ROOT\streamloop.exe control`, started from a hidden launcher console that hands it NUL handles, as a hidden `START.bat` does.

The control process opens the app window (`ROOT\streamloop.exe detach ui --root ROOT`) at the first start of a `START.bat` started by hand: `STREAMLOOP_START=first`, no update landed, and `STREAMLOOP_FROM_APP` not `1` (`detach start-bat` sets it, and `install-kit.mjs`'s `startBat` and `ui-session.mjs` do, unless a run passes `{ fromApp: false }`). A restart, an update, a rollback and a start with no `START.bat` word (`control --root`, as `node-session.mjs` and `api-parity.mjs` run it) open nothing. `autoOpenAdmin` stays in `config.json`, loaded and saved, and nothing reads it.

Test-only environment variables, never set by `config.json` or a release:
- `STREAMLOOP_DEBUG_FAULTS=1`: the launcher starts the supervisor with `--debug-faults` (opens `POST /debug/fault`). The flag belongs to the supervisor process, not to the control process: a control process restarted without the variable attaches to the running supervisor, and `/debug/fault` stays open until that supervisor is restarted.
- `STREAMLOOP_TEST_HOOKS=1`: adds `POST /api/test/exit-75`, the update restart without an update.
- `STREAMLOOP_TEST_HLS_URL`: the Twitch read-back reads this HLS instead of the channel's, and the channel counts as live, so no request reaches Twitch and Helix credentials are not needed.
- `STREAMLOOP_TEST_RELEASES=<file>`: the updater reads its release list from this JSON file (GitHub's shape) instead of GitHub, and takes asset URLs only from `http://127.0.0.1:<port>/`.
- `STREAMLOOP_TEST_BREAK_AT_S=<seconds>`: a staged update lands at this encoder uptime instead of 47 h 45 min.
- `STREAMLOOP_TEST_AUTOSTART_KEY=<subkey>`: a subkey of HKCU used in place of the Run key, with its `StartupApproved\Run` and `StartupApproved\StartupFolder` beside it, so autostart never touches the user's own entry.
- `STREAMLOOP_TEST_STARTUP_DIR=<dir>`: used in place of the Startup folder. The harness sets both for every control process it starts (`autostartSandbox` in `supervisor/scripts/lib.mjs`) and deletes the key when it ends.

Test-only builds, never released: the `streamloop` crate's features `test-supervisor-fails` and `test-control-fails` make supervisor mode or control mode exit 1 at start, for the rollback runs of `install-session.mjs`. A test in `release_layout.rs` fails if the release build turns either on. The updater stages only an exe that prints the release's `X.Y.Z`, so test builds take a plain version (`STREAMLOOP_BUILD_VERSION=3.1.1`), not a `-dev` one.

## Commands

```bash
cargo build --release       # at the repo root: target\release\streamloop.exe (the control process, and the supervisor, launch, protect, check, ui and detach modes; builds gpui, so a clean build takes several minutes)
cargo test --workspace      # all Rust tests: supervisor, common, control, ui, the exe's CLI and the repo guards
cargo test                  # in tokenserver/: the token server's tests (it is no workspace member and compiles V8)
npm test                    # vitest once: the Notion-link and leak guards (scripts/__tests__), the harness and the spikes
npm run test:watch          # vitest in watch mode
npm run build:release       # dist/: streamloop-vX.Y.Z.zip (the install), streamloop-vX.Y.Z.exe, a .sha256 for each
node build/verify-release.mjs <zip>   # check a ZIP against build/release-layout.js, that its exe prints the ZIP's version and that its token server's /ping names it (the build runs it)
node supervisor/scripts/api-parity.mjs record --server rust --out FILE   # the app's requests against target\release\streamloop.exe and a fake supervisor (ports 4417, 7654, 8791 free)
node supervisor/scripts/api-parity.mjs compare supervisor/scripts/api-parity/node.json FILE   # exit 0 when the answers match Node's, apart from the differences it names
node supervisor/scripts/node-session.mjs A|B|C|treekill [--port N]   # end-to-end runs of streamloop.exe (control process and supervisor) against local MediaMTX; each ends with its verdict; --port when 7654 is taken
node supervisor/scripts/install-session.mjs all|<run>[,<run>...]|build   # dist/'s full ZIP in a temp folder against local MediaMTX and a local release list; runs: install, app (the app as a user starts it: the spec's A1, A2, A11, A9 and A10, A18, A15 and A16, A7 and A8), update (planned break, Update Now, landings while stopped and while waiting for Start), rollback (by the control process), rollback-bat (by START.bat), move (from v3.0.5), from312 (v3.1.2's own updater lands this exe, built as 3.2.0 and 3.2.1, with 2.x's stream guard read against it), faults (the gate's fault checks); the update and rollback runs start the app before the landing and check that it follows the exe; build only makes the test exes
node crates/ui/scripts/ui-session.mjs dev|update|update-fail [--no-app]   # dist/'s full ZIP, local MediaMTX and a Discord catcher, for driving the app (`ROOT\streamloop.exe ui`) through every action; update lists N+1, update-fail the N+1 whose supervisor fails (install-session.mjs's test builds); from PowerShell
```

`api-parity/node.json` is the Node backend's recording, made before 3.1 removed `src/server/`; it is fixed history, and `record --server node` only says so.

No linter is configured.

## Release Checklist (3.x)

Before pushing and building a 3.x release (2.x releases come from the `2.x` branch and 3.0.x releases from the `3.0` branch, each following its own CLAUDE.md):

1. Bump the version in `Cargo.toml` (`workspace.package.version`) and the same number in `package.json`, and in `tokenserver\Cargo.toml` (then run `cargo build` there so its `Cargo.lock` follows; `/ping` names the release by it); `cargo_and_package_json_versions_match` and `token_server_and_workspace_versions_match` fail when they differ. The exe reports it (`--version`, `/status`, the app), and the updater compares it against GitHub Releases.
2. Update `RELEASE_NOTES.md` with the new version's changelog. This file is used when creating GitHub Releases.
3. Before pushing, run `cargo test --workspace` and `npm test`, whose leak guard fails on a key, token or webhook URL in a tracked file. **Build** after pushing: `npm run build:release`. It builds `streamloop.exe` with `cargo build --release -p streamloop` and `streamloop-pot.exe` with `cargo build --release` in `tokenserver\` (which needs that project's toolchain: its first build fetches rusty_v8's prebuilt library, and its `.cargo\config.toml` sets the static CRT), and writes `dist/streamloop-vX.Y.Z.zip`, `dist/streamloop-vX.Y.Z.exe`, `dist/streamloop-vX.Y.Z.sha256`, `dist/streamloop-vX.Y.Z.exe.sha256` (each `<hash>  <filename>`), then runs `verify-release.mjs`, which fails on a missing layout path, an exe that does not print the ZIP's version, or a token server whose `/ping` does not answer `streamloop-pot X.Y.Z` (it starts the server on a free local port, which sends nothing to YouTube). It needs `cargo` and `spikes/buffer/media/slate.mp4`.
4. The ZIP is the install: `streamloop.exe`, `app\config.example.json`, `supervisor\slate.mp4`, `supervisor\streamloop-pot.exe` (the token server), `ffmpeg\ffmpeg.exe`, `yt-dlp\` (yt-dlp, deno, `plugins\bgutil-ytdlp-pot-provider.zip`), `data\`, `README.txt` and `START.bat`. It holds no `app\config.json`: the control process creates it from `app\config.example.json` on first start, so a reinstall keeps the config and the protected stream key. The loose `.exe` is what the updater downloads. There is no update ZIP and no `components.json`; a new ffmpeg or token server means the full ZIP by hand. The ZIP holds no `node\` or `pot\`; the control process removes them from an install that has `supervisor\streamloop-pot.exe`.
5. Create the GitHub release with `gh release create ... --prerelease --latest=false` and upload the ZIP, the exe and both `.sha256` files. The updater takes the exe's checksum from `streamloop-vX.Y.Z.exe.sha256`.
6. **3.x releases are never marked latest**, and 3.x is `--prerelease` while it is on trial: 2.x installs older than 2.3.7 still read GitHub's "latest release" and would otherwise swap 3.x into a 2.x folder; 2.3.7 and later only take releases with their own major version.
7. **Existing installs.** The updater delivers `streamloop.exe` only. yt-dlp keeps itself current through `yt-dlp -U` on start, and at most once an hour while YouTube lookups are paused (`autoUpdateYtdlp`). A release that changes ffmpeg or the token server says in `RELEASE_NOTES.md` that installs need the full ZIP: Stop, Quit StreamLoop from the tray, extract the ZIP into the folder it was first extracted into (`C:\StreamLoop3`, so its top folder `streamloop\` lands on `C:\StreamLoop3\streamloop`, never into the install itself, which would make `streamloop\streamloop\`), start `streamloop.exe`, press Start. Config, key and `data\` survive because the ZIP ships no `config.json`. 3.0.x installs move to 3.1 the same way, since their updater skips a release with no update ZIP.
8. Prove a release candidate with `node build/verify-release.mjs dist/streamloop-vX.Y.Z.zip` and `node supervisor/scripts/install-session.mjs all`, started from PowerShell. It installs the ZIP into `%TEMP%\sl3 install Ö`, takes it live, lands test builds N+1 to N+4 (the release's patch number plus 1 to 4) at a planned break moved to 300 s, by Update Now, while stopped and while waiting for Start, proves both rollbacks with broken N+1 builds, moves a v3.0.5 install onto it (with Stop, and with its supervisor still streaming), has v3.1.2's own updater land it (as the next minor version) at the break, by Update Now and while stopped, and runs the gate's fault checks. The `app` run starts `streamloop.exe` as Explorer does and goes through the app's actions, and the update and rollback runs check that the app follows the exe. Each run prints PASS, FAIL or SKIPPED and writes `supervisor\results\install\<run>\results.json`. SKIPPED means a step that opens or activates a window waited 30 min behind a full-screen app and was left out; it counts as no pass.

## Switching back to 2.x

3.x installs beside 2.x in its own folder. Both use port 7654, so only one of them runs at a time; 3.x also refuses to launch its supervisor while `obs64.exe` runs, and 2.x (from 2.3.7) refuses to start its OBS stream while `127.0.0.1:4417/status` reports `encoder.alive: true`.

1. In the 3.x app press Stop and wait for Stopped (the supervisor exits; `data\stopped-by-user` is written).
2. Quit StreamLoop from the tray icon (or close the 3.x `START.bat` window, when it was started by hand).
3. Start 2.x's `START.bat` and OBS.

Back to 3.x: close 2.x and OBS, start 3.x's `streamloop.exe`, press Start. Until Start, the stopped-by-user marker keeps the supervisor down, also across restarts. Autostart points at one install; turning it on in one moves it there.

## Architecture

**One exe, three processes: the control process, the supervisor and the app.** `ARCHITECTURE.md` has the diagram, the modes, the supervisor's interface and the main flows.

- **`crates/streamloop`**: the exe, a Windows GUI program (`windows_subsystem = "windows"`), so a double-click opens no console. `dispatch.rs` picks the mode from the first argument: `control`, `ui`, `detach` (`detach ui ARGS` or `detach start-bat [--root DIR]`: starts the app or `START.bat` hidden through `detach.rs` and exits, so what it started has no living parent), `supervisor`, `launch`, `protect`, `check`, `--version`; with no mode word, the control process when `STREAMLOOP_START` is set (`START.bat`) and the app otherwise, with every argument passed on. `main.rs` attaches `control`, `check`, `protect` and `--version` to their parent's console first (`streamloop_common::console::attach_parent`: unless stdout is a pipe or a disk file), never `ui`, `detach`, `launch` or `supervisor`, then runs the mode. `ui_mode.rs` parses the `ui` arguments, sets the working folder and the panic log (`app\logs\ui.log`) and calls the window crate.
- **`crates/ui`** (crate `streamloop-ui`, a library, edition 2024): R3's native GPUI window on gpui-kit `=0.9.0`, with its tray icon, built into the exe as `streamloop.exe ui`. It talks to the control process over the REST API on 127.0.0.1:7654 with its own API types (`api/types.rs`).
- **`supervisor/`** (crate `streamloop-supervisor`, a library): the streaming core, launched by the control process through `streamloop.exe launch`, a short-lived intermediate, so it is a sibling with a dead parent: a control process crash, restart or tree kill leaves the stream up. The control process talks to it over HTTP on 127.0.0.1:4417 and reads its events from `ROOT\data\logs\supervisor-events-DATE.jsonl` (`events-DATE.jsonl` up to 3.0.4; both are read). It stays threads plus tiny_http, with no async runtime.
- **`crates/common`**: what the processes share, with no async runtime and no HTTP: `config/` (the schema, load, save and merge; the supervisor's read-only view), `api.rs` (the `/status`, `/control`, `/reload` and `/cache` types), `paths.rs` (the install layout and root), `procs.rs` (the one place a child process is built), `console.rs` (the console attach of the GUI exe), `secret.rs` (DPAPI), `encoder.rs` (the encoder argument sets the supervisor streams with and the setup check probes with), `playlist.rs` (yt-dlp's playlist listing), `ytdlp_temp.rs` (each yt-dlp run's own temp folder under `yt-dlp\temp\`, so a killed run's 30 MB unpack does not land in the user's `%TEMP%`), `events.rs`, `time.rs`, `version.rs`.
- **`crates/control`**: the control process on tokio, axum and reqwest (rustls), a module per Node module of 3.0.
- **App** (`crates/ui/`, crate `streamloop-ui`, built into the exe as `streamloop.exe ui`): the native GPUI window with the first-run wizard, status, timeline, transport, Stop and Start stream, the Cache tab (the cache, the next download, the look-ahead and the last deletions), settings, webhooks, logs and the update button, all through the REST API on 127.0.0.1:7654. There is no web dashboard: `/admin` answers 404 like any unknown path.

**Control process modules** (`crates/control/src/`):

| Module | Responsibility |
|--------|---------------|
| `app.rs` | Wiring, the start order, a config save and the supervisor reload after it, shutdown and exit codes (3.0's `index.ts`) |
| `api/` | The REST routes (`/api/status`, `/api/state`, `/api/events`, `/api/cache`, `/api/config`, `/api/stream-key`, `/api/player/*`, `/api/stream/stop`, `/api/stream/start`, `/api/playlist/*`, `/api/logs`, `/api/update/*`, `/api/autostart`, `/api/discord/*`, `/api/setup-check`, `/api/yt-dlp/update`, `/api/shutdown`, `/api/quit`), the token check and the browser check |
| `supervisor_client.rs` | HTTP client for the supervisor (`/status`, `/control`, `/reload`, `/shutdown`, `/cache`); 1 s timeout, never fails; a status that does not parse still gives its `version` and `encoder.alive` |
| `launcher.rs` | Polls `/status` every 2 s, launches the supervisor after 3 misses (backoff 10 s, 30 s, 60 s, 5 min), stays down after Stop stream until Start, across restarts (`ROOT\data\stopped-by-user`); the start holds (`autoStartStream`, or a landing or rollback of a held stream); replaces a supervisor of another version |
| `setup_check.rs` | The first-run setup check (encoder probe and pick, cache sizing, dependency list, `data\setup-check.json`) and the guard before each launch (no launch while `obs64.exe` runs, the encoder fallback) |
| `events.rs` | Tails the supervisor's events file; maps each event to timeline text and an optional Discord alert; one alert per outage |
| `timeline.rs` | The 100-entry dashboard timeline, persisted as daily JSONL in `app\logs\` |
| `twitch.rs` | Helix check that the channel is live while the encoder runs; restarts the encoder at most once per 30 min, then only alerts |
| `readback.rs` | Every 5 min, two frames of the channel's own HLS 5 s apart; an overlay clock that stands still twice asks for `restart-encoder` |
| `discord.rs` | Webhook notifications with templates, batched over 5 s, criticals sent at once |
| `playlists.rs` | Playlist video lists for the dashboard, fetched with the install's yt-dlp and cached |
| `updater.rs` | GitHub Releases: picks a release, stages its exe, decides when it lands, writes the notice line |
| `landing.rs` | Lands a staged exe, confirms it on the next start or rolls it back; `data\update-state.json` |
| `ytdlp_update.rs` | yt-dlp self-update: `-U` and `--version` on a copy, kept as `yt-dlp\yt-dlp-<version>.exe`, which every yt-dlp run then takes; at start and at most hourly while YouTube lookups are paused |
| `autostart_store.rs` | The Run value and its Task Manager flag in the registry behind a trait, and the VBS migration at start |
| `leftovers.rs` | Removes what 3.0 left in an install, by name, at every start |
| `logging.rs` | `tracing`, with 3.0's log line format on the console and in `app\logs\streamloop-DATE.log` |
| `clock.rs`, `http.rs`, `runner.rs` | The wall clock, outbound HTTP and child processes behind traits, so tests answer instead |

**Browsers are refused.** The control process's API (7654) and the supervisor's (4417) answer 403 to a Host other than `127.0.0.1:PORT` or `localhost:PORT` (the port the server bound) and to any `Origin` header, before routing and before the token check (`crates/common/src/local_only.rs`). A new client of either must send no Origin and its own loopback address as Host, which HTTP libraries do by default.

**Configuration hot-reload:** `POST /api/config` saves `config.json`, rebuilds Discord and the Twitch checks, and asks the supervisor to `POST /reload`. The reply says when a change (ingest, key, encoder, freeze or silence duration) needs a supervisor restart, and why the reload failed (`reloadError`). A reload that nothing answers while the launcher has no supervisor up (a first run, after Stop, waiting for Start, down) is no failure.

**Exit codes** (`app.rs`): 0 for Ctrl+C, a closed console window, `/api/shutdown` (a landing that has begun to stop the supervisor finishes first, up to 100 s) or the app's Quit (`/api/quit`, once it has stopped the supervisor), 1 with no install root or a config that does not load, 2 when the port is taken, 64 for an unknown argument, 75 after a landed update, a rollback or the test hook. `ARCHITECTURE.md` (Control process) has the start order, the 3.0 leftovers it removes and the exe update.

## Testing

- **Rust:** `cargo test --workspace`. Each crate's tests sit in its own `tests/` (one file per module in `crates/control/tests/`, with `support/` for the shared fakes; `tests/app.rs` drives the wired process on a real socket), plus unit tests inside the modules; `crates/ui` keeps its tests inside its modules, on GPUI's `TestAppContext`. The behaviours 3.0's Node tests covered carry over by name. Golden fixtures recorded from the TypeScript before it was removed live in `crates/*/tests/fixtures/` and are test data now. Repo guards are cargo tests in `crates/streamloop/tests/`: `start_bat.rs` (the batch rules), `release_layout.rs` (the layout covers `Layout`, ships `supervisor\streamloop-pot.exe` and no `config.json`, `pot\` or `node\`, no test feature in the release build, the versions match, the token server's too) and `repo_guards.rs` (no `Command::new` outside `procs.rs`).
- **JavaScript:** `npm test` runs vitest over every `*.test.*` file outside `node_modules` and `.claude/` (where other sessions' worktrees live): the guard that no tracked file holds a Notion URL (`scripts/__tests__/no-notion-links.test.ts`), the leak guard that no tracked file holds a key, token or webhook URL apart from YouTube's public API key, which it allows by its SHA-256 (`scripts/__tests__/leak-guard.test.ts`), the harness (`supervisor/scripts/*.test.mjs`) and the spikes. The token server has its own tests: `cargo test` in `tokenserver\`.
- Test names describe the behaviour; tests are arrange-act-assert.

## Platform Notes

Windows-specific: the `/api/autostart` endpoint manages the `StreamLoop` value under `HKCU\Software\Microsoft\Windows\CurrentVersion\Run` (`"ROOT\streamloop.exe" ui --tray`); a `StreamLoop.vbs` that 3.1 wrote in the Startup folder is replaced by it at start. `streamloop.exe` is a GUI-subsystem program whose icon and version resource `crates/streamloop/build.rs` embeds; the app's tray icon (`tray-icon`) and its single-instance lock are Windows only. The release build (`build/prepare-release.js`) lays out the install that `crates/common/src/paths.rs` expects; `build/release-layout.js` lists the required paths and `crates/streamloop/tests/release_layout.rs` checks them against `Layout` so the two cannot drift. It bundles no Node: `streamloop-pot.exe` is built in `tokenserver\` (outside the root workspace, with the static CRT its `.cargo\config.toml` sets, so it needs no VC++ runtime) and copied to `supervisor\`. It also bundles yt-dlp nightly and deno (latest), and pinned inputs whose SHA-256 must match: ffmpeg 8.0 essentials (gyan.dev, only `ffmpeg.exe`) and the bgutil-ytdlp-pot-provider 2.0.0 yt-dlp plugin.

The harness scripts are ES modules (`"type": "module"` in package.json).

## Auto-Updater

The control process checks GitHub Releases for `Sponsorn/streamloop` and replaces `streamloop.exe`. `ARCHITECTURE.md` (Control process, Updates) has the whole flow.

**API endpoints:**
- `GET /api/update/status`: the updater's status (version info, availability, the notice line in `nodeNotice`), plus `progress` (`data\update-state.json`, or null) and `swapLog` (always empty; kept for the app)
- `POST /api/update/check`: a check now (60 s cooldown)
- `POST /api/update/apply`: Update Now; stages first if needed, then lands at once

**Release selection:** `pick_release()` takes only releases with the running major version; from 3 up, prereleases count, since 3.x ships as `--prerelease` while on trial. Drafts and tags that are not `vX.Y.Z` are skipped, and so is a release without the full ZIP and `streamloop-vX.Y.Z.exe` with its checksum, so one still uploading never wins and 3.0.x releases (no exe) are never offered. `compare_versions()` sorts a suffixed version such as `3.1.0-dev` below its release.

**Staging:** when a check finds an update outside dev mode, the exe goes to `_update\streamloop.exe`, its SHA-256 is checked as it streams in, and it must print `X.Y.Z` to `--version` within 10 s. Downloads come only from `github.com` and `objects.githubusercontent.com`; the GitHub API gets 10 s. A failed stage keeps the status `idle`, says why in the notice line and is tried again at the next check.

**Landing:** with `autoUpdateCheck` on, a staged update lands at 47 h 45 min of encoder uptime (171,900 s, just before Twitch's 48 h cut), once the stream has had no live encoder for 5 min, or when it was stopped from the dashboard (on two polls in a row, or with the supervisor gone, so Restart stream's Stop and Start land nothing); with it off, only Update Now lands it. A version that rolled back once waits for Update Now. A landing holds the launcher, posts "Installing StreamLoop X.Y.Z. The stream restarts." ("The stream stays stopped." when it was held), stops the supervisor (after up to 10 s for a `launch` helper still running, then `POST /shutdown`, 20 s, then its process tree), writes `_update\landed`, renames `streamloop.exe` to `streamloop.exe.old` and the staged exe into its place, starts the supervisor from the new exe unless the stream was held (a first run counts as held), and exits 75. `START.bat` starts the control process again 10 s later, from the new exe.

**Confirming or rolling back:** a start that finds `_update\landed` confirms the update within 180 s when the supervisor answers with the new version and its encoder is alive or the stream was off air before the landing (or, for a held stream, `streamloop.exe check` passes; a failed check is tried again every 5 s until the window ends, since a first start of the token server can take longer than one check allows), then removes `landed`, `.old` and `_update\` and adds "Updated to StreamLoop X.Y.Z." to the timeline. Otherwise it renames `streamloop.exe` to `streamloop.exe.bad` and `.old` back, moves `landed` to `data\update-rolled-back`, relaunches the supervisor from the old exe, sends a critical and exits 75. A new control process that crashes before it confirms is put back by `START.bat` the same way (see START.bat below), and the old one then adds the failure to the timeline and sends it as a critical, as it does for a landing that was cut off.

**START.bat.** `scripts/START.bat` is a short launcher around `streamloop.exe`. The exe does the updating, so the launcher swaps nothing and replaces nothing, itself included. It checks that `streamloop.exe`, `ffmpeg\ffmpeg.exe` and a token server exist (`supervisor\streamloop-pot.exe`, or both `node\node.exe` and `pot\build\main.js`, the TypeScript fallback an exe-only update keeps) and says "Reinstall StreamLoop from the full ZIP." when one does not. An update cut off between its two renames leaves only `streamloop.exe.old`, which it renames back before that check and again before every run. It then runs `streamloop.exe` in a loop with `STREAMLOOP_START` set to `first` for the first run and `restart` for every later one. Exit codes: 0 stays down; 2 means another StreamLoop holds the port, so it says so and stops, with no restart and no rollback; 75 is an exit on purpose (a landed update, a rollback, the test hook) and restarts after the same 10 s wait without the rollback check; anything else is a crash and restarts after 10 s. After a crash, when `_update\landed` and `streamloop.exe.old` both exist, the new exe stopped before it confirmed itself, so `:rollback` puts the previous exe back (`streamloop.exe` becomes `streamloop.exe.bad`, `streamloop.exe.old` becomes `streamloop.exe`, `_update\landed` moves to `data\update-rolled-back`) and the next run starts with `STREAMLOOP_START=rollback`. It checks each rename: when the first fails (antivirus or an indexer holding the file), or the second fails and the first is undone, it prints a `[WARN]` line and leaves `_update\landed` where it is, so the next crash tries again; `landed` moves only once both renames worked. When `streamloop.exe` is already gone, the update was cut off between its two renames and the new exe never ran: only the old exe is renamed back, `_update\landed` stays and nothing is written to `data\update-rolled-back`. Batch rules, all held by `crates/streamloop/tests/start_bat.rs`: CRLF line endings only (`.gitattributes` marks the file `-text`, so the committed blob is CRLF too), `ping -n 11 127.0.0.1 >nul` for the wait and never `timeout` (it fails without a console), and inside a parenthesised block only `rem` comments without parentheses, never `::`, since cmd parses the whole block at once and a `::` line aborts the launcher.

**Safety guards:**
- Two Update Now clicks at once land the update once
- Nothing is checked, staged or landed while a confirmation runs, and while a landing or its confirmation runs, `POST /stream/start`, `/player/*`, `/playlist/switch`, `/config`, `/stream-key`, `/update/apply`, `/yt-dlp/update`, `/setup-check/run` and `/quit` answer 409 "An update is running. Try again when it is done." `POST /stream/stop` answers 409 only during the landing: during the confirmation it goes through, and the check then confirms the update
- A staged exe still held (antivirus) when the swap starts leaves the old exe in place, reports the failure and relaunches the old supervisor; each rename gets three more tries 200 ms apart
- A supervisor that ignores `POST /shutdown` during a landing is ended after 20 s and the landing goes on
- An exe whose checksum or `--version` does not match is never staged; a release without `streamloop-vX.Y.Z.exe.sha256` is skipped as still uploading, before anything is downloaded (`streamloop-vX.Y.Z.sha256` is the ZIP's and never stands in)
- Every process the control process starts goes through `streamloop_common::procs::command` (the temp folder as cwd, no window), and the control process keeps its std handles out of its children, so a supervisor holds neither the console nor a file the control process's output was redirected to

**Dev mode:** when the root did not come from the exe's folder (`--root` or `STREAMLOOP_ROOT`), the updater only checks: it can report an available update but never stages or lands one.

**Config options:** `autoUpdateCheck` (boolean, default true) and `updateCheckIntervalMs` (number, default 21600000 = 6 hours). The first check runs 30 s after start.

**Start the stream when StreamLoop starts:** `autoStartStream` (boolean, default true). When it is off, the first control process start after `START.bat` opens comes up without launching the supervisor and the app shows "Ready to stream. Press Start." (`START.bat` sets `STREAMLOOP_START` to `first`, `restart` or `rollback`; the control process derives `update` itself from `_update\landed`; `data\waiting-for-start` carries a wait across a restart). An update never changes whether the stream is held: the first start after a landing, or after `START.bat`'s rollback, holds only when `_update\landed` or `data\update-rolled-back` says it was held before the landing. A supervisor that is already running is attached, never stopped, and a crashed one is still relaunched; a dashboard Stop wins over everything.
