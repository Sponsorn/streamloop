import { describe, it, expect } from 'vitest';
import {
  FORMAT, SLATE_SECONDS, playlistArgs, sizeArgs, downloadArgs, slateArgs, parseDownloadProgress,
  bytesOf, decide, walkAfter, nextAtSeam, deletable,
} from './lib.mjs';

const MB = 1024 * 1024;
const entry = (id, bytes) => ({ id, bytes });
const cached = (id, bytes, extra = {}) => ({ id, bytes, complete: true, played: false, feederAlive: false, ...extra });

describe('budget rule', () => {
  it('starts the next download when it still fits in the budget', () => {
    // Arrange
    const state = { entries: [entry('a', 40 * MB)], cursor: 0, onDisk: [cached('z', 50 * MB)], budgetBytes: 100 * MB };

    // Act
    const decision = decide(state);

    // Assert
    expect(decision).toEqual({ type: 'download', index: 0 });
  });

  it('holds off when the next video would push the cache over the budget', () => {
    // Arrange
    const state = { entries: [entry('a', 60 * MB)], cursor: 0, onDisk: [cached('z', 50 * MB)], budgetBytes: 100 * MB };

    // Act + Assert
    expect(decide(state).type).toBe('full');
  });

  it('downloads a video larger than the whole budget when the cache is empty', () => {
    // Arrange: a video must always be able to play, so an empty cache ignores the cap
    const state = { entries: [entry('big', 400 * MB)], cursor: 0, onDisk: [], budgetBytes: 20 * MB };

    // Act + Assert
    expect(decide(state)).toEqual({ type: 'download', index: 0 });
  });

  it('prefetches nothing else while an over-budget video sits in the cache', () => {
    // Arrange
    const state = { entries: [entry('next', 1 * MB)], cursor: 0, onDisk: [cached('big', 400 * MB)], budgetBytes: 20 * MB };

    // Act + Assert
    expect(decide(state).type).toBe('full');
  });

  it('treats an unknown estimated size as zero rather than blocking the walk', () => {
    // Arrange: yt-dlp reports neither filesize nor filesize_approx for some entries
    const state = { entries: [{ id: 'a' }], cursor: 0, onDisk: [cached('z', 50 * MB)], budgetBytes: 100 * MB };

    // Act + Assert
    expect(decide(state).type).toBe('download');
  });

  it('reports the walk finished once the cursor passes the last entry', () => {
    expect(decide({ entries: [entry('a', 1)], cursor: 1, onDisk: [], budgetBytes: 0 }).type).toBe('done');
  });

  it('sums the bytes of everything in the cache', () => {
    expect(bytesOf([cached('a', 3), cached('b', 4)])).toBe(7);
  });
});

describe('walk order', () => {
  it('visits every entry once, in playlist order, when all downloads succeed', () => {
    // Arrange
    const entries = [entry('a', MB), entry('b', MB), entry('c', MB)];
    let state = { cursor: 0, attempts: 0 };
    const onDisk = [];
    const order = [];

    // Act: download everything, nothing plays, budget is generous
    for (let step = 0; step < 10; step += 1) {
      const decision = decide({ entries, cursor: state.cursor, onDisk, budgetBytes: 100 * MB });
      if (decision.type === 'done') break;
      order.push(entries[decision.index].id);
      onDisk.push(cached(entries[decision.index].id, MB));
      state = walkAfter({ ...state, attempts: 0 }, true);
    }

    // Assert
    expect(order).toEqual(['a', 'b', 'c']);
  });

  it('never revisits a video the budget made it wait for', () => {
    // Arrange: 'b' does not fit until 'a' is played and deleted
    const entries = [entry('a', 60 * MB), entry('b', 60 * MB)];
    const onDisk = [];
    let cursor = 0;

    // Act
    onDisk.push(cached('a', 60 * MB));
    cursor = walkAfter({ cursor, attempts: 0 }, true).cursor;
    const blocked = decide({ entries, cursor, onDisk, budgetBytes: 100 * MB });
    onDisk.length = 0;
    const freed = decide({ entries, cursor, onDisk, budgetBytes: 100 * MB });

    // Assert
    expect(blocked.type).toBe('full');
    expect(entries[freed.index].id).toBe('b');
  });
});

describe('retry and skip rule', () => {
  it('stays on the same video after its first failure', () => {
    expect(walkAfter({ cursor: 3, attempts: 1 }, false)).toEqual({ cursor: 3, attempts: 1, skipped: false });
  });

  it('skips the video and moves on after the second failure', () => {
    expect(walkAfter({ cursor: 3, attempts: 2 }, false)).toEqual({ cursor: 4, attempts: 0, skipped: true });
  });

  it('clears the attempt count once a video downloads', () => {
    expect(walkAfter({ cursor: 3, attempts: 1 }, true)).toEqual({ cursor: 4, attempts: 0, skipped: false });
  });

  it('spends exactly two attempts on a broken entry and then plays the next one', () => {
    // Arrange
    const entries = [entry('good1', MB), entry('broken', MB), entry('good2', MB)];
    const fails = new Set(['broken']);
    const attemptsMade = [];
    let state = { cursor: 0, attempts: 0 };
    const downloaded = [];

    // Act
    for (let step = 0; step < 20; step += 1) {
      const decision = decide({ entries, cursor: state.cursor, onDisk: [], budgetBytes: 100 * MB });
      if (decision.type === 'done') break;
      const id = entries[decision.index].id;
      attemptsMade.push(id);
      const ok = !fails.has(id);
      if (ok) downloaded.push(id);
      state = walkAfter({ cursor: state.cursor, attempts: state.attempts + 1 }, ok);
    }

    // Assert
    expect(attemptsMade).toEqual(['good1', 'broken', 'broken', 'good2']);
    expect(downloaded).toEqual(['good1', 'good2']);
  });
});

describe('deletion rule', () => {
  it('keeps a file whose feeder is still reading it', () => {
    // Arrange
    const onDisk = [cached('a', MB, { played: true, feederAlive: true })];

    // Act + Assert
    expect(deletable(onDisk)).toEqual([]);
  });

  it('deletes a file only once its feeder has exited', () => {
    // Arrange
    const onDisk = [cached('a', MB, { played: true, feederAlive: false }), cached('b', MB)];

    // Act
    const gone = deletable(onDisk);

    // Assert
    expect(gone.map((v) => v.id)).toEqual(['a']);
  });
});

describe('seam choice', () => {
  it('picks the oldest unplayed complete video, which is the next one in playlist order', () => {
    // Arrange
    const onDisk = [cached('a', MB, { played: true }), cached('b', MB), cached('c', MB)];

    // Act + Assert
    expect(nextAtSeam(onDisk).id).toBe('b');
  });

  it('falls back to the slate when the next video is not fully downloaded', () => {
    // Arrange
    const onDisk = [cached('a', MB, { played: true }), { id: 'b', bytes: MB, complete: false, played: false }];

    // Act + Assert
    expect(nextAtSeam(onDisk)).toBeNull();
  });

  it('falls back to the slate when the cache is empty', () => {
    expect(nextAtSeam([])).toBeNull();
  });
});

describe('yt-dlp arguments', () => {
  it('resolves the playlist flat, one id and title per line', () => {
    // Act
    const args = playlistArgs('https://youtube.com/playlist?list=X', 'android_vr');

    // Assert
    expect(args).toContain('--flat-playlist');
    expect(args[args.indexOf('--print') + 1]).toBe('%(id)s\t%(title)s');
    expect(args[args.indexOf('--extractor-args') + 1]).toBe('youtube:player_client=android_vr');
    expect(args[args.length - 1]).toBe('https://youtube.com/playlist?list=X');
  });

  it('asks for the merged size without downloading anything', () => {
    // Act
    const args = sizeArgs('abc123', 'android_vr');

    // Assert
    expect(args[args.indexOf('-O') + 1]).toBe('%(filesize,filesize_approx)s');
    expect(args[args.indexOf('-f') + 1]).toBe(FORMAT);
    expect(args).toContain('--no-playlist');
    expect(args[args.length - 1]).toBe('https://www.youtube.com/watch?v=abc123');
  });

  it('downloads best-up-to-1080p merged to a fixed file, with no rate limit by default', () => {
    // Act
    const args = downloadArgs('abc123', 'cache/abc123.mp4', { client: 'android_vr' });

    // Assert
    expect(args[args.indexOf('-o') + 1]).toBe('cache/abc123.mp4');
    expect(args.slice(args.indexOf('--merge-output-format'), args.indexOf('--merge-output-format') + 2))
      .toEqual(['--merge-output-format', 'mp4']);
    expect(args).not.toContain('--limit-rate');
    // Half a download left over from a previous run would otherwise resume into a wrong byte count.
    expect(args).toContain('--no-continue');
  });

  it('passes a rate limit through to yt-dlp when one is given', () => {
    // Act
    const args = downloadArgs('abc123', 'cache/abc123.mp4', { client: 'android_vr', limitRate: '300K' });

    // Assert
    expect(args[args.indexOf('--limit-rate') + 1]).toBe('300K');
  });

  it('reads the percentage out of a yt-dlp progress line and ignores everything else', () => {
    // Act + Assert
    expect(parseDownloadProgress('[download]  42.7% of  39.42MiB at 1.20MiB/s ETA 00:18')).toBe(42.7);
    expect(parseDownloadProgress('[youtube] abc123: Downloading webpage')).toBeNull();
  });
});

describe('slate clip', () => {
  it('is ten seconds of 1080p30 over silent 48 kHz stereo', () => {
    // Act
    const args = slateArgs('media/slate.mp4');

    // Assert
    expect(args[args.indexOf('-t') + 1]).toBe(String(SLATE_SECONDS));
    expect(args).toContain('color=c=0x0c0c14:s=1920x1080:r=30');
    expect(args).toContain('anullsrc=r=48000:cl=stereo');
    expect(args[args.length - 1]).toBe('media/slate.mp4');
  });
});
