import { spawnSync } from 'node:child_process';
import { mkdirSync } from 'node:fs';
import { SLATE, SLATE_SECONDS, slateArgs } from './lib.mjs';

mkdirSync(new URL('./media/', import.meta.url), { recursive: true });
const made = spawnSync('ffmpeg', slateArgs(SLATE), { stdio: 'inherit' });
const probe = spawnSync('ffprobe', ['-v', 'error', '-show_entries',
  'format=duration:stream=codec_type,width,height,r_frame_rate,sample_rate,channels', '-of', 'json', SLATE], { encoding: 'utf8' });
const info = JSON.parse(probe.stdout || '{}');
const video = info.streams?.find((s) => s.codec_type === 'video');
const audio = info.streams?.find((s) => s.codec_type === 'audio');
const ok = made.status === 0 && video?.width === 1920 && video?.height === 1080 && video?.r_frame_rate === '30/1'
  && Number(audio?.sample_rate) === 48000 && audio?.channels === 2
  && Math.abs(Number(info.format?.duration) - SLATE_SECONDS) < 0.2;
console.log(`${ok ? 'ok  ' : 'BAD '} ${SLATE}  ${video?.width}x${video?.height}@${video?.r_frame_rate}  ${audio?.sample_rate} Hz x${audio?.channels}  ${info.format?.duration}s`);
process.exit(ok ? 0 : 1);
