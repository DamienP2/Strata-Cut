/* Système › Serveur › Mises à jour: never installs by itself, the check only tells. */
import { useCallback, useEffect, useRef, useState, type ReactNode } from 'react';
import { CloudDownload, History, RefreshCw, RotateCcw, Upload } from 'lucide-react';
import { t } from '../i18n';
import { api, useApp } from '../store';
import { PageHead } from '../pages/SidePage';
import { confirmDialog } from '../ui/dialogs';
import { TextInput } from '../ui/fields';
import { formatBytes, formatDate, timeAgo } from '../ui/format';
import { friendlyError, showError } from '../ui/serverError';
import { Card, Field, Switch } from './ui';

export interface Release {
  version: string;
  name?: string;
  /** ISO text or ms. */
  publishedAt?: string | number;
  date?: string | number;
  notes?: string;
  body?: string;
  /** Size of this platform's package, bytes. */
  size?: number;
}

export interface UpdateStatus {
  phase: 'idle' | 'checking' | 'downloading' | 'installing' | 'restarting' | 'error';
  progress: number;
  message: string;
  latest: Release | null;
  releases: Release[];
  lastCheck: number | string | null;
}

export interface UpdateInfo {
  current: string;
  platform: string;
  package: string;
  repo: string;
  hasToken: boolean;
  autoCheck: boolean;
  previous: string | null;
  history: { from: string; to: string; at: number | string; outcome: string }[];
  status: UpdateStatus;
  supervised: boolean;
}

const ACTIVE = new Set(['checking', 'downloading', 'installing', 'restarting']);

/** "1.2.10" > "1.2.9". */
export function compareVersions(a: string, b: string): number {
  const pa = a.replace(/^v/, '').split(/[.-]/).map((x) => parseInt(x, 10) || 0);
  const pb = b.replace(/^v/, '').split(/[.-]/).map((x) => parseInt(x, 10) || 0);
  for (let i = 0; i < Math.max(pa.length, pb.length); i++) {
    const d = (pa[i] ?? 0) - (pb[i] ?? 0);
    if (d) return d;
  }
  return 0;
}

const dateOf = (r: Release) => r.publishedAt ?? r.date;
const notesOf = (r: Release) => (r.notes ?? r.body ?? '').trim();
const sleep = (ms: number) => new Promise((r) => setTimeout(r, ms));

/** A translated sentence with `{v}` drawn in bold. */
function withBold(text: string, v: string): ReactNode {
  const [a, b = ''] = text.split('{v}');
  return (
    <>
      {a}
      <strong>{v}</strong>
      {b}
    </>
  );
}

export function Updates() {
  const [info, setInfo] = useState<UpdateInfo | null>(null);
  const [token, setToken] = useState('');
  const [allowOlder, setAllowOlder] = useState(false);
  const [waiting, setWaiting] = useState<string | null>(null);
  const file = useRef<HTMLInputElement>(null);

  const load = useCallback(async () => {
    try {
      const u = await api<UpdateInfo>('GET', '/api/system/update');
      setInfo(u);
      return u;
    } catch {
      return null;
    }
  }, []);

  useEffect(() => void load(), [load]);

  /** Waits for the server to come back with another version, then reloads the page. */
  const waitForReturn = useCallback(async (from: string) => {
    setWaiting(from);
    const until = Date.now() + 120_000;
    while (Date.now() < until) {
      await sleep(1000);
      try {
        const res = await fetch('/api/health', { cache: 'no-store' });
        if (!res.ok) continue;
        const h = (await res.json()) as { version?: string };
        if (h.version && h.version !== from) {
          useApp.getState().toast(t('Strata Cut {version} is running', { version: h.version }));
          setTimeout(() => location.reload(), 600);
          return;
        }
      } catch {
        /* restarting */
      }
    }
    setWaiting(null);
    useApp.getState().toast(t('The server did not come back within 2 minutes: check its logs.'), 'error');
    void load();
  }, [load]);

  // Reload every 700 ms during an active phase; restarting → wait for the new version.
  const phase = info?.status.phase;
  useEffect(() => {
    if (!info || !phase || !ACTIVE.has(phase) || waiting) return;
    if (phase === 'restarting') {
      void waitForReturn(info.current);
      return;
    }
    const id = window.setTimeout(async () => {
      const u = await load();
      if (!u) void waitForReturn(info.current); // gone: restarting
    }, 700);
    return () => clearTimeout(id);
  }, [info, phase, waiting, load, waitForReturn]);

  if (!info) {
    return (
      <>
        <PageHead title={t('Updates')} />
        <div className="loading">{t('Loading…')}</div>
      </>
    );
  }

  const s = info.status;
  const busy = ACTIVE.has(s.phase) || !!waiting;
  const latest = s.latest;
  const newer = latest && compareVersions(latest.version, info.current) > 0 ? latest : null;
  const releases = [...(s.releases ?? [])].sort((a, b) => compareVersions(b.version, a.version));

  const check = async () => {
    setInfo({ ...info, status: { ...s, phase: 'checking', message: '' } });
    try {
      await api('POST', '/api/system/update/check');
    } catch {
      /* the status carries the error */
    }
    await load();
  };

  const install = async (version: string) => {
    const back = compareVersions(version, info.current) < 0;
    const ok = await confirmDialog({
      title: back ? t('Go back to version {v}?', { v: version }) : t('Install Strata Cut {v}?', { v: version }),
      message: t('The server restarts: it is unavailable for a few seconds.'),
      confirm: back ? t('Go back') : t('Install'),
      danger: back,
    });
    if (!ok) return;
    try {
      await api('POST', `/api/system/update/install?version=${encodeURIComponent(version)}`);
      await load();
    } catch (e) {
      showError(e);
    }
  };

  const rollback = async () => {
    if (!info.previous) return;
    const ok = await confirmDialog({ title: t('Go back to version {v}?', { v: info.previous }), message: t('The server restarts: it is unavailable for a few seconds.'), confirm: t('Go back'), danger: true });
    if (!ok) return;
    try {
      await api('POST', '/api/system/update/rollback');
      void waitForReturn(info.current);
    } catch (e) {
      showError(e);
    }
  };

  const upload = async (f: File) => {
    const ok = await confirmDialog({ title: t('Install the package “{name}”?', { name: f.name }), message: t('The server restarts: it is unavailable for a few seconds.'), confirm: t('Install') });
    if (!ok) return;
    try {
      await api('POST', `/api/system/update/upload?allowOlder=${allowOlder}`, f);
      void waitForReturn(info.current);
    } catch (e) {
      showError(e);
    }
  };

  const saveSettings = async (patch: { repo?: string; token?: string | null; autoCheck?: boolean }) => {
    try {
      await api('PUT', '/api/system/update/settings', { repo: info.repo, token: null, autoCheck: info.autoCheck, ...patch });
      if (patch.token !== undefined && patch.token !== null) useApp.getState().toast(patch.token ? t('Token saved') : t('Token removed'));
    } catch (e) {
      showError(e);
    }
    await load();
  };

  const phaseText: Record<string, string> = { checking: t('Checking…'), downloading: t('Downloading…'), installing: t('Installing…'), restarting: t('Restarting…') };
  const busyText = waiting ? t('Restarting…') : s.message ? friendlyError(s.message) : (phaseText[s.phase] ?? '');

  return (
    <>
      <PageHead title={t('Updates')} />
      <Card
        icon={<CloudDownload size={16} />}
        title={t('Updates')}
        actions={
          <button type="button" className="btn small" disabled={busy} onClick={() => void check()}>
            <RefreshCw size={13} className={s.phase === 'checking' ? 'spin' : undefined} /> {t('Check now')}
          </button>
        }
      >
        <div className="update-head">
          <div>
            <span className="set-label">{t('Installed')}</span>
            <span className="update-version">{info.current}</span>
            <span className="muted">{info.platform}</span>
          </div>
          <div className="update-latest">
            {busy ? (
              <span className="update-busy">
                <RefreshCw size={14} className="spin" /> {busyText}
                {s.phase === 'downloading' && !waiting && <progress value={s.progress} max={1} />}
              </span>
            ) : s.phase === 'error' ? (
              <span className="update-error" title={s.message}>
                {friendlyError(s.message)}
              </span>
            ) : newer ? (
              <>
                <span>{withBold(t('Version {v} is available ({date}).', { date: formatDate(dateOf(newer)) }), newer.version)}</span>
                <button type="button" className="btn primary" onClick={() => void install(newer.version)}>
                  <CloudDownload size={14} /> {t('Install {v}', { v: newer.version })}
                </button>
              </>
            ) : s.lastCheck ? (
              <span className="muted">{t('Up to date: latest version {v}, checked {ago}.', { v: latest?.version ?? info.current, ago: timeAgo(s.lastCheck) })}</span>
            ) : (
              <span className="muted">{t('Not checked yet.')}</span>
            )}
          </div>
        </div>

        {newer && notesOf(newer) && (
          <details className="update-notes">
            <summary>{t('What’s new in {v}', { v: newer.version })}</summary>
            <pre>{notesOf(newer)}</pre>
          </details>
        )}

        {releases.length > 0 && (
          <div className="update-list">
            <span className="set-label">{t('Available versions')}</span>
            <div className="update-rows">
              {releases.map((r) => {
                const here = r.version === info.current;
                const up = compareVersions(r.version, info.current) > 0;
                return (
                  <div key={r.version} className={`update-row ${here ? 'here' : ''}`}>
                    <strong>{r.version}</strong>
                    <span className="muted">{formatDate(dateOf(r))}</span>
                    {r.size ? <span className="muted">{formatBytes(r.size)}</span> : null}
                    <div className="spacer" />
                    {here ? (
                      <span className="badge">{t('Installed')}</span>
                    ) : (
                      <button type="button" className="btn small" disabled={busy} onClick={() => void install(r.version)}>
                        {up ? <CloudDownload size={13} /> : <RotateCcw size={13} />} {up ? t('Install') : t('Go back')}
                      </button>
                    )}
                  </div>
                );
              })}
            </div>
          </div>
        )}

        {info.history.length > 0 && (
          <div className="update-history">
            {[...info.history]
              .reverse()
              .slice(0, 5)
              .map((h, i) => (
                <p key={i} className="hint">
                  <History size={12} />{' '}
                  {h.outcome === 'restored automatically'
                    ? t('Version {from} did not start: {to} was put back automatically', { from: h.from, to: h.to })
                    : `${h.from} → ${h.to} (${t(h.outcome)}), ${timeAgo(h.at)}`}
                </p>
              ))}
          </div>
        )}

        <div className="set-grid">
          <Field label={t('GitHub repository')}>
            <TextInput value={info.repo} commitOnBlur spellCheck={false} placeholder={t('owner/name')} onChange={(repo) => void saveSettings({ repo: repo.trim() })} />
          </Field>
          <Field
            label={
              <>
                {t('Access token')} {info.hasToken && <span className="badge">{t('saved')}</span>}
              </>
            }
            group
          >
            <input
              type="password"
              value={token}
              autoComplete="new-password"
              spellCheck={false}
              placeholder={info.hasToken ? '••••••••' : 'github_pat_…'}
              onChange={(e) => setToken(e.target.value)}
              onKeyDown={(e) => {
                if (e.key !== 'Escape') e.stopPropagation();
                if (e.key === 'Enter' && token.trim()) void saveSettings({ token: token.trim() }).then(() => setToken(''));
              }}
            />
            {(token.trim() || info.hasToken) && (
              <div className="row">
                {token.trim() && (
                  <button type="button" className="btn small" onClick={() => void saveSettings({ token: token.trim() }).then(() => setToken(''))}>
                    {t('Save the token')}
                  </button>
                )}
                {info.hasToken && (
                  <button type="button" className="btn small danger" onClick={() => void saveSettings({ token: '' })}>
                    {t('Remove the token')}
                  </button>
                )}
              </div>
            )}
          </Field>
          <Field label={t('Automatic check')} group>
            <Switch checked={info.autoCheck} onChange={(autoCheck) => void saveSettings({ autoCheck })} label={t('Look for a new version every 6 hours')} />
          </Field>
        </div>

        <div className="update-offline">
          <div>
            <strong>{t('Offline update')}</strong>
            <span className="muted">
              <code>{info.package}</code>
            </span>
          </div>
          <Switch checked={allowOlder} onChange={setAllowOlder} label={t('Allow older versions')} />
          <button type="button" className="btn" disabled={busy} onClick={() => file.current?.click()}>
            <Upload size={14} /> {t('Import a package')}
          </button>
          <input
            ref={file}
            type="file"
            accept=".gz,.tgz,.zip,.pkg,.msi"
            hidden
            onChange={(e) => {
              const f = e.target.files?.[0];
              e.target.value = '';
              if (f) void upload(f);
            }}
          />
          {info.previous && (
            <button type="button" className="btn" disabled={busy} onClick={() => void rollback()}>
              <RotateCcw size={14} /> {t('Go back to {v}', { v: info.previous })}
            </button>
          )}
        </div>
      </Card>
    </>
  );
}
