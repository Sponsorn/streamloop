Freeze Monitor - YouTube Playlist Stream Monitor for OBS
=========================================================

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
Freeze Monitor plays a YouTube playlist in an OBS Browser Source and
watches for playback freezes, errors, and failures. When something goes
wrong, it automatically recovers by retrying, refreshing, or toggling
the source in OBS. Optionally sends alerts to a Discord webhook.

OBS Setup
---------
1. In OBS, create a Browser Source (e.g. named "Playlist Player")
2. Set its URL to:  http://localhost:3000
3. Set width/height to match your stream layout
4. Make sure OBS WebSocket Server is enabled:
   Tools > WebSocket Server Settings > Enable WebSocket Server

Dashboard
---------
Open http://localhost:3000/admin in any browser to:
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

Troubleshooting
---------------
- "Port 3000 already in use": Another instance may be running, or
  another app is using port 3000. Close it and try again.
- OBS not connecting: Make sure OBS is running and WebSocket Server
  is enabled in Tools > WebSocket Server Settings.
- Player not loading: Check that the Browser Source URL is set to
  http://localhost:3000 and the source name matches your config.

For more help, visit the project repository.
