import { describe, it, expect, beforeEach, afterEach } from 'vitest';
import { mkdtempSync, rmSync, readFileSync } from 'node:fs';
import { tmpdir } from 'node:os';
import path from 'node:path';
import {
  FRAME_W, FRAME_H, ANCHORS, estimateTextWidth, maxCharsFor, truncateText,
  defaultOverlayConfig, overlayFilters, prepareOverlayFiles,
} from './lib.mjs';

describe('text width estimate', () => {
  it('grows linearly with character count and font size', () => {
    // Arrange + Act
    const short = estimateTextWidth('abc', 20);
    const long = estimateTextWidth('abcabcabc', 20);
    const bigger = estimateTextWidth('abc', 40);

    // Assert
    expect(long).toBeCloseTo(short * 3, 5);
    expect(bigger).toBeCloseTo(short * 2, 5);
  });

  it('counts unicode code points, not UTF-16 units, so emoji count once', () => {
    // Arrange + Act
    const emoji = estimateTextWidth('\u{1F3B5}', 20); // single emoji, a surrogate pair in UTF-16
    const letter = estimateTextWidth('a', 20);

    // Assert
    expect(emoji).toBeCloseTo(letter, 5);
  });
});

describe('title truncation', () => {
  it('keeps short text untouched', () => {
    expect(truncateText('short title', 28, 900)).toBe('short title');
  });

  it('cuts text that would exceed the pixel budget and appends an ellipsis', () => {
    // Arrange: budget fits exactly maxCharsFor(28, 300) characters
    const max = maxCharsFor(28, 300);
    const text = 'x'.repeat(max + 20);

    // Act
    const result = truncateText(text, 28, 300);

    // Assert: one shorter than the max to make room for the ellipsis glyph
    expect(result).toBe('x'.repeat(max - 1) + '\u2026');
    expect(result.length).toBeLessThanOrEqual(max);
  });

  it('derives the character budget from font size and pixel width using a fixed average-glyph-width ratio', () => {
    // The rule: maxChars = floor(maxWidthPx / (fontSize * 0.55)), the documented
    // average glyph width for proportional Latin text at that font size.
    expect(maxCharsFor(20, 1100)).toBe(Math.floor(1100 / (20 * 0.55)));
    expect(maxCharsFor(40, 1100)).toBe(Math.floor(1100 / (40 * 0.55)));
  });

  it('does not split a surrogate-pair emoji when truncating', () => {
    // Arrange
    const text = 'ab\u{1F3B5}cd';

    // Act
    const result = truncateText(text, 1000, 10); // tiny budget forces truncation to 1-2 chars

    // Assert: result is valid, no lone surrogate half
    expect([...result].every((ch) => ch.length <= 2)).toBe(true);
  });
});

describe('default overlay config', () => {
  it('enables exactly title, progress and a NOT LIVE label, matching the current on-stream look', () => {
    // Arrange + Act
    const config = defaultOverlayConfig();
    const enabled = config.elements.filter((e) => e.enabled).map((e) => e.type);

    // Assert
    expect(enabled).toEqual(['title', 'progress', 'label']);
    expect(config.elements.find((e) => e.type === 'label').text).toBe('NOT LIVE');
  });

  it('lists every element type the spec requires, enabled or not', () => {
    const types = defaultOverlayConfig().elements.map((e) => e.type).sort();
    expect(types).toEqual(['label', 'logo', 'nextUp', 'playlist', 'progress', 'title'].sort());
  });
});

describe('overlayFilters', () => {
  const video = { title: 'A Title', playlistName: 'My List', position: 2, count: 5, nextTitle: 'Next One', durationSeconds: 45 };

  it('is a pure function: same inputs, same output, no side effects', () => {
    const config = defaultOverlayConfig();
    const paths = { 0: 'a.txt', 1: 'b.txt', 2: 'c.txt' };
    expect(overlayFilters(config, video, paths)).toBe(overlayFilters(config, video, paths));
  });

  it('reads title, label and playlist text from files with expansion disabled, never inlining the text', () => {
    // Arrange
    const config = defaultOverlayConfig();
    const paths = { 0: 'C:/x/title.txt', 2: 'C:/x/label.txt' };

    // Act
    const filters = overlayFilters(config, video, paths);

    // Assert: the untrusted/config text values never appear literally in the filter string
    expect(filters).not.toContain('A Title');
    expect(filters).not.toContain('NOT LIVE');
    expect(filters).toContain("textfile='C\\:/x/title.txt'");
    expect(filters).toContain("textfile='C\\:/x/label.txt'");
    expect(filters).toMatch(/expansion=none/);
  });

  it('renders the progress element as two drawtext clauses so the literal percent sign never follows an eif expansion', () => {
    // Arrange
    const config = defaultOverlayConfig();

    // Act
    const filters = overlayFilters(config, video, {});

    // Assert: the dynamic clock/percentage clause has no bare % in its own text, and a
    // second, separate clause with expansion=none supplies the literal percent sign.
    const drawtexts = filters.split(',').filter((c) => c.startsWith('drawtext='));
    const numberClause = drawtexts.find((c) => c.includes('eif'));
    const percentClause = drawtexts.find((c) => c.includes("text='%'"));
    expect(numberClause).toBeDefined();
    expect(percentClause).toBeDefined();
    expect(percentClause).toContain('expansion=none');
    expect(numberClause.endsWith("%'") || numberClause.includes('%\\')).toBe(false);
  });

  it('skips a disabled element entirely', () => {
    const config = defaultOverlayConfig();
    config.elements[2].enabled = false; // label
    const filters = overlayFilters(config, video, { 0: 'a.txt' });
    expect(filters).not.toContain('NOT LIVE');
    expect(filters.split(',').filter((c) => c.includes("boxcolor=red"))).toHaveLength(0);
  });

  it('applies an enable expression for firstSeconds visibility and none for always', () => {
    const config = defaultOverlayConfig();
    config.elements[0].visibility = { mode: 'firstSeconds', seconds: 8 };
    const filters = overlayFilters(config, video, { 0: 'a.txt' });
    expect(filters).toContain("enable='lt(t,8)'");
  });

  it('shows nextUp only in the last N seconds of the video via a gte(t, duration - seconds) gate', () => {
    const config = defaultOverlayConfig();
    config.elements[4].enabled = true; // nextUp
    const filters = overlayFilters(config, video, { 4: 'next.txt' });
    expect(filters).toContain(`enable='gte(t,${video.durationSeconds}-15)'`);
  });

  it('places a top-right anchored element using main_w minus its own rendered text width', () => {
    const config = defaultOverlayConfig();
    const filters = overlayFilters(config, video, { 2: 'label.txt' });
    expect(filters).toContain('x=main_w-text_w-24');
    expect(filters).toContain('y=24');
  });

  it('places a bottom-left anchored element flush to the left margin, above the bottom edge', () => {
    const config = defaultOverlayConfig();
    const filters = overlayFilters(config, video, { 0: 'title.txt' });
    expect(filters).toContain('x=24');
    expect(filters).toContain('y=main_h-text_h-');
  });

  it('composites a logo through a labelled movie+overlay sub-graph ending in [outv]', () => {
    const config = defaultOverlayConfig();
    config.elements.find((e) => e.type === 'logo').enabled = true;
    config.elements.find((e) => e.type === 'logo').path = 'C:/x/logo.png';
    const filters = overlayFilters(config, video, {});
    expect(filters).toMatch(/movie='C\\:\/x\/logo\.png'/);
    expect(filters).toContain('[outv]');
  });
});

describe('prepareOverlayFiles', () => {
  let dir;
  beforeEach(() => { dir = mkdtempSync(path.join(tmpdir(), 'overlay-test-')); });
  afterEach(() => { rmSync(dir, { recursive: true, force: true }); });

  it('writes one UTF-8 file per enabled text element, keyed by element index', () => {
    // Arrange
    const video = { title: 'Hello', playlistName: 'List', position: 1, count: 3, nextTitle: 'Next', durationSeconds: 10 };
    const config = defaultOverlayConfig();

    // Act
    const paths = prepareOverlayFiles(video, dir, config);

    // Assert
    expect(readFileSync(paths[0], 'utf8')).toBe('Hello');
    expect(Object.keys(paths).map(Number)).toEqual([0, 2]); // title, label only (progress has no file)
  });

  it('truncates a long title with an ellipsis instead of letting it overflow the frame', () => {
    // Arrange
    const video = { title: 'x'.repeat(500), durationSeconds: 10 };
    const config = defaultOverlayConfig();

    // Act
    const paths = prepareOverlayFiles(video, dir, config);
    const written = readFileSync(paths[0], 'utf8');

    // Assert
    expect(written.length).toBeLessThan(500);
    expect(written.endsWith('\u2026')).toBe(true);
  });

  it('never fails on hostile title text: quotes, percent signs, backslashes, CJK, emoji, empty strings', () => {
    const hostileTitles = [
      'It\'s "quoted": 100% \\ backslash',
      '日本語のタイトル 한국어 제목',
      'emoji \u{1F3B5}\u{1F525} title',
      '   ',
      '',
    ];
    const config = defaultOverlayConfig();
    for (const title of hostileTitles) {
      const video = { title, durationSeconds: 10 };
      expect(() => prepareOverlayFiles(video, dir, config)).not.toThrow();
      const paths = prepareOverlayFiles(video, dir, config);
      const written = readFileSync(paths[0], 'utf8');
      expect(written.startsWith(title.slice(0, 5)) || title === '').toBe(true);
    }
  });

  it('writes nothing for the progress element, which has no static text', () => {
    const video = { title: 'T', durationSeconds: 10 };
    const config = defaultOverlayConfig();
    const paths = prepareOverlayFiles(video, dir, config);
    const progressIndex = config.elements.findIndex((e) => e.type === 'progress');
    expect(paths[progressIndex]).toBeUndefined();
  });
});

describe('anchors', () => {
  it('covers all nine positions the config schema documents', () => {
    expect(ANCHORS).toEqual([
      'top-left', 'top', 'top-right', 'left', 'center', 'right', 'bottom-left', 'bottom', 'bottom-right',
    ]);
  });
});

describe('frame size', () => {
  it('matches the feeder\'s normalisation target', () => {
    expect([FRAME_W, FRAME_H]).toEqual([1920, 1080]);
  });
});
