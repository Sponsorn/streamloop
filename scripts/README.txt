StreamLoop - 24/7 YouTube Streamer for OBS
===========================================

Quick Start
-----------
1. Extract this ZIP to any folder
2. Double-click START.bat
3. Your browser opens to the setup wizard
4. Enter your YouTube Playlist ID and OBS Browser Source name
5. Click "Save & Start" — monitoring begins immediately

That's it! No need to install Node.js or anything else.

What This Does
--------------
StreamLoop plays a YouTube playlist in an OBS Browser Source and
watches for playback freezes, errors, and failures. When something goes
wrong, it automatically recovers by retrying, refreshing, or toggling
the source in OBS. Optionally sends alerts to a Discord webhook.

OBS Setup
---------
1. In OBS, create a Browser Source (e.g. named "Playlist Player")
2. Set its URL to:  http://localhost:7654
3. Set width/height to match your stream layout
4. Make sure OBS WebSocket Server is enabled:
   Tools > WebSocket Server Settings > Enable WebSocket Server

Dashboard
---------
Open http://localhost:7654/admin in any browser to:
- See live player status, OBS connection, recovery state
- View the event log of errors and recovery actions
- Edit settings (playlist, OBS source name, Discord webhook)
- Enable "Run on Windows startup" for unattended operation

Folder Structure
----------------
  node/         Portable Node.js runtime (do not modify)
  app/          Application code and dependencies
  START.bat     Launch script — double-click to run
  README.txt    This file

Playlist Limits
---------------
YouTube's IFrame API only loads the first 200 videos from a playlist,
regardless of how many it actually contains. If your playlist has more
than 200 videos, split it into multiple playlists of 200 or fewer and
add them all in the settings. StreamLoop plays through each playlist
in order and loops back to the first one automatically.

Troubleshooting
---------------
- "Port 7654 already in use": Another instance may be running, or
  another app is using port 7654. Close it and try again.
- OBS not connecting: Make sure OBS is running and WebSocket Server
  is enabled in Tools > WebSocket Server Settings.
- Player not loading: Check that the Browser Source URL is set to
  http://localhost:7654 and the source name matches your config.

For more help, visit the project repository.
