/* Système › Accès › Utilisateurs. */
import { useEffect, useMemo, useState } from 'react';
import { Ban, KeyRound, Pencil, Plus, Search, SearchX, ShieldCheck, Trash2, X } from 'lucide-react';
import { t, tn } from '../i18n';
import { api, useApp, useChanged, type PublicUser } from '../store';
import { confirmDialog, promptDialog } from '../ui/dialogs';
import { showError } from '../ui/serverError';
import { displayName, initial } from '../ui/UserMenu';
import { Field, Switch } from '../settings/ui';
import { PageHead } from './SidePage';
import { loadUsers, useUserList } from './userList';
import { GroupIcon, groupName, loadGroups, loadPerms, RightsSummary, useGroups, type Group } from './Groups';

/** All, the admins, a group's members (its id), the disabled. */
type Filter = 'all' | 'admin' | 'disabled' | `g:${string}`;
const isAdmin = (u: PublicUser) => u.role === 'admin';

export function Users() {
  const users = useUserList((s) => s.users);
  const me = useApp((s) => s.auth?.user);
  const ssoOn = useApp((s) => !!s.auth?.sso);
  const [q, setQ] = useState('');
  const [filter, setFilter] = useState<Filter>('all');
  const [editing, setEditing] = useState<PublicUser | 'new' | null>(null);

  const groups = useGroups((s) => s.groups) ?? [];
  useEffect(() => {
    void loadUsers();
    void loadGroups();
    void loadPerms();
  }, []);
  useChanged('users', () => void loadUsers());
  useChanged('groups', () => void loadGroups());
  const inGroup = (u: PublicUser, g: Group) => g.members.includes(u.id);

  const list = users ?? [];
  const shown = useMemo(() => {
    const s = q.trim().toLowerCase();
    return list.filter(
      (u) =>
        (filter === 'all' || (filter === 'disabled' ? u.disabled : filter === 'admin' ? isAdmin(u) : groups.some((g) => `g:${g.id}` === filter && inGroup(u, g)))) &&
        (!s || u.username.toLowerCase().includes(s) || u.displayName.toLowerCase().includes(s)),
    );
  }, [list, q, filter, groups]); // eslint-disable-line react-hooks/exhaustive-deps
  const disabledCount = list.filter((u) => u.disabled).length;

  const remove = async (u: PublicUser) => {
    if (!(await confirmDialog({ title: t('Delete the user “{name}”?', { name: displayName(u) }), confirm: t('Delete'), danger: true }))) return;
    try {
      await api('DELETE', `/api/users/${u.id}`);
      await loadUsers();
    } catch (e) {
      showError(e);
    }
  };

  const setPassword = async (u: PublicUser) => {
    const password = await promptDialog({ title: t('Password of “{name}”', { name: displayName(u) }), label: t('New password'), placeholder: t('At least 8 characters'), password: true, confirm: t('Save') });
    if (!password) return;
    try {
      await api('PUT', `/api/users/${u.id}`, { username: u.username, displayName: u.displayName, role: u.role, disabled: u.disabled, roleFixed: u.roleFixed, password });
      useApp.getState().toast(t('Password changed'));
    } catch (e) {
      showError(e);
    }
  };

  return (
    <>
      <PageHead title={t('Users')} sub={users ? tn(list.length, '{n} user', '{n} users') : undefined}>
        <button type="button" className="btn primary" onClick={() => setEditing('new')}>
          <Plus size={14} /> {t('New user')}
        </button>
      </PageHead>
      <div className="media-toolbar">
        <label className="search">
          <Search size={14} />
          <input value={q} placeholder={t('Search users')} onChange={(e) => setQ(e.target.value)} onKeyDown={(e) => e.key !== 'Escape' && e.stopPropagation()} />
        </label>
        <div className="asr-chips">
          <button type="button" className={`asr-chip ${filter === 'all' ? 'on' : ''}`} onClick={() => setFilter('all')}>
            {t('All users')} <span className="asr-chip-n">{list.length}</span>
          </button>
          <button type="button" className={`asr-chip ${filter === 'admin' ? 'on' : ''}`} onClick={() => setFilter('admin')}>
            {t('Administrators')} <span className="asr-chip-n">{list.filter(isAdmin).length}</span>
          </button>
          {groups.map((g) => (
            <button key={g.id} type="button" className={`asr-chip ${filter === `g:${g.id}` ? 'on' : ''}`} onClick={() => setFilter(`g:${g.id}`)}>
              {groupName(g)} <span className="asr-chip-n">{list.filter((u) => inGroup(u, g)).length}</span>
            </button>
          ))}
          {disabledCount > 0 && (
            <button type="button" className={`asr-chip ${filter === 'disabled' ? 'on' : ''}`} onClick={() => setFilter('disabled')}>
              {t('Disabled')} <span className="asr-chip-n">{disabledCount}</span>
            </button>
          )}
        </div>
      </div>
      {users === null ? (
        <div className="loading">{t('Loading…')}</div>
      ) : (
        <div className="user-grid">
          {shown.map((u) => {
            const self = u.id === me?.id;
            const mine = groups.filter((g) => inGroup(u, g));
            return (
              <div key={u.id} className={`user-card ${isAdmin(u) ? 'role-admin' : ''} ${u.disabled ? 'dim' : ''}`} onClick={() => setEditing(u)}>
                <div className="user-card-head">
                  <span className="user-avatar">{initial(u)}</span>
                  <div className="user-card-name">
                    <strong>{displayName(u)}</strong>
                    <span className="muted user-card-sub">
                      <span className="user-card-login">
                        @{u.username}
                        {self && ` · ${t('you')}`}
                      </span>
                      {u.sso && <span className="badge sso">SSO</span>}
                    </span>
                  </div>
                </div>
                <div className="gr-tags">
                  {isAdmin(u) && (
                    <span className="gr-tag admin">
                      <ShieldCheck size={11} /> {t('Administrator')}
                    </span>
                  )}
                  {mine.map((g) => (
                    <span key={g.id} className={`gr-tag ${g.base ? g.id : 'other'}`}>
                      {groupName(g)}
                    </span>
                  ))}
                  {!isAdmin(u) && mine.length === 0 && <span className="gr-tag none">{t('No group')}</span>}
                </div>
                {u.disabled && (
                  <span className="user-disabled">
                    <Ban size={12} /> {t('Account disabled')}
                  </span>
                )}
                <div className="user-card-actions" onClick={(e) => e.stopPropagation()}>
                  <button type="button" className="btn small" onClick={() => setEditing(u)}>
                    <Pencil size={13} /> {t('Edit')}
                  </button>
                  {!u.sso && (
                    <button type="button" className="btn small" onClick={() => void setPassword(u)}>
                      <KeyRound size={13} /> {t('Password')}
                    </button>
                  )}
                  <div className="spacer" />
                  {!self && (
                    <button type="button" className="icon-btn ghost" title={t('Delete')} onClick={() => void remove(u)}>
                      <Trash2 size={14} />
                    </button>
                  )}
                </div>
              </div>
            );
          })}
          {shown.length === 0 && list.length > 0 ? (
            <div className="empty big">
              <SearchX size={22} />
              <p>{t('No match')}</p>
            </div>
          ) : (
            <button type="button" className="project-new user-new" onClick={() => setEditing('new')}>
              <Plus size={22} />
              {t('New user')}
            </button>
          )}
        </div>
      )}
      {editing && <UserModal user={editing === 'new' ? null : editing} groups={groups} ssoOn={ssoOn} onClose={() => setEditing(null)} />}
    </>
  );
}

function UserModal({ user, groups, ssoOn, onClose }: { user: PublicUser | null; groups: Group[]; ssoOn: boolean; onClose: () => void }) {
  const [username, setUsername] = useState(user?.username ?? '');
  const [name, setName] = useState(user?.displayName ?? '');
  const [password, setPassword] = useState('');
  const [admin, setAdmin] = useState(user?.role === 'admin');
  const [mine, setMine] = useState<Set<string>>(() => new Set(user ? groups.filter((g) => g.members.includes(user.id)).map((g) => g.id) : []));
  const flip = (id: string) =>
    setMine((m) => {
      const n = new Set(m);
      if (n.has(id)) n.delete(id);
      else n.add(id);
      return n;
    });
  // What its groups give, all together.
  const rights = [...new Set(groups.filter((g) => mine.has(g.id)).flatMap((g) => g.rights))];
  const [disabled, setDisabled] = useState(user?.disabled ?? false);
  const [roleFixed, setRoleFixed] = useState(user?.roleFixed ?? false);
  const [busy, setBusy] = useState(false);
  const isNew = !user;
  const ok = username.trim() && (!isNew || ssoOn || password.length > 0);

  const submit = async (e: React.FormEvent) => {
    e.preventDefault();
    if (!ok || busy) return;
    setBusy(true);
    const body = { username: username.trim(), displayName: name.trim(), admin, groups: [...mine], disabled, password, roleFixed };
    try {
      if (isNew) await api('POST', '/api/users', body);
      else await api('PUT', `/api/users/${user.id}`, body);
      await loadUsers();
      useApp.getState().toast(isNew ? t('User created') : t('User saved'));
      onClose();
    } catch (err) {
      showError(err);
      setBusy(false);
    }
  };

  const keys = (e: React.KeyboardEvent) => e.key !== 'Escape' && e.stopPropagation();
  return (
    <div className="modal-back" onPointerDown={onClose}>
      <form className="modal wide" role="dialog" aria-labelledby="user-title" onPointerDown={(e) => e.stopPropagation()} onSubmit={submit} onKeyDown={keys}>
        <header>
          <h2 id="user-title">{isNew ? t('New user') : t('Edit {name}', { name: displayName(user) })}</h2>
          <button type="button" className="icon-btn ghost" title={t('Close')} onClick={onClose}>
            <X size={16} />
          </button>
        </header>
        <div className="set-grid two">
          <Field label={t('User name')}>
            <input value={username} autoFocus autoComplete="off" spellCheck={false} onChange={(e) => setUsername(e.target.value)} />
          </Field>
          <Field label={t('Display name')}>
            <input value={name} placeholder={username || undefined} onChange={(e) => setName(e.target.value)} />
          </Field>
          {user?.sso ? (
            <Field label={t('Sign-in')}>
              <input readOnly value="SSO" />
            </Field>
          ) : (
            <Field label={isNew ? t('Password') : t('New password')}>
              <input
                type="password"
                value={password}
                autoComplete="new-password"
                placeholder={isNew ? t('At least 8 characters') : t('Leave empty to keep it')}
                onChange={(e) => setPassword(e.target.value)}
              />
            </Field>
          )}
          <Field label={t('State')} group>
            <Switch checked={disabled} onChange={setDisabled} label={t('Account disabled')} />
          </Field>
        </div>
        <label className={`gr-admin ${admin ? 'on' : ''}`}>
          <input type="checkbox" checked={admin} onChange={(e) => setAdmin(e.target.checked)} />
          <ShieldCheck size={15} />
          <b>{t('Administrator')}</b>
          <span className="muted">{t('Everything, Système included')}</span>
        </label>
        <div className="set-grid two">
          <Field label={t('Groups')} group>
            <div className={`gr-pick ${admin ? 'off' : ''}`}>
              {groups.map((g) => (
                <label key={g.id} className={`gr-opt ${mine.has(g.id) ? 'on' : ''}`}>
                  <input type="checkbox" checked={mine.has(g.id)} disabled={admin} onChange={() => flip(g.id)} />
                  <GroupIcon g={g} size={13} />
                  <b>{groupName(g)}</b>
                  {g.base && <span className="muted">{t('base')}</span>}
                </label>
              ))}
            </div>
          </Field>
          <Field label={t('What it may do')} group>
            <div className="gr-areas">
              <RightsSummary rights={rights} all={admin} />
            </div>
          </Field>
        </div>
        {ssoOn && <Switch checked={roleFixed} onChange={setRoleFixed} label={t('Groups set by hand')} />}
        <footer>
          <button type="button" className="btn" onClick={onClose}>
            {t('Cancel')}
          </button>
          <button type="submit" className="btn primary" disabled={!ok || busy}>
            {isNew ? <Plus size={14} /> : <KeyRound size={14} />} {isNew ? t('Create the user') : t('Save')}
          </button>
        </footer>
      </form>
    </div>
  );
}
