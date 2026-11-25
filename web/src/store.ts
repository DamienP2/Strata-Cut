import { useEffect, useRef } from 'react';
import { create } from 'zustand';
import { pickedByHand, lang, setLang } from './i18n';
import { setTheme } from './theme';
import type { Recorder, RecorderStatus, Job } from './types';

export type Role = 'admin' | 'operator' | 'streamer' | 'viewer';
export const ROLES: Role[] = ['admin', 'operator', 'streamer', 'viewer'];

export interface PublicUser {
  id: string;
  username: string;
  displayName: string;
  role: Role;
  disabled: boolean;
  lang: string;
  theme: string;
  sso: boolean;
  roleFixed: boolean;
  /** What its groups let it do (Système › Accès › Autorisations): "extract.cut", "stream.run"… */
  perms?: string[];
  /** The groups it is in. */
  groups?: string[];
}

export interface AuthState {
  needsSetup: boolean;
  user: PublicUser | null;
  serverName: string;
  sso: string | null;
  localAccounts: 'none' | 'adminOnly' | 'all';
  /** The live shown in the app: its fixed delay (s), or null (the player's own). */
  liveDelay?: number | null;
}

export interface Toast {
  id: number;
  message: string;
  kind: 'info' | 'error';
}

/** What a role may do (mirrors the server's checks). */
export const can = {
  admin: (u: PublicUser | null | undefined) => u?.role === 'admin',
  /** A right of its role (an admin has them all). */
  has: (u: PublicUser | null | undefined, perm: string) => u?.role === 'admin' || !!u?.perms?.includes(perm),
  extract: (u: PublicUser | null | undefined) => u?.role === 'admin' || !!u?.perms?.includes('extract.cut'),
};

export class ApiError extends Error {
  constructor(
    public status: number,
    message: string,
  ) {
    super(message);
  }
}

export async function api<T = unknown>(method: string, path: string, body?: unknown): Promise<T> {
  const headers: Record<string, string> = {};
  const raw = typeof Blob !== 'undefined' && body instanceof Blob; // a file goes as is
  if (body !== undefined) headers['Content-Type'] = raw ? (body as Blob).type || 'application/octet-stream' : 'application/json';
  const res = await fetch(path, { method, credentials: 'same-origin', headers, body: body === undefined ? undefined : raw ? (body as Blob) : JSON.stringify(body) });
  if (res.status === 401 && !path.startsWith('/api/auth/')) {
    const a = useApp.getState().auth;
    if (a?.user) useApp.setState({ auth: { ...a, user: null } }); // session expired → login page
  }
  if (!res.ok) throw new ApiError(res.status, (await res.text()) || res.statusText);
  const text = await res.text();
  return (text ? JSON.parse(text) : undefined) as T;
}

/** Lists changed by another client: `{"type":"changed","what":…}`. */
export const changeEvents = new EventTarget();
/** Every other server event, by its `type`. */
export const serverEvents = new EventTarget();

/** Runs `fn` when the server says `what` changed (and after a reconnection). */
export function useChanged(what: string, fn: () => void) {
  const ref = useRef(fn);
  ref.current = fn;
  useEffect(() => {
    const on = () => ref.current();
    changeEvents.addEventListener(what, on);
    changeEvents.addEventListener('*', on);
    return () => {
      changeEvents.removeEventListener(what, on);
      changeEvents.removeEventListener('*', on);
    };
  }, [what]);
}

interface AppStore {
  auth: AuthState | null;
  /** The same user signed in elsewhere: this session ended. */
  elsewhere: boolean;
  connected: boolean;
  /** server time − local time, ms. */
  serverOffset: number;
  timezone: string;
  toasts: Toast[];
  recorders: Recorder[];
  status: Record<string, RecorderStatus>;
  jobs: Job[];
  init: () => Promise<void>;
  login: (username: string, password: string) => Promise<void>;
  setup: (username: string, password: string, displayName: string) => Promise<void>;
  logout: () => Promise<void>;
  connect: () => void;
  toast: (message: string, kind?: 'info' | 'error') => void;
}

let socket: WebSocket | null = null;
let retry = 500;
let wanted = false;
let toastId = 0;
/** A hello was already received in this session: the next one follows a reconnection. */
let greeted = false;

function applyUser(u: PublicUser | null) {
  if (!u) return;
  setLang(u.lang);
  setTheme(u.theme || 'dark');
}

/** After signing in: the language picked by hand on the login page wins and is saved. */
async function adoptLang(u: PublicUser): Promise<PublicUser> {
  if (pickedByHand() && u.lang !== lang()) {
    try {
      return await api<PublicUser>('PUT', '/api/me/lang', { lang: lang() });
    } catch {
      return u;
    }
  }
  return u;
}

export const useApp = create<AppStore>((set, get) => ({
  auth: null,
  elsewhere: false,
  connected: false,
  serverOffset: 0,
  timezone: Intl.DateTimeFormat().resolvedOptions().timeZone,
  toasts: [],
  recorders: [],
  status: {},
  jobs: [],

  init: async () => {
    for (;;) {
      try {
        const auth = await api<AuthState>('GET', '/api/auth/state');
        applyUser(auth.user);
        set({ auth });
        return;
      } catch {
        await new Promise((r) => setTimeout(r, 2000));
      }
    }
  },

  login: async (username, password) => {
    const r = await api<{ user: PublicUser }>('POST', '/api/auth/login', { username, password });
    const user = await adoptLang(r.user);
    applyUser(user);
    set((s) => ({ elsewhere: false, auth: { ...(s.auth as AuthState), needsSetup: false, user } }));
  },

  setup: async (username, password, displayName) => {
    const r = await api<{ user: PublicUser }>('POST', '/api/auth/setup', { username, password, displayName });
    const user = await adoptLang(r.user);
    applyUser(user);
    set((s) => ({ elsewhere: false, auth: { ...(s.auth as AuthState), needsSetup: false, user } }));
  },

  logout: async () => {
    wanted = false;
    greeted = false;
    socket?.close();
    socket = null;
    try {
      await api('POST', '/api/auth/logout');
    } catch {
      /* */
    }
    set((s) => ({ connected: false, recorders: [], status: {}, jobs: [], auth: s.auth ? { ...s.auth, user: null } : s.auth }));
  },

  connect: () => {
    wanted = true;
    if (socket) return;
    const ws = new WebSocket(`${location.protocol === 'https:' ? 'wss' : 'ws'}://${location.host}/ws`);
    socket = ws;
    ws.onopen = () => {
      retry = 500;
    };
    ws.onmessage = (m) => {
      let e: { type: string; [k: string]: unknown };
      try {
        e = JSON.parse(m.data);
      } catch {
        return;
      }
      switch (e.type) {
        case 'hello': {
          set({
            connected: true,
            serverOffset: (e.serverTime as number) - Date.now(),
            timezone: (e.timezone as string) || get().timezone,
            recorders: (e.recorders as Recorder[]) ?? [],
            status: Object.fromEntries(((e.status as RecorderStatus[]) ?? []).map((s) => [s.id, s])),
            jobs: (e.jobs as Job[]) ?? [],
          });
          const a = get().auth;
          if (a && e.user) set({ auth: { ...a, user: e.user as PublicUser, serverName: (e.serverName as string) ?? a.serverName } });
          if (greeted) changeEvents.dispatchEvent(new Event('*')); // reconnected: reload what is open
          greeted = true;
          break;
        }
        case 'changed':
          changeEvents.dispatchEvent(new Event(e.what as string));
          break;
        case 'recorders':
          set({ recorders: e.recorders as Recorder[] });
          break;
        case 'status':
          set((s) => ({ status: { ...s.status, ...Object.fromEntries((e.status as RecorderStatus[]).map((x) => [x.id, x])) } }));
          break;
        case 'jobs':
          set({ jobs: e.jobs as Job[] });
          break;
        case 'sessionEnded':
          wanted = false;
          set((s) => ({ elsewhere: true, connected: false, auth: s.auth ? { ...s.auth, user: null } : s.auth }));
          ws.close();
          break;
        default:
          serverEvents.dispatchEvent(new CustomEvent(e.type, { detail: e }));
      }
    };
    ws.onclose = () => {
      if (socket === ws) socket = null;
      set({ connected: false });
      if (!wanted) return;
      setTimeout(() => wanted && get().connect(), retry);
      retry = Math.min(retry * 2, 5000);
    };
  },

  toast: (message, kind = 'info') => {
    const id = ++toastId;
    set((s) => ({ toasts: [...s.toasts, { id, message, kind }] }));
    setTimeout(() => set((s) => ({ toasts: s.toasts.filter((t) => t.id !== id) })), 4000);
  },
}));

/** Server time now, ms. */
export const serverNow = () => Date.now() + useApp.getState().serverOffset;
