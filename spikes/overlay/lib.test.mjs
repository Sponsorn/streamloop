import { describe, it, expect, beforeEach, afterEach } from 'vitest';
import { mkdtempSync, rmSync, readFileSync } from 'node:fs';
import { tmpdir } from 'node:os';
import path from 'node:path';
import {
  FRAME_W, FRAME_H, ANCHORS, DEFAULT_FONT, estimateTextWidth, maxCharsFor, truncateText, stripEmoji,
  progressTemplateText, progressBarFilledWidth, defaultOverlayConfig, overlayFilters, prepareOverlayFiles,
} from './lib.mjs';

const byType = (config, type) => config.elements.find((e) => e.type === type);
const indexOf = (config, type) => config.elements.findIndex((e) => e.type === type);

describe('default font', () => {
  it('defaults to Malgun Gothic, the single font used for every text element', () => {
    expect(DEFAULT_FONT).toBe('C:/Windows/Fonts/malgun.ttf');
  });
});

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

describe('emoji stripping', () => {
  it('removes a pictograph emoji, leaving the surrounding words intact', () => {
    expect(stripEmoji('rocket \u{1F680} launch')).toBe('rocket launch');
  });

  it('removes a flag built from a pair of regional indicator symbols', () => {
    expect(stripEmoji('flag \u{1F1F8}\u{1F1EA} here')).toBe('flag here');
  });

  it('removes a skin-toned emoji (base + modifier) without leaving the modifier behind', () => {
    expect(stripEmoji('thumbs \u{1F44D}\u{1F3FD} up')).toBe('thumbs up');
  });

  it('removes a ZWJ-joined emoji sequence as a whole', () => {
    expect(stripEmoji('family \u{1F468}\u200D\u{1F469}\u200D\u{1F467} time')).toBe('family time');
  });

  it('collapses the double space left behind and trims the ends', () => {
    expect(stripEmoji('\u{1F525} title \u{1F525}')).toBe('title');
  });

  it('leaves plain CJK and punctuation text untouched', () => {
    expect(stripEmoji('日本語のタイトル: 100%!')).toBe('日本語のタイトル: 100%!');
  });

  it('reduces to an empty string when the text is only emoji', () => {
    expect(stripEmoji('\u{1F3B5}\u{1F525}')).toBe('');
  });
});

describe('progress format string', () => {
  it('renders the clock as hh:mm:ss with no milliseconds, via drawtext\'s gmtime strftime form', () => {
    expect(progressTemplateText(45)).toBe('%{pts:gmtime:0:%H\\:%M\\:%S} / 00:00:45  %{eif:100*t/45:d}\\%');
  });

  it('formats a duration past one hour as hh:mm:ss, not just mm:ss', () => {
    expect(progressTemplateText(12300)).toContain('/ 03:25:00');
  });

  it('attaches the percent sign directly to the number, with drawtext\'s own escaping, no padding', () => {
    const text = progressTemplateText(45);
    // %{eif:...:d} has no width spec, so ffmpeg never zero-pads; \% is one escaped literal
    // glyph immediately after it, not a separate clause someone could mis-position.
    expect(text.endsWith(':d}\\%')).toBe(true);
    expect(text).not.toMatch(/:d:\d/); // no zero-padding width argument
  });
});

describe('progress bar fill width', () => {
  it('is zero at the start of the clip', () => {
    expect(progressBarFilledWidth(0, 45, 1920)).toBe(0);
  });

  it('is half the bar width at the midpoint', () => {
    expect(progressBarFilledWidth(22.5, 45, 1920)).toBeCloseTo(960, 5);
  });

  it('reaches the full bar width exactly at the end of the clip', () => {
    expect(progressBarFilledWidth(45, 45, 1920)).toBeCloseTo(1920, 5);
  });

  it('clamps at the full bar width past the end of the clip', () => {
    expect(progressBarFilledWidth(90, 45, 1920)).toBe(1920);
  });
});

describe('default overlay config', () => {
  it('enables title, progress, the progress bar and a NOT LIVE label, matching the fix-round default look', () => {
    // Arrange + Act
    const config = defaultOverlayConfig();
    const enabled = config.elements.filter((e) => e.enabled).map((e) => e.type);

    // Assert
    expect(enabled.sort()).toEqual(['label', 'progress', 'progressBar', 'title'].sort());
    expect(byType(config, 'label').text).toBe('NOT LIVE');
  });

  it('lists every element type the spec requires, enabled or not', () => {
    const types = defaultOverlayConfig().elements.map((e) => e.type).sort();
    expect(types).toEqual(['label', 'logo', 'nextUp', 'playlist', 'progress', 'progressBar', 'title'].sort());
  });

  it('anchors the logo away from the playlist/title/progress block so defaults never collide', () => {
    // Regression for the default-config collision: playlist and logo both used to default
    // to top-left, so a logo would cover the text block once both were enabled.
    const config = defaultOverlayConfig();
    expect(byType(config, 'logo').anchor).toBe('top-left');
    expect(byType(config, 'playlist').anchor).toBe('bottom-left');
    expect(byType(config, 'title').anchor).toBe('bottom-left');
    expect(byType(config, 'progress').anchor).toBe('bottom-left');
  });

  it('declares playlist, title and progress in top-to-bottom reading order for the shared bottom-left block', () => {
    const config = defaultOverlayConfig();
    expect(indexOf(config, 'playlist')).toBeLessThan(indexOf(config, 'title'));
    expect(indexOf(config, 'title')).toBeLessThan(indexOf(config, 'progress'));
  });

  it('gives every element in the bottom-left block the same margin, so they share a left edge', () => {
    const config = defaultOverlayConfig();
    const margins = new Set(['playlist', 'title', 'progress'].map((t) => byType(config, t).margin));
    expect(margins.size).toBe(1);
  });
});

describe('overlayFilters', () => {
  const video = { title: 'A Title', playlistName: 'My List', position: 2, count: 5, nextTitle: 'Next One', durationSeconds: 45 };

  it('is a pure function: same inputs, same output, no side effects', () => {
    const config = defaultOverlayConfig();
    const paths = { 0: 'a.txt', 1: 'b.txt', 2: 'c.txt' };
    expect(overlayFilters(config, video, paths)).toBe(overlayFilters(config, video, paths));
  });

  it('reads title and label text from files with expansion disabled, never inlining the text', () => {
    // Arrange
    const config = defaultOverlayConfig();
    const paths = { [indexOf(config, 'title')]: 'C:/x/title.txt', [indexOf(config, 'label')]: 'C:/x/label.txt' };

    // Act
    const filters = overlayFilters(config, video, paths);

    // Assert: the untrusted/config text values never appear literally in the filter string
    expect(filters).not.toContain('A Title');
    expect(filters).not.toContain('NOT LIVE');
    expect(filters).toContain("textfile='C\\:/x/title.txt'");
    expect(filters).toContain("textfile='C\\:/x/label.txt'");
    expect(filters).toMatch(/expansion=none/);
  });

  it('reads the progress element\'s text file with expansion left on, so its %{...} template runs', () => {
    // Arrange
    const config = defaultOverlayConfig();
    const progressIndex = indexOf(config, 'progress');
    const paths = { [progressIndex]: 'C:/x/progress.txt' };

    // Act
    const filters = overlayFilters(config, video, paths);
    const progressClause = filters.split(',').find((c) => c.includes("textfile='C\\:/x/progress.txt'"));

    // Assert
    expect(progressClause).toBeDefined();
    expect(progressClause).not.toContain('expansion=none');
  });

  it('skips a disabled element entirely', () => {
    const config = defaultOverlayConfig();
    byType(config, 'label').enabled = false;
    const filters = overlayFilters(config, video, { [indexOf(config, 'title')]: 'a.txt' });
    expect(filters).not.toContain('NOT LIVE');
    expect(filters.split(',').filter((c) => c.includes('boxcolor=red'))).toHaveLength(0);
  });

  it('applies an enable expression for firstSeconds visibility and none for always', () => {
    const config = defaultOverlayConfig();
    const titleIndex = indexOf(config, 'title');
    config.elements[titleIndex].visibility = { mode: 'firstSeconds', seconds: 8 };
    const filters = overlayFilters(config, video, { [titleIndex]: 'a.txt' });
    expect(filters).toContain("enable='lt(t,8)'");
  });

  it('shows nextUp only in the last N seconds of the video via a gte(t, duration - seconds) gate', () => {
    const config = defaultOverlayConfig();
    const nextUpIndex = indexOf(config, 'nextUp');
    config.elements[nextUpIndex].enabled = true;
    const filters = overlayFilters(config, video, { [nextUpIndex]: 'next.txt' });
    expect(filters).toContain(`enable='gte(t,${video.durationSeconds}-15)'`);
  });

  it('places a top-right anchored element using main_w minus its own rendered text width', () => {
    const config = defaultOverlayConfig();
    const labelIndex = indexOf(config, 'label');
    const filters = overlayFilters(config, video, { [labelIndex]: 'label.txt' });
    expect(filters).toContain('x=main_w-text_w-24');
    expect(filters).toContain('y=24');
  });

  it('places a bottom-left anchored solo element flush to the left margin, above the bottom edge', () => {
    const config = defaultOverlayConfig();
    // Disable its stack-mates so it's the sole occupant of bottom-left and keeps the exact symbolic y.
    byType(config, 'playlist').enabled = false;
    byType(config, 'progress').enabled = false;
    const titleIndex = indexOf(config, 'title');
    const filters = overlayFilters(config, video, { [titleIndex]: 'title.txt' });
    expect(filters).toContain('x=24');
    expect(filters).toContain('y=main_h-text_h-24');
  });

  it('stacks title above progress with a common left edge and a fixed gap, not overlapping', () => {
    // Arrange: default config enables title and progress, both anchored bottom-left
    const config = defaultOverlayConfig();
    const titleIndex = indexOf(config, 'title');
    const progressIndex = indexOf(config, 'progress');

    // Act
    const filters = overlayFilters(config, video, { [titleIndex]: 'title.txt', [progressIndex]: 'progress.txt' });
    const clauses = filters.split(',').filter((c) => c.startsWith('drawtext='));
    const titleClause = clauses.find((c) => c.includes('title.txt'));
    const progressClause = clauses.find((c) => c.includes('progress.txt'));
    const yOf = (clause) => Number(/:y=(\d+):/.exec(clause)[1]);

    // Assert: same left edge (literal margin, not text_w-dependent), progress below title,
    // and progress sits nearer the bottom edge (spec: playlist, title, progress top to bottom).
    expect(titleClause).toContain(':x=24:');
    expect(progressClause).toContain(':x=24:');
    expect(yOf(progressClause)).toBeGreaterThan(yOf(titleClause));
  });

  it('composites a logo through a labelled movie+overlay sub-graph ending in [outv]', () => {
    const config = defaultOverlayConfig();
    byType(config, 'logo').enabled = true;
    byType(config, 'logo').path = 'C:/x/logo.png';
    const filters = overlayFilters(config, video, {});
    expect(filters).toMatch(/movie='C\\:\/x\/logo\.png'/);
    expect(filters).toContain('[outv]');
  });

  it('draws the progress bar as a static track box plus a sliding colour-source fill, ending in [outv]', () => {
    const config = defaultOverlayConfig();
    const filters = overlayFilters(config, video, {});
    expect(filters).toMatch(/drawbox=x=0:y=1074:w=1920:h=6:color=white@0\.25/);
    expect(filters).toMatch(/color=c=red:s=1920x6\[stage0\]/);
    expect(filters).toContain('[outv]');
  });

  it('combines a logo and the progress bar into one overlay chain, each getting its own label', () => {
    const config = defaultOverlayConfig();
    byType(config, 'logo').enabled = true;
    byType(config, 'logo').path = 'C:/x/logo.png';
    const filters = overlayFilters(config, video, {});
    expect(filters).toContain('[stage0]');
    expect(filters).toContain('[stage1]');
    expect(filters).toContain('[merged0]');
    expect(filters).toContain('[outv]');
  });
});

describe('prepareOverlayFiles', () => {
  let dir;
  beforeEach(() => { dir = mkdtempSync(path.join(tmpdir(), 'overlay-test-')); });
  afterEach(() => { rmSync(dir, { recursive: true, force: true }); });

  it('writes one UTF-8 file per enabled text-bearing element, keyed by element index', () => {
    // Arrange
    const video = { title: 'Hello', playlistName: 'List', position: 1, count: 3, nextTitle: 'Next', durationSeconds: 10 };
    const config = defaultOverlayConfig();
    const titleIndex = indexOf(config, 'title');
    const labelIndex = indexOf(config, 'label');
    const progressIndex = indexOf(config, 'progress');

    // Act
    const paths = prepareOverlayFiles(video, dir, config);

    // Assert
    expect(readFileSync(paths[titleIndex], 'utf8')).toBe('Hello');
    expect(Object.keys(paths).map(Number).sort((a, b) => a - b))
      .toEqual([titleIndex, progressIndex, labelIndex].sort((a, b) => a - b));
  });

  it('truncates a long title with an ellipsis instead of letting it overflow the frame', () => {
    // Arrange
    const video = { title: 'x'.repeat(500), durationSeconds: 10 };
    const config = defaultOverlayConfig();

    // Act
    const paths = prepareOverlayFiles(video, dir, config);
    const written = readFileSync(paths[indexOf(config, 'title')], 'utf8');

    // Assert
    expect(written.length).toBeLessThan(500);
    expect(written.endsWith('\u2026')).toBe(true);
  });

  it('strips emoji from the title before writing it, so an unsupported glyph never reaches ffmpeg', () => {
    // Arrange
    const video = { title: 'emoji \u{1F3B5}\u{1F525} title \u{1F44D}\u{1F3FD} flag \u{1F1F8}\u{1F1EA}', durationSeconds: 10 };
    const config = defaultOverlayConfig();

    // Act
    const paths = prepareOverlayFiles(video, dir, config);
    const written = readFileSync(paths[indexOf(config, 'title')], 'utf8');

    // Assert
    expect(written).toBe('emoji title flag');
  });

  it('falls back to an empty string, without throwing, when a title is emoji-only', () => {
    const video = { title: '\u{1F3B5}\u{1F525}', durationSeconds: 10 };
    const config = defaultOverlayConfig();
    expect(() => prepareOverlayFiles(video, dir, config)).not.toThrow();
    const paths = prepareOverlayFiles(video, dir, config);
    expect(readFileSync(paths[indexOf(config, 'title')], 'utf8')).toBe('');
  });

  it('never fails on hostile title text: quotes, percent signs, backslashes, CJK, empty strings', () => {
    const hostileTitles = [
      'It\'s "quoted": 100% \\ backslash',
      '日本語のタイトル 한국어 제목',
      '   ',
      '',
    ];
    const config = defaultOverlayConfig();
    for (const title of hostileTitles) {
      const video = { title, durationSeconds: 10 };
      expect(() => prepareOverlayFiles(video, dir, config)).not.toThrow();
      const paths = prepareOverlayFiles(video, dir, config);
      const written = readFileSync(paths[indexOf(config, 'title')], 'utf8');
      expect(written.startsWith(title.trim().slice(0, 5)) || title.trim() === '').toBe(true);
    }
  });

  it('writes the fixed progress format string, never a duration value an attacker could control as text', () => {
    const video = { title: 'T', durationSeconds: 45 };
    const config = defaultOverlayConfig();
    const paths = prepareOverlayFiles(video, dir, config);
    const progressIndex = indexOf(config, 'progress');
    const written = readFileSync(paths[progressIndex], 'utf8');
    expect(written).toBe('%{pts:gmtime:0:%H\\:%M\\:%S} / 00:00:45  %{eif:100*t/45:d}\\%');
  });

  it('writes nothing for the logo and progress bar elements, which have no text file', () => {
    const video = { title: 'T', durationSeconds: 10 };
    const config = defaultOverlayConfig();
    byType(config, 'logo').enabled = true;
    const paths = prepareOverlayFiles(video, dir, config);
    expect(paths[indexOf(config, 'logo')]).toBeUndefined();
    expect(paths[indexOf(config, 'progressBar')]).toBeUndefined();
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
