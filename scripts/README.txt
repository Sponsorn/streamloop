StreamLoop 3 - 24/7 YouTube playlist to Twitch
================================================

StreamLoop 3 plays a YouTube playlist and streams it to Twitch on its own.
It needs no OBS and no mpv: ffmpeg encodes the stream and streamloop.exe
keeps it running, skipping or retrying videos that fail. It can send
alerts to a Discord webhook.

It installs beside StreamLoop 2.x and does not touch a 2.x folder.

Install
-------
1. Make a new, empty folder, for example C:\StreamLoop3, and extract
   this ZIP into it. The ZIP holds one folder, streamloop, so the
   install is C:\StreamLoop3\streamloop. Do not extract it into a 2.x
   folder: the 2.x ZIP has a streamloop folder too.
2. Close OBS. StreamLoop 3 will not start its stream while OBS is
   running, so 2.x and 3 never stream on the same key at once.
3. Stop StreamLoop 2.x if it is running. Both use port 7654, so only one
   of them can run at a time.
4. Double-click streamloop.exe in C:\StreamLoop3\streamloop.

ffmpeg, yt-dlp and the token server are all in the folder. Nothing
else needs installing.

Coming from StreamLoop 3.0? Install the same way you install an update
that needs the full ZIP (see Updates): your settings, stream key and
timeline are kept. The old pot\ and node\ folders are removed at the
next start, since the token server is now a single exe in supervisor\.

First run
---------
The StreamLoop window opens, starts StreamLoop in the background and
walks you through setup:

1. Playlist: the YouTube playlist ID to play.
2. Stream key: your Twitch stream key. It is stored encrypted for your
   Windows user and never shown again.
3. Discord (optional): a webhook URL for alerts, and whether StreamLoop
   starts with Windows.
4. System check: finds the hardware encoder that works on this PC
   (NVIDIA, AMD or Intel, with software encoding as the last resort) and
   suggests a download cache size for the free disk space.
5. Finish: saves everything and waits until the stream is live.

The window and the tray icon
----------------------------
The window shows what is playing and the stream state, and has Stop and
Start, skip and jump controls, the event timeline, logs and settings.
Stop holds until you press Start, even across restarts of the PC.

Closing the window does not stop the stream. StreamLoop keeps running
in the notification area of the taskbar, and the colour of its tray
icon shows the stream:

  green   live
  amber   starting, recovering or installing an update
  red     off air, StreamLoop no longer answers, or it did not start
          within 30 seconds
  grey    stopped, or StreamLoop has not answered yet

Point at the icon for the state and the reason. Click it, or
double-click streamloop.exe again, to open the window.

To stop the stream and close StreamLoop, right-click the tray icon and
choose Quit StreamLoop, or press Quit StreamLoop under Settings. To go
back on air, double-click streamloop.exe. A stream you stopped with Stop
before you quit stays stopped until you press Start.

Starting with Windows
---------------------
Under Settings, Startup, turn on "Run on Windows startup". At every
logon StreamLoop then starts in the tray, without its window, and starts
the stream, unless "Start the stream when StreamLoop starts" is off. It
shows as StreamLoop in Task Manager's Startup tab.

Autostart can point at one install only. Turning it on in one install
moves it there from the other.

Updates
-------
The window offers new 3.x versions. An update replaces streamloop.exe
and keeps your settings and stream key. Update Now installs it at once.
With automatic update checks on (autoUpdateCheck in app\config.json, on
by default), it also downloads in the background and installs by itself:
just before Twitch ends the broadcast at its 48-hour limit, about 5
minutes after the stream goes off air, or right after you press Stop.
With them off, only Update Now installs it. An update that installs
while the stream is on air restarts it, and the stream is off air for
about 10 seconds. An open window restarts into the new version.

If the new version does not start properly, StreamLoop goes back to the
previous one by itself, and installs that version again only when you
press Update Now.

yt-dlp updates itself each time StreamLoop starts, and once an hour
while YouTube lookups keep failing. Some releases also
change ffmpeg or the token server, which an update does not replace.
Their release notes say they need the full ZIP. For those:

1. Press Stop, then Quit StreamLoop from the tray icon.
2. Extract the new full ZIP into the same folder as the first time
   (C:\StreamLoop3 in the example above, not C:\StreamLoop3\streamloop).
   Its streamloop folder lands on the existing one; let Windows replace
   the files.
3. Double-click streamloop.exe again and press Start.

Your settings, the stream key and data\ are kept, because the ZIP holds
only config.example.json and never a config.json.

Switching back to 2.x
---------------------
1. In the StreamLoop 3 window, press Stop and wait until it shows
   Stopped.
2. Quit StreamLoop from the tray icon.
3. Start 2.x from its own folder with its START.bat, and start OBS.

2.x will not start its OBS stream while StreamLoop 3 is still streaming.
To return to 3, close 2.x and OBS, double-click streamloop.exe here and
press Start: the stream stays stopped until you do.

Folders
-------
  streamloop.exe  StreamLoop itself: the window, the tray icon and the
                  stream. Double-click to start.
  START.bat       Runs StreamLoop and restarts it after a crash.
                  streamloop.exe starts it hidden; double-click it to
                  see why StreamLoop does not start.
  app\            Your settings (config.json), StreamLoop's log and timeline
  supervisor\     The standby picture shown while no video is ready, and
                  the token server that yt-dlp needs for YouTube
  ffmpeg\         The encoder
  yt-dlp\         Fetches the videos from YouTube
  data\           Logs, the download cache and the resume point

Troubleshooting
---------------
- The window says "StreamLoop did not start": double-click START.bat in
  the install folder. Its window shows why, for example a file that is
  missing. When nothing is wrong, StreamLoop then runs in that window.
- START.bat's window says "Another StreamLoop is already running":
  StreamLoop 2.x, or another copy of 3, holds port 7654, and this one
  stays down. Close the other one, then start this one again.
- "OBS is running": close OBS. The stream then starts by itself.
- The stream key is rejected: set it again under Settings.
- START.bat's window shows a [RECOVER] line: an update did not start
  properly, and StreamLoop went back to the version before it.
- START.bat's window shows a [WARN] line about an update: the new
  version stopped, and Windows did not let StreamLoop put the previous
  one back yet, for example while antivirus was scanning it. StreamLoop
  tries again by itself.
- The logs are under Logs on the Monitor tab, and in data\logs and
  app\logs.
