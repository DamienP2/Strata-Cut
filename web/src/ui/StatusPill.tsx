/*
 * The connection pill: « ● Connecté », « Connexion… », « CPU 92 % » when the
 * machine is above 85 % (back under 80 %), « Serveur perdu · reconnexion… »
 * when the WebSocket has been closed for 800 ms (4 s on the first load).
 */
import { useEffect, useRef, useState } from 'react';
import { TriangleAlert, WifiOff } from 'lucide-react';
import { useShallow } from 'zustand/react/shallow';
import { t } from '../i18n';
import { navigate } from '../router';
import { api, can, useApp, type AuthState } from '../store';
import { formatBytes } from './format';
import { useMetrics, watchMetrics } from './metrics';

const started = Date.now();

export function StatusPill() {
  const { connected, user, serverName } = useApp(useShallow((s) => ({ connected: s.connected, user: s.auth?.user, serverName: s.auth?.serverName ?? '' })));
  const machine = useMetrics((s) => s.metrics?.machine);
  const [lost, setLost] = useState(false);
  const hot = useRef(false);

  useEffect(() => watchMetrics(), []);

  // Lost after 800 ms without the socket (4 s on the first load).
  useEffect(() => {
    if (connected) {
      setLost(false);
      return;
    }
    const wait = Date.now() - started < 4000 ? 4000 - (Date.now() - started) : 800;
    const id = window.setTimeout(() => setLost(true), Math.max(800, wait));
    return () => clearTimeout(id);
  }, [connected]);

  // While lost: is the session still there? (the server may have restarted)
  useEffect(() => {
    if (!lost) return;
    const id = window.setInterval(async () => {
      try {
        const a = await api<AuthState>('GET', '/api/auth/state');
        if (!a.user) useApp.setState({ auth: a });
      } catch {
        /* still away */
      }
    }, 3000);
    return () => clearInterval(id);
  }, [lost]);

  const cpu = machine ? Math.round(machine.cpu) : null;
  if (cpu !== null) {
    if (cpu > 85) hot.current = true;
    else if (cpu < 80) hot.current = false;
  }

  const state = lost ? 'lost' : !connected ? 'wait' : hot.current ? 'hot' : 'ok';
  const stateText = state === 'lost' ? t('Server lost · reconnecting…') : state === 'wait' ? t('Connecting…') : t('Connected');
  const tip = [
    serverName,
    stateText,
    machine && `${t('CPU {n}%', { n: cpu ?? 0 })} · ${t('{n} cores', { n: machine.cores })}`,
    machine && t('Memory {used} of {total}', { used: formatBytes(machine.memUsed), total: formatBytes(machine.memTotal) }),
    machine?.hostname,
  ]
    .filter(Boolean)
    .join('\n');

  const body =
    state === 'lost' ? (
      <>
        <WifiOff size={14} />
        <span className="ux1-lost-txt">
          <span>{t('Server lost')}</span>
          <span className="ux1-lost-sep"> · </span>
          <span>{t('reconnecting…')}</span>
        </span>
      </>
    ) : state === 'hot' ? (
      <>
        <TriangleAlert size={14} />
        {t('CPU {n}%', { n: cpu ?? 0 })}
      </>
    ) : (
      <>
        <span className="ux1-dot" />
        {state === 'wait' ? t('Connecting…') : t('Connected')}
      </>
    );

  const cls = `ux1-pill ${state === 'ok' ? '' : state}`;
  if (can.admin(user))
    return (
      <button type="button" className={cls} title={tip} onClick={() => navigate('/system/performance')}>
        {body}
      </button>
    );
  return (
    <span className={cls} title={tip}>
      {body}
    </span>
  );
}
