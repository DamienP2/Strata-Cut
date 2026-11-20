import { lazy, Suspense, useEffect } from 'react';
import { CalendarClock, CircleHelp, Clapperboard, Radio, Scissors, Server } from 'lucide-react';
import { useShallow } from 'zustand/react/shallow';
import { loadDisplayPrefs, useDisplayPrefs } from './displayPrefs';
import { t, useLang } from './i18n';
import { navigate, useRoute, type Route } from './router';
import { can, useApp } from './store';
import { Brand } from './ui/Brand';
import { ContextMenuHost } from './ui/ContextMenu';
import { CrashBanner } from './ui/CrashBanner';
import { DialogHost } from './ui/dialogs';
import { needsLook, useHealth, watchHealth } from './ui/health';
import { Shortcuts } from './ui/Shortcuts';
import { StatusPill } from './ui/StatusPill';
import { Toasts } from './ui/Toasts';
import { FollowTip } from './ui/FollowTip';
import { ServerLost } from './ui/ServerLost';
import { UserMenu } from './ui/UserMenu';
import { Login } from './pages/Login';
import { SystemPage, visibleSystem } from './pages/SystemPage';

const Streamer = lazy(() => import('./pages/Streamer').then((m) => ({ default: m.Streamer })));
const Extract = lazy(() => import('./pages/Extract').then((m) => ({ default: m.Extract })));
const Schedule = lazy(() => import('./pages/Schedule').then((m) => ({ default: m.Schedule })));
const Clips = lazy(() => import('./pages/Clips').then((m) => ({ default: m.Clips })));
const Help = lazy(() => import('./pages/Help').then((m) => ({ default: m.Help })));

export function App() {
  const route = useRoute();
  const { auth, init, connect } = useApp(useShallow((s) => ({ auth: s.auth, init: s.init, connect: s.connect })));
  const user = auth?.user;
  const lang = useLang((s) => s.lang); // everything is redrawn in the new language
  useEffect(() => void init(), [init]);
  useEffect(() => {
    if (user) connect();
  }, [user, connect]);
  const prefsOf = useDisplayPrefs((s) => s.user);
  useEffect(() => {
    if (user) void loadDisplayPrefs(user.id);
  }, [user]);
  useEffect(() => {
    document.title = 'Strata Cut';
  }, []);
  if (!auth) return <div className="loading">{t('Connecting…')}</div>;
  if (!user) return <Login key={lang} />;
  if (prefsOf !== user.id) return <div className="loading">{t('Connecting…')}</div>;
  return (
    <div className="app" key={lang}>
      <Shell route={route} />
      {can.admin(user) && <CrashBanner hidden={route.page === 'extract'} />}
      <Shortcuts />
      <ContextMenuHost />
      <DialogHost />
      <Toasts />
      <FollowTip />
      <ServerLost />
    </div>
  );
}

function HelpButton({ on }: { on: boolean }) {
  return (
    <button type="button" className={`learn-help ${on ? 'on' : ''}`} title={t('Help')} aria-current={on ? 'page' : undefined} onClick={() => void navigate('/help')}>
      <CircleHelp size={17} />
    </button>
  );
}

function Tab({ on, icon, label, path, dot, count }: { on: boolean; icon: React.ReactNode; label: string; path: string; dot?: boolean; count?: number }) {
  return (
    <button type="button" className={on ? 'on' : ''} aria-current={on ? 'page' : undefined} onClick={() => void navigate(path)}>
      {icon} <span className="tab-label">{label}</span>
      {dot && <span className="nav-dot warn" />}
      {!!count && <span className="tab-count">{count}</span>}
    </button>
  );
}

function Shell({ route }: { route: Route }) {
  const user = useApp((s) => s.auth?.user);
  const admin = can.admin(user);
  const health = useHealth((s) => s.health);
  const system = visibleSystem(user).length > 0;
  const streamer = can.extract(user);
  // The extractions under way, on the Clips tab (seen from every page).
  const extracting = useApp((s) => s.jobs.filter((j) => j.state === 'queued' || j.state === 'running').length);

  useEffect(() => {
    if (admin) return watchHealth();
  }, [admin]);

  // A page this user may not open goes back home.
  useEffect(() => {
    if (route.page === 'system' && !system) void navigate('/extract', true);
    else if (route.page === 'streamer' && !streamer) void navigate('/extract', true);
    // The sources are in Système now; the home is Extraction.
    else if (route.page === 'recorders') void navigate(system ? '/system/sources' : '/extract', true);
    else if (route.page === 'home') void navigate('/extract', true);
  }, [route.page, system, streamer]);

  let page: React.ReactNode;
  switch (route.page) {
    case 'extract':
      page = <Extract recorder={route.recorder} />;
      break;
    case 'clips':
      page = <Clips />;
      break;
    case 'schedule':
      page = <Schedule />;
      break;
    case 'system':
      page = system ? <SystemPage section={route.section} /> : null;
      break;
    case 'help':
      page = <Help />;
      break;
    case 'streamer':
      page = streamer ? <Streamer source={route.source} /> : null;
      break;
    default:
      page = null;
  }

  return (
    <>
      <nav className="topbar">
        <Brand />
        <div className="tabs home-tabs">
          <Tab on={route.page === 'schedule'} icon={<CalendarClock size={14} />} label={t('Schedule')} path="/schedule" />
          <Tab on={route.page === 'extract'} icon={<Scissors size={14} />} label={t('Extraction')} path="/extract" />
          {streamer && <Tab on={route.page === 'streamer'} icon={<Radio size={14} />} label={t('Streamer')} path="/streamer" />}
          <Tab on={route.page === 'clips'} icon={<Clapperboard size={14} />} label={t('Clips')} path="/clips" count={extracting} />
          {system && <Tab on={route.page === 'system'} icon={<Server size={14} />} label={t('System')} path="/system" dot={admin && needsLook(health)} />}
        </div>
        <div className="spacer" />
        <StatusPill />
        <HelpButton on={route.page === 'help'} />
        <UserMenu named />
      </nav>
      <div className="content">
        <Suspense fallback={<div className="loading">{t('Loading…')}</div>}>{page}</Suspense>
      </div>
    </>
  );
}
