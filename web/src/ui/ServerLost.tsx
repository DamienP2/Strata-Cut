/*
 * The server does not answer any more (the connection lost, and not back): a countdown, then
 * signed out after 60 s. Back in time: it goes away by itself.
 */
import { useEffect, useState } from 'react';
import { useApp } from '../store';
import { t } from '../i18n';

const GRACE_MS = 60_000;
/** Lost for this long before the countdown shows (a short cut is not one). */
const SHOW_AFTER_MS = 3_000;

export function ServerLost() {
  const connected = useApp((s) => s.connected);
  const [since, setSince] = useState<number | null>(null);
  const [now, setNow] = useState(Date.now());
  useEffect(() => {
    if (connected) return setSince(null);
    setSince((x) => x ?? Date.now());
  }, [connected]);
  useEffect(() => {
    if (since === null) return;
    const i = setInterval(() => setNow(Date.now()), 250);
    return () => clearInterval(i);
  }, [since]);
  const left = since === null ? GRACE_MS : GRACE_MS - (now - since);
  useEffect(() => {
    if (since !== null && left <= 0) {
      setSince(null);
      void useApp.getState().logout();
      useApp.getState().toast(t('The server did not answer for a minute: you were signed out.'), 'error');
    }
  }, [left, since]);
  if (since === null || now - since < SHOW_AFTER_MS) return null;
  const s = Math.max(0, Math.ceil(left / 1000));
  const r = 12;
  const c = 2 * Math.PI * r;
  return (
    <div className="server-lost" role="alert">
      <svg className="server-lost-ring" viewBox="0 0 30 30" aria-hidden="true">
        <circle className="bg" cx="15" cy="15" r={r} />
        <circle className="fg" cx="15" cy="15" r={r} strokeDasharray={c} strokeDashoffset={c * (1 - Math.max(0, left) / GRACE_MS)} />
      </svg>
      <div>
        <strong>{t('The server does not answer')}</strong>
        <div className="muted">{t('Reconnecting… signed out in {s}', { s: `0:${String(s).padStart(2, '0')}` })}</div>
      </div>
      <button type="button" className="btn small" onClick={() => void useApp.getState().logout()}>
        {t('Sign out now')}
      </button>
    </div>
  );
}
