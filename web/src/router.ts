import { useEffect, useState } from 'react';

/** The pages of the app, from the URL. */
export type Route =
  | { page: 'home' }
  | { page: 'recorders' }
  | { page: 'streamer'; source?: string }
  | { page: 'schedule' }
  | { page: 'extract'; recorder?: string }
  | { page: 'clips' }
  | { page: 'system'; section?: string }
  | { page: 'help' };

export function parseRoute(path: string): Route {
  const p = path.split('/').filter(Boolean);
  switch (p[0]) {
    case 'extract':
      return { page: 'extract', recorder: p[1] ? decodeURIComponent(p[1]) : undefined };
    case 'clips':
      return { page: 'clips' };
    case 'schedule':
      return { page: 'schedule' };
    case 'system':
      return { page: 'system', section: p[1] };
    case 'help':
      return { page: 'help' };
    case 'streamer':
      return { page: 'streamer', source: p[1] ? decodeURIComponent(p[1]) : undefined };
    // Former address: the sources are in Système now.
    case 'recorders':
      return { page: 'recorders' };
    default:
      return { page: 'home' };
  }
}

let guard: { leaves: (next: string) => boolean; ask: () => Promise<boolean> } | null = null;

/** Asks before leaving a page with unsaved changes (Back included; closing the tab too). */
export function setLeaveGuard(leaves: ((next: string) => boolean) | null, ask?: () => Promise<boolean>) {
  guard = leaves && ask ? { leaves, ask } : null;
}

addEventListener('beforeunload', (e) => {
  if (guard?.leaves('')) e.preventDefault();
});

export async function navigate(path: string, replace = false) {
  if (path === location.pathname + location.search) return;
  if (guard?.leaves(path) && !(await guard.ask())) return;
  guard = null;
  if (replace) history.replaceState(null, '', path);
  else history.pushState(null, '', path);
  dispatchEvent(new PopStateEvent('popstate'));
}

let current = location.pathname;
export function useRoute(): Route {
  const [route, setRoute] = useState(() => parseRoute(location.pathname));
  useEffect(() => {
    const on = async () => {
      if (guard?.leaves(location.pathname) && location.pathname !== current) {
        const back = current;
        history.pushState(null, '', back); // stay until the user says yes
        if (!(await guard.ask())) return;
        guard = null;
        history.back();
        return;
      }
      current = location.pathname;
      setRoute(parseRoute(location.pathname));
    };
    addEventListener('popstate', on);
    return () => removeEventListener('popstate', on);
  }, []);
  return route;
}
