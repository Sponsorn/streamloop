import pino from 'pino';
import { resolve, dirname } from 'path';
import { fileURLToPath } from 'url';

const __dirname = dirname(fileURLToPath(import.meta.url));
const logsDir = resolve(__dirname, '..', '..', 'logs');

export const logger = pino({
  transport: {
    targets: [
      {
        target: 'pino/file',
        options: { destination: 1 }, // stdout
        level: process.env.LOG_LEVEL ?? 'info',
      },
      {
        target: resolve(__dirname, 'file-log-transport.js'),
        options: {
          logsDir,
          retentionDays: 7,
          filenamePrefix: 'streamloop-',
        },
        level: process.env.LOG_LEVEL ?? 'info',
      },
    ],
  },
  level: process.env.LOG_LEVEL ?? 'info',
});
