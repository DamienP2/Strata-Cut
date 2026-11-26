/*
 * Système › Accès › Autorisations: what each group may do, function by function (a column a
 * group, saved at once). The admins may do everything, the Système pages alone.
 */
import { useEffect } from 'react';
import { Check, ShieldCheck } from 'lucide-react';
import { t } from '../i18n';
import { api, useApp, useChanged } from '../store';
import { PageHead } from '../pages/SidePage';
import { GroupIcon, groupName, loadGroups, loadPerms, partsOf, subsOf, useGroups, usePerms, type Group } from '../pages/Groups';
import { showError } from '../ui/serverError';

/** What only the admins do (shown, never given). */
const ADMIN_ONLY = ['Sources, destinations and file servers', 'Users, groups and rights', 'Server, storage, hardware, maintenance'];

export function Roles() {
  const groups = useGroups((s) => s.groups);
  const perms = usePerms((s) => s.perms);
  useEffect(() => {
    void loadGroups();
    void loadPerms();
  }, []);
  useChanged('groups', () => void loadGroups());

  const toggle = async (g: Group, perm: string, on: boolean) => {
    const rights = on ? [...g.rights, perm] : g.rights.filter((p) => p !== perm);
    useGroups.setState({ groups: (groups ?? []).map((x) => (x.id === g.id ? { ...x, rights } : x)) });
    try {
      await api('PUT', `/api/groups/${g.id}`, { name: g.name, members: g.members, rights });
      useApp.getState().toast(t('Rights saved'));
    } catch (e) {
      showError(e);
    }
    void loadGroups();
  };

  const cols = groups ?? [];
  return (
    <>
      <PageHead title={t('Rights')} />
      {groups && perms && (
        <div className="rl-wrap">
          <table className="user-table rl-table">
            <thead>
              <tr>
                <th />
                <th className="rl-role">
                  <span className="gr-ico admin">
                    <ShieldCheck size={13} />
                  </span>{' '}
                  {t('Administrators')}
                </th>
                {cols.map((g) => (
                  <th key={g.id} className="rl-role">
                    <GroupIcon g={g} size={13} /> {groupName(g)}
                  </th>
                ))}
              </tr>
            </thead>
            {partsOf(perms).map(([part, list]) => (
              <tbody key={part}>
                <tr className="rl-part">
                  <td colSpan={cols.length + 2}>{t(part)}</td>
                </tr>
                {subsOf(list).flatMap(([sub, items]) => [
                  ...(sub
                    ? [
                        <tr key={`sub-${sub}`} className="rl-sub">
                          <td colSpan={cols.length + 2}>{t(sub)}</td>
                        </tr>,
                      ]
                    : []),
                  ...items.map((p) => (
                  <tr key={p.key} className={sub ? 'rl-in' : ''}>
                    <td>{t(p.label)}</td>
                    <td className="rl-cell">
                      <Check size={14} className="rl-always" />
                    </td>
                    {cols.map((g) => (
                      <td key={g.id} className="rl-cell">
                        <input type="checkbox" checked={g.rights.includes(p.key)} aria-label={`${groupName(g)} · ${t(p.label)}`} onChange={(e) => void toggle(g, p.key, e.target.checked)} />
                      </td>
                    ))}
                  </tr>
                  )),
                ])}
              </tbody>
            ))}
            <tbody>
              <tr className="rl-part">
                <td colSpan={cols.length + 2}>{t('System')}</td>
              </tr>
              {ADMIN_ONLY.map((l) => (
                <tr key={l} className="rl-admin">
                  <td>{t(l)}</td>
                  <td className="rl-cell">
                    <Check size={14} className="rl-always" />
                  </td>
                  {cols.map((g) => (
                    <td key={g.id} className="rl-cell rl-none">
                      —
                    </td>
                  ))}
                </tr>
              ))}
            </tbody>
          </table>
        </div>
      )}
    </>
  );
}
