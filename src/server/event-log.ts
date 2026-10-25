import type { EventStore } from './event-store.js';
import type { EventLogEntry } from './types.js';

const MAX_EVENT_LOG = 100;

/** The dashboard timeline: the last 100 events, persisted through the EventStore. */
export class EventLog {
  private entries: EventLogEntry[];

  constructor(private readonly store: EventStore) {
    this.entries = store.loadRecent(MAX_EVENT_LOG);
  }

  addEvent(message: string): void {
    const entry = { timestamp: new Date().toISOString(), message };
    this.entries.push(entry);
    if (this.entries.length > MAX_EVENT_LOG) this.entries.shift();
    this.store.append(entry);
  }

  getEvents(): EventLogEntry[] {
    return [...this.entries];
  }
}
