import { mkdirSync, writeFileSync } from 'node:fs';
import path from 'node:path';

export const FRAME_W = 1920;
export const FRAME_H = 1080;
export const ANCHORS = ['top-left', 'top', 'top-right', 'left', 'center', 'right', 'bottom-left', 'bottom', 'bottom-right'];
export const DEFAULT_FONT = 'C:/Windows/Fonts/malgun.ttf';

// Average glyph width for Malgun Gothic as a fraction of font size, tuned against rendered frames.
const CHAR_WIDTH_RATIO = 0.55;
// Vertical space a single line of text needs, as a multiple of font size (ascent+descent+leading).
const LINE_HEIGHT_RATIO = 1.3;
// Gap between stacked elements that share an anchor, in pixels.
const STACK_GAP = 10;

export function estimateTextWidth(text, fontSize) {
  return [...text].length * fontSize * CHAR_WIDTH_RATIO;
}

export function maxCharsFor(fontSize, maxWidthPx) {
  return Math.max(1, Math.floor(maxWidthPx / (fontSize * CHAR_WIDTH_RATIO)));
}

export function truncateText(text, fontSize, maxWidthPx) {
  const chars = [...text];
  const max = maxCharsFor(fontSize, maxWidthPx);
  if (chars.length <= max) return text;
  return `${chars.slice(0, Math.max(0, max - 1)).join('')}\u2026`;
}

// Extended_Pictographic covers emoji pictographs; the rest are the modifier characters that
// combine with them (variation selectors, ZWJ sequences, skin tones, two-symbol flag pairs).
const EMOJI_RE = /\p{Extended_Pictographic}|[\u{FE0E}\u{FE0F}\u{200D}]|[\u{1F3FB}-\u{1F3FF}]|[\u{1F1E6}-\u{1F1FF}]{2}/gu;

export function stripEmoji(text) {
  return String(text).replace(EMOJI_RE, '').replace(/ {2,}/g, ' ').trim();
}

// ':' is the filtergraph option separator, so a Windows path needs forward slashes and an escaped drive colon.
function escapePath(p) {
  return p.replace(/\\/g, '/').replace(/:/g, '\\:');
}

function q(s) {
  return `'${s}'`;
}

function fmtHms(totalSeconds) {
  const s = Math.max(0, Math.round(totalSeconds));
  const hh = String(Math.floor(s / 3600)).padStart(2, '0');
  const mm = String(Math.floor((s % 3600) / 60)).padStart(2, '0');
  const ss = String(s % 60).padStart(2, '0');
  return `${hh}:${mm}:${ss}`;
}

/** Expansion stays on (not none): this is our own fixed format string, never untrusted
 *  text. Its colons and '%' use drawtext's own backslash escaping, not filtergraph escaping. */
export function progressTemplateText(durationSeconds) {
  const total = fmtHms(durationSeconds);
  return `%{pts:gmtime:0:%H\\:%M\\:%S} / ${total}  %{eif:100*t/${durationSeconds}:d}\\%`;
}

export function defaultOverlayConfig() {
  return {
    elements: [
      { type: 'playlist', enabled: false, anchor: 'bottom-left', margin: 24, fontSize: 20, color: 'white', box: { on: true, color: 'black', opacity: 0.55 }, visibility: { mode: 'always' } },
      { type: 'title', enabled: true, anchor: 'bottom-left', margin: 24, fontSize: 28, color: 'white', box: { on: true, color: 'black', opacity: 0.55 }, visibility: { mode: 'always' } },
      { type: 'progress', enabled: true, anchor: 'bottom-left', margin: 24, fontSize: 22, color: 'white', box: { on: true, color: 'black', opacity: 0.55 }, visibility: { mode: 'always' } },
      { type: 'label', enabled: true, text: 'NOT LIVE', anchor: 'top-right', margin: 24, fontSize: 22, color: 'white', box: { on: true, color: 'red', opacity: 0.8 }, visibility: { mode: 'always' } },
      { type: 'nextUp', enabled: false, anchor: 'bottom-right', margin: 24, fontSize: 20, color: 'white', box: { on: true, color: 'black', opacity: 0.55 }, visibility: { mode: 'lastSeconds', seconds: 15 } },
      { type: 'logo', enabled: false, anchor: 'top-left', margin: 24, path: '', height: 60, visibility: { mode: 'always' } },
      { type: 'progressBar', enabled: true, edge: 'bottom', height: 6, margin: 0, color: 'red', trackColor: { color: 'white', opacity: 0.25 }, visibility: { mode: 'always' } },
    ],
  };
}

/** x,y expressions for a single-segment drawtext/overlay element, anchored by name.
 *  widthVar/heightVar let the same nine formulas serve drawtext (text_w/text_h) and overlay (overlay_w/overlay_h). */
function anchorExpr(anchor, margin, widthVar = 'text_w', heightVar = 'text_h') {
  const left = `${margin}`;
  const right = `main_w-${widthVar}-${margin}`;
  const centerX = `(main_w-${widthVar})/2`;
  const top = `${margin}`;
  const bottom = `main_h-${heightVar}-${margin}`;
  const centerY = `(main_h-${heightVar})/2`;
  const table = {
    'top-left': [left, top], top: [centerX, top], 'top-right': [right, top],
    left: [left, centerY], center: [centerX, centerY], right: [right, centerY],
    'bottom-left': [left, bottom], bottom: [centerX, bottom], 'bottom-right': [right, bottom],
  };
  if (!(anchor in table)) throw new Error(`unknown anchor: ${anchor}`);
  return table[anchor];
}

/** ffmpeg can't read one drawtext filter's text_h from another, so a shared anchor is stacked
 *  here: estimated heights, declaration order top-to-bottom, one shared margin for a common edge.
 *  A lone occupant of an anchor is left alone, keeping anchorExpr's exact symbolic y. */
function stackedLayout(elements) {
  const groups = new Map();
  elements.forEach((el, i) => {
    if (!el.enabled || el.type === 'logo' || el.type === 'progressBar') return;
    if (!groups.has(el.anchor)) groups.set(el.anchor, []);
    groups.get(el.anchor).push(i);
  });
  const layout = {};
  for (const idxs of groups.values()) {
    const margin = elements[idxs[0]].margin;
    if (idxs.length === 1) { layout[idxs[0]] = { margin }; continue; }
    const anchor = elements[idxs[0]].anchor;
    const heights = idxs.map((i) => Math.round(elements[i].fontSize * LINE_HEIGHT_RATIO));
    const totalHeight = heights.reduce((a, b) => a + b, 0) + STACK_GAP * (idxs.length - 1);
    let cursor;
    if (anchor.startsWith('top')) cursor = margin;
    else if (anchor.startsWith('bottom')) cursor = FRAME_H - margin - totalHeight;
    else cursor = Math.round((FRAME_H - totalHeight) / 2);
    idxs.forEach((i, k) => {
      layout[i] = { margin, y: cursor };
      cursor += heights[k] + STACK_GAP;
    });
  }
  return layout;
}

function enableExpr(visibility, durationSeconds) {
  const mode = visibility?.mode ?? 'always';
  if (mode === 'always') return null;
  if (mode === 'firstSeconds') return `lt(t,${visibility.seconds})`;
  if (mode === 'every') return `lt(mod(t,${visibility.minutes * 60}),${visibility.forSeconds})`;
  if (mode === 'lastSeconds') return `gte(t,${durationSeconds}-${visibility.seconds})`;
  throw new Error(`unknown visibility mode: ${mode}`);
}

function boxParts(box) {
  if (!box?.on) return [];
  return ['box=1', `boxcolor=${box.color}@${box.opacity ?? 0.5}`, 'boxborderw=10'];
}

/** expandTemplate=true leaves expansion on for the progress element's fixed %{...} template.
 *  Every other element disables it: its text file may hold untrusted title/label text. */
function textClause({ font, textPath, x, y, fontSize, color, box, enable, expandTemplate }) {
  const parts = [
    `fontfile=${q(escapePath(font))}`,
    `textfile=${q(escapePath(textPath))}`,
    ...(expandTemplate ? [] : ['expansion=none']),
    `fontsize=${fontSize}`, `fontcolor=${color}`, `x=${x}`, `y=${y}`,
    ...boxParts(box),
  ];
  if (enable) parts.push(`enable=${q(enable)}`);
  return `drawtext=${parts.join(':')}`;
}

/** Bar geometry shared by the static track and the sliding fill. */
function progressBarGeometry(el) {
  const barWidth = FRAME_W - 2 * el.margin;
  const y = el.edge === 'top' ? 0 : FRAME_H - el.height;
  return { barWidth, y };
}

/** Mirrors the clamp in progressBarFillXExpr's ffmpeg expression, as a plain function so the
 *  growth/clamp rule is unit-testable without evaluating an ffmpeg expression string. */
export function progressBarFilledWidth(t, durationSeconds, barWidth) {
  return Math.max(0, Math.min(barWidth, barWidth * (t / durationSeconds)));
}

/** drawbox's w is evaluated once at init on this build (no eval=frame option), so a
 *  time-varying width won't grow. overlay's x IS per-frame, so the fill instead slides
 *  in from off-screen-left, cheaper than a per-pixel geq for a solid-color rectangle. */
function progressBarFillXExpr(barWidth, durationSeconds, margin) {
  return `${margin}+max(-${barWidth},min(0,${barWidth}*(t/${durationSeconds}-1)))`;
}

function progressBarTrackClause(el, enable) {
  const { barWidth, y } = progressBarGeometry(el);
  const track = el.trackColor ?? { color: 'white', opacity: 0.25 };
  const parts = [`x=${el.margin}`, `y=${y}`, `w=${barWidth}`, `h=${el.height}`, `color=${track.color}@${track.opacity ?? 0.25}`, 't=fill'];
  if (enable) parts.push(`enable=${q(enable)}`);
  return `drawbox=${parts.join(':')}`;
}

function progressBarFillStage(el, video, enable) {
  const { barWidth, y } = progressBarGeometry(el);
  return {
    source: `color=c=${el.color}:s=${barWidth}x${el.height}`,
    x: q(progressBarFillXExpr(barWidth, video.durationSeconds, el.margin)),
    y: `${y}`,
    enable,
  };
}

/** Pure: builds the video filter chain to append after the feeder's normalisation.
 *  Reads text from `paths` (from prepareOverlayFiles); never receives raw title/label strings. */
export function overlayFilters(config, video, paths) {
  const font = config.fontFile ?? DEFAULT_FONT;
  const layout = stackedLayout(config.elements);
  const clauses = [];
  const overlayStages = [];
  config.elements.forEach((el, i) => {
    if (!el.enabled) return;
    const enable = enableExpr(el.visibility, video.durationSeconds);
    if (el.type === 'logo') {
      overlayStages.push({
        source: `movie=${q(escapePath(el.path))},scale=-1:${el.height}`,
        ...(() => { const [x, y] = anchorExpr(el.anchor, el.margin, 'overlay_w', 'overlay_h'); return { x, y }; })(),
        enable,
      });
      return;
    }
    if (el.type === 'progressBar') {
      clauses.push(progressBarTrackClause(el, enable));
      overlayStages.push(progressBarFillStage(el, video, enable));
      return;
    }
    const textPath = paths[i];
    if (textPath === undefined) return;
    const group = layout[i] ?? { margin: el.margin };
    const [x, symbolicY] = anchorExpr(el.anchor, group.margin);
    const y = group.y !== undefined ? `${group.y}` : symbolicY;
    clauses.push(textClause({
      font, textPath, x, y, fontSize: el.fontSize, color: el.color, box: el.box, enable,
      expandTemplate: el.type === 'progress',
    }));
  });
  const mainChain = clauses.join(',');
  if (overlayStages.length === 0) return mainChain;

  // The main chain must come first: it's the only segment with no input label, so it implicitly
  // receives whatever the caller prepends. ffmpeg resolves labels wherever declared, so sources can follow.
  const parts = [`${mainChain}[base]`];
  overlayStages.forEach((stage, n) => parts.push(`${stage.source}[stage${n}]`));
  let prevLabel = 'base';
  overlayStages.forEach((stage, n) => {
    const outLabel = n === overlayStages.length - 1 ? 'outv' : `merged${n}`;
    const enablePart = stage.enable ? `:enable=${q(stage.enable)}` : '';
    parts.push(`[${prevLabel}][stage${n}]overlay=x=${stage.x}:y=${stage.y}${enablePart}[${outLabel}]`);
    prevLabel = outLabel;
  });
  return parts.join(';\n');
}

function playlistLine(video) {
  const name = video.playlistName ?? '';
  const pos = video.position ?? '?';
  const count = video.count ?? '?';
  return `${name} (${pos}/${count})`;
}

/** Writes the UTF-8 text files overlayFilters' textfile= clauses read, keyed by element index.
 *  Untrusted text is emoji-stripped and truncated to fit before it reaches a filtergraph. */
export function prepareOverlayFiles(video, dir, config = defaultOverlayConfig()) {
  mkdirSync(dir, { recursive: true });
  const paths = {};
  config.elements.forEach((el, i) => {
    if (!el.enabled) return;
    if (el.type === 'progress') {
      const file = path.join(dir, `el-${i}.txt`);
      writeFileSync(file, progressTemplateText(video.durationSeconds), 'utf8');
      paths[i] = file;
      return;
    }
    let raw;
    if (el.type === 'title') raw = video.title ?? '';
    else if (el.type === 'playlist') raw = playlistLine(video);
    else if (el.type === 'nextUp') raw = video.nextTitle ? `Next: ${video.nextTitle}` : '';
    else if (el.type === 'label') raw = el.text ?? '';
    else return; // logo and progressBar have no text file
    const budget = FRAME_W - 2 * el.margin;
    const truncated = truncateText(stripEmoji(raw), el.fontSize, budget);
    const file = path.join(dir, `el-${i}.txt`);
    writeFileSync(file, truncated, 'utf8');
    paths[i] = file;
  });
  return paths;
}
