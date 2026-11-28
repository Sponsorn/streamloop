## v3.2.0

A prerelease for trial, not marked latest. 3.1.x installs get it through the dashboard's update, at the next planned break or with Update Now. `START.bat` has not changed, so no full ZIP is needed. 3.0 installs move to it with the full ZIP by hand, as described for 3.1.0.

- **StreamLoop is now one Windows app.** Double-click `streamloop.exe` to start it. The StreamLoop window opens and, when StreamLoop is not running yet, starts it in the background, so no console window opens. The window is built into `streamloop.exe`, so updates bring it along. While it waits it says "Starting StreamLoop...", and after 30 seconds with no answer it says to double-click `START.bat`, whose window shows why StreamLoop did not start.
- **A tray icon shows the stream.** It is green when live, amber while the stream starts, recovers or updates, red when it is off air, when StreamLoop does not answer, or when StreamLoop has not started 30 seconds after the app started it, and grey when it is stopped. Pointing at it shows the state and the reason. A click opens the window, and a right-click offers Open StreamLoop and Quit StreamLoop.
- **Closing the window keeps the stream on air.** StreamLoop stays in the tray, and the first time a notification says so. Double-clicking `streamloop.exe` again brings the window back instead of starting a second StreamLoop.
- **Quit StreamLoop stops the stream and closes StreamLoop.** It is in the tray icon's menu and under Settings, where it replaces "Shut down the dashboard", and it asks first. Start StreamLoop again to go back on air. A stream you stopped with Stop before quitting stays stopped until you press Start.
- **The window follows updates.** After an update or a rollback it restarts into the version that now runs, where it was.
- **Autostart starts StreamLoop in the tray at logon.** "Run on Windows startup" now writes a `StreamLoop` entry under Windows' Run key instead of `StreamLoop.vbs` in the Startup folder, and the entry shows in Task Manager's Startup tab.
- **The web dashboard is gone.** `http://localhost:7654/admin` no longer answers, the "Open the web dashboard when StreamLoop starts" setting is gone, and Discord alerts no longer link to it. The window does everything the dashboard did.
- **Web pages can no longer reach StreamLoop.** Its two local ports, 7654 and 4417, now refuse every request from a browser, so a web page open on the PC cannot stop or steer the stream, or read the API token.
- **After the update, the console window stays until StreamLoop next starts from `streamloop.exe`, or until the next logon with autostart on.** StreamLoop goes on running in the `START.bat` window it was started in. To close that window now: double-click `streamloop.exe`, which opens the StreamLoop window beside the running StreamLoop, choose Quit StreamLoop from the tray icon, close the `START.bat` window once it says "StreamLoop stopped.", then double-click `streamloop.exe` again. The stream is off air in between.
- **An existing `StreamLoop.vbs` becomes the Run entry by itself.** At its first start after the update, StreamLoop replaces a `StreamLoop.vbs` it wrote for this install with the Run entry, so the next logon starts StreamLoop in the tray. A `StreamLoop.vbs` of a 2.x install, or one StreamLoop did not write, is left alone.
- **Close and delete the trial copy of the window in `C:\StreamLoop-ui`.** That is the earlier `streamloop-ui.exe`. Left running, it would show as a second window beside the one in `streamloop.exe`.

---

## v3.1.2

A prerelease for trial, not marked latest. 3.1.0 and 3.1.1 installs get it through the dashboard's update, at the next planned break or with Update Now. 3.0 installs move to it with the full ZIP by hand, as described for 3.1.0.

- **Videos download at 720p or better again.** Since 24 November, YouTube's mweb player client, the first in the default list, offers only 360p that yt-dlp can download, and StreamLoop took it, so the box streamed 360p for days. Each player client is now asked for 720p to 1080p first, and a client that offers nothing in that range is passed over for the next one. On the dev machine, web_embedded, the second client in the default list, offered 1080p. Only when no client offers 720p does StreamLoop take the best there is. It then puts a warning on the timeline, "Video N (id) was downloaded below 720p: none of the player clients offered 720p or better.", and sends it to Discord through the critical alerts setting, so there it starts with CRITICAL. The next time that video comes up for download, StreamLoop asks for 720p again.
- **yt-dlp's update no longer breaks lookups and downloads that are running.** yt-dlp reads its own code from `yt-dlp.exe` while it runs, and its update replaced that file, so a playlist fetch or a download running at that moment could fail with "Error -3 while decompressing data". On the box this happened when 3.1.1 installed. The update now works on a copy and keeps the result as `yt-dlp\yt-dlp-<version>.exe`, which every later yt-dlp run uses. `yt-dlp.exe` stays as the ZIP shipped it. StreamLoop keeps the two newest copies and removes older ones at a later update, once nothing runs them.
- **No full ZIP is needed.** The update installs the new `streamloop.exe`.

---

## v3.1.1

A prerelease for trial, not marked latest. 3.1.0 installs get it through the dashboard's update, at the next planned break or with Update Now. 3.0 installs move to it with the full ZIP by hand, as described for 3.1.0.

- **Downloads pick up again after a YouTube outage.** When a video's size lookup failed, StreamLoop marked the video as failed and did not ask for it again until it had played. On the box, after the outage on 23 November, a 7.8-hour video aired straight from YouTube. While it played, StreamLoop deleted the next two videos, which it had already downloaded, and downloaded nothing. A failed size lookup is now tried again the way a failed download is: up to an hour later while the video is far off, down to 2 minutes as its turn comes.
- **A video that YouTube gives no size for now downloads without one.** StreamLoop used to ask YouTube for that video's size again every 5 seconds, which could set off YouTube's bot check. This hasn't been seen on the box.
- **yt-dlp updates itself while YouTube lookups keep failing.** When YouTube changes its player in a way yt-dlp can't handle yet, every lookup fails until yt-dlp ships a fix, usually within hours. While lookups are paused, StreamLoop now runs yt-dlp's update when the pause starts and at most once an hour after that, on top of the update at each start. yt-dlp's warnings now go into `ytdlp-DATE.log` with its errors, since a warning often names the cause.
- **The status line says why YouTube lookups are paused.** It said "YouTube is blocking requests." for every pause. When lookups fail for another reason, as with the new YouTube player on 23 November, it now says "YouTube lookups keep failing." The Discord alert says the same.
- **A security fix in the `url` library** (RUSTSEC-2026-0592): a deeply nested `blob:` address could crash it.
- **No full ZIP is needed.** The update installs the new `streamloop.exe`. The full ZIP also carries a token server with the `url` fix. That server only talks to yt-dlp on the same machine and to YouTube, so there is no need to install the ZIP for that alone.

---

## v3.1.0

A prerelease for trial, not marked latest. 2.x installs do not update to it, and the 3.0 dashboard does not offer it: install the full ZIP by hand, as the last point says.

- **StreamLoop is now one program, `streamloop.exe`.** The Node.js server that ran the dashboard, the Discord alerts, the Twitch checks and the updater is gone, and `streamloop.exe` does that work itself. The stream runs in a second `streamloop.exe` process, apart from the dashboard as before, so closing the window still leaves the stream on air. The dashboard and its settings work as they did, at http://localhost:7654/admin. The full ZIP no longer contains Node.js.
- **The token server is now one exe, `supervisor\streamloop-pot.exe`.** It replaces the TypeScript token server in `pot\` and the Node.js in `node\` that ran it, and StreamLoop removes both folders when it starts. In a 2-hour run on the dev machine, all 52 download tries with its tokens finished, with no "HTTP Error 403: Forbidden".
- **Updates now replace the whole program and install themselves at a planned break.** The 3.0 update replaced only `app\` and `START.bat`, so every release that changed the supervisor needed the full ZIP. With automatic update checks on (the default), the dashboard now downloads the new `streamloop.exe`, checks it and installs it by itself: just before Twitch ends the broadcast at its 48-hour limit, about 5 minutes after the stream goes off air, or right after you press Stop. Update Now installs it at once, also with the checks off. The stream is off air for about 10 seconds while an update installs. A release that changes ffmpeg or the token server says so here and still needs the full ZIP.
- **A failed update goes back by itself.** If the new version does not come up within 3 minutes, StreamLoop puts the previous one back, restarts it and sends a Discord alert. If the new version crashes before then, `START.bat` puts the previous one back and prints a `[RECOVER]` line. Either way, that version installs again only when you press Update Now.
- **New setting: Start the stream when StreamLoop starts** (Settings, under Startup). When it is off, StreamLoop comes up without streaming and waits for you to press Start. It is on by default, which is how 3.0 behaved. After an update the stream is as it was before: streaming, stopped or waiting for Start.
- **The stream comes back sooner after an internet outage.** When the encoder lost Twitch, it waited out its whole retry step, up to 60 seconds, even when Twitch answered again sooner, because the check for that never worked with Twitch's address. It now reconnects within a few seconds of Twitch answering.
- **A second StreamLoop window no longer restarts every 10 seconds.** When another StreamLoop (2.x, or another copy of 3) holds port 7654, the window says "Another StreamLoop is already running. Close it and start this one again." and stays down.
- **To move from 3.0, install the full ZIP by hand:** press Stop, close the window, extract the ZIP to a separate folder, copy what is inside its `streamloop` folder over the install, start it again and press Start. Settings, the stream key, the timeline and `data\` are kept. At its first start StreamLoop removes what 3.0 left behind (`app\src`, `app\node_modules`, `node\`, `pot\` and the old supervisor exe). Later updates come through the dashboard.

---

## v3.0.5

A prerelease for trial, not marked latest. 2.x installs do not update to it.

- **Playlists now play one after another.** 3.0 only ever played one playlist: when it ended, that playlist started over and the others never aired. Now the next playlist starts when one ends, and after the last one the first starts again, as in 2.x. The next playlist's first video downloads ahead of time, so the change looks like any other change of video.
- **The order in Settings stays as you set it.** Pressing ▶ on another playlist used to move it to the top of the list. It now switches the stream and leaves the order alone, and the dropdown on the Monitor tab marks the playlist that is playing. A restart goes back to the playlist that was on air. Reordering in Settings no longer switches the stream, and ▶ on another playlist while the stream is stopped now tells you to start it first.
- **Next and Previous cross into the neighbouring playlist** at the end of a list.
- **The timeline says when a playlist ends:** "Playlist finished. Now playing ..." or, with one playlist, "Starting it again". A playlist that cannot be loaded is tried again every 10 minutes, and if a playlist ends while another is still missing, Discord gets an alert.
- **Downloads recover when YouTube refuses a token.** On the box, every token from a new token minter was refused for one video. After two downloads in a row that used a token and still got "HTTP Error 403: Forbidden", the supervisor now clears the token server's cache, at most once per 30 minutes, so the next token comes from a new minter. The timeline notes each clear.
- **Each download try now uses one player client,** in the order from Settings, and `downloads-DATE.csv` names the client in a new last column. The first try used to combine them, so the log could not say which one served a file.
- **The supervisor's events log is now `supervisor-events-DATE.jsonl`.** It shared the name `events-DATE.jsonl` with Node's timeline, so the two collided when copied into one folder.
- **Zod is updated to version 4,** which fixes a high-severity advisory about memory use when validating oversized collections (GHSA-mrhx-c7xc-m3w5). Config files from earlier versions load with the same values. When Settings refuses a value, the message is worded a little differently.
- **This release changes the supervisor, so install the full ZIP**, the same way as 3.0.4: press Stop, close the window, extract the ZIP to a separate folder, copy what is inside its `streamloop` folder over the install, start it again and press Start. Settings, the stream key and `data\` are kept. With the update alone the stream stays on one playlist, and ▶ on another playlist says the full ZIP is needed.

---

## v3.0.4

A prerelease for trial, not marked latest. 2.x installs do not update to it.

- **Twitch's 48-hour cut is no longer reported as a stream drop.** Twitch ends a broadcast after 48 hours, and StreamLoop reconnects within seconds. That reconnect used to send "Stream stopped unexpectedly" and "Stream restarted successfully" to Discord. It now shows on the timeline only: "Twitch ended the stream at its 48 hour limit". If the reconnect fails three times in a row, Discord still gets the alert.
- **A video YouTube refuses to download is no longer asked for every 35 seconds.** On the box, one video got "HTTP Error 403: Forbidden" and was downloaded again every 35 seconds, around the clock, which also set off "3 downloads failed in a row" on its own. Now each download tries the player clients from Settings together, then one at a time, since YouTube can refuse a video to one client and serve it to another. If all of them fail, it waits before trying again: up to an hour while the video is far off, down to 2 minutes as its turn comes. The timeline says when the next try is. "Downloads failed in a row" counts each video once.
- **The Startup toggle can take autostart over from another install.** When autostart pointed at the 2.x install, the toggle showed as on and only offered to turn it off, which it then refused. It now shows as off in that case, and turning it on moves autostart to this install.
- **This release changes the supervisor, so install the full ZIP**, the same way as 3.0.3: press Stop, close the window, extract the ZIP to a separate folder, copy what is inside its `streamloop` folder over the install, start it again and press Start. Settings, the stream key and `data\` are kept. With the update alone, only the Startup toggle fix applies.

---

## v3.0.3

A prerelease for trial, not marked latest. 2.x installs do not update to it.

- **The video title and the clock are now at the top left.** They sat at the bottom left, over the stream's own social media overlay. The progress bar stays along the bottom edge, and the "NOT LIVE" label stays at the top right. The Twitch picture check now reads the clock at its new place.
- **The system check sizes the download cache from the playlist.** It used to recommend at most 4 GB, too little to download the next of a playlist's 8-hour videos while the current one plays, so every video change had to stream straight from YouTube first. It now recommends room for the two longest videos in the playlist, at most half the drive's free space, and says so when the drive is too small. Run the check again on the Playback tab to get the new figure.
- **This release changes the supervisor, so install the full ZIP**, the same way as 3.0.2: press Stop, close the window, extract the ZIP to a separate folder, copy what is inside its `streamloop` folder over the install, start it again and press Start. Settings, the stream key and `data\` are kept.

---

## v3.0.2

A prerelease for trial, not marked latest. 2.x installs do not update to it.

- **The cookies setting now applies to the stream.** "yt-dlp cookies from browser" in Settings used to reach only the video list on the Monitor tab. The supervisor now passes it to every YouTube lookup and download too, and picks up a new value as soon as you save. Firefox works while it is open. Chrome often doesn't, because it locks its cookie file while it runs.
- **A cookie problem is no longer mistaken for YouTube blocking.** When yt-dlp cannot read the browser's cookies (wrong browser name, missing profile, a locked file), the timeline and Discord say so once, with yt-dlp's reason, and yt-dlp tries again a minute later. It no longer counts toward the pause that YouTube's bot check sets off.
- **Downloaded videos survive a restart.** The supervisor used to empty its download folder every time it started, so a restart or an update threw away a fully downloaded video and had to ask YouTube for it again. Finished downloads now stay, and a resume plays straight from the file. Only half-finished files, and files for videos no longer in the playlist, are removed.
- **The stream starts even when YouTube won't hand over the playlist.** If loading the playlist fails at start, the supervisor uses the last list it saved for the same playlist, so a downloaded video can play on. It keeps trying YouTube and switches to the fresh list once it loads. A list saved for another playlist is never used.
- **A config.json the supervisor refuses now says why.** Before, the supervisor exited without a word and the stream stayed down. The dashboard now shows the reason and Discord gets one alert. Settings also refuses a cookies value longer than 200 characters before saving it.
- **The Monitor tab shows when YouTube will be tried again.** While YouTube is blocking requests, the stream line says so with the time of the next try, for example "YouTube is blocking requests. Next try at 10:41."
- **This release changes the supervisor, so install the full ZIP.** The dashboard's update replaces `app\` and `START.bat` only, and the 3.0.1 supervisor ignores the cookies setting and still empties its download folder. To install the full ZIP, press Stop, close the window, extract it into the same folder as the first time (`C:\StreamLoop3`, not `C:\StreamLoop3\streamloop`), start it again and press Start. Settings, the stream key and `data\` are kept.

---

## v3.0.1

A prerelease for trial, not marked latest. 2.x installs do not update to it.

- **Choosing a playlist on the Monitor tab no longer changes the stream.** In 3.0.0 the dropdown switched the stream to that playlist's first video at once. It now only shows that playlist's videos, with a note that it is not playing. Press ▶ on one of them to switch the stream to that playlist, starting at that video. In the playlist that is playing, ▶ jumps to the video as before.
- **▶ now starts the video you pressed.** The dashboard's list counted private and deleted videos that the supervisor leaves out, so ▶ could start the next video instead. The list now leaves them out too, and ▶ also sends the video's id, so the right video plays even when the playlist changed on YouTube after the dashboard loaded it.
- **Silence no longer interrupts the stream.** In 3.0.0 a silent stretch restarted the video once, which cut the stream for a moment during quiet parts of a video. Silence is now only noted on the timeline. A real audio failure stops the feeder, which is still caught and healed; a frozen picture is still retried and skipped as before.
- **Far fewer YouTube requests.** 3.0.0 looked up the previous video again every 10 minutes, so that Previous would start without waiting. During long videos that was nearly all of its YouTube traffic, and on the box it set off YouTube's bot check after an hour. The lookup now happens only when you press Previous, which shows the standby slate for a few seconds first.
- **This release changes the supervisor, so install the full ZIP.** The dashboard's update replaces `app\` and `START.bat` only. With the update alone the dropdown fix works, but ▶ on another playlist's video starts that playlist at its first video: the 3.0.0 supervisor ignores the start video and the video id it is sent. To install the full ZIP, press Stop, close the window, extract it into the same folder as the first time (`C:\StreamLoop3`, not `C:\StreamLoop3\streamloop`), start it again and press Start. Settings, the stream key and `data\` are kept.

---

## v3.0.0

A prerelease for trial, not marked latest. 2.x installs do not update to it.

- **StreamLoop 3 streams without OBS or mpv.** A supervisor program plays the YouTube playlist through ffmpeg and sends it straight to Twitch. It downloads the next videos ahead, keeps one connection to Twitch across every video change, and retries or skips a video that fails. A frozen picture gets retries before a skip, and a long silence gets one retry. The dashboard, Discord alerts and the Twitch liveness check stay, now driving the supervisor.
- **It installs beside 2.x.** Make a new, empty folder (for example `C:\StreamLoop3`), extract the ZIP into it and run `START.bat` in the `streamloop` folder it creates (`C:\StreamLoop3\streamloop`). It does not touch a 2.x folder; do not extract it into one, since the 2.x ZIP has a `streamloop` folder too. The first run opens a setup wizard: playlist, Twitch stream key (stored encrypted for your Windows user), optional Discord webhook and autostart, and a system check that picks the hardware encoder that works on this PC and a download cache size for the free disk space.
- **One stream at a time.** Both versions use port 7654, so only one runs. 3 does not start its stream while OBS is running, and 2.x (from 2.3.7) does not start its OBS stream while 3 is streaming.
- **Switching back to 2.x.** In the 3 dashboard press Stop and close its window, then start 2.x and OBS. To return, close 2.x and OBS, start 3 and press Start. Stop holds until you press Start, even across restarts. Autostart points at one install; turning it on in one moves it there.
- **What updates deliver.** The dashboard's update replaces `app\` and `START.bat` only, and the stream stays up while it installs. Settings, the stream key and the resume point are kept. yt-dlp updates itself each time StreamLoop starts. Node.js, the supervisor, ffmpeg and the token server only change with the full ZIP: press Stop, close the window, extract the full ZIP into the same folder as the first time (`C:\StreamLoop3`, not `C:\StreamLoop3\streamloop`) so its `streamloop` folder lands on the existing one, then start it again and press Start. Your settings, the stream key and `data\` are kept, because no ZIP holds a `config.json`. The dashboard says when a release bundles a newer Node.js.

---

## v2.3.7

- **Updates now stay on 2.x.** StreamLoop 3.0 is coming and installs in its own folder next to 2.x, so you can switch back. The updater used to take whatever GitHub listed as the newest release, which would have pulled 3.0 into a 2.x folder. It now looks through the releases and only installs the newest 2.x one.
- **2.x will not start its stream while 3.0 is streaming on the same PC.** Two programs streaming to one stream key knock each other off Twitch. Before OBS starts or restarts the stream, 2.x asks 3.0 (on `127.0.0.1:4417`) whether it is streaming. If it is, 2.x holds off, posts one Discord alert and adds a line to the dashboard timeline. It tries again on its normal 30-second check, so stopping 3.0 is enough to let 2.x stream again. If nothing answers on that port, or something unrelated does, 2.x streams as before.

---

## v2.3.6

- **Fixed the launcher dying on every server exit, so crashes were not restarted and updates closed the window.** `START.bat` had `::` comment lines inside the parenthesized update block. cmd parses that whole block each time the server exits, whatever the exit code, and a `::` line inside a block can abort the script with `. was unexpected at this time.`, which closes the window. Reproduced locally: with the 2.3.1-2.3.5 launcher a server that simply crashed took the launcher down with it (no restart after 10 s), and an update closed the terminal without relaunching. The comments inside blocks are now `rem` lines without parentheses. Verified in a fake install across all four paths: crash restart, clean update swap, update with `app\` locked (aborts, restarts the old version) and an update that replaces the launcher (trampoline relaunch).
  - **Existing installs need one manual step.** This update is applied by your current, broken launcher, so the window will close once more. When it does: in the install folder, rename `app` to `_update_old`, move `_update_tmp\app` to `app`, copy `config.json`, `state.json` and `logs` from `_update_old` into `app`, copy `_update_tmp\START.bat` over `START.bat`, then start `START.bat`. Updates after 2.3.6 apply on their own.
- **Fixed the update swap failing because `app\` was locked (#1).** Confirmed on the live box with Resource Monitor: Firefox, opened by the server with `start`, inherited `app\` as its working directory and kept it open, so `rename app _update_old` failed on all five tries. The log showed a second holder too: a playlist metadata fetch that started 0.2 s before the update exit left a `yt-dlp.exe` running with the same working directory. Every process the server starts (browser, mpv, taskkill, OBS helpers, playlist fetch, yt-dlp self-update) now gets an explicit working directory outside the install (`SPAWN_CWD`, the temp folder); OBS keeps its own folder as before. `autoOpenAdmin: false` is no longer needed to make updates work.
- **Internal:** a test scans `START.bat` for `::` lines inside blocks, parentheses in `rem` comments and non-CRLF line endings; another fails when a server file starts a process without a working directory. `.gitattributes` pins `*.bat` to CRLF.

---

## v2.3.5

- **Playback errors now show yt-dlp's real error instead of "unrecognized file format".** When yt-dlp fails to extract a video, mpv tries to open the YouTube watch page as media and reports a generic "unrecognized file format", so every extraction failure looked like a format problem. The server now subscribes to mpv's error-level log messages over IPC, keeps yt-dlp's own `ERROR:` line for the file being loaded, and shows that line in the playback-error event and the Discord alert.
  - **The YouTube bot check is named as a cookies problem.** "Sign in to confirm you're not a bot" is reported as "YouTube bot check: yt-dlp has no logged-in YouTube cookies (check the browser set in ytdlCookiesFromBrowser)". The usual cause is the cookie browser being logged out of YouTube; no format or player-client change fixes it.
  - Recovery behaviour is unchanged: the same retry and skip rules apply, only the reported cause is different.
- **New `autoOpenAdmin` config option (default `true`).** Set it to `false` to stop the server opening the admin dashboard in the default browser on every start. On a streaming box that saves the 500-700 MB a browser uses for that one tab; the dashboard is still reachable at `http://localhost:7654/admin` whenever you want it. Set it in `config.json`; there is no dashboard toggle yet.
- **Internal:** the repo gains `shell/`, an experimental Tauri window for the dashboard plus a memory measurement script. It is not part of either release ZIP and nothing in the app uses it.

---

## v2.3.4

- **Player client is now a multi-client fallback list, default `tv,web_safari` — fixes the "unrecognized file format" skips.** YouTube's per-client stream availability keeps shifting: on June 27 the `tv` client 403'd every video (so 2.3.2 switched the default to `web_safari`), but by July 7 `web_safari` had started returning *only* thumbnails ("Requested format is not available" → "unrecognized file format" → skip) for older VODs, while `tv` worked again. Pinning any single client just moves the breakage around. `ytdlPlayerClient` now accepts a **comma-separated list** that yt-dlp tries in order, merging formats, so if one client goes thin or gets CDN-403'd for a given video the other covers it (verified end-to-end that mpv passes the comma through to yt-dlp intact, and that `tv,web_safari` serves videos that `tv` alone 403s). The default is `tv,web_safari`; the dashboard field and `config.example.json` are updated to match. An explicitly-set value is preserved on update, and `''` still falls back to yt-dlp's own default selection.

---

## v2.3.3

- **Fixed auto-update silently failing to apply (server had to be updated by hand).** Every update exited cleanly and the server restarted — but on the *old* app, leaving `_update_tmp` full, because `START.bat`'s `rename app _update_old` failed with "the process cannot access the file because it is being used by another process." Root cause: mpv is spawned with no working directory, so it (and the `yt-dlp.exe` → `node.exe` n-param challenge solver it launches) inherits `cwd = app\`. On shutdown the server called `proc.kill()`, which on Windows terminates **only mpv.exe** and orphans those `node` grandchildren — and since they hold `app\` as their working directory, the directory stays locked and the swap can never rename it. `mpv.stop()` now kills the **entire process tree** (`taskkill /T` on Windows) so no descendants survive to lock `app\`, and the update swap completes on its own.
  - Note: this fix lands *in* 2.3.3, but the update that delivers it is still applied by the pre-fix 2.3.2 process — so installing 2.3.3 itself may need one last manual swap of `_update_tmp\app` → `app`. Every update after 2.3.3 applies automatically.
- **Internal:** extracted `treeKillCommand` for the platform-specific tree kill, with unit tests.

---

## v2.3.2

- **Fixed videos being skipped with "no audio or video data played."** YouTube has been returning **HTTP 403 Forbidden** on the stream URLs resolved by yt-dlp's default `tv` (TVHTML5) client — it now requires a GVS PO token that headless extraction doesn't have. yt-dlp resolved each video fine, but the CDN rejected the stream the instant mpv opened it, so the video "played" zero bytes and got counted as a playback error; three of those in a row skipped a perfectly good video (logs showed bursts of "Playback error … — no audio or video data played"). This was **not** the screenshot freeze monitor — it never fired on the skipped videos; the skips came entirely from the 403s. Two changes fix it:
  - **New `ytdlPlayerClient` config option**, defaulting to **`web_safari`**, which uses YouTube's HLS delivery path that is still served normally (verified on the live box: `tv` → 403, `web_safari` → 206). Passed to mpv as `--extractor-args youtube:player_client=<value>`. Editable from the dashboard under **Playback Settings → yt-dlp player client** (single client only, no commas; blank = yt-dlp's own default). The new default fills itself into existing `config.json` on update, so the fix applies with no manual editing. Requires a server restart to take effect.
  - **Recovery now retries a 403 in place instead of skipping.** `shouldRetryUrl` previously only recognised `http`/`network`/`tls` error strings, but mpv surfaces a 403 as "no audio or video data played", which slipped through and burned a consecutive-error slot toward a skip. A 403 / "no audio or video data played" now triggers an in-place re-resolve (bounded to 2 tries per video) before counting as an error — a safety net if a 403 wave gets past the client change.
- **Internal:** extracted mpv-arg construction into a testable `mpv-args.ts` module (behavior unchanged); added unit tests for the player-client arg and the 403 retry path.

---

## v2.3.1

- **Recovery and restarts now resume directly on the saved video instead of walking the playlist head.** On every reload/restart mpv used to start at playlist position 0 and play through videos #0..#N before a deferred jump fired. With the playlist head now frequently failing (expired URLs / 403s), each recovery produced a burst of "Playback error on video #0/1/2/3" and ~20–35s of viewer disruption — and because the jump listener was torn down after 15s, a slow or failing head could drop the resume jump entirely and silently resume on the wrong video at the wrong position. StreamLoop now sets `playlist-start` to the resume index before loading (so mpv begins on the target entry and never walks the head), uses `reloadIndex` for the resume jump so the seek still applies, and raises the jump-listener teardown from 15s to 120s so a slow/failing head can't silently drop the resume.
- **Real mpv error reason in events and Discord alerts.** Playback-error events and their Discord notifications now carry mpv's actual `file_error` string instead of a hardcoded `0` (the confusing "Playback error **0**").
- **Fixed the "update applied, terminal never reappears" hang.** When an update shipped a new `START.bat`, the running launcher copied the new file over its own path mid-execution. cmd.exe reads a batch file from disk by byte offset as it runs, so replacing it desynced the read position and usually closed the window before the relaunch — the update's `app/` swap had already happened, but the server never came back up. `START.bat` now **never overwrites itself in place**: it stages a changed launcher to `START.bat.new` (only when it actually differs, via `fc /b`) and hands off to a detached one-shot script that swaps the launcher and relaunches in a fresh window after the original exits.
- **Self-heal a half-swapped install on launch.** The `app/` update swap is non-atomic (rename old app aside, then move the new app into place). If the process was interrupted between those steps there was no `app/` at all, and every subsequent launch died at the preflight check — one bad update bricked the box. `START.bat` now restores `app/` from `_update_old/` (or promotes the staged `_update_tmp\app`) at startup before preflight, so an interrupted update recovers itself.

  > **Note for existing installs:** this update is applied by your *current* (pre-fix) `START.bat`, so the delivering update can still hit the old self-overwrite hang. If the window closes and the server doesn't come back, the new `app/` is already in place — just relaunch `START.bat` once. After that, the new trampoline launcher makes all future updates safe. For a guaranteed-clean transition, replace `START.bat` (or reinstall from the full ZIP) out-of-band.

- **Internal:** decomposed `recovery.ts`'s ~180-line `processHeartbeat` into intent-named steps and removed dead code (behavior unchanged, all tests pass); rewrote the README for the current mpv + OBS Window Capture architecture.

---

## v2.3.0

- **In-place freeze/EOF recovery actually re-resolves the stream now.** The "URL retry" used by freeze and premature-EOF recovery reloaded via `jumpTo(pos)` (`set_property playlist-pos`). During a video-stream freeze mpv is still "playing" the current index, so setting `playlist-pos` to its own value was a **no-op** — mpv never re-resolved the URL, and the frozen frame persisted (logs showed ~8 min, until the audio track ran out) while recovery silently burned through to a full mpv restart. Both `retryCurrentAtPosition` and the `RetryCurrent` recovery step now reload via `playlist-play-index` (`reloadIndex`), which restarts the current entry even when it's already current — so the in-place retry gets a real chance to fix the freeze before any restart.
- **Proactive signed-URL refresh.** YouTube googlevideo URLs expire ~6h after yt-dlp resolves them, so any video longer than the TTL freezes around the 6h mark. StreamLoop now tracks URL age on a monotonic clock (immune to NTP/RTC steps after a power outage) and forces an in-place reload at `proactiveUrlRefreshMs` (default 5.5h), turning an unplanned freeze into a planned few-second rebuffer. Short videos never trigger it; it's gated on healthy playback and skips near end-of-file; `0` disables it. Exposed in the dashboard under **Playback > Recovery** as a preset dropdown (Disabled / 4–5.5h), hot-reloaded through the config path.
- **No more spurious mpv restart on every server start.** On startup the recovery engine resumes the saved position, which on a cold yt-dlp run (playlist resolution, then a second resolution for a deep-playlist video, then the seek) keeps mpv non-playing well past the ~30s non-playing budget. The non-playing watchdog mistook that for a fault and escalated to an mpv restart on **every** start — a black screen for viewers — before settling. A configurable **startup grace** (`initialLoadGraceMs`, default 90s) now suppresses the non-playing watchdog until playback is confirmed once since the current mpv connect, or the window elapses. It re-applies on every connect (including periodic restarts), so genuine mid-stream stalls still recover exactly as before, and it logs a one-shot event ("holding off recovery during startup grace") so the hold-off is visible in the dashboard rather than looking like a hang. The stall, video-freeze, output-freeze, heartbeat-timeout, and file-error paths are unchanged.
- **yt-dlp auto-update + manual button.** An outdated yt-dlp silently truncates YouTube playlists after the first ~100-entry page, so longer playlists looked incomplete in the dashboard. yt-dlp now self-updates in the background on startup (gated by the new `autoUpdateYtdlp` config option, default on), and the dashboard's **Update yt-dlp** button is wired to the previously-unreachable endpoint. After a manual update the playlist-metadata cache is cleared so corrected counts appear without a restart.
- **Slim "update-only" release bundles.** `npm run build:release:slim` produces `streamloop-vX.Y.Z-update.zip` containing only `app/` + `yt-dlp/` — no portable Node.js or mpv, which the auto-updater never swaps anyway. The updater now **prefers the `-update.zip` asset** over the full bundle, so existing installs get a much smaller download; the full ZIP is still used for first-time installs. Checksums are matched to the chosen ZIP by name, so a release can safely carry both bundles.
- Cut mpv log-file noise ~98%: cplayer's `video EOF (status=4)` was logged thousands of times per second during every freeze. cplayer is now capped at `verbose` while `ytdl_hook=v` is kept for yt-dlp extractor errors.

---

## v2.2.0

- **Ground-truth output-freeze detection.** A new watchdog screenshots the OBS capture source every 10s and detects when the *streamed picture itself* is frozen — catching failures the mpv-side detectors can't see (a "video unavailable" still that mpv reports as healthy playback, or OBS losing the window capture). It compares frame hashes only — **no screenshots are ever written to disk**. When the picture stays byte-identical for a configurable window it takes a second confirmation screenshot a moment later before acting, then recovers **in place** at the current position (re-resolves a fresh stream) — **no mpv restart**, so OBS never captures a black screen. It shares the existing in-place URL-retry budget with the bitrate/vfps freeze detector and escalates to the old restart sequence only after those retries are exhausted. Two new settings under **Playback > Recovery**: *Detect frozen output* (on by default) and *Output freeze window* (default 30s — raise it if you ever stream legitimately static content). The window is editable from the dashboard, so it can be tuned without a new release.
- **Persistent recovery event log.** The dashboard's recovery-event timeline is now written to daily-rotated JSONL files in `logs/` (7-day retention) and reloaded on startup, so the history survives auto-updates, mpv restarts, and crashes instead of resetting to empty each time.
- **"Video skipped" Discord alerts now actually fire.** When a video is dropped after repeated playback errors (`maxConsecutiveErrors`), a skip notification is sent — previously the skip event was advertised in the dashboard but never triggered.
- Removed the orphaned quality-recovery feature (config keys, engine code, and the dashboard "Quality" display + "Quality Recovery" settings). It was v1-era scaffolding built around the YouTube IFrame player's quality signal, which mpv doesn't expose, so it never did anything. Existing `config.json` files migrate automatically — the stale keys are dropped on next load.

---

## v2.1.9

- **Video-freeze recovery now fixes the freeze in place instead of restarting mpv.** YouTube serves video and audio as separate streams; when the *video* stream stalls or EOFs but audio keeps playing, mpv holds the last frame and never fires a file-level `end-file` — so the existing URL-retry never triggered, and the heartbeat detector almost never caught it (Emma's logs: the condition occurred constantly but auto-recovery fired ~once a week, so the app had to be restarted by hand). The detector now:
  - trips on `estimated-vf-fps < 1` **or** `video-bitrate ≈ 0` (two independent signals — a frozen video stream shows up in at least one);
  - ignores the brief normal video-EOF burst at a video's natural end (`duration − time-pos < 10s`) and tolerates single-heartbeat blips, cutting false positives;
  - responds with an **in-place URL retry at the current position** (re-resolves a fresh stream, resumes near the break) — **no mpv restart**, so OBS never captures a black screen — escalating to the old restart sequence only after 3 in-place retries fail;
  - logs `vfps`, `video-bitrate`, `audio-bitrate`, `time-pos`, and `duration` whenever it trips, so each freeze self-documents in the logs.
- Separately confirmed the recurring ~6-hour `Premature stream end (eof)` events are normal YouTube signed-URL expiry, already handled cleanly by the existing in-place URL retry — left unchanged.

---

## v2.1.8

- Auto-updater now refreshes the bundled `yt-dlp/` directory (yt-dlp.exe + deno.exe) on every update, not just `app/`. YouTube rotates its player JS challenge faster than our release cadence — without this, updating wouldn't carry new yt-dlp/deno binaries into existing installs. Same retry-and-rollback safety as the existing app swap.

---

## v2.1.7

- Bundle Deno with the release. YouTube's player JavaScript now requires solving an "n-parameter" challenge before video URLs resolve. yt-dlp's EJS subsystem can solve it but needs a JS runtime — we ship `deno.exe` alongside `yt-dlp.exe` and yt-dlp picks it up automatically. Without this, v2.1.6 fails with "Only images are available for download" on every video.
- Switch bundled yt-dlp from stable to nightly builds. YouTube rotates challenge shapes faster than yt-dlp's stable cadence; nightly tracks them more reliably. If nightly ever breaks, manually replacing `yt-dlp/yt-dlp.exe` with a stable build is always a valid fallback.

**Upgrade note for existing v2.1.6 installs:** the admin dashboard auto-updater only swaps the `app/` directory, so it won't drop a new `deno.exe` into your existing `yt-dlp/` folder. After auto-updating, either manually download `deno.exe` from https://github.com/denoland/deno/releases/latest (Windows x64) and place it at `streamloop/yt-dlp/deno.exe`, or do a clean reinstall from the v2.1.7 ZIP.

---

## v2.1.6

- Added `ytdlCookiesFromBrowser` config (admin dashboard > Playback > Maintenance). When set (e.g. `brave`, `chrome:Profile 1`), yt-dlp passes the logged-in browser session via `--cookies-from-browser`. Resolves YouTube's "Sign in to confirm you're not a bot" challenge that blocks playback on flagged IPs.
- Freeze recovery: premature stream-EOF and network errors (HTTP 5xx, TLS) now trigger an in-place retry at the last known playback position via `jumpTo(currentIndex)`. yt-dlp re-signs the googlevideo URL and playback resumes near the break. Up to 2 retries per video before falling through to the existing skip path. `keep-open=yes` preserved so viewers see the last frame during the ~2-5s retry, not a black cut.

---

## v2.1.5

### Bug Fixes

- **Infinite recovery loop when saved resume position exceeds the target video's duration.** If a long-playing video (e.g. a 6-hour VOD) left a stale `currentTime` in `state.json` and recovery then landed on a shorter video, every resume attempt applied an impossible `--start=+<too-large>` seek and failed immediately, triggering another restart — forever. Resume time is now discarded when it exceeds the last-known `videoDuration`, and mpv's `start` property is cleared immediately on seek-failure so a bad seek can't propagate to auto-advanced videos instead of waiting for the 30s cleanup timer.
- **Non-playing recovery fired during legitimate broken-video skipping.** When a run of broken/unavailable videos appeared in the playlist, mpv would fire `end-file` errors every ~4-5s and try to skip past them — but the 30s non-playing threshold tripped first, restarting mpv and throwing away all skip progress back to position 0. The `nonPlayingHeartbeats` counter now resets on each `end-file` error (mpv is clearly not stuck when it's actively erroring), letting the skip mechanism do its job. Heartbeat-timeout and stall detection are unchanged.

### Improvements

- **mpv diagnostic logs.** Each mpv spawn now writes a dedicated `logs/mpv-<timestamp>.log` with `--msg-level=ytdl_hook=v`, capturing the actual yt-dlp extractor output (HTTP status codes, signature errors, network failures) that was previously invisible. The last 10 spawn logs are retained. New "mpv Diagnostic Logs" section on the dashboard Overview tab lets you pick any past spawn and view its contents — the next recovery loop self-documents instead of leaving you guessing.

---

## v2.1.4

### Bug Fixes

- **Video freeze recovery loops forever instead of escalating.** When video froze but audio kept playing, recovery started `retryCurrent` but never escalated to `restartMpv` because advancing `time-pos` (from audio) kept cancelling the recovery. Now freeze-triggered recovery properly escalates through the full sequence.
- **Resume position lost after recovery restart.** During recovery, heartbeat polls could overwrite the saved `videoIndex` with a transient `playlistPos=0` from mpv mid-reload. State writes are now suppressed during active recovery.
- **`mpv.restart()` didn't wait for process exit.** The `stop()` call inside `restart()` was not awaited, so the old mpv process might not have fully exited before spawning a new one.

### Improvements

- **Much faster update extraction.** Switched from PowerShell `Expand-Archive` to Windows built-in `tar.exe` (bsdtar) for both the updater and build script. Extraction that took 30+ minutes now completes in seconds.

---

## v2.1.3

### Bug Fixes

- **Update swap fails with "Access is denied".** `mpv.stop()` now waits for the process to fully exit (with 5s SIGKILL fallback) instead of fire-and-forget. START.bat also retries the app directory rename up to 5 times (with 3s delays) as a safety net.

---

## v2.1.2

### New Features

- **"This is not live!" overlay badge.** Red banner in the top-right corner of the OBS overlay so viewers know the stream is pre-recorded.

---

## v2.1.1

### Bug Fixes

- **Fixed triple event handling after config reload.** Recovery engine event listeners accumulated on the mpv client each time the config was reloaded, causing 3x duplicate log messages and 3x competing playlist loads on every mpv restart. Listeners are now properly removed on stop.
- **Video freeze detection.** When mpv's video output froze but audio continued playing, recovery never triggered because `time-pos` was still advancing. Now monitors `estimated-vf-fps` and triggers recovery when video framerate drops to zero for 4 consecutive heartbeats.
- **Resume lands on wrong video after restart.** The mpv `start` property (seek position) was set before loading the playlist even when the target video wasn't index 0, causing video 0 to attempt an impossible seek and error out before the jump to the correct video could happen. Now only pre-sets seek for video 0; for other indices, seek is set right before the jump.
- **IPC command timeout.** mpv IPC commands now time out after 5 seconds instead of hanging forever when mpv is unresponsive, allowing heartbeat timeout detection to work correctly.

### Improvements

- **Now-playing overlay readability.** Darkened overlay background from white 30% to black 70% opacity.

---

## v2.1.0

### New Features

- **Stream starts only after video is visible.** OBS auto-stream now waits until mpv is confirmed rendering video frames (not just connected), preventing viewers from seeing a black screen on startup or recovery.
- **Restart mpv button.** One-click restart in the dashboard if the mpv window disappears or gets stuck.

### Bug Fixes

- **Resume position after restart.** Uses mpv's `start` property to request the correct byte range from YouTube, instead of post-load seeking which YouTube rejected.
- **Seek failure recovery.** If resuming at a saved position fails, replays from the beginning instead of entering an error loop.
- **mpv window visibility.** Reverted detached spawn, added `--force-window=yes`, switched to `--hwdec=auto` for broader hardware compatibility.
- **Overlay opacity** increased to 0.30 for better readability.

---

## v2.0.5

### Bug Fixes

- **mpv window visible again.** Reverted detached spawn that caused mpv to open without a window on some systems. Uses `--force-window=yes` and `--hwdec=auto` for broader hardware compatibility.

---

## v2.0.4

### Bug Fixes

- **mpv window now opens visible.** Spawns mpv as a detached process so it gets its own window instead of inheriting the parent's hidden state.
- **Test files excluded from release ZIP.** Reduces download size.

---

## v2.0.3

### Bug Fixes

- **Auto-updater extraction timeout increased to 10 minutes.** The larger v2.0.x ZIP (with bundled mpv) was timing out during extraction on low-RAM systems.

---

## v2.0.2

### Bug Fixes

- **Resumes at correct timestamp after recovery.** Fixed race condition where playlist loaded multiple times after mpv restart, preventing the seek to saved position. Now uses event-based waiting instead of fragile timeouts.

- **Play button in video list works.** (Also in v2.0.1)

---

## v2.0.1

### Bug Fixes

- **Play button in video list now works.** Fixed missing Content-Type header on POST requests from dashboard playback controls.

---

## v2.0.0

### Breaking Changes

- **mpv replaces the OBS Browser Source player.** StreamLoop now uses mpv (bundled) to play YouTube playlists, controlled via Windows named pipe IPC. This eliminates the Chromium memory leak that caused playback stalls on low-RAM systems. **You must set up a Window Capture source in OBS instead of a Browser Source.** Download the full v2.0.0 ZIP — the auto-updater from v1.x cannot upgrade to this version.

- **Now Playing overlay requires a separate Browser Source.** Add a Browser Source in OBS pointing to `http://localhost:7654/overlay` and layer it above the Window Capture.

### New Features

- **Playlist selector and playback controls.** The dashboard Monitor tab now has a playlist dropdown, transport controls (previous/pause/next/stop), seek bar, and a paginated video list. Click any video to play it.

- **Stop button.** Intentionally pause playback without the server auto-resuming. The stop button prevents recovery from restarting the video.

- **Shutdown button.** Cleanly stop StreamLoop (mpv, server, all processes) from the Settings tab.

- **System memory in Discord webhooks.** Recovery and critical alerts include RAM usage.

- **Zombie OBS process detection.** If OBS is running but unresponsive to WebSocket after 5 attempts, it's force-killed and relaunched.

- **yt-dlp update from dashboard.** Update yt-dlp to the latest version from the Settings tab to keep up with YouTube changes.

### Migration from v1.x

1. Download the full v2.0.0 ZIP and extract to a new folder
2. Copy `config.json` and `state.json` from your old installation
3. In OBS: remove the old Browser Source, add a **Window Capture** (select mpv window) and a **Browser Source** for the overlay (`http://localhost:7654/overlay`)
4. Run `START.bat`

---

## v1.4.3

### Bug Fixes

- **Fixed resume position after source refresh.** The YouTube player now receives `startSeconds` in the `loadPlaylist()` call, so playback starts at the correct position immediately. Previously, the player briefly started at 0:00 before seeking, which could cause state.json to save an incorrect position if a heartbeat fired during that window.

### Improvements

- **Proactive source refresh enabled by default (every 30 minutes).** Periodically reloads the OBS browser source to prevent Chromium memory buildup during long sessions. Particularly important on systems with 8-16GB RAM where the browser source can grow to 1-2GB over time. Set `sourceRefreshIntervalMs` to `0` to disable.

- **System memory logged on stalls, timeouts, and recovery events.** RAM usage (used/total GB and percentage) is now included in log messages and Discord webhook notifications when recovery triggers, making it easier to diagnose memory-pressure-related playback issues.

- **Dashboard status includes system memory.** The `/api/status` endpoint now returns `systemMemory` with current RAM usage.

---

## v1.4.2

### Bug Fixes

- **Quality recovery now properly escalates.** Previously, quality recovery could only ever attempt a browser source refresh — escalation to toggle visibility and critical alert was silently cancelled because the escalation check didn't account for quality-triggered recovery. A new `recoveryReason` field tracks why recovery started, allowing correct escalation decisions.

- **Quality recovery no longer cancelled by normal playback.** When quality was low but the video was still playing and advancing, the heartbeat progress detection would cancel the in-progress quality recovery. Now playback progress correctly skips the reset when the recovery reason is quality.

- **Error retry timer no longer leaks on config reload or shutdown.** The delayed `retryCurrent` after a playback error was an untracked `setTimeout` that could fire after `stop()`, sending a ghost message on a stale WebSocket. Now tracked and cleared properly.

- **Detection counters reset on playlist transition.** Stall, quality, non-playing, and paused heartbeat counters now reset when advancing to the next playlist, preventing stale counts from triggering false recovery on the new playlist.

### Improvements

- **Quality threshold uses configured heartbeat interval.** The low-quality detection threshold now derives from `heartbeatIntervalMs` in config instead of a hardcoded 5000ms divisor, so custom heartbeat intervals calculate correctly.

- **Gradual quality counter decay.** When quality improves above the minimum, the low-quality heartbeat counter now decrements by 1 instead of resetting to 0. This prevents quality oscillating at the threshold boundary from indefinitely delaying recovery.

- **Unknown YouTube quality strings are logged.** If the YouTube API sends an unrecognized quality string, a warning is logged instead of silently ignoring it.

- **Discord quality notifications include details.** Quality recovery Discord messages now include the actual quality, minimum expected quality, and video index (e.g. "Low quality recovery: medium (minimum: hd720) on video #3").

- **More accurate recovery cancellation log message.** Changed from "Heartbeat restored" to "Recovery condition resolved" with the recovery reason included in structured log output.

---

## v1.4.1

### Bug Fixes

- **Quality recovery no longer restarts the video from the beginning.** Previously, recovering from sustained low quality would trigger `RetryCurrent`, which restarted the video at 0:00. Now it skips straight to refreshing the OBS browser source, which reloads the player and resumes at the saved position.

- **WebSocket reconnection no longer triggers false recovery.** When the OBS browser source reconnected, the old WebSocket was left open. When it eventually closed, it would fire a disconnect signal and trigger unnecessary recovery steps even though the new connection was healthy. The old connection is now explicitly closed, and only the active client's disconnect fires the callback.

### Improvements

- **Discord rate-limit handling.** The Discord notifier now respects HTTP 429 responses and waits for the `retry-after` duration before continuing, preventing dropped notifications during bursts of recovery events.

- **Safer update extraction.** The auto-updater now uses `execFileSync` instead of `execSync` for the PowerShell extraction step, avoiding potential issues with special characters in file paths.

- **Resilient state loading.** Missing or corrupt fields in `state.json` now fall back to safe defaults instead of propagating bad values at runtime.

- **Reduced log noise.** Config rewrite-on-load messages (triggered by key reordering, not actual changes) downgraded from `info` to `debug`.

- **Fixed TypeScript errors in test mocks.** Added missing `twitchMismatch` and `twitchRestart` fields to test config mocks in `recovery.test.ts` and `discord.test.ts`.
