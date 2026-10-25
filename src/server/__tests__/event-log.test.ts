import { describe, it, expect, beforeEach, afterEach } from 'vitest';
import { mkdirSync, rmSync } from 'fs';
import { join } from 'path';
import { EventLog } from '../event-log.js';
import { EventStore } from '../event-store.js';

let dir = '';
let n = 0;

describe('EventLog', () => {
  // Own folder per test: the store appends asynchronously, so a late write must not reach the next test.
  beforeEach(() => { dir = join(process.cwd(), '.test-tmp', `event-log-${n++}`); rmSync(dir, { recursive: true, force: true }); mkdirSync(dir, { recursive: true }); });
  afterEach(() => { rmSync(dir, { recursive: true, force: true }); });

  it('keeps only the last 100 events', () => {
    const log = new EventLog(new EventStore({ dir }));
    for (let i = 0; i < 105; i++) log.addEvent(`e${i}`);

    const events = log.getEvents();

    expect(events).toHaveLength(100);
    expect(events[0].message).toBe('e5');
    expect(events[99].message).toBe('e104');
  });

  it('reloads persisted events in a new instance', async () => {
    const log = new EventLog(new EventStore({ dir }));
    log.addEvent('first');
    log.addEvent('second');
    await new Promise((r) => setTimeout(r, 100));

    const again = new EventLog(new EventStore({ dir }));

    expect(again.getEvents().map((e) => e.message)).toEqual(['first', 'second']);
  });
});
