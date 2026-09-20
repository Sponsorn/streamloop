import { spawnSync } from 'node:child_process';
import { mkdirSync } from 'node:fs';
import { CLIPS, SHORT_CLIPS, inputArgs } from './lib.mjs';

mkdirSync('media', { recursive: true });
let failed = false;
for (const clip of [...CLIPS, ...SHORT_CLIPS]) {
  const file = `media/${clip.name}.mp4`;
  const made = spawnSync('ffmpeg', inputArgs(clip, file), { stdio: 'inherit' });
  const probe = spawnSync('ffprobe', [
    '-v', 'error', '-show_entries', 'format=duration:stream=codec_type,width,height,r_frame_rate,sample_rate,channels',
    '-of', 'json', file,
  ], { encoding: 'utf8' });
  const info = JSON.parse(probe.stdout || '{}');
  const video = info.streams?.find((s) => s.codec_type === 'video');
  const audio = info.streams?.find((s) => s.codec_type === 'audio');
  const ok = made.status === 0
    && `${video?.width}x${video?.height}` === clip.size
    && video?.r_frame_rate === `${clip.rate}/1`
    && Number(audio?.sample_rate) === clip.sampleRate
    && audio?.channels === clip.channels
    && Math.abs(Number(info.format?.duration) - clip.seconds) < 0.2;
  console.log(`${ok ? 'ok  ' : 'BAD '} ${file}  ${video?.width}x${video?.height}@${video?.r_frame_rate}  ${audio?.sample_rate} Hz x${audio?.channels}  ${info.format?.duration}s`);
  if (!ok) failed = true;
}
process.exit(failed ? 1 : 0);
