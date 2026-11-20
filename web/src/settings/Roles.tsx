/*
 * Système › Accès › Rôles: what each role may do, function by function. The admins may do
 * everything; the operators, streamers and readers get what is ticked (saved at once).
 */
import { useEffect, useState } from 'react';
import { Check } from 'lucide-react';
import { t, tc } from '../i18n';
import { api, useApp, useChanged, type Role } from '../store';
import { PageHead } from '../pages/SidePage';
import { ROLE_ICON } from '../pages/Users';
import { showError } from '../ui/serverError';

interface Perm {
  key: string;
  part: string;
  label: string;
}
interface Table {
  perms: Perm[];
  roles: Record<string, string[]>;
}

const EDITABLE: Role[] = ['operator', 'streamer', 'viewer'];

export function Roles() {
  const [table, setTable] = useState<Table | null>(null);
  const load = () => void api<Table>('GET', '/api/roles').then(setTable, showError);
  useChanged('roles', load);
  useEffect(load, []); // eslint-disable-line react-hooks/exhaustive-deps

  const toggle = async (role: Role, perm: string, on: boolean) => {
    if (!table) return;
    const list = new Set(table.roles[role] ?? []);
    if (on) list.add(perm);
    else list.delete(perm);
    const roles = { ...table.roles, [role]: [...list] };
    setTable({ ...table, roles });
    try {
      setTable(await api<Table>('PUT', '/api/roles', roles));
      useApp.getState().toast(t('Rights saved'));
    } catch (e) {
      showError(e);
      load();
    }
  };

  // The rights by part of the app, in order.
  const parts: [string, Perm[]][] = [];
  for (const p of table?.perms ?? []) {
    const last = parts[parts.length - 1];
    if (last && last[0] === p.part) last[1].push(p);
    else parts.push([p.part, [p]]);
  }
  const roles: Role[] = ['admin', ...EDITABLE];

  return (
    <>
      <PageHead title={t('Roles')} />
      {table && (
        <table className="user-table rl-table">
          <thead>
            <tr>
              <th />
              {roles.map((r) => {
                const Icon = ROLE_ICON[r];
                return (
                  <th key={r} className="rl-role">
                    <Icon size={14} /> {tc('role', r)}
                  </th>
                );
              })}
            </tr>
          </thead>
          {parts.map(([part, perms]) => (
            <tbody key={part}>
              <tr className="rl-part">
                <td colSpan={roles.length + 1}>{t(part)}</td>
              </tr>
              {perms.map((p) => (
                <tr key={p.key}>
                  <td>{t(p.label)}</td>
                  <td className="rl-cell">
                    <Check size={14} className="rl-always" />
                  </td>
                  {EDITABLE.map((r) => (
                    <td key={r} className="rl-cell">
                      <input type="checkbox" checked={(table.roles[r] ?? []).includes(p.key)} onChange={(e) => void toggle(r, p.key, e.target.checked)} />
                    </td>
                  ))}
                </tr>
              ))}
            </tbody>
          ))}
        </table>
      )}
    </>
  );
}
