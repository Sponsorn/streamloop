StreamLoop 3 - 24/7 YouTube playlist to Twitch
================================================

StreamLoop 3 plays a YouTube playlist and streams it to Twitch on its own.
It needs no OBS and no mpv: ffmpeg encodes the stream and a small
supervisor program keeps it running, skipping or retrying videos that
fail. It can send alerts to a Discord webhook.

It installs beside StreamLoop 2.x and does not touch a 2.x folder.

Install
-------
1. Extract this ZIP to a new folder, for example C:\StreamLoop3.
   Do not extract it over a 2.x folder.
2. Close OBS. StreamLoop 3 will not start its stream while OBS is
   running, so 2.x and 3 never stream on the same key at once.
3. Stop StreamLoop 2.x if it is running. Both use port 7654, so only one
   of them can run at a time.
4. Double-click START.bat.

Node.js, ffmpeg and yt-dlp are all in the folder. Nothing else needs
installing.

First run
---------
The dashboard opens in your browser at http://localhost:7654/admin and
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

Keep the START.bat window open. Closing it stops the dashboard, but the
stream keeps running. Start START.bat again to get the dashboard back.

Dashboard
---------
http://localhost:7654/admin shows what is playing and the stream state,
and has Stop and Start, skip and jump controls, the event timeline,
logs and settings. Stop holds until you press Start, even across
restarts of the PC.

Updates
-------
The dashboard offers new 3.x versions. An update replaces only the app\
folder and keeps your settings and stream key; the stream stays up while
it installs.

yt-dlp updates itself each time StreamLoop starts. Some releases also
change Node.js, the supervisor, ffmpeg or the token server. The release
notes say so, and the dashboard tells you when a release brings a new
Node.js. For those, press Stop, close the START.bat window, extract the
new full ZIP over this folder and start START.bat again. Your settings
stay, because the ZIP holds only config.example.json and never a
config.json.

Switching back to 2.x
---------------------
1. In the 3 dashboard, press Stop and wait until it shows Stopped.
2. Close the StreamLoop 3 window.
3. Start 2.x from its own folder with its START.bat, and start OBS.

2.x will not start its OBS stream while StreamLoop 3 is still streaming.
To return to 3, close 2.x and OBS, start START.bat here and press Start:
the stream stays stopped until you do.

Autostart can point at one install only. Turning it on in one install
moves it there from the other.

Folders
-------
  START.bat     Starts StreamLoop. Double-click to run.
  app\          The dashboard and server, with config.json (your settings)
  supervisor\   The program that runs the stream
  ffmpeg\       The encoder
  yt-dlp\       Fetches the videos from YouTube
  pot\          Token server that yt-dlp needs for YouTube
  node\         Node.js
  data\         Logs, the download cache and the resume point

Troubleshooting
---------------
- "Port 7654 is in use": StreamLoop 2.x, or another copy of 3, is
  running. Stop it, and 3 starts by itself.
- "OBS is running": close OBS. The stream then starts by itself.
- The stream key is rejected: set it again under Settings in the
  dashboard.
- The logs are under Logs in the dashboard, and in data\logs and
  app\logs.
