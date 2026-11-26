/*
 * Système › Accès › Groupes: the groups of users, each with its members and what they may do.
 * The base groups (operators, streamers, readers) are always there; the others are made here,
 * to give more rights or only to share clips with. An account gets what each of its groups gives.
 */
import { useEffect, useMemo, useState, type ComponentType } from 'react';
import { Eye, Plus, Radio, Scissors, Search, Trash2, UsersRound, X } from 'lucide-react';
import { create } from 'zustand';
import { t, tc, tn } from '../i18n';
import { api, useApp, useChanged, type PublicUser } from '../store';
import { confirmDialog } from '../ui/dialogs';
import { showError } from '../ui/serverError';
import { displayName, initial } from '../ui/UserMenu';
import { Switch } from '../settings/ui';
import { RecLogo } from '../ui/RecLogo';
import { KIND_TAG, useSendTargets, type SendTarget } from '../sends';
import { useDestinations } from '../streams';
import { PageHead } from './SidePage';
import { loadUsers, useUserList } from './userList';

export interface Group {
  id: string;
  name: string;
  /** The accounts' ids (an admin sees them all; another account, only itself). */
  members: string[];
  /** What its members may do (only an admin sees them). */
  rights: string[];
  /** A base group: its name is fixed, it cannot be deleted. */
  base: boolean;
  /** The sources its members see, the file servers and the destinations they may send to. */
  sources?: Scope;
  sends?: Scope;
  dests?: Scope;
}

/** All of them, or these only. */
export interface Scope {
  all: boolean;
  ids: string[];
}

/** The groups, reloaded when the server says they changed. */
export const useGroups = create<{ groups: Group[] | null }>(() => ({ groups: null }));
export async function loadGroups() {
  try {
    useGroups.setState({ groups: await api<Group[]>('GET', '/api/groups') });
  } catch {
    /* kept as it was */
  }
}

/** A group's name as shown (a base group's, translated). */
export const groupName = (g: Pick<Group, 'name' | 'base'>) => (g.base ? t(g.name) : g.name);

const BASE_ICON: Record<string, ComponentType<{ size?: number }>> = { operators: Scissors, streamers: Radio, viewers: Eye };
export function GroupIcon({ g, size = 15 }: { g: Pick<Group, 'id' | 'base'>; size?: number }) {
  const Icon = (g.base && BASE_ICON[g.id]) || UsersRound;
  return (
    <span className={`gr-ico ${g.base ? g.id : 'other'}`}>
      <Icon size={size} />
    </span>
  );
}

/** A right: its key, its part of the app, what it lets do (English). */
export interface Perm {
  key: string;
  part: string;
  /** The clips': "Theirs" / "The others'" (else empty). */
  sub: string;
  label: string;
}
/** The rights there are (asked once, admins only). */
export const usePerms = create<{ perms: Perm[] | null }>(() => ({ perms: null }));
export async function loadPerms() {
  if (usePerms.getState().perms) return;
  try {
    usePerms.setState({ perms: (await api<{ perms: Perm[] }>('GET', '/api/roles')).perms });
  } catch {
    /* none shown */
  }
}
/** The rights by part of the app, in order. */
export function partsOf(perms: Perm[]): [string, Perm[]][] {
  const out: [string, Perm[]][] = [];
  for (const p of perms) {
    const last = out[out.length - 1];
    if (last && last[0] === p.part) last[1].push(p);
    else out.push([p.part, [p]]);
  }
  return out;
}

/** What rights give, by part: « Extraits · 3/4 », a whole part by its name alone. */
export function RightsSummary({ rights, all }: { rights: string[]; all?: boolean }) {
  const perms = usePerms((s) => s.perms) ?? [];
  if (all) return <span className="gr-area">{t('Everything, Système included')}</span>;
  const parts = partsOf(perms)
    .map(([part, list]) => [part, list.filter((p) => rights.includes(p.key)).length, list.length] as const)
    .filter(([, n]) => n > 0);
  if (parts.length === 0) return <span className="gr-area none">{t('No right · sharing only')}</span>;
  return (
    <>
      {parts.map(([part, n, of]) => (
        <span key={part} className={`gr-area ${n < of ? 'part' : ''}`}>
          {t(part)}
          {n < of ? ` · ${n}/${of}` : ''}
        </span>
      ))}
    </>
  );
}

export function Groups() {
  const groups = useGroups((s) => s.groups);
  const users = useUserList((s) => s.users);
  const [editing, setEditing] = useState<Group | 'new' | null>(null);
  useEffect(() => {
    void loadGroups();
    void loadUsers();
    void loadPerms();
  }, []);
  useChanged('groups', () => void loadGroups());
  useChanged('users', () => void loadUsers());
  const byId = useMemo(() => new Map((users ?? []).map((u) => [u.id, u])), [users]);

  const card = (g: Group) => (
    <div key={g.id} className="gr-card" onClick={() => setEditing(g)}>
      <div className="gr-top">
        <GroupIcon g={g} />
        <div className="gr-name">
          <strong>{groupName(g)}</strong>
          <span className="muted">
            {tn(g.members.length, '{n} member', '{n} members')} · {tn(g.rights.length, '{n} right', '{n} rights')}
          </span>
        </div>
        {g.base && <span className="gr-badge">{t('base')}</span>}
      </div>
      <div className="gr-avs">
        {g.members.slice(0, 8).map((id) => {
          const u = byId.get(id);
          return u ? (
            <span key={id} className="user-avatar small" title={displayName(u)}>
              {initial(u)}
            </span>
          ) : null;
        })}
        {g.members.length > 8 && <span className="muted">+{g.members.length - 8}</span>}
        {g.members.length === 0 && <span className="muted">{t('No member yet')}</span>}
      </div>
      <div className="gr-areas">
        <RightsSummary rights={g.rights} />
      </div>
      <div className="gr-scopes">
        {(
          [
            [t('Sources'), g.sources],
            [t('File servers'), g.sends],
            [t('Destinations'), g.dests],
          ] as const
        ).map(([label, sc]) => (
          <span key={label}>
            {label} <b>{!sc || sc.all ? tc('scope', 'All') : sc.ids.length}</b>
          </span>
        ))}
      </div>
    </div>
  );

  return (
    <>
      <PageHead title={t('Groups')} sub={groups ? tn(groups.length, '{n} group', '{n} groups') : undefined}>
        <button type="button" className="btn primary" onClick={() => setEditing('new')}>
          <Plus size={14} /> {t('New group')}
        </button>
      </PageHead>
      {groups === null ? (
        <div className="loading">{t('Loading…')}</div>
      ) : (
        <>
          <div className="gr-sec">{t('Base groups')}</div>
          <div className="gr-grid">{groups.filter((g) => g.base).map(card)}</div>
          <div className="gr-sec">{t('Other groups')}</div>
          <div className="gr-grid">
            {groups.filter((g) => !g.base).map(card)}
            <button type="button" className="project-new gr-new" onClick={() => setEditing('new')}>
              <Plus size={20} />
              {t('New group')}
            </button>
          </div>
        </>
      )}
      {editing && <GroupModal group={editing === 'new' ? null : editing} users={users ?? []} onClose={() => setEditing(null)} />}
    </>
  );
}

/** The rights of a part, under their sub-heading when they have one (the clips'). */
export function subsOf(list: Perm[]): [string, Perm[]][] {
  const out: [string, Perm[]][] = [];
  for (const p of list) {
    const last = out[out.length - 1];
    if (last && last[0] === p.sub) last[1].push(p);
    else out.push([p.sub, [p]]);
  }
  return out;
}

type ModalTab = 'members' | 'rights' | 'sources' | 'sends' | 'dests';

/** All of them, or these only: a switch, then the list to tick. */
function ScopePick({ scope, items, onChange, empty }: { scope: Scope; items: { id: string; name: string; sub?: string; icon?: React.ReactNode }[]; onChange: (s: Scope) => void; empty: string }) {
  const flip = (id: string) => onChange({ all: false, ids: scope.ids.includes(id) ? scope.ids.filter((x) => x !== id) : [...scope.ids, id] });
  return (
    <div className="gm-scope">
      <Switch checked={scope.all} onChange={(all) => onChange({ all, ids: all ? [] : items.map((x) => x.id) })} label={t('All, the new ones too')} />
      {items.length === 0 && <span className="muted">{empty}</span>}
      <div className={`gm-list ${scope.all ? 'off' : ''}`}>
        {items.map((x) => (
          <label key={x.id} className={`gm-item ${scope.all || scope.ids.includes(x.id) ? 'on' : ''}`}>
            <input type="checkbox" checked={scope.all || scope.ids.includes(x.id)} disabled={scope.all} onChange={() => flip(x.id)} />
            {x.icon}
            <b>{x.name}</b>
            {x.sub && <span className="muted">{x.sub}</span>}
          </label>
        ))}
      </div>
    </div>
  );
}

function GroupModal({ group, users, onClose }: { group: Group | null; users: PublicUser[]; onClose: () => void }) {
  const perms = usePerms((s) => s.perms) ?? [];
  const recorders = useApp((s) => s.allRecorders);
  const [targets] = useSendTargets<SendTarget>();
  const [dests] = useDestinations();
  const [tab, setTab] = useState<ModalTab>('members');
  const [name, setName] = useState(group?.name ?? '');
  const [members, setMembers] = useState<Set<string>>(() => new Set(group?.members ?? []));
  const [rights, setRights] = useState<Set<string>>(() => new Set(group?.rights ?? []));
  const [sources, setSources] = useState<Scope>(group?.sources ?? { all: true, ids: [] });
  const [sends, setSends] = useState<Scope>(group?.sends ?? { all: true, ids: [] });
  const [destsScope, setDests] = useState<Scope>(group?.dests ?? { all: true, ids: [] });
  const [q, setQ] = useState('');
  const [busy, setBusy] = useState(false);
  const base = !!group?.base;
  // The admins may do everything: no group to give them.
  const shown = users.filter((u) => u.role !== 'admin' && (!q.trim() || `${u.username} ${u.displayName}`.toLowerCase().includes(q.trim().toLowerCase())));
  const flip = (set: (f: (s: Set<string>) => Set<string>) => void, id: string) =>
    set((m) => {
      const n = new Set(m);
      if (n.has(id)) n.delete(id);
      else n.add(id);
      return n;
    });
  const submit = async (e: React.FormEvent) => {
    e.preventDefault();
    if ((!base && !name.trim()) || busy) return;
    setBusy(true);
    const body = { name: name.trim(), members: [...members], rights: [...rights], sources, sends, dests: destsScope };
    try {
      if (group) await api('PUT', `/api/groups/${group.id}`, body);
      else await api('POST', '/api/groups', body);
      await loadGroups();
      useApp.getState().toast(group ? t('Group saved') : t('Group created'));
      onClose();
    } catch (err) {
      showError(err);
      setBusy(false);
    }
  };
  const remove = async () => {
    if (!group || !(await confirmDialog({ title: t('Delete the group “{name}”?', { name: group.name }), message: t('Its members lose what it gave them; the clips shared with it are no longer shared with them.'), confirm: t('Delete'), danger: true }))) return;
    try {
      await api('DELETE', `/api/groups/${group.id}`);
      await loadGroups();
      onClose();
    } catch (e) {
      showError(e);
    }
  };
  const count = (sc: Scope, n: number) => (sc.all ? tc('scope', 'All') : `${sc.ids.length}/${n}`);
  const tabs: { id: ModalTab; label: string; n: string }[] = [
    { id: 'members', label: t('Members'), n: String(members.size) },
    { id: 'rights', label: t('Rights'), n: String(rights.size) },
    { id: 'sources', label: t('Sources'), n: count(sources, recorders.length) },
    { id: 'sends', label: t('File servers'), n: count(sends, targets?.length ?? 0) },
    { id: 'dests', label: t('Destinations'), n: count(destsScope, dests?.length ?? 0) },
  ];
  const keys = (e: React.KeyboardEvent) => e.key !== 'Escape' && e.stopPropagation();
  return (
    <div className="modal-back" onPointerDown={onClose}>
      <form className="modal xwide gm" role="dialog" onPointerDown={(e) => e.stopPropagation()} onSubmit={submit} onKeyDown={keys}>
        <header>
          {group ? <GroupIcon g={group} /> : <GroupIcon g={{ id: '', base: false }} />}
          {base ? <h2>{groupName(group!)}</h2> : <input className="gm-name" value={name} autoFocus maxLength={60} placeholder={t('Name of the group')} onChange={(e) => setName(e.target.value)} />}
          <button type="button" className="icon-btn ghost" title={t('Close')} onClick={onClose}>
            <X size={16} />
          </button>
        </header>
        <div className="gm-body">
          <nav className="gm-tabs" role="tablist">
            {tabs.map((x) => (
              <button key={x.id} type="button" role="tab" aria-selected={tab === x.id} className={tab === x.id ? 'on' : ''} onClick={() => setTab(x.id)}>
                <span>{x.label}</span>
                <em>{x.n}</em>
              </button>
            ))}
          </nav>
          <div className="gm-pane">
            {tab === 'members' && (
              <div className="sc-members-pick">
                <label className="search">
                  <Search size={14} />
                  <input value={q} placeholder={t('Search users')} onChange={(e) => setQ(e.target.value)} />
                </label>
                <div className="sc-members-list">
                  {shown.map((u) => (
                    <label key={u.id} className={`sc-member-row ${members.has(u.id) ? 'on' : ''}`}>
                      <input type="checkbox" checked={members.has(u.id)} onChange={() => flip(setMembers, u.id)} />
                      <span className="user-avatar small">{initial(u)}</span>
                      <span className="sc-member-name">
                        <strong>{displayName(u)}</strong>
                        <span className="muted">@{u.username}</span>
                      </span>
                    </label>
                  ))}
                  {shown.length === 0 && <span className="muted">{t('No match')}</span>}
                </div>
              </div>
            )}
            {tab === 'rights' && (
              <div className="gm-rights">
                {partsOf(perms).map(([part, list]) => (
                  <section key={part} className="gm-part">
                    <h3>{t(part)}</h3>
                    {subsOf(list).map(([sub, items]) => (
                      <div key={sub} className="gm-sub">
                        {sub && <span className="gm-sub-name">{t(sub)}</span>}
                        <div className={sub ? 'gm-row' : 'gm-col'}>
                          {items.map((p) => (
                            <Switch key={p.key} checked={rights.has(p.key)} onChange={() => flip(setRights, p.key)} label={t(p.label)} />
                          ))}
                        </div>
                      </div>
                    ))}
                  </section>
                ))}
                <section className="gm-part dim">
                  <h3>{t('System')}</h3>
                  <span className="muted">{t('Administrators only')}</span>
                </section>
              </div>
            )}
            {tab === 'sources' && <ScopePick scope={sources} onChange={setSources} empty={t('No recorder')} items={recorders.map((r) => ({ id: r.id, name: r.name, icon: <RecLogo rec={r} size="sm" />, sub: r.protocol.toUpperCase() }))} />}
            {tab === 'sends' && <ScopePick scope={sends} onChange={setSends} empty={t('No file server')} items={(targets ?? []).map((x) => ({ id: x.id, name: x.name, sub: KIND_TAG[x.kind] }))} />}
            {tab === 'dests' && <ScopePick scope={destsScope} onChange={setDests} empty={t('No destination')} items={(dests ?? []).map((d) => ({ id: d.id, name: d.name, sub: d.protocol.toUpperCase() }))} />}
          </div>
        </div>
        <footer>
          {group && !base && (
            <button type="button" className="btn danger gr-del" onClick={() => void remove()}>
              <Trash2 size={14} /> {t('Delete the group')}
            </button>
          )}
          <button type="button" className="btn" onClick={onClose}>
            {t('Cancel')}
          </button>
          <button type="submit" className="btn primary" disabled={(!base && !name.trim()) || busy}>
            {group ? t('Save') : (
              <>
                <Plus size={14} /> {t('Create the group')}
              </>
            )}
          </button>
        </footer>
      </form>
    </div>
  );
}
