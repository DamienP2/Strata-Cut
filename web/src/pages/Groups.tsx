/*
 * Système › Accès › Groupes: the groups of users, each with its members and what they may do.
 * The base groups (operators, streamers, readers) are always there; the others are made here,
 * to give more rights or only to share clips with. An account gets what each of its groups gives.
 */
import { useEffect, useMemo, useState, type ComponentType } from 'react';
import { Eye, Plus, Radio, Scissors, Search, Trash2, UsersRound, X } from 'lucide-react';
import { create } from 'zustand';
import { t, tn } from '../i18n';
import { api, useApp, useChanged, type PublicUser } from '../store';
import { confirmDialog } from '../ui/dialogs';
import { showError } from '../ui/serverError';
import { displayName, initial } from '../ui/UserMenu';
import { Field, Switch } from '../settings/ui';
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

function GroupModal({ group, users, onClose }: { group: Group | null; users: PublicUser[]; onClose: () => void }) {
  const perms = usePerms((s) => s.perms) ?? [];
  const [name, setName] = useState(group?.name ?? '');
  const [members, setMembers] = useState<Set<string>>(() => new Set(group?.members ?? []));
  const [rights, setRights] = useState<Set<string>>(() => new Set(group?.rights ?? []));
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
    const body = { name: name.trim(), members: [...members], rights: [...rights] };
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
  const keys = (e: React.KeyboardEvent) => e.key !== 'Escape' && e.stopPropagation();
  return (
    <div className="modal-back" onPointerDown={onClose}>
      <form className="modal xwide gr-modal" role="dialog" onPointerDown={(e) => e.stopPropagation()} onSubmit={submit} onKeyDown={keys}>
        <header>
          {group && <GroupIcon g={group} />}
          <h2>{group ? groupName(group) : t('New group')}</h2>
          <button type="button" className="icon-btn ghost" title={t('Close')} onClick={onClose}>
            <X size={16} />
          </button>
        </header>
        <div className="gr-modal-body">
          <div className="gr-col">
            <Field label={t('Name')}>
              <input value={base ? groupName(group!) : name} disabled={base} autoFocus={!base} maxLength={60} placeholder={t('Journalists, Sport desk…')} onChange={(e) => setName(e.target.value)} />
            </Field>
            <Field label={t('Members')} group hint={tn(members.size, '{n} member', '{n} members')}>
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
            </Field>
          </div>
          <div className="gr-col">
            <Field label={t('Rights')} group>
              <div className="gr-rights">
                {partsOf(perms).map(([part, list]) => (
                  <div key={part} className="gr-part">
                    <span className="gr-part-name">{t(part)}</span>
                    {list.map((p) => (
                      <Switch key={p.key} checked={rights.has(p.key)} onChange={() => flip(setRights, p.key)} label={t(p.label)} />
                    ))}
                  </div>
                ))}
              </div>
            </Field>
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
