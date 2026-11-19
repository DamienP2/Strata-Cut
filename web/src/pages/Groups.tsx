/* Système › Accès › Groupes: groups of users, to share clips with. */
import { useEffect, useMemo, useState } from 'react';
import { Pencil, Plus, Search, Trash2, UsersRound, X } from 'lucide-react';
import { t, tn } from '../i18n';
import { api, useApp, useChanged, type PublicUser } from '../store';
import { confirmDialog } from '../ui/dialogs';
import { showError } from '../ui/serverError';
import { displayName, initial } from '../ui/UserMenu';
import { Field } from '../settings/ui';
import { PageHead } from './SidePage';
import { loadUsers, useUserList } from './userList';
import { create } from 'zustand';

export interface Group {
  id: string;
  name: string;
  /** The accounts' ids (an admin sees them all; another account, only itself). */
  members: string[];
}

/** The groups (to share clips with), reloaded when the server says they changed. */
export const useGroups = create<{ groups: Group[] | null }>(() => ({ groups: null }));
export async function loadGroups() {
  try {
    useGroups.setState({ groups: await api<Group[]>('GET', '/api/groups') });
  } catch {
    /* kept as it was */
  }
}

export function Groups() {
  const groups = useGroups((s) => s.groups);
  const users = useUserList((s) => s.users);
  const [editing, setEditing] = useState<Group | 'new' | null>(null);
  useEffect(() => {
    void loadGroups();
    void loadUsers();
  }, []);
  useChanged('groups', () => void loadGroups());
  useChanged('users', () => void loadUsers());
  const byId = useMemo(() => new Map((users ?? []).map((u) => [u.id, u])), [users]);

  const remove = async (g: Group) => {
    if (!(await confirmDialog({ title: t('Delete the group “{name}”?', { name: g.name }), message: t('The clips shared with it are no longer shared with its members.'), confirm: t('Delete'), danger: true }))) return;
    try {
      await api('DELETE', `/api/groups/${g.id}`);
      await loadGroups();
    } catch (e) {
      showError(e);
    }
  };

  return (
    <>
      <PageHead title={t('Groups')} sub={groups ? tn(groups.length, '{n} group', '{n} groups') : undefined}>
        <button type="button" className="btn primary" onClick={() => setEditing('new')}>
          <Plus size={14} /> {t('New group')}
        </button>
      </PageHead>
      <p className="muted sc-groups-intro">{t('A clip can be shared with a group: its members find it in their clips.')}</p>
      {groups === null ? (
        <div className="loading">{t('Loading…')}</div>
      ) : (
        <div className="user-grid">
          {groups.map((g) => (
            <div key={g.id} className="user-card sc-group-card" onClick={() => setEditing(g)}>
              <div className="user-card-head">
                <span className="user-avatar sc-group-avatar">
                  <UsersRound size={16} />
                </span>
                <div className="user-card-name">
                  <strong>{g.name}</strong>
                  <span className="muted user-card-sub">{tn(g.members.length, '{n} member', '{n} members')}</span>
                </div>
              </div>
              <div className="sc-group-members">
                {g.members.slice(0, 8).map((id) => {
                  const u = byId.get(id);
                  return u ? (
                    <span key={id} className="sc-member" title={displayName(u)}>
                      <span className="user-avatar small">{initial(u)}</span>
                      {displayName(u)}
                    </span>
                  ) : null;
                })}
                {g.members.length > 8 && <span className="muted">+{g.members.length - 8}</span>}
                {g.members.length === 0 && <span className="muted">{t('No member yet')}</span>}
              </div>
              <div className="user-card-actions" onClick={(e) => e.stopPropagation()}>
                <button type="button" className="btn small" onClick={() => setEditing(g)}>
                  <Pencil size={13} /> {t('Edit')}
                </button>
                <div className="spacer" />
                <button type="button" className="icon-btn ghost" title={t('Delete')} onClick={() => void remove(g)}>
                  <Trash2 size={14} />
                </button>
              </div>
            </div>
          ))}
          <button type="button" className="project-new user-new" onClick={() => setEditing('new')}>
            <Plus size={22} />
            {t('New group')}
          </button>
        </div>
      )}
      {editing && <GroupModal group={editing === 'new' ? null : editing} users={users ?? []} onClose={() => setEditing(null)} />}
    </>
  );
}

function GroupModal({ group, users, onClose }: { group: Group | null; users: PublicUser[]; onClose: () => void }) {
  const [name, setName] = useState(group?.name ?? '');
  const [members, setMembers] = useState<Set<string>>(() => new Set(group?.members ?? []));
  const [q, setQ] = useState('');
  const [busy, setBusy] = useState(false);
  const shown = users.filter((u) => !q.trim() || `${u.username} ${u.displayName}`.toLowerCase().includes(q.trim().toLowerCase()));
  const toggle = (id: string) =>
    setMembers((m) => {
      const n = new Set(m);
      if (n.has(id)) n.delete(id);
      else n.add(id);
      return n;
    });
  const submit = async (e: React.FormEvent) => {
    e.preventDefault();
    if (!name.trim() || busy) return;
    setBusy(true);
    const body = { name: name.trim(), members: [...members] };
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
  const keys = (e: React.KeyboardEvent) => e.key !== 'Escape' && e.stopPropagation();
  return (
    <div className="modal-back" onPointerDown={onClose}>
      <form className="modal wide" role="dialog" onPointerDown={(e) => e.stopPropagation()} onSubmit={submit} onKeyDown={keys}>
        <header>
          <h2>{group ? t('Edit {name}', { name: group.name }) : t('New group')}</h2>
          <button type="button" className="icon-btn ghost" title={t('Close')} onClick={onClose}>
            <X size={16} />
          </button>
        </header>
        <Field label={t('Name')}>
          <input value={name} autoFocus maxLength={60} placeholder={t('Journalists, Sport desk…')} onChange={(e) => setName(e.target.value)} />
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
                  <input type="checkbox" checked={members.has(u.id)} onChange={() => toggle(u.id)} />
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
        <footer>
          <button type="button" className="btn" onClick={onClose}>
            {t('Cancel')}
          </button>
          <button type="submit" className="btn primary" disabled={!name.trim() || busy}>
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
