# Freeze Monitor

YouTube Playlist Stream Monitor for OBS. Plays YouTube playlists in an OBS Browser Source and watches for playback freezes, errors, and failures. When something goes wrong, it automatically recovers through an escalating sequence of actions. Optionally sends alerts to Discord webhooks.

## Features

- Automatic playback recovery with escalating steps (retry, refresh source, toggle visibility)
- Persistent playback state across restarts
- Admin dashboard for live monitoring and configuration
- Discord webhook alerts for critical failures
- Windows autostart support for unattended operation

## Setup

```bash
npm install
cp config.example.json config.json
```

Edit `config.json` with your YouTube playlist ID, OBS Browser Source name, and optional OBS/Discord settings.

## Usage

```bash
npm start       # Start the server
npm run dev     # Start with file watching
```

### OBS Configuration

1. Create a Browser Source in OBS (e.g. named "Playlist Player")
2. Set its URL to `http://localhost:3000`
3. Enable OBS WebSocket Server: Tools > WebSocket Server Settings > Enable

### Admin Dashboard

Open `http://localhost:3000/admin` to monitor player status, view recovery events, and edit settings.

## Configuration

| Field | Default | Description |
|-------|---------|-------------|
| `playlists` | — | Array of `{ id, name? }` YouTube playlist objects |
| `obsBrowserSourceName` | — | Name of the OBS Browser Source to control |
| `port` | `3000` | HTTP server port |
| `obsWebsocketUrl` | `ws://127.0.0.1:4455` | OBS WebSocket server URL |
| `obsWebsocketPassword` | `""` | OBS WebSocket password |
| `discordWebhookUrl` | `""` | Discord webhook for alerts |
| `heartbeatIntervalMs` | `5000` | Player heartbeat interval |
| `heartbeatTimeoutMs` | `15000` | Time before declaring a freeze |
| `maxConsecutiveErrors` | `3` | Errors before skipping a video |
| `recoveryDelayMs` | `5000` | Delay between recovery steps |

## Windows Release

```bash
npm run build:release
```

Packages the app with portable Node.js into a standalone ZIP that requires no installation.

## Testing

```bash
npm test             # Run once
npm run test:watch   # Watch mode
```
