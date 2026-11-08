/**
 * Records golden fixtures from the TypeScript backend (src/server/) for the Rust port in crates/.
 * Every later Rust test that says "against the fixture" iterates one of the files written here.
 *
 *   npx tsx scripts/r1-fixtures.ts
 *
 * Output is deterministic (fixed clocks, stable key order, 2-space JSON), so a second run changes no file.
 * Shape per case: { name, input, output }. Config cases are one file each:
 * { name, before, example?, patch?, after, error? }.
 * Removed in R1 Task 24, once the Rust tests carry the fixtures.
 */
import { existsSync, mkdirSync, readFileSync, rmSync, writeFileSync } from 'fs';
import { dirname, join, resolve } from 'path';
import { fileURLToPath } from 'url';
import { loadConfig, saveConfig, DEFAULT_DISCORD_TEMPLATES } from '../src/server/config.js';
import { describe as describeEvent, AlertGate, withPlaylistNames, launcherEvent, type SupervisorEvent } from '../src/server/supervisor-events.js';
import { streamState } from '../src/server/api.js';
import { DiscordNotifier } from '../src/server/discord.js';
import { compareVersions, parseNodeVersion, pickRelease, selectReleaseAssets, type GithubRelease } from '../src/server/updater.js';
import { isAdPlaylist, liveRendition, meanAbsDiff, timerRegion, grabGrayRegion, type GrabRun } from '../src/server/twitch-readback.js';
import { recommendCacheMb } from '../src/server/setup-check.js';
// @ts-ignore plain JS module without a declaration file
import { formatLogLine } from '../src/server/file-log-transport.js';
import type { AppConfig, DiscordConfig } from '../src/server/types.js';
import type { SupervisorStatus } from '../src/server/supervisor-client.js';
import type { LauncherState } from '../src/server/supervisor-launcher.js';

const root = resolve(dirname(fileURLToPath(import.meta.url)), '..');
const CONFIG_DIR = join(root, 'crates', 'common', 'tests', 'fixtures', 'config');
const CONTROL_DIR = join(root, 'crates', 'control', 'tests', 'fixtures');
const TMP = join(root, '.test-tmp', 'r1-fixtures');

interface Case { name: string; input: unknown; output: unknown }

function writeJson(path: string, value: unknown): void {
  mkdirSync(dirname(path), { recursive: true });
  writeFileSync(path, JSON.stringify(value, null, 2) + '\n', 'utf-8');
}

const counts: Record<string, number> = {};
function writeCases(file: string, cases: Case[]): void {
  const names = new Set<string>();
  for (const c of cases) {
    if (names.has(c.name)) throw new Error(`duplicate case name in ${file}: ${c.name}`);
    names.add(c.name);
  }
  writeJson(join(CONTROL_DIR, file), cases);
  counts[file] = cases.length;
}

// ---------------------------------------------------------------- config

interface ConfigCase {
  name: string;
  /** config.json as it is on disk before the call; null for no file. */
  before: string | null;
  /** config.example.json beside it, when the case needs one (loadConfig copies it on a fresh install). */
  example?: string;
  /** Present: saveConfig(patch). Absent: loadConfig. */
  patch?: Record<string, unknown>;
  /** config.json after the call, or null when the call left the file as it was (or threw). */
  after: string | null;
  error?: string;
}

const pretty = (v: unknown): string => JSON.stringify(v, null, 2);
const slug = (s: string): string => s.toLowerCase().replace(/[^a-z0-9]+/g, '-').replace(/^-|-$/g, '');

function runConfigCase(c: Omit<ConfigCase, 'after' | 'error'>): ConfigCase {
  const dir = join(TMP, 'config', slug(c.name));
  rmSync(dir, { recursive: true, force: true });
  mkdirSync(dir, { recursive: true });
  const path = join(dir, 'config.json');
  if (c.before !== null) writeFileSync(path, c.before, 'utf-8');
  if (c.example !== undefined) writeFileSync(join(dir, 'config.example.json'), c.example, 'utf-8');
  const out: ConfigCase = {
    name: c.name,
    before: c.before,
    ...(c.example !== undefined ? { example: c.example } : {}),
    ...(c.patch ? { patch: c.patch } : {}),
    after: null,
  };
  try {
    if (c.patch) saveConfig(structuredClone(c.patch) as Partial<AppConfig>, path);
    else loadConfig(path);
  } catch (err) {
    // The temp folder differs per machine, so it never reaches a fixture.
    out.error = (err as Error).message.split(dir).join('<dir>').split(dir.replace(/\\/g, '/')).join('<dir>');
  }
  const text = existsSync(path) ? readFileSync(path, 'utf-8') : null;
  out.after = text !== null && text !== c.before ? text : null;
  rmSync(dir, { recursive: true, force: true });
  return out;
}

function recordConfig(): number {
  rmSync(CONFIG_DIR, { recursive: true, force: true });
  mkdirSync(CONFIG_DIR, { recursive: true });
  const base = { playlists: [{ id: 'PLtest123' }] };
  const example = JSON.parse(readFileSync(join(root, 'config.example.json'), 'utf-8'));
  const exampleText = pretty(example);
  // Typed out as 3.0.x wrote it (mirrors config.test.ts): key order counts, a reordered file is rewritten on every start.
  const written = {
    port: 7700,
    playlists: [{ id: 'PLone', name: 'One' }, { id: 'PLtwo' }],
    discord: {
      webhookUrl: 'https://discord.example/hook', botName: 'Bot', avatarUrl: '', rolePing: '123',
      events: { error: true, skip: false, recovery: true, critical: true, resume: true, streamDrop: true, streamRestart: true, outputFrozen: true, twitchMismatch: false, twitchRestart: true },
      templates: {
        error: 'custom {errorCode}', skip: 'custom {reason}', recovery: 'custom {step}', critical: 'custom {message}',
        resume: 'custom {videoId}', streamDrop: 'custom {attempt}', streamRestart: 'custom {attempts}',
        outputFrozen: 'custom {kind}', twitchMismatch: 'custom {channel}', twitchRestart: 'custom {channel}',
      },
    },
    supervisor: {
      ingestUrl: 'rtmp://ingest.example/app', streamKeyProtected: 'AQAAANCMnd8BFdERjHoAwE_fake_blob', encoder: 'h264_amf',
      preferredEncoder: 'h264_nvenc', cacheBudgetMb: 8192, readAheadSeconds: 12.5, playerClient: 'tv', overlay: false,
      freezeSeconds: 25, silenceSeconds: 45, futureKnob: 7,
    },
    autoUpdateCheck: false, updateCheckIntervalMs: 3600000, autoUpdateYtdlp: false, autoOpenAdmin: false,
    twitchClientId: 'client-id', twitchClientSecret: 'client-secret', twitchChannel: 'channel', twitchLivenessEnabled: true,
    twitchPollIntervalMs: 30000, ytdlCookiesFromBrowser: 'firefox',
  };
  const j = JSON.stringify;
  const withSup = (sup: Record<string, unknown>) => j({ ...base, supervisor: sup });

  const cases: Omit<ConfigCase, 'after' | 'error'>[] = [
    // fresh install and existing files
    { name: 'creates config.json from the example next to it when it is missing', before: null, example: j({ playlists: [{ id: 'PLxxxxxxxxxxxxxxxx' }] }) },
    { name: 'creates config.json from the full example', before: null, example: exampleText },
    { name: 'never replaces an existing config.json with the example', before: j({ playlists: [{ id: 'PLmine' }], supervisor: { streamKeyProtected: 'BLOB' } }), example: j({ playlists: [{ id: 'PLxxxxxxxxxxxxxxxx' }] }) },
    { name: 'throws when config.json is missing and there is no example', before: null },
    { name: 'throws on a file that is not JSON', before: '{ not json' },
    // basic loads
    { name: 'loads valid config with playlists array and port', before: j({ playlists: [{ id: 'PLtest123' }], port: 4000 }) },
    { name: 'loads config with multiple playlists and names', before: j({ playlists: [{ id: 'PL1', name: 'First' }, { id: 'PL2' }] }) },
    { name: 'migrates old playlistId format to playlists array', before: j({ playlistId: 'PLlegacy123' }) },
    { name: 'keeps playlists and drops playlistId when both are present', before: j({ playlistId: 'PLold', playlists: [{ id: 'PLnew' }] }) },
    { name: 'applies defaults for optional fields', before: j(base) },
    { name: 'throws on missing required fields', before: j({ port: 7654 }) },
    { name: 'throws on empty playlists array', before: j({ playlists: [] }) },
    { name: 'throws on a playlist with an empty id', before: j({ playlists: [{ id: '' }] }) },
    { name: 'throws on a playlist name that is not text', before: j({ playlists: [{ id: 'PL1', name: 5 }] }) },
    { name: 'throws on port zero', before: j({ ...base, port: 0 }) },
    { name: 'throws on a fractional port', before: j({ ...base, port: 7654.5 }) },
    { name: 'throws on updateCheckIntervalMs zero', before: j({ ...base, updateCheckIntervalMs: 0 }) },
    { name: 'throws on twitchPollIntervalMs below 30000', before: j({ ...base, twitchPollIntervalMs: 29999 }) },
    { name: 'accepts twitchPollIntervalMs of exactly 30000', before: j({ ...base, twitchPollIntervalMs: 30000 }) },
    { name: 'opens the admin panel on boot by default', before: j(base) },
    { name: 'keeps autoOpenAdmin false', before: j({ ...base, autoOpenAdmin: false }) },
    { name: 'preserves ytdlCookiesFromBrowser when set', before: j({ ...base, ytdlCookiesFromBrowser: 'brave:Profile 1' }) },
    { name: 'drops unknown top-level keys on write-back', before: j({ ...base, somethingNew: 1, another: { nested: true } }) },
    { name: 'rewrites a file whose keys are in another order', before: j({ twitchChannel: 'c', playlists: [{ id: 'PLtest123' }], port: 7654 }) },
    { name: 'does not rewrite a complete file written compact on one line', before: j(example) },
    { name: 'does not rewrite a complete file with CRLF line endings', before: exampleText.replace(/\n/g, '\r\n') },
    { name: 'writes non-ASCII and escaped characters the way JSON.stringify does', before: j({ ...base, discord: { botName: 'Bøt ✓ 😀 "q" \\ \n tab\t', rolePing: '\u0001' } }) },
    // discord
    { name: 'migrates old flat discordWebhookUrl to nested discord.webhookUrl', before: j({ ...base, discordWebhookUrl: 'https://discord.com/api/webhooks/old' }) },
    { name: 'does not overwrite existing discord object during migration', before: j({ ...base, discord: { webhookUrl: 'https://discord.com/api/webhooks/new' }, discordWebhookUrl: 'https://discord.com/api/webhooks/old' }) },
    { name: 'preserves partial event overrides without clobbering others', before: j({ ...base, discord: { webhookUrl: 'https://discord.com/api/webhooks/test', events: { error: false } } }) },
    { name: 'drops old discord keys obsDisconnect and obsReconnect', before: j({ ...base, discord: { events: { obsDisconnect: true }, templates: { obsReconnect: 'x' } } }) },
    { name: 'throws on a discord toggle that is not a boolean', before: j({ ...base, discord: { events: { error: 'yes' } } }) },
    { name: 'save merges discord events and templates into the existing ones', before: j({ ...base, discord: { botName: 'Old', events: { skip: false }, templates: { skip: 'custom {reason}' } } }), patch: { discord: { webhookUrl: 'https://example.com/hook', events: { error: false }, templates: { error: 'e {errorCode}' } } } },
    { name: 'save with a discord patch and no existing discord section', before: j(base), patch: { discord: { botName: 'New' } } },
    // supervisor
    { name: 'keeps the supervisor section across load and write-back', before: j({ ...base, supervisor: { streamKeyProtected: 'blob', encoder: 'libx264' } }) },
    { name: 'drops stale 2.x OBS, mpv and recovery keys on write-back', before: j({ ...base, obsBrowserSourceName: 'Source', obsWebsocketUrl: 'ws://x', obsAutoStream: true, mpvGeometry: '1x1+0+0', ytdlPlayerClient: 'tv', heartbeatIntervalMs: 1000, stateFilePath: './state.json' }) },
    { name: 'rejects readAheadSeconds below its minimum, naming the field', before: withSup({ readAheadSeconds: 1.9 }) },
    { name: 'rejects freezeSeconds below its minimum, naming the field', before: withSup({ freezeSeconds: 4 }) },
    { name: 'rejects silenceSeconds below its minimum, naming the field', before: withSup({ silenceSeconds: 4 }) },
    { name: 'rejects cacheBudgetMb below its minimum, naming the field', before: withSup({ cacheBudgetMb: 49 }) },
    { name: 'accepts every supervisor value at its minimum', before: withSup({ readAheadSeconds: 2, freezeSeconds: 5, silenceSeconds: 5, cacheBudgetMb: 50 }) },
    { name: 'rejects an unknown encoder', before: withSup({ encoder: 'h265' }) },
    { name: 'rejects an unknown preferredEncoder', before: withSup({ preferredEncoder: 'h265' }) },
    { name: 'keeps preferredEncoder when set', before: withSup({ preferredEncoder: 'h264_qsv', encoder: 'libx264' }) },
    { name: 'rejects a fractional cacheBudgetMb', before: withSup({ cacheBudgetMb: 300.5 }) },
    { name: 'rejects a supervisor value of the wrong type', before: withSup({ overlay: 'yes' }) },
    { name: 'rewrites an integer-valued float as a plain integer', before: '{\n  "playlists": [{ "id": "PLtest123" }],\n  "supervisor": { "readAheadSeconds": 20.0, "freezeSeconds": 2.5e1, "cacheBudgetMb": 3e2 }\n}' },
    { name: 'loads a blank ingestUrl and playerClient as the defaults', before: withSup({ ingestUrl: '   ', playerClient: '' }) },
    { name: 'keeps unknown supervisor keys through load', before: withSup({ futureKnob: 7, futureObject: { a: [1, 2] } }) },
    { name: 'keeps unknown supervisor keys through save', before: withSup({ futureKnob: 7 }), patch: { port: 7000 } },
    { name: 'keeps streamKeyProtected when saving a partial supervisor patch', before: withSup({ streamKeyProtected: 'blob' }), patch: { supervisor: { encoder: 'h264_amf' } } },
    { name: 'keeps streamKeyProtected when a save carries an empty one', before: withSup({ streamKeyProtected: 'blob' }), patch: { supervisor: { streamKeyProtected: '', encoder: 'libx264' } } },
    { name: 'replaces streamKeyProtected when a save carries a non-empty one', before: withSup({ streamKeyProtected: 'blob' }), patch: { supervisor: { streamKeyProtected: 'new' } } },
    { name: 'saves a blank ingestUrl or playerClient as the default', before: withSup({ ingestUrl: 'rtmp://custom/app', playerClient: 'tv' }), patch: { supervisor: { ingestUrl: '', playerClient: ' ' } } },
    { name: 'save rejects a supervisor patch below a minimum', before: j(base), patch: { supervisor: { freezeSeconds: 1 } } },
    { name: 'save with no existing file and no playlists throws', before: null, patch: { port: 7000 } },
    { name: 'save with no existing file and a playlist creates it', before: null, patch: { playlists: [{ id: 'PLnew' }] } },
    { name: 'save migrates a legacy playlistId in the existing file', before: j({ playlistId: 'PLlegacy' }), patch: { port: 7001 } },
    { name: 'save replaces the playlists array whole', before: j({ playlists: [{ id: 'PL1', name: 'One' }, { id: 'PL2' }] }), patch: { playlists: [{ id: 'PL2' }, { id: 'PL1' }] } },
    // a config.json written by 3.0.x
    { name: '3.0.x file loads with every value as written and is left untouched', before: pretty(written) },
    { name: '3.0.x file changes only the saved field on a save', before: pretty(written), patch: { port: 7000 } },
    { name: 'absent sections are filled with the values in config.example.json', before: j({ playlists: example.playlists }) },
    { name: 'empty sections are filled with the values in config.example.json', before: j({ playlists: example.playlists, discord: {}, supervisor: {} }) },
    { name: 'empty nested sections are filled with the values in config.example.json', before: j({ playlists: example.playlists, discord: { events: {}, templates: {} } }) },
  ];
  const seen = new Set<string>();
  for (const c of cases) {
    const file = slug(c.name);
    if (seen.has(file)) throw new Error(`duplicate config case: ${c.name}`);
    seen.add(file);
    writeJson(join(CONFIG_DIR, `${file}.json`), runConfigCase(c));
  }
  rmSync(TMP, { recursive: true, force: true });
  return cases.length;
}

// ---------------------------------------------------------------- describe, gate, names, launcher events

const TS = '2026-10-25T10:00:00Z';
const ev = (kind: string, extra: Record<string, unknown> = {}, level: SupervisorEvent['level'] = 'info'): SupervisorEvent => ({ ts: TS, level, kind, ...extra });

function recordDescribe(): Case[] {
  const longDetail = 'first\n' + 'y'.repeat(500);
  const table: [string, SupervisorEvent][] = [
    // encoder
    ['encoder-exit: first failure is a streamDrop warning', ev('encoder-exit', { why: 'crash', failures: 1, restartInS: 2 }, 'warn')],
    ['encoder-exit: second failure is a streamDrop warning', ev('encoder-exit', { why: 'crash', failures: 2, restartInS: 5 }, 'warn')],
    ['encoder-exit: third failure is critical', ev('encoder-exit', { why: 'crash', failures: 3, restartInS: 5 }, 'critical')],
    ['encoder-exit: a critical level wins even on failure 1', ev('encoder-exit', { why: 'x', failures: 1, restartInS: 1 }, 'critical')],
    ['encoder-exit: no failures or restart given', ev('encoder-exit', { why: 'x' }, 'warn')],
    ['encoder-exit: no why', ev('encoder-exit', { failures: 1, restartInS: 2 }, 'warn')],
    ['encoder-exit: first exit after 48 hours is Twitch ending the stream', ev('encoder-exit', { why: 'exited (exit code: 0xffffd8bb)', code: -10053, failures: 1, restartInS: 2, uptimeS: 172802 }, 'warn')],
    ['encoder-exit: uptime exactly 47.5 hours counts as the Twitch limit', ev('encoder-exit', { why: 'x', failures: 1, restartInS: 2, uptimeS: 171000 }, 'warn')],
    ['encoder-exit: uptime just under 47.5 hours is an ordinary drop', ev('encoder-exit', { why: 'x', failures: 1, restartInS: 2, uptimeS: 170999.9 }, 'warn')],
    ['encoder-exit: a second failure after a long uptime is an ordinary drop', ev('encoder-exit', { why: 'x', failures: 2, restartInS: 2, uptimeS: 172802 }, 'warn')],
    ['encoder-exit: why with a newline keeps the first line', ev('encoder-exit', { why: 'first line\nsecond line', failures: 1, restartInS: 2 }, 'warn')],
    ['encoder-start: first start is timeline only', ev('encoder-start', { generation: 1, restarts: 0 })],
    ['encoder-start: a restart is a streamRestart', ev('encoder-start', { generation: 2, restarts: 1 })],
    ['encoder-start: restart count 20', ev('encoder-start', { generation: 21, restarts: 20 })],
    ['encoder-start: no restarts field', ev('encoder-start')],
    ['encoder-spawn-failed is critical', ev('encoder-spawn-failed', { error: 'e' }, 'critical')],
    ['encoder-spawn-failed: no error', ev('encoder-spawn-failed', {}, 'critical')],
    ['encoder-restart-requested names the reason', ev('encoder-restart-requested', { reason: 'twitch read-back frozen' })],
    // skip, retry, resume
    ['skip: video numbers count from 1', ev('skip', { index: 3, id: 'abc', why: 'unavailable', consecutive: 1 }, 'warn')],
    ['skip: a third skip in a row is critical', ev('skip', { index: 2, id: 'z', why: 'x', consecutive: 3 }, 'critical')],
    ['skip: a freeze skip stays on the timeline', ev('skip', { index: 1, id: 'x', why: 'freeze', consecutive: 1 }, 'warn')],
    ['skip: a freeze skip in a critical streak still alerts', ev('skip', { index: 1, id: 'x', why: 'freeze', consecutive: 3 }, 'critical')],
    ['skip: detail text is appended to the reason', ev('skip', { index: 0, id: 'a', why: 'unavailable', detail: 'private video', consecutive: 1 }, 'warn')],
    ['skip: detail.reason string is appended', ev('skip', { index: 0, id: 'a', why: 'unavailable', detail: { reason: 'private video' }, consecutive: 1 }, 'warn')],
    ['skip: a detail object without a reason adds nothing', ev('skip', { index: 0, id: 'a', why: 'other', detail: { since: 12.3, pos: 40 }, consecutive: 1 }, 'warn')],
    ['skip: a blank detail adds nothing', ev('skip', { index: 0, id: 'a', why: 'other', detail: '   ', consecutive: 1 }, 'warn')],
    ['skip: a detail.reason that is not text adds nothing', ev('skip', { index: 0, id: 'a', why: 'other', detail: { reason: 5 }, consecutive: 1 }, 'warn')],
    ['skip: detail is trimmed to one line of at most 200 characters', ev('skip', { index: 0, id: 'a', why: 'x', detail: longDetail, consecutive: 1 }, 'warn')],
    ['skip: a 200 character reason is kept whole', ev('skip', { index: 0, id: 'a', why: 'x', detail: 'd'.repeat(200 - 3), consecutive: 1 }, 'warn')],
    ['skip: no index or id', ev('skip', { why: 'x' }, 'warn')],
    ['skip: a numeric why is written as JSON', ev('skip', { index: 0, id: 'a', why: 42 }, 'warn')],
    ['retry: attempt and position', ev('retry', { index: 0, id: 'x', why: 'stall', retry: 2, pos: 12.5 }, 'warn')],
    ['retry: a freeze retry stays on the timeline', ev('retry', { index: 1, id: 'x', why: 'freeze', retry: 1, pos: 3 }, 'warn')],
    ['retry: position is floored', ev('retry', { index: 4, id: 'y', why: 'eof', retry: 1, pos: 59.99 }, 'warn')],
    ['resume: video and position', ev('resume', { index: 1, id: 'r', pos: 30 })],
    ['resume: fractional position is floored', ev('resume', { index: 0, id: 'r', pos: 30.9 })],
    // watchdog
    ['watchdog-freeze: retry goes to outputFrozen', ev('watchdog-freeze', { index: 1, outcome: 'retry' }, 'warn')],
    ['watchdog-freeze: skip goes to outputFrozen', ev('watchdog-freeze', { index: 1, outcome: 'skip' }, 'warn')],
    ['watchdog-freeze: ignored stays on the timeline', ev('watchdog-freeze', { index: 1, outcome: 'ignored' }, 'warn')],
    ['watchdog-freeze: content stays on the timeline', ev('watchdog-freeze', { index: 1, outcome: 'content' }, 'warn')],
    ['watchdog-silence: skip goes to outputFrozen', ev('watchdog-silence', { index: 1, outcome: 'skip' }, 'warn')],
    ['watchdog-silence: retry goes to outputFrozen', ev('watchdog-silence', { index: 1, outcome: 'retry' }, 'warn')],
    ['watchdog-silence: content is the video playing on', ev('watchdog-silence', { index: 1, outcome: 'content' }, 'info')],
    ['watchdog-silence: ignored stays on the timeline', ev('watchdog-silence', { index: 1, outcome: 'ignored' }, 'warn')],
    ['watchdog-silence: no outcome', ev('watchdog-silence', { index: 1 }, 'warn')],
    // lists
    ['all-skipped is critical', ev('all-skipped', { holdS: 60 }, 'critical')],
    ['playlist-advance: finished list by name', ev('playlist-advance', { from: 'PLa', to: 'PLb', name: '2021/2022', count: 5, finished: true })],
    ['playlist-advance: finished list by id', ev('playlist-advance', { from: 'PLa', to: 'PLb', count: 5, finished: true })],
    ['playlist-advance: a jump without finishing', ev('playlist-advance', { from: 'PLa', to: 'PLb', name: '2021/2022', count: 5, finished: false })],
    ['playlist-advance: finished missing counts as not finished', ev('playlist-advance', { from: 'PLa', to: 'PLb', count: 5 })],
    ['playlist-advance: finished as the text true counts as not finished', ev('playlist-advance', { to: 'PLb', finished: 'true' })],
    ['playlist-advance: missing lists are critical', ev('playlist-advance', { from: 'PLa', to: 'PLb', name: 'B', count: 5, finished: true, missing: ['C', 'D'] }, 'critical')],
    ['playlist-advance: an empty missing list is not critical', ev('playlist-advance', { to: 'PLb', name: 'B', finished: true, missing: [] })],
    ['playlist-advance: a name that is blank falls back to the id', ev('playlist-advance', { to: 'PLb', name: '  ', finished: false })],
    ['playlist-restart: went round', ev('playlist-restart', { playlistId: 'PLa', count: 192 })],
    ['playlist-restart: next list could not load is critical', ev('playlist-restart', { playlistId: 'PLa', count: 192, missing: ['2021/2022'] }, 'critical')],
    ['playlist-restart: several missing lists', ev('playlist-restart', { playlistId: 'PLa', count: 3, missing: ['A', 'B', 'C'] }, 'critical')],
    ['playlist-failed: names the list', ev('playlist-failed', { reason: 'r', playlistId: 'PLb', name: '2021/2022', refetch: true }, 'warn')],
    ['playlist-failed: falls back to the id', ev('playlist-failed', { reason: 'r', playlistId: 'PLb' }, 'warn')],
    ['playlist-failed: no list named', ev('playlist-failed', { reason: 'r' }, 'warn')],
    ['playlist-failed: first load is critical', ev('playlist-failed', { reason: 'r' }, 'critical')],
    ['playlist-failed: critical with a named list', ev('playlist-failed', { reason: 'r', playlistId: 'PLb', name: 'B' }, 'critical')],
    ['playlist: loaded', ev('playlist', { count: 12 })],
    ['playlist-recovered', ev('playlist-recovered')],
    ['playlist-from-saved', ev('playlist-from-saved', { playlistId: 'PLx', count: 12 }, 'warn')],
    ['playlist-switch-failed', ev('playlist-switch-failed', { reason: 'no such list' }, 'warn')],
    // stream key, config, youtube
    ['stream-key-unreadable', ev('stream-key-unreadable', { reason: 'r' }, 'critical')],
    ['stream-key-missing', ev('stream-key-missing', {}, 'critical')],
    ['youtube-breaker-open', ev('youtube-breaker-open', { reason: 'bot' }, 'critical')],
    ['youtube-breaker-closed', ev('youtube-breaker-closed')],
    ['youtube-probe-failed', ev('youtube-probe-failed', { reason: 'timeout' })],
    ['cookies-unreadable: critical names the browser and reason', ev('cookies-unreadable', { browser: 'firefox', reason: 'could not find firefox cookies database' }, 'critical')],
    ['cookies-unreadable: warn went on without the cookies', ev('cookies-unreadable', { browser: 'chrome', reason: 'Failed to decrypt with DPAPI' }, 'warn')],
    ['config-refused', ev('config-refused', { reason: 'config.json: ytdlCookiesFromBrowser is longer than 200 characters' }, 'critical')],
    ['downloads-failing', ev('downloads-failing', { inARow: 3, reason: 'HTTP Error 403' }, 'critical')],
    ['download-failed: with a retry in an hour', ev('download-failed', { index: 165, reason: 'ERROR: unable to download video data: HTTP Error 403: Forbidden', retryInS: 3600 }, 'warn')],
    ['download-failed: from before retryInS existed', ev('download-failed', { index: 165, reason: 'ERROR: unable to download video data: HTTP Error 403: Forbidden' }, 'warn')],
    ['download-failed: a retry under a minute rounds up to 1 min', ev('download-failed', { index: 0, reason: 'r', retryInS: 20 }, 'warn')],
    ['download-failed: 90 seconds rounds to 2 min', ev('download-failed', { index: 0, reason: 'r', retryInS: 90 }, 'warn')],
    ['download-failed: 150 seconds rounds to 3 min', ev('download-failed', { index: 0, reason: 'r', retryInS: 150 }, 'warn')],
    ['download-failed: a zero retry says nothing about trying again', ev('download-failed', { index: 0, reason: 'r', retryInS: 0 }, 'warn')],
    ['resolve-failed', ev('resolve-failed', { index: 7, reason: 'private' }, 'warn')],
    ['url-refresh', ev('url-refresh', { index: 2 })],
    ['breaker-retarget', ev('breaker-retarget')],
    // feeder
    ['feeder-spawn-failed is critical', ev('feeder-spawn-failed', { index: 0, error: 'e' }, 'critical')],
    ['feeder-spawn-recovered', ev('feeder-spawn-recovered', { index: 4 })],
    ['slate-stall', ev('slate-stall', {}, 'warn')],
    ['overlay-failed', ev('overlay-failed', { error: 'drawtext failed' }, 'warn')],
    ['log-prune-failed', ev('log-prune-failed', { first: 'access denied' }, 'warn')],
    // supervisor and launcher
    ['supervisor-down is critical', ev('supervisor-down', {}, 'critical')],
    ['supervisor-back is an info recovery alert', ev('supervisor-back')],
    ['supervisor-missing is critical', ev('supervisor-missing', {}, 'critical')],
    ['supervisor-launched', ev('supervisor-launched')],
    ['supervisor-blocked-by-obs', ev('supervisor-blocked-by-obs', {}, 'critical')],
    ['launched: a launch that broke away stays off the timeline', ev('launched', { breakaway: true, pid: 1 })],
    ['launched: a launch that stayed inside the caller job is noted', ev('launched', { breakaway: false, pid: 1 })],
    ['launched: no breakaway field stays off the timeline', ev('launched', { pid: 1 })],
    ['encoder-fallback names both encoders', ev('encoder-fallback', { from: 'h264_nvenc', to: 'h264_amf' }, 'critical')],
    ['encoder-restored is an info all-clear', ev('encoder-restored', { from: 'h264_amf', to: 'h264_nvenc' })],
    ['no-encoder', ev('no-encoder', {}, 'critical')],
    ['ffmpeg-missing', ev('ffmpeg-missing', {}, 'critical')],
    ['start', ev('start')],
    ['shutdown', ev('shutdown')],
    ['reload', ev('reload')],
    ['control', ev('control', { cmd: 'next' })],
    ['deadline', ev('deadline', { hours: 2 })],
    ['fault', ev('fault', { fault: 'freeze-video' })],
    ['fault-applied', ev('fault-applied', { fault: 'kill-encoder' })],
    // token server
    ['token-server-spawn', ev('token-server-spawn')],
    ['token-server-adopt', ev('token-server-adopt')],
    ['token-server-down', ev('token-server-down', {}, 'warn')],
    ['token-server-restart', ev('token-server-restart')],
    ['token-server-spawn-failed is critical', ev('token-server-spawn-failed', { error: 'e' }, 'critical')],
    ['token-caches-cleared: cleared', ev('token-caches-cleared', { refusals: 2, ok: true })],
    ['token-caches-cleared: could not be cleared', ev('token-caches-cleared', { refusals: 2, ok: false }, 'warn')],
    ['token-caches-cleared: ok missing counts as cleared', ev('token-caches-cleared', { refusals: 2 })],
    // quiet kinds and unknown ones
    ['feeder-start is quiet', ev('feeder-start')],
    ['handover is quiet', ev('handover')],
    ['spare-resolve is quiet', ev('spare-resolve')],
    ['spare-used is quiet', ev('spare-used')],
    ['youtube-probe is quiet', ev('youtube-probe')],
    ['download-cancel is quiet', ev('download-cancel')],
    ['an unknown kind goes on the timeline by name', ev('something-new')],
    ['an unknown critical kind is sent to Discord as critical', ev('brand-new', {}, 'critical')],
    ['an unknown warn kind stays on the timeline', ev('brand-new', {}, 'warn')],
    // detail handling shared by every kind
    ['a detail that is an object is written as JSON on its first line', ev('control', { cmd: { a: 1, b: [2, 3] } })],
    ['a detail that is null is empty', ev('control', { cmd: null })],
    ['a detail that is a boolean is written as JSON', ev('control', { cmd: true })],
    ['a detail with CRLF keeps the first line', ev('control', { cmd: 'one\r\ntwo' })],
    ['a detail of exactly 200 characters is kept', ev('control', { cmd: 'z'.repeat(200) })],
    ['a detail of 201 characters is cut to 197 and an ellipsis', ev('control', { cmd: 'z'.repeat(201) })],
    ['a detail with leading and trailing space is trimmed', ev('control', { cmd: '   padded   ' })],
    ['a non-ASCII detail is cut by UTF-16 units', ev('control', { cmd: 'é'.repeat(250) })],
    ['a detail with an emoji at the cut', ev('control', { cmd: 'a'.repeat(196) + '😀' + 'b'.repeat(10) })],
    ['a counter that is not a number counts as zero', ev('playlist', { count: '12' })],
  ];
  return table.map(([name, e]) => ({ name, input: e, output: describeEvent(e) }));
}

function recordGate(): Case[] {
  const refused = ev('config-refused', { reason: 'config.json: ytdlCookiesFromBrowser is longer than 200 characters' }, 'critical');
  const exit = (failures: number, extra: Record<string, unknown> = {}, level: SupervisorEvent['level'] = 'warn') =>
    ev('encoder-exit', { why: 'x', failures, restartInS: 1, ...extra }, level);
  const tokenFail = ev('token-server-spawn-failed', { error: 'e' }, 'critical');
  const spawnFail = ev('encoder-spawn-failed', { error: 'e' }, 'critical');
  const fb = (from: string, to: string) => ev('encoder-fallback', { from, to }, 'critical');
  const restored = (from: string, to: string) => ev('encoder-restored', { from, to });
  const sequences: [string, SupervisorEvent[]][] = [
    ['a refused config alerts once per streak and again after a supervisor start', [refused, refused, refused, ev('start'), refused]],
    ['a refused config with a different reason is still the same streak', [refused, ev('config-refused', { reason: 'other' }, 'critical')]],
    ['one outage of 20 encoder exits sends one drop, one critical and one restart', [
      ...Array.from({ length: 20 }, (_, i) => exit(i + 1, { restartInS: 5 }, i + 1 >= 3 ? 'critical' : 'warn')),
      ev('encoder-start', { generation: 21, restarts: 20 }),
    ]],
    ['a new streak starts after the restart', [exit(1), exit(2), exit(3, {}, 'critical'), exit(4, {}, 'critical'), ev('encoder-start', { restarts: 4 }), exit(1)]],
    ['spawn failures alert once since the last start, then one restart', [spawnFail, spawnFail, spawnFail, ev('encoder-start', { restarts: 0 }), spawnFail]],
    ['the 48 hour cut sends nothing, and neither does its restart', [exit(1, { restartInS: 2, uptimeS: 172802 }), ev('encoder-start', { generation: 1, restarts: 1 })]],
    ['a restart after the 48 hour cut that keeps failing still alerts', [exit(1, { restartInS: 2, uptimeS: 172802 }), exit(2, { restartInS: 5, uptimeS: 1 }), exit(3, { restartInS: 10, uptimeS: 1 }, 'critical'), ev('encoder-start', { restarts: 4 })]],
    ['a start that follows no alert sends nothing', [ev('encoder-start', { restarts: 2 })]],
    ['a first start after no exits sends nothing', [ev('encoder-start', { restarts: 0 })]],
    ['token spawn failures alert once until the next token-server-spawn', [tokenFail, tokenFail, ev('token-server-spawn', {}), tokenFail]],
    ['an adopted token server re-arms the token alert', [tokenFail, ev('token-server-adopt', {}), tokenFail]],
    ['encoder-fallback alerts once per change', [fb('h264_nvenc', 'h264_amf'), fb('h264_nvenc', 'h264_amf'), fb('h264_amf', 'libx264')]],
    ['the same fallback alerts again after the encoder was restored', [fb('h264_nvenc', 'h264_amf'), restored('h264_amf', 'h264_nvenc'), fb('h264_nvenc', 'h264_amf')]],
    ['a restored encoder without a fallback still sends its all-clear', [restored('h264_amf', 'h264_nvenc')]],
    ['events the gate does not track pass through unchanged', [ev('skip', { index: 1, id: 'a', why: 'x', consecutive: 1 }, 'warn'), ev('skip', { index: 1, id: 'a', why: 'x', consecutive: 1 }, 'warn'), ev('resume', { index: 1, id: 'a', pos: 1 })]],
    ['a spawn failure then an exit share one encoder alert until the start', [spawnFail, exit(1), ev('encoder-start', { restarts: 1 })]],
    ['an exit then spawn failures: the spawn failure alerts once, the start sends a restart', [exit(1), spawnFail, spawnFail, ev('encoder-start', { restarts: 2 })]],
    ['exit 2 and exits past 3 are timeline only', [exit(2), exit(5, {}, 'critical')]],
  ];
  return sequences.map(([name, events]) => {
    const gate = new AlertGate();
    return { name, input: events, output: events.map((e) => gate.describe(e)) };
  });
}

function recordPlaylistNames(): Case[] {
  const lists = [{ id: 'PLa', name: 'first' }, { id: 'PLb' }];
  const rows: [string, SupervisorEvent][] = [
    ['names the list a list moved on to', ev('playlist-advance', { to: 'PLa' })],
    ['leaves a list with no name to be shown by its id', ev('playlist-advance', { to: 'PLb' })],
    ['names the list a failed load was for', ev('playlist-failed', { playlistId: 'PLa' })],
    ['names the lists that could not load, by id where they have no name', ev('playlist-restart', { missing: ['PLa', 'PLb', 'PLgone'] })],
    ['a restart names no list of its own, only its missing ones', ev('playlist-restart', { playlistId: 'PLa', missing: ['PLa'] })],
    ['takes the advance name from to, not from', ev('playlist-advance', { from: 'PLa', to: 'PLb' })],
    ['looks up a failed load by playlistId, not by to', ev('playlist-failed', { to: 'PLa', playlistId: 'PLb' })],
    ['an unknown list gets no name', ev('playlist-advance', { to: 'PLgone' })],
    ['leaves every other event as it is', ev('skip', { to: 'PLa', playlistId: 'PLa' })],
    ['keeps the other fields of a named event', ev('playlist-advance', { to: 'PLa', count: 5, finished: true })],
  ];
  return rows.map(([name, e]) => ({ name, input: { event: e, playlists: lists }, output: withPlaylistNames(e, lists) }));
}

function recordLauncherEvents(): Case[] {
  const kinds = ['supervisor-down', 'supervisor-missing', 'supervisor-blocked-by-obs', 'encoder-fallback', 'no-encoder', 'ffmpeg-missing', 'supervisor-back', 'supervisor-launched', 'encoder-restored', 'something-else'];
  return kinds.map((kind) => {
    const e = launcherEvent(kind, { from: 'a', to: 'b' });
    // ts is the wall clock; the shape and level are what the port must match.
    return { name: `launcherEvent ${kind}`, input: { kind, detail: { from: 'a', to: 'b' } }, output: { ...e, ts: '<ts>' } };
  });
}

// ---------------------------------------------------------------- stream state

function recordStreamState(): Case[] {
  const playing = (extra: Record<string, unknown> = {}): SupervisorStatus =>
    ({ state: 'playing', reason: null, encoder: { alive: true }, recovery: { step: 'none' }, hold: null, ...extra }) as SupervisorStatus;
  type Row = [string, SupervisorStatus | null, LauncherState, boolean?, string | null?, string | null?];
  const rows: Row[] = [
    // the 12-row table of api.test.ts
    ['live while the encoder is up and nothing recovers', playing(), 'running'],
    ['recovering during a retry', playing({ state: 'recovering', recovery: { step: 'retry' } }), 'running'],
    ['recovering while the encoder restarts', playing({ state: 'recovering', encoder: { alive: false } }), 'running'],
    ['recovering during a hold on the slate', playing({ hold: { reason: 'every video failed', level: 1 } }), 'running'],
    ['starting while the supervisor starts', playing({ state: 'starting', encoder: { alive: false } }), 'running'],
    ['off-air without a stream key', playing({ state: 'needs-stream-key', encoder: { alive: false } }), 'running'],
    ['off-air without a playlist', playing({ state: 'waiting-for-playlist', encoder: { alive: false } }), 'running'],
    ['off-air while the supervisor stops on its own', playing({ state: 'stopping' }), 'running'],
    ['off-air when playing with the encoder down', playing({ encoder: { alive: false } }), 'running'],
    ['off-air when the supervisor does not answer', null, 'down'],
    ['off-air when the supervisor is missing', null, 'missing'],
    ['starting while the launcher starts it', null, 'starting'],
    // precedence
    ['stopped wins while the old supervisor still answers', playing(), 'running', true],
    ['stopped wins over a wait reason', playing({ state: 'stopping' }), 'starting', true, 'Waiting for the stopped supervisor to exit before starting it again.'],
    ['stopped wins over a block reason with no supervisor', null, 'stopped-by-user', true, null, 'OBS is running.'],
    ['starting with the launcher reason while a Start waits for the stopped supervisor', playing({ state: 'stopping' }), 'starting', false, 'Waiting for the stopped supervisor to exit before starting it again.'],
    ['a wait reason wins over a live supervisor', playing(), 'running', false, 'Waiting for the stopped supervisor to exit before starting it again.'],
    ['a wait reason wins over a block reason', null, 'starting', false, 'Waiting.', 'OBS is running.'],
    ['off-air with the launcher block reason', null, 'starting', false, null, 'OBS is running.'],
    ['a block reason wins over a missing supervisor program', null, 'missing', false, null, 'OBS is running.'],
    ['a block reason is ignored while the supervisor answers', playing(), 'running', false, null, 'OBS is running.'],
    ['an empty wait reason does not count', playing(), 'running', false, '', null],
    ['an empty block reason does not count', null, 'down', false, null, ''],
    ['the stopped-by-user launcher state without the flag is not answering', null, 'stopped-by-user'],
    // the rest of the status shapes
    ['live with the recovery step missing', playing({ recovery: undefined }), 'running'],
    ['live with recovery step none', playing({ recovery: { step: 'none' } }), 'running'],
    ['recovering with a recovery step while playing', playing({ recovery: { step: 'restart-feeder' } }), 'running'],
    ['live with an empty recovery step', playing({ recovery: { step: '' } }), 'running'],
    ['off-air when the encoder object is missing', playing({ encoder: undefined }), 'running'],
    ['off-air when encoder.alive is missing', playing({ encoder: {} }), 'running'],
    ['recovering state with the encoder alive', playing({ state: 'recovering' }), 'running'],
    ['recovering state with the encoder object missing', playing({ state: 'recovering', encoder: undefined }), 'running'],
    ['a hold wins over a recovery step', playing({ hold: { reason: 'r' }, recovery: { step: 'retry' } }), 'running'],
    ['the encoder is checked before a hold', playing({ hold: { reason: 'r' }, encoder: { alive: false } }), 'running'],
    ['an unknown supervisor state', playing({ state: 'weird' }), 'running'],
    ['a missing supervisor state', playing({ state: undefined }), 'running'],
    ['starting ignores the launcher state', playing({ state: 'starting' }), 'missing'],
  ];
  return rows.map(([name, sup, launcher, stopped = false, wait = null, block = null]) => ({
    name,
    input: { sup, launcher, stoppedByUser: stopped, waitReason: wait, blockReason: block },
    output: streamState(sup, launcher, stopped, wait, block),
  }));
}

// ---------------------------------------------------------------- discord

const HOOK = 'https://discord.example/api/webhooks/test';
const BLANK_TIMESTAMP = '<timestamp>';
const BLANK_MEMORY = '<memory>';

type Step =
  | { call: 'send'; content: string; level?: 'info' | 'warn' | 'error'; fields?: { name: string; value: string; inline?: boolean }[] }
  | { call: 'notifyCritical'; message: string }
  | { call: 'notifyEvent'; alert: { kind: keyof DiscordConfig['events']; vars: Record<string, string | number>; level: 'info' | 'warn' | 'error' } }
  | { call: 'notifyTwitchMismatch' | 'notifyTwitchRestart'; channel: string }
  | { call: 'flush' };

interface DiscordInput {
  discord: Partial<Omit<DiscordConfig, 'events' | 'templates'>> & { events?: Partial<DiscordConfig['events']>; templates?: Partial<DiscordConfig['templates']> };
  appVersion: string;
  uptimeMs: number;
  adminUrl: string;
  steps: Step[];
}

/** Runs the steps through the real notifier with fetch replaced, and returns every POST it made. */
async function runDiscord(input: DiscordInput): Promise<unknown[]> {
  const exampleConfig = JSON.parse(readFileSync(join(root, 'config.example.json'), 'utf-8')) as AppConfig;
  const defaults = exampleConfig.discord;
  const discord: DiscordConfig = {
    ...defaults,
    ...input.discord,
    events: { ...defaults.events, ...input.discord.events },
    templates: { ...DEFAULT_DISCORD_TEMPLATES, ...input.discord.templates },
  };
  const posted: unknown[] = [];
  const realFetch = globalThis.fetch;
  globalThis.fetch = (async (url: string | URL | Request, init?: RequestInit) => {
    const body = JSON.parse(String(init?.body));
    for (const embed of body.embeds ?? []) {
      if (typeof embed.timestamp !== 'string' || Number.isNaN(Date.parse(embed.timestamp))) throw new Error('embed timestamp is not an ISO date');
      embed.timestamp = BLANK_TIMESTAMP;
      for (const f of embed.fields ?? []) if (f.name === 'RAM') f.value = BLANK_MEMORY;
    }
    posted.push({ url: String(url), method: init?.method, headers: init?.headers, body });
    return new Response(null, { status: 200 });
  }) as typeof fetch;
  try {
    const n = new DiscordNotifier({ ...exampleConfig, discord }, input.appVersion, () => input.uptimeMs, input.adminUrl);
    for (const s of input.steps) {
      if (s.call === 'send') await n.send(s.content, s.level, s.fields);
      else if (s.call === 'notifyCritical') await n.notifyCritical(s.message);
      else if (s.call === 'notifyEvent') await n.notifyEvent(s.alert);
      else if (s.call === 'notifyTwitchMismatch') await n.notifyTwitchMismatch(s.channel);
      else if (s.call === 'notifyTwitchRestart') await n.notifyTwitchRestart(s.channel);
      else await n.flush();
    }
    // The 5 s batch window is a timer; flushing here is what it would do.
    await n.flush();
  } finally {
    globalThis.fetch = realFetch;
  }
  return posted;
}

async function recordDiscord(): Promise<Case[]> {
  const base = { appVersion: '3.0.5', uptimeMs: 3_725_000, adminUrl: 'http://localhost:7654/admin' };
  const mk = (name: string, steps: Step[], discord: DiscordInput['discord'] = {}, over: Partial<DiscordInput> = {}) =>
    ({ name, input: { ...base, discord: { webhookUrl: HOOK, ...discord }, steps, ...over } as DiscordInput });
  const rows: { name: string; input: DiscordInput }[] = [
    mk('a single info message', [{ call: 'send', content: 'Hello', level: 'info' }]),
    mk('a single warn message', [{ call: 'send', content: 'Careful', level: 'warn' }]),
    mk('a batch of two messages is one embed with the highest colour', [{ call: 'send', content: 'First', level: 'info' }, { call: 'send', content: 'Second', level: 'warn' }]),
    mk('a batch flushed by an error carries every queued message and the role ping', [
      { call: 'send', content: 'queued info', level: 'info' }, { call: 'send', content: 'queued warn', level: 'warn' }, { call: 'notifyCritical', message: 'urgent' },
    ], { rolePing: '123456789' }),
    mk('a critical with role ping, fields and bot identity', [{ call: 'notifyCritical', message: 'All steps exhausted' }], { rolePing: '123456789', botName: 'MyBot', avatarUrl: 'https://example.com/avatar.png' }),
    mk('a critical without a role ping', [{ call: 'notifyCritical', message: 'All steps exhausted' }]),
    mk('an info message never pings the role', [{ call: 'send', content: 'info message', level: 'info' }], { rolePing: '123456789' }),
    mk('bot identity on a single message', [{ call: 'send', content: 'test', level: 'warn' }], { botName: 'MyBot', avatarUrl: 'https://example.com/avatar.png' }),
    mk('a bot name without an avatar', [{ call: 'send', content: 'test', level: 'warn' }], { botName: 'MyBot' }),
    mk('footer for an uptime under a minute', [{ call: 'send', content: 'x', level: 'info' }], {}, { uptimeMs: 42_000 }),
    mk('footer for an uptime of minutes', [{ call: 'send', content: 'x', level: 'info' }], {}, { uptimeMs: 125_000 }),
    mk('footer for an uptime of hours', [{ call: 'send', content: 'x', level: 'info' }], {}, { uptimeMs: 90_061_000 }),
    mk('footer without a dashboard URL', [{ call: 'send', content: 'x', level: 'info' }], {}, { adminUrl: '' }),
    mk('structured fields on a single message', [{ call: 'send', content: 'with fields', level: 'warn', fields: [{ name: 'Status', value: 'ok', inline: false }, { name: 'Extra', value: 'v', inline: true }] }]),
    mk('structured fields are dropped from a batch', [{ call: 'send', content: 'one', level: 'info', fields: [{ name: 'Status', value: 'ok' }] }, { call: 'send', content: 'two', level: 'info' }]),
    mk('three queued messages in a batch', [{ call: 'send', content: 'a', level: 'info' }, { call: 'send', content: 'b', level: 'error' }, { call: 'send', content: 'c', level: 'warn' }]),
    mk('an explicit flush sends two posts', [{ call: 'send', content: 'one', level: 'info' }, { call: 'flush' }, { call: 'send', content: 'two', level: 'warn' }]),
    mk('no webhook URL sends nothing', [{ call: 'send', content: 'x', level: 'error' }], { webhookUrl: '' }),
    mk('a described event renders its template and level', [{ call: 'notifyEvent', alert: { kind: 'critical', vars: { message: 'All videos failed' }, level: 'error' } }]),
    mk('a described skip renders the default template', [{ call: 'notifyEvent', alert: { kind: 'skip', vars: { videoIndex: 4, videoId: 'abc', reason: 'unavailable' }, level: 'warn' } }]),
    mk('a described error renders the default template', [{ call: 'notifyEvent', alert: { kind: 'error', vars: { videoIndex: 1, videoId: 'x', errorCode: 'stall', attempt: 2 }, level: 'warn' } }]),
    mk('a described resume renders the default template', [{ call: 'notifyEvent', alert: { kind: 'resume', vars: { videoIndex: 2, videoId: 'r' }, level: 'info' } }]),
    mk('a described streamDrop renders the default template', [{ call: 'notifyEvent', alert: { kind: 'streamDrop', vars: { attempt: 1, maxAttempts: 3 }, level: 'warn' } }]),
    mk('a described streamRestart renders the default template', [{ call: 'notifyEvent', alert: { kind: 'streamRestart', vars: { attempts: 1 }, level: 'info' } }]),
    mk('a described outputFrozen renders the default template', [{ call: 'notifyEvent', alert: { kind: 'outputFrozen', vars: { kind: 'freeze', videoIndex: 2 }, level: 'warn' } }]),
    mk('a described recovery renders the default template', [{ call: 'notifyEvent', alert: { kind: 'recovery', vars: { step: 'Supervisor answering again' }, level: 'info' } }]),
    mk('a described event with a custom template', [{ call: 'notifyEvent', alert: { kind: 'skip', vars: { videoIndex: 4, videoId: 'abc', reason: 'r', extra: 'x' }, level: 'warn' } }], { templates: { skip: 'Skip #{videoIndex} {unknown} {reason} {videoIndex}' } }),
    mk('a described event whose toggle is off sends nothing', [{ call: 'notifyEvent', alert: { kind: 'skip', vars: { videoIndex: 1, videoId: 'x', reason: 'r' }, level: 'warn' } }], { events: { skip: false } }),
    mk('a critical whose toggle is off sends nothing', [{ call: 'notifyCritical', message: 'x' }], { events: { critical: false } }),
    mk('a critical with a custom template', [{ call: 'notifyCritical', message: 'boom' }], { templates: { critical: 'ALERT {message} {message}' } }),
    mk('a critical message with markdown and non-ASCII', [{ call: 'notifyCritical', message: 'Strøm **av** ✓ 😀 "q" \\ \n line two' }]),
    mk('twitch mismatch', [{ call: 'notifyTwitchMismatch', channel: 'testchannel' }]),
    mk('twitch restart', [{ call: 'notifyTwitchRestart', channel: 'testchannel' }]),
    mk('twitch mismatch with its toggle off sends nothing', [{ call: 'notifyTwitchMismatch', channel: 'testchannel' }], { events: { twitchMismatch: false } }),
    mk('a placeholder with digits and underscores is replaced', [{ call: 'notifyEvent', alert: { kind: 'recovery', vars: { step_2: 'ok' }, level: 'info' } }], { templates: { recovery: '{step_2} {step-2} {}' } }),
  ];
  const out: Case[] = [];
  for (const r of rows) out.push({ name: r.name, input: r.input, output: await runDiscord(r.input) });
  return out;
}

// ---------------------------------------------------------------- releases

function recordReleases(): Case[] {
  const asset = (name: string) => ({ name, browser_download_url: `https://github.com/x/y/releases/download/v1/${name}` });
  const release = (tag: string, o: { draft?: boolean; prerelease?: boolean; assets?: GithubRelease['assets'] } = {}): GithubRelease => {
    const v = tag.replace(/^v/, '');
    return { tag_name: tag, draft: o.draft ?? false, prerelease: o.prerelease ?? false, assets: o.assets ?? [asset(`streamloop-v${v}.zip`), asset(`streamloop-v${v}-update.zip`)] };
  };
  const out: Case[] = [];
  const cmp = (name: string, a: string, b: string) => out.push({ name: `compareVersions: ${name}`, input: { fn: 'compareVersions', a, b }, output: compareVersions(a, b) });
  cmp('equal versions', '1.0.0', '1.0.0');
  cmp('a higher major', '2.0.0', '1.0.0'); cmp('a lower major', '1.0.0', '2.0.0');
  cmp('a higher minor', '1.2.0', '1.1.0'); cmp('a lower minor', '1.1.0', '1.2.0');
  cmp('a higher patch', '1.0.2', '1.0.1'); cmp('a lower patch', '1.0.1', '1.0.2');
  cmp('a leading v is ignored', 'v1.2.0', '1.2.0'); cmp('a leading v on both', 'v2.0.0', 'v1.0.0');
  cmp('a missing patch counts as zero', '1.0', '1.0.0'); cmp('a missing patch against a higher patch', '1.0.1', '1.0');
  cmp('a missing patch below a higher patch', '1.0', '1.0.1');
  cmp('numeric not lexical on the minor', '1.10.0', '1.9.0'); cmp('numeric not lexical on the patch', '1.2.10', '1.2.9');
  cmp('a release is above its -dev build', 'v3.0.0', '3.0.0-dev'); cmp('a -dev build is below its release', '3.0.0-dev', '3.0.0');
  cmp('two equal -dev builds', '3.0.0-dev', '3.0.0-dev'); cmp('a newer patch -dev above an older release', '3.0.1-dev', '3.0.0');
  cmp('different suffixes on the same numbers compare equal', '3.0.0-a', '3.0.0-b');
  cmp('only the first dash splits the suffix', '3.0.0-rc-1', '3.0.0-rc-2');
  cmp('a four part version', '1.2.3.4', '1.2.3'); cmp('a four part version below', '1.2.3', '1.2.3.1');
  cmp('a non-numeric part counts as NaN and compares equal', '1.x.0', '1.0.0');

  const sel = (name: string, names: string[]) => out.push({ name: `selectReleaseAssets: ${name}`, input: { fn: 'selectReleaseAssets', assets: names.map(asset) }, output: selectReleaseAssets(names.map(asset)) });
  sel('prefers the slim -update.zip over the full bundle', ['streamloop-v2.3.0.zip', 'streamloop-v2.3.0-update.zip']);
  sel('falls back to the full zip when no -update.zip is present', ['streamloop-v2.3.0.zip']);
  sel('returns nulls when there is no zip asset', ['notes.txt']);
  sel('returns nulls for no assets', []);
  sel('matches the checksum named <file>.zip.sha256', ['streamloop-v2.3.0.zip', 'streamloop-v2.3.0.zip.sha256']);
  sel('matches the checksum named <file>.sha256', ['streamloop-v2.3.0.zip', 'streamloop-v2.3.0.sha256']);
  sel('picks the slim checksum when both bundles ship together', ['streamloop-v2.3.0.zip', 'streamloop-v2.3.0.sha256', 'streamloop-v2.3.0-update.zip', 'streamloop-v2.3.0-update.sha256']);
  sel('returns a null checksum when none matches the chosen zip', ['streamloop-v2.3.0-update.zip']);
  sel('does not take the full checksum for the slim zip', ['streamloop-v2.3.0.zip', 'streamloop-v2.3.0.zip.sha256', 'streamloop-v2.3.0-update.zip']);
  sel('downloads the slim -update.zip of a 3.x release', ['streamloop-v3.0.1.zip', 'streamloop-v3.0.1.zip.sha256', 'streamloop-v3.0.1-update.zip', 'streamloop-v3.0.1-update.zip.sha256', 'node-version.txt']);
  sel('prefers the zip.sha256 form when both forms exist', ['a.zip', 'a.sha256', 'a.zip.sha256']);
  sel('takes the first -update.zip when there are two', ['one-update.zip', 'two-update.zip']);
  sel('takes the first zip when there are two and no -update.zip', ['one.zip', 'two.zip']);

  const pick = (name: string, releases: GithubRelease[], current: string) =>
    out.push({ name: `pickRelease: ${name}`, input: { fn: 'pickRelease', releases, currentVersion: current }, output: pickRelease(releases, current)?.tag_name ?? null });
  pick('the newest stable release with the current major', [release('v3.0.0'), release('v2.3.8'), release('v2.3.7')], '2.3.6');
  pick('finds a 2.x hotfix listed after a newer 3.x release', [release('v3.1.0'), release('v3.0.0'), release('v2.3.9')], '2.3.6');
  pick('ignores drafts and prereleases on 2.x', [release('v2.4.0', { prerelease: true }), release('v2.3.9', { draft: true }), release('v2.3.7')], '2.3.6');
  pick('ignores tags that are not X.Y.Z', [release('spike-overlay'), release('v3'), release('2.4.0-beta'), release('v2.3.7')], '2.3.6');
  pick('nothing matches the major', [release('v3.0.0')], '2.3.6');
  pick('an empty list', [], '2.3.6');
  pick('accepts tags with and without a leading v', [release('2.3.8'), release('v2.3.7')], '2.3.6');
  pick('offers a 3.0.0 install the v3.0.1 prerelease', [release('v3.0.1', { prerelease: true }), release('v3.0.0', { prerelease: true })], '3.0.0');
  pick('offers the dev build the v3.0.0 prerelease', [release('v3.0.0', { prerelease: true })], '3.0.0-dev');
  pick('never offers a 3.0.0 install a 2.x or 4.x release', [release('v4.0.0'), release('v4.0.0', { prerelease: true }), release('v2.3.8')], '3.0.0');
  pick('never offers a 2.3.8 install a 3.x prerelease', [release('v3.0.1', { prerelease: true })], '2.3.8');
  pick('never offers a 2.3.8 install a 3.x stable release', [release('v3.0.1')], '2.3.8');
  pick('skips a newer release whose update or full ZIP is not uploaded yet', [
    release('v3.0.3', { prerelease: true, assets: [asset('streamloop-v3.0.3.zip')] }),
    release('v3.0.2', { prerelease: true, assets: [asset('streamloop-v3.0.2-update.zip')] }),
    release('v3.0.1', { prerelease: true }),
  ], '3.0.0');
  pick('a release with no assets is skipped', [release('v3.0.2', { prerelease: true, assets: [] }), release('v3.0.1', { prerelease: true })], '3.0.0');
  pick('the newest wins regardless of list order', [release('v3.0.1', { prerelease: true }), release('v3.0.5', { prerelease: true }), release('v3.0.2', { prerelease: true })], '3.0.0');
  pick('an equal version is still picked (the caller compares)', [release('v3.0.5', { prerelease: true })], '3.0.5');
  pick('a draft 3.x release is skipped', [release('v3.0.2', { draft: true, prerelease: true }), release('v3.0.1', { prerelease: true })], '3.0.0');
  pick('a leading v on the current version is ignored', [release('v3.0.1', { prerelease: true })], 'v3.0.0');
  pick('a tag with a suffix is not a release', [release('v3.0.2-beta', { prerelease: true }), release('v3.0.1', { prerelease: true })], '3.0.0');
  pick('a tag with four parts is not a release', [release('v3.0.2.1', { prerelease: true }), release('v3.0.1', { prerelease: true })], '3.0.0');
  pick('only an -update.zip is not enough', [release('v3.0.2', { prerelease: true, assets: [asset('streamloop-v3.0.2-update.zip')] })], '3.0.0');
  pick('only a full zip is not enough', [release('v3.0.2', { prerelease: true, assets: [asset('streamloop-v3.0.2.zip')] })], '3.0.0');
  pick('a stable 3.x release is taken too', [release('v3.1.0')], '3.0.5');

  const nv = (name: string, text: string) => out.push({ name: `parseNodeVersion: ${name}`, input: { fn: 'parseNodeVersion', text }, output: parseNodeVersion(text) });
  nv('accepts a v prefix and surrounding whitespace', '  v22.23.4\r\n'); nv('accepts a bare version', '22.23.4');
  nv('empty', ''); nv('words', 'node 22'); nv('two parts', '22.23'); nv('four parts', '22.23.4.1');
  nv('a suffix', 'v22.23.4-rc1'); nv('two lines', '22.23.4\n22.23.5'); nv('a trailing newline only', '22.23.4\n'); nv('a double v', 'vv22.23.4');
  nv('a capital V', 'V22.23.4');
  return out;
}

// ---------------------------------------------------------------- read-back

const MASTER = [
  '#EXTM3U',
  '#EXT-X-MEDIA:TYPE=VIDEO,GROUP-ID="720p30",NAME="720p",AUTOSELECT=YES,DEFAULT=YES',
  '#EXT-X-STREAM-INF:BANDWIDTH=3000000,RESOLUTION=1280x720,CODECS="avc1.4D401F,mp4a.40.2",VIDEO="720p30",FRAME-RATE=30.000',
  'https://video.example/720p30/index.m3u8',
  '#EXT-X-MEDIA:TYPE=VIDEO,GROUP-ID="chunked",NAME="1080p30 (source)",AUTOSELECT=YES,DEFAULT=YES',
  '#EXT-X-STREAM-INF:BANDWIDTH=6000000,RESOLUTION=1920x1080,CODECS="avc1.64002A,mp4a.40.2",VIDEO="chunked",FRAME-RATE=30.000',
  'https://video.example/chunked/index.m3u8',
  '#EXT-X-STREAM-INF:BANDWIDTH=160000,CODECS="mp4a.40.2",VIDEO="audio_only"',
  'https://video.example/audio_only/index.m3u8',
].join('\n');

const LIVE_MEDIA = [
  '#EXTM3U', '#EXT-X-TARGETDURATION:2', '#EXT-X-PROGRAM-DATE-TIME:2026-10-25T10:00:00.000Z',
  '#EXTINF:2.000,live', 'https://video.example/seg1.ts', '#EXTINF:2.000,live', 'https://video.example/seg2.ts',
].join('\n');

async function recordReadback(): Promise<Case[]> {
  const out: Case[] = [];

  const diff = (name: string, a: number[], b: number[]) => {
    const r = meanAbsDiff(new Uint8Array(a), new Uint8Array(b));
    // Infinity is not JSON; it is written as the text "Infinity".
    out.push({ name: `meanAbsDiff: ${name}`, input: { fn: 'meanAbsDiff', a, b }, output: Number.isFinite(r) ? r : String(r) });
  };
  diff('the average absolute difference per pixel', [0, 10, 200, 255], [4, 10, 190, 255]);
  diff('identical frames', [0, 10, 200, 255], [0, 10, 200, 255]);
  diff('frames of different sizes count as different', [0, 0, 0, 0], [0, 0, 0, 0, 0, 0]);
  diff('two empty frames count as different', [], []);
  diff('a full swing per pixel', [0, 0], [255, 255]);
  diff('a difference that does not divide evenly', [0, 0, 0], [1, 0, 0]);
  diff('one grey level per pixel', [10, 20, 30, 40], [11, 21, 31, 41]);
  diff('one pixel of 104 changed', Array.from({ length: 104 }, () => 40), [90, ...Array.from({ length: 103 }, () => 40)]);

  for (const [w, h] of [[1920, 1080], [1280, 720], [854, 480], [640, 360], [1366, 768], [3840, 2160], [32, 18], [1, 1], [1000, 1000]]) {
    out.push({ name: `timerRegion: ${w}x${h}`, input: { fn: 'timerRegion', width: w, height: h }, output: timerRegion(w, h) });
  }

  const rendition = async (name: string, master: string | null) => {
    const fetchImpl = (async (url: string | URL | Request) => {
      const u = String(url);
      if (u.startsWith('https://gql.twitch.tv/')) return new Response(JSON.stringify({ data: { streamPlaybackAccessToken: { value: '{"t":1}', signature: 'sig' } } }));
      if (u.startsWith('https://usher.ttvnw.net/') && master !== null) return new Response(master);
      return new Response('not found', { status: 404 });
    }) as typeof fetch;
    out.push({ name: `liveRendition: ${name}`, input: { fn: 'liveRendition', master }, output: await liveRendition('c', fetchImpl) });
  };
  await rendition('picks the source rendition from the master playlist', MASTER);
  await rendition('falls back to the highest resolution when no rendition is marked chunked', MASTER.replace(/chunked/g, '1080p30'));
  await rendition('assumes 1920x1080 when the playlist gives no resolution', '#EXTM3U\n#EXT-X-STREAM-INF:BANDWIDTH=1,VIDEO="chunked"\nhttps://video.example/s.m3u8\n');
  await rendition('the channel is offline', null);
  await rendition('an empty master playlist', '');
  await rendition('only an audio_only rendition', '#EXTM3U\n#EXT-X-STREAM-INF:BANDWIDTH=160000,VIDEO="audio_only"\nhttps://video.example/audio_only/index.m3u8\n');
  await rendition('a chunked rendition beats a larger one that is not chunked', [
    '#EXTM3U',
    '#EXT-X-STREAM-INF:BANDWIDTH=1,RESOLUTION=2560x1440,VIDEO="1440p60"', 'https://video.example/1440/index.m3u8',
    '#EXT-X-STREAM-INF:BANDWIDTH=1,RESOLUTION=1280x720,VIDEO="chunked"', 'https://video.example/chunked/index.m3u8',
  ].join('\n'));
  await rendition('the larger of two chunked renditions wins', [
    '#EXTM3U',
    '#EXT-X-STREAM-INF:BANDWIDTH=1,RESOLUTION=1280x720,VIDEO="chunked"', 'https://video.example/a.m3u8',
    '#EXT-X-STREAM-INF:BANDWIDTH=1,RESOLUTION=1920x1080,VIDEO="chunked"', 'https://video.example/b.m3u8',
  ].join('\n'));
  await rendition('the first of two equal renditions wins', [
    '#EXTM3U',
    '#EXT-X-STREAM-INF:BANDWIDTH=1,RESOLUTION=1280x720,VIDEO="a"', 'https://video.example/first.m3u8',
    '#EXT-X-STREAM-INF:BANDWIDTH=2,RESOLUTION=1280x720,VIDEO="b"', 'https://video.example/second.m3u8',
  ].join('\n'));
  await rendition('a later chunked rendition replaces a larger one', [
    '#EXTM3U',
    '#EXT-X-STREAM-INF:BANDWIDTH=1,RESOLUTION=1920x1080,VIDEO="a"', 'https://video.example/big.m3u8',
    '#EXT-X-STREAM-INF:BANDWIDTH=1,RESOLUTION=640x360,VIDEO="chunked"', 'https://video.example/small-chunked.m3u8',
  ].join('\n'));
  await rendition('CRLF line endings', MASTER.replace(/\n/g, '\r\n'));
  await rendition('a stream-inf followed by a comment line is skipped', '#EXTM3U\n#EXT-X-STREAM-INF:BANDWIDTH=1,RESOLUTION=1280x720,VIDEO="chunked"\n#EXT-X-MEDIA:TYPE=VIDEO\nhttps://video.example/x.m3u8\n');
  await rendition('a stream-inf with a blank URL line is skipped', '#EXTM3U\n#EXT-X-STREAM-INF:BANDWIDTH=1,RESOLUTION=1280x720,VIDEO="chunked"\n\nhttps://video.example/x.m3u8\n');
  await rendition('a stream-inf on the last line has no URL', '#EXTM3U\n#EXT-X-STREAM-INF:BANDWIDTH=1,RESOLUTION=1280x720,VIDEO="chunked"');
  await rendition('a URL with surrounding spaces is trimmed', '#EXTM3U\n#EXT-X-STREAM-INF:BANDWIDTH=1,RESOLUTION=1280x720\n  https://video.example/x.m3u8  \n');
  await rendition('a resolution after other attributes', '#EXTM3U\n#EXT-X-STREAM-INF:BANDWIDTH=1,CODECS="a,b",RESOLUTION=852x480,FRAME-RATE=30\nhttps://video.example/x.m3u8\n');

  const ad = (name: string, media: string) => out.push({ name: `isAdPlaylist: ${name}`, input: { fn: 'isAdPlaylist', media }, output: isAdPlaylist(media) });
  ad('plain live segments', LIVE_MEDIA);
  ad('a stitched-ad date range', ['#EXTM3U', '#EXT-X-DATERANGE:ID="stitched-ad-1",CLASS="twitch-stitched-ad",START-DATE="2026-10-25T10:00:00.000Z",DURATION=30.000', '#EXTINF:2.000,live', 'https://video.example/seg1.ts'].join('\n'));
  ad('a segment not titled live', LIVE_MEDIA.replace('#EXTINF:2.000,live\nhttps://video.example/seg2.ts', '#EXTINF:2.000,Amazon\nhttps://video.example/ad.ts'));
  ad('an untitled segment is live', LIVE_MEDIA.replaceAll('#EXTINF:2.000,live', '#EXTINF:2.000,'));
  ad('a title of only spaces is live', LIVE_MEDIA.replaceAll('#EXTINF:2.000,live', '#EXTINF:2.000,   '));
  ad('a title of live with spaces around it is live', LIVE_MEDIA.replaceAll('#EXTINF:2.000,live', '#EXTINF:2.000, live '));
  ad('a title of Live in capitals is not live', LIVE_MEDIA.replaceAll('#EXTINF:2.000,live', '#EXTINF:2.000,Live'));
  ad('a date range of another class is not an ad', ['#EXTM3U', '#EXT-X-DATERANGE:ID="x",CLASS="other"', '#EXTINF:2.000,live', 'u.ts'].join('\n'));
  ad('a date range with the ad class before the id', ['#EXTM3U', '#EXT-X-DATERANGE:CLASS="twitch-stitched-ad",ID="x"', '#EXTINF:2.000,live', 'u.ts'].join('\n'));
  ad('a date range that is not at the start of a line', ['#EXTM3U', ' #EXT-X-DATERANGE:ID="x",CLASS="twitch-stitched-ad"', '#EXTINF:2.000,live', 'u.ts'].join('\n'));
  ad('an empty playlist', '');
  ad('a title with a comma in it', LIVE_MEDIA.replace('#EXTINF:2.000,live\nhttps://video.example/seg2.ts', '#EXTINF:2.000,live, extra\nhttps://video.example/seg2.ts'));
  ad('CRLF line endings on live segments', LIVE_MEDIA.replace(/\n/g, '\r\n'));
  ad('CRLF line endings on a stitched-ad date range', ['#EXTM3U', '#EXT-X-DATERANGE:ID="x",CLASS="twitch-stitched-ad"', '#EXTINF:2.000,live'].join('\r\n'));
  ad('a segment with no comma after the duration', '#EXTM3U\n#EXTINF:2.000\nhttps://video.example/seg1.ts\n');

  const grab = async (name: string, region: { x: number; y: number; w: number; h: number } | null, returned: number | 'throws') => {
    let args: string[] = [];
    const run: GrabRun = async (_exe, a) => {
      args = a;
      if (returned === 'throws') throw new Error('exit 1');
      return Buffer.alloc(returned, 7);
    };
    const r = await grabGrayRegion('ffmpeg.exe', 'https://x/s.m3u8', region, run);
    out.push({ name: `grabGrayRegion: ${name}`, input: { fn: 'grabGrayRegion', region, returnedBytes: returned }, output: { args, length: r ? r.length : null } });
  };
  await grab('crops a region', { x: 1, y: 2, w: 4, h: 2 }, 8);
  await grab('crops the 1080p clock region', { x: 16, y: 62, w: 104, h: 32 }, 104 * 32);
  await grab('scales the whole frame to 32x18 without a region', null, 576);
  await grab('a short frame is null', null, 10);
  await grab('a long frame is null', { x: 1, y: 2, w: 4, h: 2 }, 9);
  await grab('ffmpeg failing is null', null, 'throws');
  return out;
}

// ---------------------------------------------------------------- cache size, log lines

function recordCache(): Case[] {
  const GB = 1024;
  const rows: [string, number, number[]?][] = [
    ['100 GB free and no durations', 100 * GB], ['10 GB free and no durations', 10 * GB], ['3 GB free and no durations', 3 * GB],
    ['below 2 GB free is the minimum with a warning', 1536], ['exactly 2 GB free', 2048], ['one MB under 2 GB free', 2047], ['zero free', 0],
    ['2 GB and one MB free and no durations', 2049], ['5 GB free and no durations', 5 * GB], ['41 GB free and no durations', 41 * GB],
    ['1 TB free and no durations', 1024 * GB],
    ['the two longest videos of the box fit', 400 * GB, [30_474, 26_953, 600]],
    ['only short videos keep at least 500 MB', 400 * GB, [120, 90]],
    ['40 GB free caps at half and warns', 40 * GB, [30_474, 26_953]],
    ['a single duration', 400 * GB, [3600]],
    ['a single long duration', 400 * GB, [30_474]],
    ['zero and negative durations are ignored', 400 * GB, [0, -5, 600]],
    ['only zero durations act as none', 100 * GB, [0, 0]],
    ['an empty durations list acts as none', 100 * GB, []],
    ['durations in any order use the two longest', 400 * GB, [600, 30_474, 26_953, 100]],
    ['need exactly equal to the spare space', 4096, [4096]],
    ['need one MB over the spare space warns', 4096, [4097]],
    ['spare is free minus 2 GB when that is under half', 3 * GB, [10_000, 10_000]],
    ['a fractional need rounds up', 400 * GB, [1001]],
    ['two videos of 3000 s', 400 * GB, [3000, 3000]],
    ['a warning rounds GB to the nearest', 30 * GB, [20_000, 20_000]],
  ];
  return rows.map(([name, freeMb, durationsS]) => ({
    name: `recommendCacheMb: ${name}`,
    input: durationsS === undefined ? { freeMb } : { freeMb, durationsS },
    output: durationsS === undefined ? recommendCacheMb(freeMb) : recommendCacheMb(freeMb, durationsS),
  }));
}

function recordLogLines(): Case[] {
  type Local = [number, number, number, number, number, number, number];
  const pidFields = { pid: 1234, hostname: 'test', v: 1 };
  const t1: Local = [2025, 2, 16, 14, 3, 5, 123];
  const rows: [string, Local, Record<string, unknown>][] = [
    ['a basic info line with a local timestamp', t1, { level: 30, msg: 'Server listening on port 7654', ...pidFields }],
    ['extra fields are appended as JSON', t1, { level: 30, msg: 'Server listening', ...pidFields, port: 7654 }],
    ['warn level with several extras', [2025, 2, 16, 14, 3, 6, 456], { level: 40, msg: 'Player stalled', ...pidFields, stalledHeartbeats: 2, videoIndex: 5 }],
    ['error level', [2025, 2, 16, 14, 3, 7, 789], { level: 50, msg: 'Discord webhook failed', ...pidFields, status: 429 }],
    ['a missing msg gives an empty message', t1, { level: 30, ...pidFields }],
    ['an unknown level number', t1, { level: 25, msg: 'custom level', ...pidFields }],
    ['trace level', t1, { level: 10, msg: 't', ...pidFields }],
    ['debug level', t1, { level: 20, msg: 'd', ...pidFields }],
    ['fatal level', t1, { level: 60, msg: 'f', ...pidFields }],
    ['a level above fatal', t1, { level: 70, msg: 'x', ...pidFields }],
    ['level zero is unknown', t1, { level: 0, msg: 'x', ...pidFields }],
    ['single digit month, day, hour, minute, second and millisecond are padded', [2026, 1, 2, 3, 4, 5, 6], { level: 30, msg: 'padded', ...pidFields }],
    ['end of the year', [2026, 12, 31, 23, 59, 59, 999], { level: 30, msg: 'new year soon', ...pidFields }],
    ['extras keep the order of the object', t1, { level: 30, msg: 'order', zeta: 1, alpha: 2, mid: 3 }],
    ['extras with nested objects and arrays', t1, { level: 30, msg: 'nested', ...pidFields, detail: { a: [1, 2, { b: null }], c: 'x' } }],
    ['extras with non-ASCII text and escapes', t1, { level: 30, msg: 'Strøm ✓ 😀', ...pidFields, text: 'q"\\\n\t é' }],
    ['an extra named err holds an error object', t1, { level: 50, msg: 'Failed', ...pidFields, err: { type: 'Error', message: 'boom', stack: 'Error: boom\n    at x' } }],
    ['extras that are null, false and zero', t1, { level: 30, msg: 'falsy', ...pidFields, a: null, b: false, c: 0, d: '' }],
    ['extras with a fractional and an exponent number', t1, { level: 30, msg: 'numbers', ...pidFields, a: 1.5, b: 1e21, c: 20.0, d: -0.5 }],
    ['the standard fields in another order are still consumed', t1, { v: 1, hostname: 'h', msg: 'order of consumed', pid: 9, level: 30 }],
    ['a msg that is an empty string', t1, { level: 30, msg: '', ...pidFields }],
    ['only a level and one extra', t1, { level: 30, a: 1 }],
  ];
  return rows.map(([name, l, fields]) => {
    const time = new Date(l[0], l[1] - 1, l[2], l[3], l[4], l[5], l[6]).getTime();
    // `time` is epoch milliseconds in the local zone; the input keeps the local fields so the fixture does not depend on the zone.
    return {
      name,
      input: { localTime: { year: l[0], month: l[1], day: l[2], hour: l[3], minute: l[4], second: l[5], millisecond: l[6] }, fields },
      output: formatLogLine({ time, ...fields }),
    };
  });
}

// ---------------------------------------------------------------- main

async function main(): Promise<void> {
  counts['config/*.json'] = recordConfig();
  writeCases('describe.json', recordDescribe());
  writeCases('gate.json', recordGate());
  writeCases('playlist-names.json', recordPlaylistNames());
  writeCases('launcher-events.json', recordLauncherEvents());
  writeCases('stream-state.json', recordStreamState());
  writeCases('discord.json', await recordDiscord());
  writeCases('releases.json', recordReleases());
  writeCases('readback.json', await recordReadback());
  writeCases('cache.json', recordCache());
  writeCases('log-lines.json', recordLogLines());
  for (const [file, n] of Object.entries(counts)) console.log(`${String(n).padStart(4)}  ${file}`);
  // The logger's transport threads would keep the process alive.
  process.exit(0);
}

void main();
