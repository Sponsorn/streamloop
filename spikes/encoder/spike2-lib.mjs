import { lagStats, seamLag } from '../continuity/lib.mjs';

export const HARDWARE = ['h264_nvenc', 'h264_amf', 'h264_qsv'];
const VENDOR = { h264_nvenc: /nvidia/i, h264_amf: /amd|radeon/i, h264_qsv: /intel/i };

/** Spec name of the video-encode engine is engtype_VideoEncode, but AMD's VCN shows up as
 *  "Video Codec 0": it does decode too, which is fine while the feeders decode on the CPU. */
export const ENCODE_ENGINE = /engtype_video ?(encode|codec)/i;

/** PDH through Add-Type: PdhAddEnglishCounter takes the English counter names on any Windows
 *  language (Get-Counter and typeperf only take localised ones). No admin, nothing installed. */
export const SAMPLER_PS = String.raw`
$ErrorActionPreference = 'Stop'
Add-Type -TypeDefinition @'
using System;
using System.Collections.Generic;
using System.Runtime.InteropServices;
using System.Text;
public static class Sampler {
  [DllImport("pdh.dll", CharSet = CharSet.Unicode)] static extern uint PdhOpenQueryW(string source, IntPtr user, out IntPtr query);
  [DllImport("pdh.dll", CharSet = CharSet.Unicode)] static extern uint PdhAddEnglishCounterW(IntPtr query, string path, IntPtr user, out IntPtr counter);
  [DllImport("pdh.dll")] static extern uint PdhCollectQueryData(IntPtr query);
  [DllImport("pdh.dll", CharSet = CharSet.Unicode)] static extern uint PdhGetFormattedCounterArrayW(IntPtr counter, uint format, ref uint size, out uint count, IntPtr items);
  static IntPtr query;
  static readonly List<KeyValuePair<string, IntPtr>> counters = new List<KeyValuePair<string, IntPtr>>();
  public static string Open(string[] specs) {
    PdhOpenQueryW(null, IntPtr.Zero, out query);
    var errors = new List<string>();
    foreach (var spec in specs) {
      var parts = spec.Split(new[] { '=' }, 2);
      IntPtr counter;
      uint rc = PdhAddEnglishCounterW(query, parts[1], IntPtr.Zero, out counter);
      if (rc == 0) counters.Add(new KeyValuePair<string, IntPtr>(parts[0], counter));
      else errors.Add(parts[0] + " 0x" + rc.ToString("X8"));
    }
    PdhCollectQueryData(query);
    return string.Join(", ", errors);
  }
  public static string Collect() {
    PdhCollectQueryData(query);
    var sb = new StringBuilder("{\"s\":[");
    bool first = true;
    foreach (var kv in counters) {
      uint size = 0, count;
      // PDH_FMT_DOUBLE | PDH_FMT_NOCAP100. The first call only reports the size (PDH_MORE_DATA);
      // anything else means no instance matches right now.
      if (PdhGetFormattedCounterArrayW(kv.Value, 0x8200, ref size, out count, IntPtr.Zero) != 0x800007D2) continue;
      IntPtr buf = Marshal.AllocHGlobal((int)size);
      try {
        if (PdhGetFormattedCounterArrayW(kv.Value, 0x8200, ref size, out count, buf) != 0) continue;
        int stride = IntPtr.Size + 16;
        for (int i = 0; i < count; i++) {
          IntPtr item = buf + i * stride;
          if (Marshal.ReadInt32(item, IntPtr.Size) > 1) continue;
          double value = BitConverter.Int64BitsToDouble(Marshal.ReadInt64(item, IntPtr.Size + 8));
          // Most of the hundreds of GPU engine instances are idle. Process rows stay: they pair up by position.
          if (value == 0 && kv.Key == "gpu") continue;
          string name = Marshal.PtrToStringUni(Marshal.ReadIntPtr(item)).Replace("\\", "\\\\").Replace("\"", "\\\"");
          sb.Append(first ? "" : ",").Append("{\"k\":\"").Append(kv.Key).Append("\",\"i\":\"").Append(name)
            .Append("\",\"v\":").Append(value.ToString("R", System.Globalization.CultureInfo.InvariantCulture)).Append("}");
          first = false;
        }
      } finally { Marshal.FreeHGlobal(buf); }
    }
    return sb.Append("]}").ToString();
  }
}
'@
$errors = [Sampler]::Open(@(
  'tot=\Processor(_Total)\% Processor Time',
  'pid=\Process(ffmpeg*)\ID Process',
  'cpu=\Process(ffmpeg*)\% Processor Time',
  'ws=\Process(ffmpeg*)\Working Set - Private',
  'gpu=\GPU Engine(*)\Utilization Percentage'
))
if ($errors) { [Console]::Error.WriteLine("counters not added: $errors") }
while ($true) { Start-Sleep -Seconds 5; [Console]::Out.WriteLine([Sampler]::Collect()); [Console]::Out.Flush() }
`;

/** One sampler line to one samples.csv row. Process CPU is per core in PDH; divided by the
 *  core count it is a share of the whole machine, like the total. */
export function parseSample(line, { encoderPid, recorderPid, cores }) {
  const seen = {};
  // PDH names every ffmpeg instance plain "ffmpeg" (no #1), so the n-th "ffmpeg" of each process
  // counter is the same process: one collection lists the instances in one order.
  const s = JSON.parse(line).s.map((x) => {
    const n = (seen[`${x.k}|${x.i}`] = (seen[`${x.k}|${x.i}`] ?? -1) + 1);
    return { ...x, i: x.k === 'gpu' || x.k === 'tot' ? x.i : `${x.i}|${n}` };
  });
  const pidOf = Object.fromEntries(s.filter((x) => x.k === 'pid').map((x) => [x.i, x.v]));
  const row = { cpuTotal: 0, feederCpu: 0, encoderCpu: 0, encoderWsMb: null, encodeEngine: 0, encoderEngines: {} };
  for (const { k, i, v } of s) {
    if (k === 'tot') row.cpuTotal = v;
    else if (k === 'cpu' && pidOf[i] !== undefined) {
      if (pidOf[i] === encoderPid) row.encoderCpu = v / cores;
      else if (pidOf[i] !== recorderPid) row.feederCpu += v / cores;
    } else if (k === 'ws' && pidOf[i] === encoderPid) row.encoderWsMb = v / 2 ** 20;
    else if (k === 'gpu') {
      if (ENCODE_ENGINE.test(i)) row.encodeEngine += v;
      if (encoderPid && i.startsWith(`pid_${encoderPid}_`)) row.encoderEngines[i.replace(/.*engtype_/, '')] = v;
    }
  }
  return row;
}

export function stats(values) {
  const v = values.filter(Number.isFinite).sort((a, b) => a - b);
  if (!v.length) return { avg: NaN, p95: NaN, max: NaN, n: 0 };
  return { avg: v.reduce((a, b) => a + b, 0) / v.length, p95: v[Math.ceil(v.length * 0.95) - 1], max: v[v.length - 1], n: v.length };
}

const median = (values) => {
  const v = [...values].sort((a, b) => a - b);
  return v.length ? (v.length % 2 ? v[(v.length - 1) / 2] : (v[v.length / 2 - 1] + v[v.length / 2]) / 2) : NaN;
};

/** Working set in the last minute against the second minute (the first holds start-up). rows: { t (ms), encoderWsMb }. */
export function wsGrowthMb(rows) {
  const ws = rows.filter((r) => Number.isFinite(r.encoderWsMb));
  if (ws.length < 2) return NaN;
  const t0 = ws[0].t;
  const end = ws[ws.length - 1].t;
  const early = ws.filter((r) => r.t - t0 >= 60_000 && r.t - t0 < 120_000).map((r) => r.encoderWsMb);
  const late = ws.filter((r) => end - r.t < 60_000).map((r) => r.encoderWsMb);
  return median(late) - median(early.length ? early : [ws[0].encoderWsMb]);
}

/** Spike 1's verdict inputs from its logs, minus A/V sync: the real clips carry no flash and beep. */
export function pipelineMeasures({ run, events, rows, seamOffsets }) {
  const endedAt = run.endedAt ?? Date.now();
  // The reader always loses the stream when the run ends on purpose; only earlier losses count.
  const disconnects = events.filter(([iso, type]) => type === 'disconnect' && Date.parse(iso) < endedAt - 5000).length;
  return {
    backwards: events.filter(([, type]) => type === 'backwards').length,
    jumps: events.filter(([, type]) => type === 'jump').length,
    readerDisconnects: disconnects,
    encoderExitedEarly: run.encoderExitedEarly,
    ...lagStats(rows),
    ...seamLag(rows, seamOffsets.slice(1).map((offset) => offset - seamOffsets[0])),
    minutes: (endedAt - run.startedAt) / 60_000,
    seams: run.seams,
  };
}

/** Criterion 3. Spike 1's checks are hard; the resource bars are an estimate of what else the box
 *  will run, so missing one with a clean stream goes to Anton instead of failing. */
export function pipelineChecks(m, usage) {
  const hard = {
    encoderStayed: !m.encoderExitedEarly,
    readerStayed: m.readerDisconnects === 0,
    noBackwards: m.backwards === 0,
    seamLag: m.maxSeamRiseMs < 500,
    speed: m.overallSpeed >= 0.99 && m.overallSpeed <= 1.01,
  };
  const bars = {
    cpuAvgUnder40: usage.cpu.avg < 40,
    cpuP95Under60: usage.cpu.p95 < 60,
    engineAvgUnder50: usage.engine.avg < 50,
    workingSetGrowthUnder50Mb: usage.wsGrowthMb < 50,
  };
  return { hard, bars, hardPass: Object.values(hard).every(Boolean), barsPass: Object.values(bars).every(Boolean) };
}

/** packets: ffprobe's video packets ({ pts_time, size, flags }) in order. */
export function streamCheck(stream, packets) {
  const times = packets.map((p) => Number(p.pts_time)).filter(Number.isFinite);
  const keys = packets.filter((p) => p.flags?.includes('K')).map((p) => Number(p.pts_time));
  const intervals = keys.slice(1).map((t, i) => Math.round((t - keys[i]) * 1000) / 1000);
  const first = times.reduce((a, b) => (b < a ? b : a), Infinity);
  const last = times.reduce((a, b) => (b > a ? b : a), -Infinity);
  // From the packets: Matroska's millisecond timestamps make ffprobe report 1000/33 fps.
  const fps = (times.length - 1) / (last - first);
  // The last packet lasts one frame too.
  const span = last - first + 1 / fps;
  const result = {
    profile: stream.profile,
    size: `${stream.width}x${stream.height}`,
    fps,
    pixFmt: stream.pix_fmt,
    keyframeIntervals: intervals,
    avgKbps: span > 0 ? packets.reduce((n, p) => n + Number(p.size), 0) * 8 / span / 1000 : NaN,
    seconds: span,
  };
  const checks = {
    high: result.profile === 'High',
    size: result.size === '1920x1080',
    fps: Math.abs(result.fps - 30) < 0.01,
    pixFmt: result.pixFmt === 'yuv420p',
    // Twitch wants a keyframe every 2 s; mkv timestamps are whole milliseconds.
    gop: intervals.length > 0 && intervals.every((d) => Math.abs(d - 2) <= 0.002),
    bitrate: Math.abs(result.avgKbps - 4500) <= 450,
  };
  return { ...result, checks, pass: Object.values(checks).every(Boolean) };
}

export function vendorMatches(codec, gpuNames) {
  return gpuNames.some((name) => VENDOR[codec]?.test(name));
}

/** The first hardware encoder whose test encode worked, or the forced one. */
export function chooseEncoder(detection, force) {
  return force ?? detection.find((d) => d.ok)?.codec ?? null;
}

const fmt = (n, digits = 1) => (Number.isFinite(n) ? n.toFixed(digits) : 'n/a');

/** verdict.txt: one line per pass criterion, then the overall rule from the spec. */
export function judge({ gpus, detection, chosen, headroom, pipeline, stream, twitch }) {
  const working = detection.filter((d) => d.ok).map((d) => d.codec);
  const lines = [];
  const c = {};

  const matched = chosen && HARDWARE.includes(chosen) && vendorMatches(chosen, gpus);
  c[1] = working.length > 0 && matched ? 'PASS' : 'FAIL';
  lines.push(`1 Detection: ${c[1]}. Working: ${working.join(', ') || 'none'}. Chosen: ${chosen ?? 'none'}${chosen ? (matched ? ' (matches a GPU in the inventory)' : ' (no matching GPU in the inventory)') : ''}.`);
  for (const d of detection.filter((x) => !x.ok)) lines.push(`  ${d.codec} failed: ${d.error.split('\n').filter((l) => /error/i.test(l)).slice(0, 2).join(' | ') || d.error.split('\n').pop()}`);

  const fps = headroom.find((h) => h.codec === chosen)?.fps;
  c[2] = fps >= 90 ? 'PASS' : 'FAIL';
  lines.push(`2 Headroom: ${c[2]}. ${headroom.map((h) => `${h.codec} ${fmt(h.fps, 0)} fps`).join(', ') || 'not measured'} (bar: 90 fps).`);

  if (!pipeline) {
    c[3] = 'NOT RUN';
    lines.push('3 Realtime pipeline: NOT RUN.');
  } else {
    const { m, usage, checks } = pipeline;
    c[3] = !checks.hardPass ? 'FAIL' : checks.barsPass ? 'PASS' : 'BARS MISSED';
    lines.push(`3 Realtime pipeline (${fmt(m.minutes, 0)} min, ${chosen}): ${c[3]}.`);
    const failedHard = Object.entries(checks.hard).filter(([, ok]) => !ok).map(([k]) => k);
    lines.push(`  spike 1 checks: ${failedHard.length ? `failed ${failedHard.join(', ')}` : 'all pass'}; seam lag max ${fmt(m.maxSeamRiseMs, 0)} ms, speed ${fmt(m.overallSpeed, 4)}, backwards ${m.backwards}, reader disconnects ${m.readerDisconnects}, encoder exited early ${m.encoderExitedEarly}.`);
    lines.push(`  CPU avg ${fmt(usage.cpu.avg)}% (bar 40), p95 ${fmt(usage.cpu.p95)}% (bar 60); encode engine avg ${fmt(usage.engine.avg)}% (bar 50); encoder working set +${fmt(usage.wsGrowthMb)} MB (bar 50).`);
    if (c[3] === 'BARS MISSED') lines.push('  The stream is clean and a resource bar is missed: numbers for Anton, not a hard fail.');
  }

  if (!stream) {
    c[4] = 'NOT RUN';
    lines.push('4 Stream format: NOT RUN (no recording).');
  } else {
    c[4] = stream.pass ? 'PASS' : 'FAIL';
    const gops = [...new Set(stream.keyframeIntervals)].join(', ');
    lines.push(`4 Stream format: ${c[4]}. ${stream.profile}, ${stream.size}, ${fmt(stream.fps, 2)} fps, ${stream.pixFmt}, keyframe intervals {${gops}} s, ${fmt(stream.avgKbps, 0)} kbit/s.`);
    const failed = Object.entries(stream.checks).filter(([, ok]) => !ok).map(([k]) => k);
    if (failed.length) lines.push(`  failed: ${failed.join(', ')}`);
  }

  c[5] = twitch ?? 'NOT RUN';
  lines.push(`5 Twitch: ${c[5]}.`);

  let overall;
  if (c[1] === 'FAIL') overall = 'FAIL: no working hardware encoder matching a GPU. 3.0\'s hardware requirement fails on this box.';
  else if ([2, 3, 4].some((n) => c[n] === 'FAIL' || c[n] === 'NOT RUN')) {
    const other = working.filter((w) => w !== chosen);
    overall = `FAIL on ${chosen}.${other.length && [3, 4].some((n) => c[n] === 'FAIL') ? ` ${other.join(', ')} also works: a fix round on the arguments.` : ''}`;
  } else if (c[5] !== 'NOT RUN') overall = 'PASS on 1 to 4; criterion 5 needs Twitch Inspector checked by hand.';
  else overall = c[3] === 'BARS MISSED' ? 'PASS with resource bars missed (see 3).' : 'PASS';
  return { criteria: c, text: `${lines.join('\n')}\n\nVerdict: ${overall}\n` };
}
