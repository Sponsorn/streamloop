import build from 'pino-abstract-transport';
import { createWriteStream } from 'fs';
import { mkdirSync, readdirSync, statSync, unlinkSync } from 'fs';
import { join } from 'path';

const LEVEL_LABELS = {
  10: 'TRACE',
  20: 'DEBUG',
  30: 'INFO',
  40: 'WARN',
  50: 'ERROR',
  60: 'FATAL',
};

// Standard pino fields that are consumed into the formatted line
const CONSUMED_FIELDS = new Set(['time', 'level', 'msg', 'pid', 'hostname', 'v']);

/**
 * Format a timestamp as local YYYY-MM-DD HH:mm:ss.SSS.
 * @param {Date} date
 * @returns {string}
 */
function formatLocalTimestamp(date) {
  const y = date.getFullYear();
  const mo = String(date.getMonth() + 1).padStart(2, '0');
  const d = String(date.getDate()).padStart(2, '0');
  const h = String(date.getHours()).padStart(2, '0');
  const mi = String(date.getMinutes()).padStart(2, '0');
  const s = String(date.getSeconds()).padStart(2, '0');
  const ms = String(date.getMilliseconds()).padStart(3, '0');
  return `${y}-${mo}-${d} ${h}:${mi}:${s}.${ms}`;
}

/**
 * Format a pino JSON log object into a human-readable line.
 * @param {object} obj - Parsed pino log JSON object
 * @returns {string} Formatted log line (without trailing newline)
 */
export function formatLogLine(obj) {
  const ts = formatLocalTimestamp(new Date(obj.time));
  const level = LEVEL_LABELS[obj.level] || `LVL${obj.level}`;
  const msg = obj.msg || '';

  // Collect extra fields (anything not consumed by the standard format)
  const extras = {};
  let hasExtras = false;
  for (const key of Object.keys(obj)) {
    if (!CONSUMED_FIELDS.has(key)) {
      extras[key] = obj[key];
      hasExtras = true;
    }
  }

  let line = `[${ts}] ${level}: ${msg}`;
  if (hasExtras) {
    line += ` ${JSON.stringify(extras)}`;
  }
  return line;
}

/**
 * Delete log files older than retentionDays.
 * @param {string} logsDir - Directory containing log files
 * @param {string} filenamePrefix - Prefix for log filenames (e.g. 'streamloop-')
 * @param {number} retentionDays - Number of days to retain files
 */
export function cleanupOldFiles(logsDir, filenamePrefix, retentionDays) {
  const cutoff = Date.now() - retentionDays * 24 * 60 * 60 * 1000;
  let files;
  try {
    files = readdirSync(logsDir);
  } catch {
    return; // Directory doesn't exist yet, nothing to clean
  }

  for (const file of files) {
    if (!file.startsWith(filenamePrefix) || !file.endsWith('.log')) continue;
    const filePath = join(logsDir, file);
    try {
      const stat = statSync(filePath);
      if (stat.mtimeMs < cutoff) {
        unlinkSync(filePath);
      }
    } catch {
      // Ignore errors on individual files (already deleted, permissions, etc.)
    }
  }
}

/**
 * Get today's date string in YYYY-MM-DD format.
 * @returns {string}
 */
function getTodayString() {
  const now = new Date();
  const y = now.getFullYear();
  const m = String(now.getMonth() + 1).padStart(2, '0');
  const d = String(now.getDate()).padStart(2, '0');
  return `${y}-${m}-${d}`;
}

export default async function (opts) {
  const logsDir = opts.logsDir || 'logs';
  const retentionDays = opts.retentionDays ?? 7;
  const filenamePrefix = opts.filenamePrefix || 'streamloop-';

  // Ensure logs directory exists
  mkdirSync(logsDir, { recursive: true });

  // Cleanup old files on startup
  cleanupOldFiles(logsDir, filenamePrefix, retentionDays);

  let currentDate = getTodayString();
  let stream = createWriteStream(
    join(logsDir, `${filenamePrefix}${currentDate}.log`),
    { flags: 'a' }
  );

  return build(async function (source) {
    for await (const obj of source) {
      // Check for date rollover
      const today = getTodayString();
      if (today !== currentDate) {
        stream.end();
        currentDate = today;
        cleanupOldFiles(logsDir, filenamePrefix, retentionDays);
        stream = createWriteStream(
          join(logsDir, `${filenamePrefix}${currentDate}.log`),
          { flags: 'a' }
        );
      }

      const line = formatLogLine(obj);
      stream.write(line + '\n');
    }
  }, {
    close() {
      stream.end();
    },
  });
}
