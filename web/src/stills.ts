/*
 * The timeline's stills, loaded a few at a time.
 *
 * A browser opens at most 6 connections to a server: a wide zoom asking for dozens of
 * stills at once would hold them all and the player's fragments, the waveform and every
 * other request would wait. Here at most MAX stills load at once, the newest asked first
 * (the view the user looks at now), and a still nobody shows any more is never asked.
 */
const MAX = 2;
const KEEP = 600;

interface Job {
  url: string;
  done: (src: string | null) => void;
  ctrl?: AbortController;
  dead: boolean;
}

const cache = new Map<string, string | null>();
const queue: Job[] = [];
let running = 0;

/** A still already loaded (object URL), null when it does not exist, undefined when not known yet. */
export function cachedStill(url: string): string | null | undefined {
  return cache.get(url);
}

function remember(url: string, src: string | null) {
  cache.set(url, src);
  if (cache.size > KEEP) {
    // The oldest go first (a Map keeps the order of insertion).
    for (const [k, v] of cache) {
      if (cache.size <= KEEP * 0.8) break;
      if (v) URL.revokeObjectURL(v);
      cache.delete(k);
    }
  }
}

function pump() {
  while (running < MAX && queue.length) {
    const job = queue.pop()!; // the newest first
    if (job.dead) continue;
    running++;
    job.ctrl = new AbortController();
    fetch(job.url, { credentials: 'same-origin', signal: job.ctrl.signal })
      .then(async (r) => {
        const src = r.ok ? URL.createObjectURL(await r.blob()) : null;
        // A missing still is remembered too (not asked again); a server error is not.
        if (r.ok || r.status === 404) remember(job.url, src);
        if (!job.dead) job.done(src);
        else if (src && !r.ok) URL.revokeObjectURL(src);
      })
      .catch(() => {
        if (!job.dead) job.done(null);
      })
      .finally(() => {
        running--;
        pump();
      });
  }
}

/** Asks for a still; `done` gets its object URL (or null). Returns the cancel. */
export function loadStill(url: string, done: (src: string | null) => void): () => void {
  const known = cache.get(url);
  if (known !== undefined) {
    done(known);
    return () => {};
  }
  const job: Job = { url, done, dead: false };
  queue.push(job);
  pump();
  return () => {
    job.dead = true;
    job.ctrl?.abort();
  };
}
