/*
 * The server's settings: GET /api/system on load, then every 5 s while the tab
 * is visible; an editable copy `cfg`, `dirty`, and `save()` (PUT /api/system/config,
 * then reload while the server restarts).
 */
import { useCallback, useEffect, useRef, useState } from 'react';
import { t } from '../i18n';
import { api, useApp, useChanged, type Role } from '../store';
import { showError } from '../ui/serverError';

export type SsoProvider = 'entra' | 'okta' | 'keycloak' | 'google' | 'other';

export interface SsoConfig {
  enabled: boolean;
  provider: SsoProvider;
  issuer: string;
  clientId: string;
  clientSecret: string;
  groupsClaim: string;
  groups: { group: string; role: Role }[];
  defaultRole: Role | null;
  localAccounts: 'none' | 'adminOnly' | 'all';
  /** The e-mail domains allowed to sign in (empty: any; Google needs them). */
  domains: string[];
}

export interface ServerConfig {
  name: string;
  bind?: string | null;
  trustedOrigins?: string[];
  timezone: string;
  security: { sso: SsoConfig; [k: string]: unknown };
  /** The app's own settings (recordingsDir, clipsDir…). */
  [k: string]: unknown;
}

export interface SystemInfo {
  version: string;
  os: string;
  arch: string;
  hostname: string;
  cpus: number;
  dataDir: string;
  bind: string;
  uptimeSecs: number;
  timezone: string;
  /** Admins only. */
  config?: ServerConfig;
  interfaces: { name: string; ip: string }[];
}

const same = (a: unknown, b: unknown) => JSON.stringify(a) === JSON.stringify(b);
const portOf = (bind: string | null | undefined) => bind?.split(':').pop() ?? '';
const sleep = (ms: number) => new Promise((r) => setTimeout(r, ms));

export function useServerConfig() {
  const [info, setInfo] = useState<SystemInfo | null>(null);
  const [base, setBase] = useState<ServerConfig | null>(null);
  const [cfg, setCfgState] = useState<ServerConfig | null>(null);
  const [saving, setSaving] = useState(false);
  const live = useRef({ base, cfg });
  live.current = { base, cfg };

  /** Takes a fresh answer; the local copy follows unless it has changes. */
  const take = useCallback((s: SystemInfo) => {
    setInfo(s);
    if (!s.config) return;
    const { base: b, cfg: c } = live.current;
    if (!c || same(c, b)) setCfgState(s.config);
    setBase(s.config);
  }, []);

  const reload = useCallback(async () => {
    try {
      take(await api<SystemInfo>('GET', '/api/system'));
    } catch {
      /* next time */
    }
  }, [take]);

  useEffect(() => {
    void reload();
    const id = window.setInterval(() => {
      if (document.visibilityState === 'visible') void reload();
    }, 5000);
    return () => clearInterval(id);
  }, [reload]);
  useChanged('config', () => void reload());

  const setCfg = useCallback((f: ServerConfig | ((c: ServerConfig) => ServerConfig)) => {
    setCfgState((c) => (c ? (typeof f === 'function' ? f(c) : f) : c));
  }, []);

  const dirty = !!cfg && !!base && !same(cfg, base);

  const save = useCallback(async () => {
    const c = live.current.cfg;
    if (!c) return;
    const oldBind = info?.bind;
    setSaving(true);
    try {
      await api('PUT', '/api/system/config', c);
    } catch (e) {
      showError(e);
      setSaving(false);
      return;
    }
    setBase(c);
    useApp.getState().toast(t('Settings saved'));
    // The server restarts: try again every second, 15 times.
    for (let i = 0; i < 15; i++) {
      await sleep(1000);
      try {
        const s = await api<SystemInfo>('GET', '/api/system');
        take(s);
        if (oldBind && portOf(oldBind) === location.port && portOf(s.bind) !== portOf(oldBind)) {
          location.port = portOf(s.bind); // the page follows the new port
        }
        break;
      } catch {
        /* still restarting */
      }
    }
    setSaving(false);
  }, [info, take]);

  const discard = useCallback(() => setCfgState(live.current.base), []);

  return { info, cfg, setCfg, dirty, saving, save, discard, reload };
}
