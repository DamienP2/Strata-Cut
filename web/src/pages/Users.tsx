/* Système › Accès › Utilisateurs. */
import { useEffect, useMemo, useState, type ComponentType } from 'react';
import { Ban, Eye, KeyRound, Pencil, Plus, Scissors, Search, SearchX, ShieldCheck, Trash2, X } from 'lucide-react';
import { t, tc, tn } from '../i18n';
import { api, ROLES, useApp, useChanged, type PublicUser, type Role } from '../store';
import { confirmDialog, promptDialog } from '../ui/dialogs';
import { showError } from '../ui/serverError';
import { displayName, initial } from '../ui/UserMenu';
import { Field, Switch } from '../settings/ui';
import { PageHead } from './SidePage';
import { loadUsers, useUserList } from './userList';

export const ROLE_ICON: Record<Role, ComponentType<{ size?: number }>> = { admin: ShieldCheck, operator: Scissors, viewer: Eye };
/** What each role does (English, translated where shown). */
const ROLE_DOES: Record<Role, string> = {
  admin: 'Everything: recorders, users, system',
  operator: 'Cuts clips and manages them',
  viewer: 'Watches the recordings, downloads clips',
};
const ROLE_PLURAL: Record<Role, string> = { admin: 'Admins', operator: 'Operators', viewer: 'Viewers' };

type Filter = 'all' | Role | 'disabled';

export function Users() {
  const users = useUserList((s) => s.users);
  const me = useApp((s) => s.auth?.user);
  const ssoOn = useApp((s) => !!s.auth?.sso);
  const [q, setQ] = useState('');
  const [filter, setFilter] = useState<Filter>('all');
  const [editing, setEditing] = useState<PublicUser | 'new' | null>(null);

  useEffect(() => void loadUsers(), []);
  useChanged('users', () => void loadUsers());

  const list = users ?? [];
  const shown = useMemo(() => {
    const s = q.trim().toLowerCase();
    return list.filter(
      (u) =>
        (filter === 'all' || (filter === 'disabled' ? u.disabled : u.role === filter)) &&
        (!s || u.username.toLowerCase().includes(s) || u.displayName.toLowerCase().includes(s)),
    );
  }, [list, q, filter]);
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
          {ROLES.map((r) => (
            <button key={r} type="button" className={`asr-chip ${filter === r ? 'on' : ''}`} onClick={() => setFilter(r)}>
              {t(ROLE_PLURAL[r])} <span className="asr-chip-n">{list.filter((u) => u.role === r).length}</span>
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
            const Icon = ROLE_ICON[u.role] ?? Eye;
            const self = u.id === me?.id;
            return (
              <div key={u.id} className={`user-card role-${u.role} ${u.disabled ? 'dim' : ''}`} onClick={() => setEditing(u)}>
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
                  <span className={`badge role-${u.role}`}>
                    <Icon size={11} /> {tc('role', u.role)}
                  </span>
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
      {editing && <UserModal user={editing === 'new' ? null : editing} ssoOn={ssoOn} onClose={() => setEditing(null)} />}
    </>
  );
}

function UserModal({ user, ssoOn, onClose }: { user: PublicUser | null; ssoOn: boolean; onClose: () => void }) {
  const [username, setUsername] = useState(user?.username ?? '');
  const [name, setName] = useState(user?.displayName ?? '');
  const [password, setPassword] = useState('');
  const [role, setRole] = useState<Role>(user?.role ?? 'viewer');
  const [disabled, setDisabled] = useState(user?.disabled ?? false);
  const [roleFixed, setRoleFixed] = useState(user?.roleFixed ?? false);
  const [busy, setBusy] = useState(false);
  const isNew = !user;
  const ok = username.trim() && (!isNew || ssoOn || password.length > 0);

  const submit = async (e: React.FormEvent) => {
    e.preventDefault();
    if (!ok || busy) return;
    setBusy(true);
    const body = { username: username.trim(), displayName: name.trim(), role, disabled, password, roleFixed };
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
        <Field label={t('Role')} group>
          <div className="role-pick" role="radiogroup">
            {ROLES.map((r) => {
              const Icon = ROLE_ICON[r];
              return (
                <button key={r} type="button" role="radio" aria-checked={role === r} className={role === r ? 'on' : ''} title={t(ROLE_DOES[r])} onClick={() => setRole(r)}>
                  <span className="role-pick-name">
                    <Icon size={14} /> {tc('role', r)}
                  </span>
                </button>
              );
            })}
          </div>
        </Field>
        {ssoOn && <Switch checked={roleFixed} onChange={setRoleFixed} label={t('Role set by hand')} />}
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
