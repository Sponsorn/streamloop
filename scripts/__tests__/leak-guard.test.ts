import { describe, it, expect } from 'vitest';
import { createHash } from 'crypto';
import { execFileSync } from 'child_process';
import { existsSync, readFileSync } from 'fs';
import { resolve, dirname, extname } from 'path';
import { fileURLToPath } from 'url';

const repoRoot = resolve(dirname(fileURLToPath(import.meta.url)), '..', '..');
const skippedExtensions = new Set([
  '.png', '.jpg', '.jpeg', '.gif', '.ico', '.webp', '.zip', '.exe', '.dll', '.mp4', '.mpg', '.lock', '.br',
]);
const skippedNames = new Set(['package-lock.json']);

interface LeakPattern {
  kind: string;
  pattern: RegExp;
}

interface Finding {
  kind: string;
  value: string;
}

const leakPatterns: readonly LeakPattern[] = [
  { kind: 'Google API key', pattern: /AIza[0-9A-Za-z_-]{35}/g },
  { kind: 'Twitch stream key', pattern: /live_\d{6,}_[A-Za-z0-9]{20,}/g },
  { kind: 'Twitch OAuth token', pattern: /oauth:[a-z0-9]{30}/g },
  { kind: 'Discord webhook', pattern: /discord(?:app)?\.com\/api\/webhooks\/\d+\/[A-Za-z0-9_-]{20,}/g },
  { kind: 'GitHub token', pattern: /\b(?:gh[pousr]_[A-Za-z0-9]{36,}|github_pat_[A-Za-z0-9_]{22,})/g },
  { kind: 'Anthropic key', pattern: /sk-ant-[A-Za-z0-9_-]{20,}/g },
  { kind: 'private key', pattern: /-----BEGIN [A-Z ]*PRIVATE KEY-----/g },
];

// Public values the repo must hold, by SHA-256, so this file holds no key-shaped string.
const allowed = new Map<string, string>([
  [
    '2bea43e3f40eb708710f40552321c9486c28bca9b61ac45eac3afacf6006381f',
    "YouTube's public web API key, sent with every BotGuard request (tokenserver/src/google.rs, the vendored rustypipe-botguard)",
  ],
]);

function findLeaks(text: string): Finding[] {
  return leakPatterns.flatMap(({ kind, pattern }) =>
    [...text.matchAll(pattern)].map((match) => ({ kind, value: match[0] })),
  );
}

function isAllowed(value: string): boolean {
  return allowed.has(createHash('sha256').update(value).digest('hex'));
}

// Samples are joined at run time, so neither GitHub nor this guard sees a key in the source.
const sample = (...parts: string[]): string => parts.join('');

describe('tracked files', () => {
  it('contain no key, token or webhook URL other than the allowed public ones', () => {
    // Arrange
    const tracked = execFileSync('git', ['ls-files'], { cwd: repoRoot, encoding: 'utf8' })
      .split('\n')
      .filter(Boolean)
      .filter((path) => !skippedExtensions.has(extname(path).toLowerCase()))
      .filter((path) => !skippedNames.has(path.split('/').pop() ?? ''))
      .map((path) => resolve(repoRoot, path))
      .filter((path) => existsSync(path));

    // Act
    const hits = tracked.flatMap((file) =>
      readFileSync(file, 'utf8')
        .split(/\r?\n/)
        .flatMap((line, index) =>
          findLeaks(line)
            .filter((finding) => !isAllowed(finding.value))
            .map((finding) => `${file}:${index + 1}: ${finding.kind}`),
        ),
    );

    // Assert
    expect(hits, `Keys or tokens found (values not shown):\n${hits.join('\n')}`).toEqual([]);
  });
});

describe('the leak patterns', () => {
  it.each([
    ['Google API key', sample('AIza', 'Sy', 'A1b2C3d4'.repeat(4), 'x')],
    ['Twitch stream key', sample('live_', '123456789', '_', 'Ab3'.repeat(10))],
    ['Twitch OAuth token', sample('oauth', ':', 'ab3'.repeat(10))],
    ['Discord webhook', sample('https://discord.com/api/', 'webhooks/', '1234567890', '/', 'Ab3_'.repeat(15))],
    ['GitHub token', sample('gh', 'p_', 'Ab3'.repeat(12))],
    ['Anthropic key', sample('sk-', 'ant-', 'api03-', 'Ab3'.repeat(10))],
    ['private key', sample('-----BEGIN RSA ', 'PRIVATE KEY-----')],
  ])('flag a %s', (kind, value) => {
    // Act
    const found = findLeaks(`const value = "${value}";`);

    // Assert
    expect(found.map((finding) => finding.kind)).toEqual([kind]);
  });

  it('let the short stream-key placeholders of the Rust tests through', () => {
    // Arrange
    const line = `${sample('live_', '123456', '_', 'SECRETsecret')} ${sample('live_', '987654', '_', 'SeCrEt')}`;

    // Act
    const found = findLeaks(line);

    // Assert
    expect(found).toEqual([]);
  });
});
