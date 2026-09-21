import { mkdirSync, writeFileSync } from 'node:fs';
import path from 'node:path';

export const FRAME_W = 1920;
export const FRAME_H = 1080;
export const ANCHORS = ['top-left', 'top', 'top-right', 'left', 'center', 'right', 'bottom-left', 'bottom', 'bottom-right'];
export const DEFAULT_FONT = 'C:/Windows/Fonts/segoeui.ttf';

// Average glyph width for proportional Latin text (Segoe UI) relative to font size,
// eyeballed against real rendered frames. Used only to budget space; not exact per-glyph metrics.
const CHAR_WIDTH_RATIO = 0.55;

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

// ffmpeg's filtergraph parser treats ':' as the option separator and '\' as its escape,
// so a Windows path needs forward slashes and an escaped drive-letter colon.
function escapePath(p) {
  return p.replace(/\\/g, '/').replace(/:/g, '\\:');
}

function q(s) {
  return `'${s}'`;
}

// A literal colon inside a filter option VALUE (not a file) needs the same escape as a path.
function escapeColons(s) {
  return String(s).replace(/:/g, '\\:');
}

function fmtHms(totalSeconds) {
  const s = Math.max(0, Math.round(totalSeconds));
  const hh = String(Math.floor(s / 3600)).padStart(2, '0');
  const mm = String(Math.floor((s % 3600) / 60)).padStart(2, '0');
  const ss = String(s % 60).padStart(2, '0');
  return `${hh}:${mm}:${ss}`;
}

export function defaultOverlayConfig() {
  return {
    elements: [
      { type: 'title', enabled: true, anchor: 'bottom-left', margin: 76, fontSize: 28, color: 'white', box: { on: true, color: 'black', opacity: 0.55 }, visibility: { mode: 'always' } },
      { type: 'progress', enabled: true, anchor: 'bottom-left', margin: 24, fontSize: 22, color: 'white', box: { on: true, color: 'black', opacity: 0.55 }, visibility: { mode: 'always' } },
      { type: 'label', enabled: true, text: 'NOT LIVE', anchor: 'top-right', margin: 24, fontSize: 22, color: 'white', box: { on: true, color: 'red', opacity: 0.8 }, visibility: { mode: 'always' } },
      { type: 'playlist', enabled: false, anchor: 'top-left', margin: 24, fontSize: 20, color: 'white', box: { on: true, color: 'black', opacity: 0.55 }, visibility: { mode: 'always' } },
      { type: 'nextUp', enabled: false, anchor: 'bottom-right', margin: 24, fontSize: 20, color: 'white', box: { on: true, color: 'black', opacity: 0.55 }, visibility: { mode: 'lastSeconds', seconds: 15 } },
      { type: 'logo', enabled: false, anchor: 'top-left', margin: 24, path: '', height: 60, visibility: { mode: 'always' } },
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

/** Literal pixel x,y for a block of known width/height, anchored by name. Used for the
 *  progress element, whose fixed-format text has a width we can estimate ourselves in JS
 *  instead of needing ffmpeg's per-frame text_w (which no sibling filter can read). */
function anchorBox(anchor, margin, w, h) {
  let x;
  if (anchor.endsWith('left') || anchor === 'left') x = margin;
  else if (anchor.endsWith('right') || anchor === 'right') x = FRAME_W - margin - w;
  else x = Math.round((FRAME_W - w) / 2);
  let y;
  if (anchor.startsWith('top')) y = margin;
  else if (anchor.startsWith('bottom')) y = FRAME_H - margin - h;
  else y = Math.round((FRAME_H - h) / 2);
  return { x, y };
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

function textClause({ font, textPath, x, y, fontSize, color, box, enable }) {
  const parts = [
    `fontfile=${q(escapePath(font))}`,
    `textfile=${q(escapePath(textPath))}`,
    'expansion=none',
    `fontsize=${fontSize}`, `fontcolor=${color}`, `x=${x}`, `y=${y}`,
    ...boxParts(box),
  ];
  if (enable) parts.push(`enable=${q(enable)}`);
  return `drawtext=${parts.join(':')}`;
}

/** Two drawtext clauses: a dynamic clock+percentage (expansion=normal, no bare '%' in its
 *  own text) and a literal '%' with expansion=none. Splitting them is the fix for the
 *  known "Stray %" trap: any '%' after a %{...} expansion breaks normal-mode parsing,
 *  in every quoting/escaping variant tried (%%, \%, \\%) - only a sibling drawtext with
 *  expansion=none renders a literal percent sign reliably. */
function progressClauses({ font, video, el, enable }) {
  const totalHms = escapeColons(fmtHms(video.durationSeconds));
  const numberText = `%{pts\\:hms} / ${totalHms}  %{eif\\:100*t/${video.durationSeconds}\\:d\\:3}`;
  const widthSample = `00:00:00 / ${fmtHms(video.durationSeconds)}  000`;
  const numW = estimateTextWidth(widthSample, el.fontSize);
  const pctW = estimateTextWidth('%', el.fontSize);
  const gap = 4;
  const h = Math.round(el.fontSize * 1.3);
  const { x, y } = anchorBox(el.anchor, el.margin, numW + gap + pctW, h);
  const numX = x;
  const pctX = x + numW + gap;
  const clauses = [];
  if (el.box?.on) {
    const pad = 8;
    clauses.push(`drawbox=x=${Math.round(numX - pad)}:y=${Math.round(y - pad)}:w=${Math.round(numW + gap + pctW + 2 * pad)}:h=${h + 2 * pad}:color=${el.box.color}@${el.box.opacity ?? 0.5}:t=fill${enable ? `:enable=${q(enable)}` : ''}`);
  }
  const common = (extra) => {
    const parts = [`fontfile=${q(escapePath(font))}`, ...extra, `fontsize=${el.fontSize}`, `fontcolor=${el.color}`];
    if (enable) parts.push(`enable=${q(enable)}`);
    return parts;
  };
  clauses.push(`drawtext=${common([`text=${q(numberText)}`, `x=${Math.round(numX)}`, `y=${Math.round(y)}`]).join(':')}`);
  clauses.push(`drawtext=${common(['text=\'%\'', 'expansion=none', `x=${Math.round(pctX)}`, `y=${Math.round(y)}`]).join(':')}`);
  return clauses;
}

/** Pure: builds the video filter chain (or filter-script content, for a logo) to append
 *  after the feeder's normalisation. Reads text from `paths` (from prepareOverlayFiles);
 *  never receives raw title/label strings itself. */
export function overlayFilters(config, video, paths) {
  const font = config.fontFile ?? DEFAULT_FONT;
  const clauses = [];
  const logos = [];
  config.elements.forEach((el, i) => {
    if (!el.enabled) return;
    const enable = enableExpr(el.visibility, video.durationSeconds);
    if (el.type === 'progress') {
      clauses.push(...progressClauses({ font, video, el, enable }));
    } else if (el.type === 'logo') {
      logos.push({ el, enable });
    } else {
      const textPath = paths[i];
      if (textPath === undefined) return;
      const [x, y] = anchorExpr(el.anchor, el.margin);
      clauses.push(textClause({ font, textPath, x, y, fontSize: el.fontSize, color: el.color, box: el.box, enable }));
    }
  });
  const mainChain = clauses.join(',');
  if (logos.length === 0) return mainChain;

  const parts = [];
  logos.forEach(({ el }, n) => parts.push(`movie=${q(escapePath(el.path))},scale=-1:${el.height}[logo${n}]`));
  parts.push(`${mainChain}[base]`);
  let prevLabel = 'base';
  logos.forEach(({ el, enable }, n) => {
    const [x, y] = anchorExpr(el.anchor, el.margin, 'overlay_w', 'overlay_h');
    const outLabel = n === logos.length - 1 ? 'outv' : `merged${n}`;
    const enablePart = enable ? `:enable=${q(enable)}` : '';
    parts.push(`[${prevLabel}][logo${n}]overlay=x=${x}:y=${y}${enablePart}[${outLabel}]`);
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

/** Writes the UTF-8 text files overlayFilters' textfile= clauses read, one per enabled
 *  text-bearing element, truncated to fit (1920 - 2*margin) px at that element's font size.
 *  Untrusted YouTube text never touches the ffmpeg command line or filter script directly. */
export function prepareOverlayFiles(video, dir, config = defaultOverlayConfig()) {
  mkdirSync(dir, { recursive: true });
  const paths = {};
  config.elements.forEach((el, i) => {
    if (!el.enabled) return;
    let raw;
    if (el.type === 'title') raw = video.title ?? '';
    else if (el.type === 'playlist') raw = playlistLine(video);
    else if (el.type === 'nextUp') raw = video.nextTitle ? `Next: ${video.nextTitle}` : '';
    else if (el.type === 'label') raw = el.text ?? '';
    else return; // progress and logo have no static text file
    const budget = FRAME_W - 2 * el.margin;
    const truncated = truncateText(raw, el.fontSize, budget);
    const file = path.join(dir, `el-${i}.txt`);
    writeFileSync(file, truncated, 'utf8');
    paths[i] = file;
  });
  return paths;
}
