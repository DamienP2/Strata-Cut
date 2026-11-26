/* /system/<section>: the list of sections on the left, the open one on the right. */
import { useDisplayPref } from '../displayPrefs';
import { lazy, Suspense, useEffect, type ComponentType, type LazyExoticComponent } from 'react';
import { Bell, Cpu, HardDrive, KeyRound, Gauge, HeartPulse, Lock, Radio, RadioTower, SlidersHorizontal, Upload, Users as UsersIcon, UsersRound, MonitorPlay, Wrench } from 'lucide-react';
import { t } from '../i18n';
import { navigate } from '../router';
import { can, useApp, useChanged, type PublicUser } from '../store';
import { needsLook, useHealth } from '../ui/health';
import { NavItem, NavSection, SidePage } from './SidePage';
import { loadUsers, useUserList } from './userList';

const page = <K extends string>(load: () => Promise<Record<K, ComponentType>>, name: K) => lazy(() => load().then((m) => ({ default: m[name] })));

interface Section {
  id: string;
  /** English, translated where shown. */
  label: string;
  icon: ComponentType<{ size?: number }>;
  admin: boolean;
  page: LazyExoticComponent<ComponentType>;
}

/** Sections now part of another page: their old addresses open it. */
const MOVED: Record<string, string> = { performance: 'health', updates: 'maintenance' };

interface Group {
  title: string;
  sections: Section[];
}

export const SYSTEM: Group[] = [
  {
    title: 'Monitoring',
    sections: [
      // The machine's load at the top of it.
      { id: 'health', label: 'Health', icon: HeartPulse, admin: true, page: page(() => import('../settings/Health'), 'Health') },
      { id: 'resources', label: 'Resources', icon: Gauge, admin: true, page: page(() => import('../settings/Resources'), 'Resources') },
      { id: 'alerts', label: 'Alerts', icon: Bell, admin: true, page: page(() => import('../settings/Alerts'), 'Alerts') },
    ],
  },
  {
    // What the server receives (and records), where it can send it.
    title: 'Streams',
    sections: [
      { id: 'sources', label: 'Sources', icon: RadioTower, admin: true, page: page(() => import('./Recorders'), 'Recorders') },
      { id: 'destinations', label: 'Destinations', icon: Radio, admin: true, page: page(() => import('../settings/Destinations'), 'Destinations') },
      { id: 'sends', label: 'File servers', icon: Upload, admin: true, page: page(() => import('../settings/Sends'), 'Sends') },
    ],
  },
  {
    title: 'Access',
    sections: [
      { id: 'users', label: 'Users', icon: UsersIcon, admin: true, page: page(() => import('./Users'), 'Users') },
      { id: 'groups', label: 'Groups', icon: UsersRound, admin: true, page: page(() => import('./Groups'), 'Groups') },
      { id: 'roles', label: 'Rights', icon: KeyRound, admin: true, page: page(() => import('../settings/Roles'), 'Roles') },
      { id: 'security', label: 'Security', icon: Lock, admin: true, page: page(() => import('../settings/Security'), 'Security') },
    ],
  },
  {
    title: 'Server',
    sections: [
      { id: 'general', label: 'General', icon: SlidersHorizontal, admin: true, page: page(() => import('../settings/General'), 'General') },
      { id: 'storage', label: 'Storage', icon: HardDrive, admin: true, page: page(() => import('../settings/Storage'), 'Storage') },
      { id: 'hardware', label: 'Hardware', icon: Cpu, admin: true, page: page(() => import('../settings/Hardware'), 'Hardware') },
      { id: 'youtube', label: 'YouTube', icon: MonitorPlay, admin: true, page: page(() => import('../settings/Youtube'), 'Youtube') },
      // The updates in it.
      { id: 'maintenance', label: 'Maintenance', icon: Wrench, admin: true, page: page(() => import('../settings/Maintenance'), 'Maintenance') },
    ],
  },
];

/** The sections this user may open. */
export function visibleSystem(user: PublicUser | null | undefined): Group[] {
  return SYSTEM.map((g) => ({ ...g, sections: g.sections.filter((s) => !s.admin || can.admin(user)) })).filter((g) => g.sections.length > 0);
}

export function SystemPage({ section }: { section?: string }) {
  const user = useApp((s) => s.auth?.user);
  const health = useHealth((s) => s.health);
  const users = useUserList((s) => s.users);
  const groups = visibleSystem(user);
  const all = groups.flatMap((g) => g.sections);
  const open = all.find((s) => s.id === section);
  const admin = can.admin(user);

  // No section (or one this user may not open): the last one opened, else the first visible one.
  const [last, setLast] = useDisplayPref<string | null>('system.section', null);
  useEffect(() => {
    if (section && MOVED[section]) void navigate(`/system/${MOVED[section]}`, true);
    else if (!open && all.length) void navigate(`/system/${all.some((s) => s.id === last) ? last : all[0].id}`, true);
    else if (!all.length) void navigate('/', true);
    else if (open && open.id !== last) setLast(open.id);
  }, [open, all]); // eslint-disable-line react-hooks/exhaustive-deps

  useEffect(() => {
    if (admin) void loadUsers();
  }, [admin]);
  useChanged('users', () => admin && void loadUsers());

  const Page = open?.page;
  return (
    <SidePage
      className="sys-page"
      wide={open?.id === 'sources' || open?.id === 'destinations' || open?.id === 'sends'}
      nav={groups.map((g) => (
        <div key={g.title} className="sys-group">
          <NavSection title={t(g.title)} />
          {g.sections.map((s) => (
            <NavItem
              key={s.id}
              icon={s.icon}
              label={t(s.label)}
              on={s.id === open?.id}
              onClick={() => void navigate(`/system/${s.id}`)}
              status={s.id === 'health' && needsLook(health) ? 'warn' : null}
              count={s.id === 'users' && users ? users.length : undefined}
            />
          ))}
        </div>
      ))}
    >
      {Page && (
        <Suspense fallback={<div className="loading">{t('Loading…')}</div>}>
          <Page key={open.id} />
        </Suspense>
      )}
    </SidePage>
  );
}
