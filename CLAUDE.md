# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## Project Overview

Freeze Monitor is a Node.js/TypeScript application that plays YouTube playlists in an OBS Browser Source and automatically recovers from playback freezes, errors, and failures. It communicates with a browser-based player via WebSocket, connects to OBS via obs-websocket-js, and optionally sends alerts to Discord webhooks.

## Setup

```bash
npm install
cp config.example.json config.json   # Then edit config.json with your settings
```

`config.json` is gitignored to prevent committing credentials. `config.example.json` is the tracked template.

## Commands

```bash
npm start            # Run server (tsx)
npm run dev          # Run with file watching (tsx watch)
npm test             # Run tests once (vitest run)
npm run test:watch   # Run tests in watch mode (vitest)
npm run build:release  # Package Windows distributable ZIP with portable Node.js
```

No linter is configured. TypeScript strict mode is enabled via `tsconfig.json`.

## Architecture

**Client-server model with three browser clients:**

- **Server** (`src/server/`): Express.js app that orchestrates everything. Entry point is `index.ts`, which wires components together via dependency injection.
- **Player** (`src/player/player.js`): Loaded as an OBS Browser Source. Embeds YouTube IFrame API, sends heartbeats and state changes to the server over WebSocket (`/ws`).
- **Admin Dashboard** (`src/admin/admin.js`): Browser UI for monitoring status, viewing recovery events, and editing config via REST API.

**Key server modules:**

| Module | Responsibility |
|--------|---------------|
| `config.ts` | Loads/saves `config.json` with Zod schema validation and defaults |
| `state.ts` | Persists playback position (`state.json`) with debounced 2s writes |
| `recovery.ts` | Core heartbeat monitor with escalating recovery: RetryCurrent → RefreshSource → ToggleVisibility → CriticalAlert |
| `obs-client.ts` | OBS WebSocket client for source manipulation (settings, visibility) |
| `websocket.ts` | WebSocket server for player communication |
| `discord.ts` | Discord webhook notifications |
| `api.ts` | REST endpoints (`/api/status`, `/api/state`, `/api/config`, `/api/events`, `/api/autostart`) |
| `logger.ts` | Pino structured logging |

**Recovery escalation sequence:** When heartbeats timeout, the recovery engine escalates through steps: retry current video → refresh OBS browser source URL (cache bust) → toggle OBS source visibility → send critical Discord alert, wait 60s, then restart the sequence.

**Configuration hot-reload:** POST to `/api/config` updates `config.json` and triggers component reloading (OBS reconnect, recovery engine restart) without server restart.

## WebSocket Protocol (Player ↔ Server)

Player sends: `ready`, `heartbeat` (with video position), `error` (with error code), `stateChange`, `playlistLoaded`

Server sends: `loadPlaylist` (with playlist ID and resume index), `retryCurrent`, `skip`

YouTube error codes 100/101/150 (unavailable/not embeddable) trigger automatic skip. Other errors retry up to `maxConsecutiveErrors` before skipping.

## Testing

Tests live in `src/server/__tests__/` using Vitest. Tests use a `.test-tmp` directory for file fixtures. All server modules use dependency injection for mock-friendly testing.

## Platform Notes

Windows-specific: The `/api/autostart` endpoint manages a VBS script in the Windows Startup folder. The release build (`build/prepare-release.js`) bundles portable Node.js v22.12.0 win-x64 into a standalone ZIP.

ES modules throughout (`"type": "module"` in package.json, `"module": "ES2022"` in tsconfig).
