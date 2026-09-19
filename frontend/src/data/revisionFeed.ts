import { usagiClient, type UsagiClient } from "./usagiClient";
import type { RevisionTuple } from "./types";

export type RevisionEventSource = {
  onerror: ((event: Event) => void) | null;
  onmessage: ((event: MessageEvent<string>) => void) | null;
  addEventListener?: (type: string, listener: EventListener) => void;
  removeEventListener?: (type: string, listener: EventListener) => void;
  close: () => void;
};

export type RevisionFeedOptions = {
  client?: UsagiClient;
  eventSourceFactory?: (url: string) => RevisionEventSource;
  pollIntervalMs?: number;
};

export type RevisionListener = (tuple: RevisionTuple) => void;
export type RevisionErrorListener = (error: unknown) => void;

const DEFAULT_POLL_INTERVAL = 60_000;

function defaultEventSourceFactory(url: string): RevisionEventSource {
  return new EventSource(url);
}

function tupleFrom(value: unknown): RevisionTuple | null {
  if (typeof value !== "object" || value === null || Array.isArray(value)) return null;
  const record = value as Record<string, unknown>;
  const data = record.data_revision;
  const status = record.status_revision;
  if (
    typeof data !== "number" ||
    !Number.isSafeInteger(data) ||
    data < 0 ||
    typeof status !== "number" ||
    !Number.isSafeInteger(status) ||
    status < 0
  ) {
    return null;
  }
  return { data_revision: data, status_revision: status };
}

export class RevisionFeed {
  private readonly client: UsagiClient;
  private readonly eventSourceFactory: (url: string) => RevisionEventSource;
  private readonly pollIntervalMs: number;
  private readonly listeners = new Map<RevisionListener, RevisionErrorListener | undefined>();
  private eventSource: RevisionEventSource | null = null;
  private pollTimer: ReturnType<typeof setInterval> | null = null;
  private revisionAbort: AbortController | null = null;
  private revisionGeneration = 0;
  private tuple: RevisionTuple | null = null;

  constructor(options: RevisionFeedOptions = {}) {
    this.client = options.client ?? usagiClient;
    this.eventSourceFactory = options.eventSourceFactory ?? defaultEventSourceFactory;
    this.pollIntervalMs = options.pollIntervalMs ?? DEFAULT_POLL_INTERVAL;
  }

  get_snapshot(): RevisionTuple | null {
    return this.tuple;
  }

  subscribe(listener: RevisionListener, onError?: RevisionErrorListener): () => void {
    this.listeners.set(listener, onError);
    if (this.listeners.size === 1) this.startTransport();
    return () => {
      this.listeners.delete(listener);
      if (this.listeners.size === 0) this.stopTransport();
    };
  }

  retry_now(): void {
    void this.fetchRevision(true);
  }

  dispose(): void {
    this.listeners.clear();
    this.stopTransport();
  }

  private startTransport(): void {
    if (this.eventSource) return;
    const onRevision = (event: Event | MessageEvent<string>) => {
      const raw = "data" in event ? event.data : undefined;
      if (typeof raw !== "string") return;
      try {
        const tuple = tupleFrom(JSON.parse(raw) as unknown);
        if (tuple) this.accept(tuple, true);
      } catch {
        // The next authoritative /api/revision request will repair an invalid hint.
      }
    };
    try {
      const source = this.eventSourceFactory("/api/events");
      source.onmessage = onRevision as (event: MessageEvent<string>) => void;
      source.addEventListener?.("revision", onRevision as EventListener);
      source.onerror = () => this.startPolling();
      this.eventSource = source;
    } catch {
      this.startPolling();
    }
    void this.fetchRevision(false);
  }

  private stopTransport(): void {
    this.clearPolling();
    this.revisionAbort?.abort();
    this.revisionAbort = null;
    this.revisionGeneration += 1;
    if (this.eventSource) {
      this.eventSource.close();
      this.eventSource = null;
    }
  }

  private startPolling(): void {
    if (this.pollTimer !== null || this.listeners.size === 0) return;
    this.pollTimer = setInterval(() => {
      void this.fetchRevision(false);
    }, this.pollIntervalMs);
  }

  private clearPolling(): void {
    if (this.pollTimer !== null) {
      clearInterval(this.pollTimer);
      this.pollTimer = null;
    }
  }

  private async fetchRevision(_manual: boolean): Promise<void> {
    this.revisionAbort?.abort();
    const controller = new AbortController();
    this.revisionAbort = controller;
    const generation = ++this.revisionGeneration;
    try {
      const tuple = await this.client.getRevision(controller.signal);
      if (controller.signal.aborted || generation !== this.revisionGeneration) return;
      this.accept(tuple, false);
    } catch (error: unknown) {
      if (controller.signal.aborted || generation !== this.revisionGeneration) return;
      for (const onError of this.listeners.values()) onError?.(error);
    }
  }

  private accept(tuple: RevisionTuple, fromSse: boolean): void {
    const previous = this.tuple;
    if (previous && (tuple.data_revision < previous.data_revision || tuple.status_revision < previous.status_revision)) return;
    if (fromSse) this.clearPolling();
    if (previous && tuple.data_revision === previous.data_revision && tuple.status_revision === previous.status_revision) {
      for (const listener of this.listeners.keys()) listener(tuple);
      return;
    }
    this.tuple = tuple;
    for (const listener of this.listeners.keys()) listener(tuple);
  }
}

export function createRevisionFeed(options: RevisionFeedOptions = {}): RevisionFeed {
  return new RevisionFeed(options);
}
