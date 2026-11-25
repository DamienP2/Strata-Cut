/*
 * The user menu: avatar + name; who, language, theme, keyboard shortcuts,
 * sign out. Language and theme apply at once, then are saved for the user.
 */
import { useEffect, useRef, useState } from 'react';
import { ChevronDown, Keyboard, Languages, LogOut, SunMoon } from 'lucide-react';
import { LANGS, setLang, t, tc, useLang, type Lang } from '../i18n';
import { setTheme, THEMES, useTheme, type Theme } from '../theme';
import { api, useApp, type PublicUser } from '../store';
import { useMenuKeys } from './menuKeys';
import { openShortcuts } from './Shortcuts';
import { showError } from './serverError';
import { groupName, loadGroups, useGroups } from '../pages/Groups';

const THEME_LABEL: Record<Theme, string> = { dark: 'Dark', light: 'Light', system: 'System' };

export const displayName = (u: Pick<PublicUser, 'displayName' | 'username'>) => u.displayName || u.username;
export const initial = (u: Pick<PublicUser, 'displayName' | 'username'>) => (displayName(u).trim()[0] ?? '?').toUpperCase();

export function UserMenu({ named = false }: { named?: boolean }) {
  const user = useApp((s) => s.auth?.user);
  // Its groups, by name (asked when the menu opens).
  const groups = useGroups((s) => s.groups);
  const lang = useLang((s) => s.lang);
  const theme = useTheme((s) => s.theme);
  const [open, setOpen] = useState(false);
  useEffect(() => {
    if (open && !groups) void loadGroups();
  }, [open, groups]);
  const btn = useRef<HTMLButtonElement>(null);
  const menu = useRef<HTMLDivElement>(null);
  const close = useRef((focus: boolean) => {
    setOpen(false);
    if (focus) btn.current?.focus();
  }).current;
  useMenuKeys(open, menu, close);
  useEffect(() => {
    if (open) menu.current?.querySelector<HTMLElement>('.lang-switch button.on')?.focus();
  }, [open]);
  if (!user) return null;

  const saveUser = (u: PublicUser | undefined) => {
    const a = useApp.getState().auth;
    if (a?.user && u?.id === a.user.id) useApp.setState({ auth: { ...a, user: u } });
  };
  const pickLang = async (l: Lang) => {
    setLang(l);
    try {
      saveUser(await api<PublicUser | undefined>('PUT', '/api/me/lang', { lang: l }));
    } catch (e) {
      showError(e);
    }
  };
  const pickTheme = async (th: Theme) => {
    setTheme(th);
    try {
      saveUser(await api<PublicUser | undefined>('PUT', '/api/me/theme', { theme: th }));
    } catch (e) {
      showError(e);
    }
  };

  return (
    <div className="ux1-user">
      <button
        ref={btn}
        type="button"
        className={`ux1-user-btn ${named ? 'named' : ''} ${open ? 'on' : ''}`}
        aria-haspopup="menu"
        aria-expanded={open}
        title={named ? undefined : displayName(user)}
        onClick={() => setOpen((o) => !o)}
        onKeyDown={(e) => {
          if (!open && e.key === 'ArrowDown') {
            e.preventDefault();
            setOpen(true);
          }
        }}
      >
        <span className="avatar">{initial(user)}</span>
        {named && <span className="user-name">{displayName(user)}</span>}
        <ChevronDown size={13} />
      </button>
      {open && (
        <div ref={menu} className="menu ux1-umenu" role="menu">
          <div className="ux1-who">
            <span className="avatar">{initial(user)}</span>
            <div>
              <b>{displayName(user)}</b>
              <small>{user.role === 'admin' ? tc('role', 'admin') : (groups ?? []).filter((g) => user.groups?.includes(g.id)).map(groupName).join(' · ') || t('No group')}</small>
            </div>
          </div>
          <div className="menu-sep" />
          <div className="ux1-mrow">
            <Languages size={14} /> {t('Language')}
            <div className="lang-switch" role="group" aria-label={t('Language')}>
              {LANGS.map((l) => (
                <button key={l.id} type="button" className={lang === l.id ? 'on' : ''} aria-pressed={lang === l.id} title={l.name} onClick={() => void pickLang(l.id)}>
                  {l.label}
                </button>
              ))}
            </div>
          </div>
          <div className="ux1-mrow">
            <SunMoon size={14} /> {t('Theme')}
            <div className="lang-switch theme-switch" role="group" aria-label={t('Theme')}>
              {THEMES.map((th) => (
                <button key={th} type="button" className={theme === th ? 'on' : ''} aria-pressed={theme === th} onClick={() => void pickTheme(th)}>
                  {tc('theme', THEME_LABEL[th])}
                </button>
              ))}
            </div>
          </div>
          <button
            type="button"
            role="menuitem"
            className="ux1-mi"
            onClick={() => {
              setOpen(false);
              openShortcuts();
            }}
          >
            <Keyboard size={14} /> {t('Keyboard shortcuts')}
            <span className="ux1-cap">?</span>
          </button>
          <div className="menu-sep" />
          <button
            type="button"
            role="menuitem"
            className="ux1-mi danger"
            onClick={() => {
              setOpen(false);
              void useApp.getState().logout();
            }}
          >
            <LogOut size={14} /> {t('Sign out')}
          </button>
        </div>
      )}
    </div>
  );
}
