/*
 * The sign-in page; on the first start (no account yet) it creates the
 * administrator account instead.
 */
import { useEffect, useState } from 'react';
import { KeyRound, LogIn, ShieldCheck, UserPlus } from 'lucide-react';
import { LANGS, pickLang, t, useLang } from '../i18n';
import { ApiError, useApp } from '../store';
import { Brand } from '../ui/Brand';
import { friendlyError } from '../ui/serverError';

const BRAND = 'Strata LiveCut';

/** The SSO error the server sent back in the URL (?sso_error=…&sso_detail=…), then cleans the URL. */
function ssoError(): string | null {
  const q = new URLSearchParams(location.search);
  const code = q.get('sso_error');
  if (!code) return null;
  const detail = q.get('sso_detail') ?? '';
  q.delete('sso_error');
  q.delete('sso_detail');
  const rest = q.toString();
  history.replaceState(null, '', location.pathname + (rest ? `?${rest}` : ''));
  switch (code) {
    case 'noGroup':
      return t('None of your directory groups gives access to Strata.');
    case 'disabled':
      return t('This account is disabled.');
    case 'domain':
      return t('This account is not in a domain allowed to sign in.');
    case 'expired':
      return t('Signing in took too long: try again.');
    default:
      return t('The directory refused the sign-in: {detail}', { detail });
  }
}

export function Login() {
  const auth = useApp((s) => s.auth);
  const elsewhere = useApp((s) => s.elsewhere);
  const lang = useLang((s) => s.lang);
  const setup = !!auth?.needsSetup;
  const sso = auth?.sso ?? null;
  const [username, setUsername] = useState(setup ? 'admin' : '');
  const [displayName, setDisplayName] = useState('');
  const [password, setPassword] = useState('');
  const [repeat, setRepeat] = useState('');
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);

  useEffect(() => {
    const e = ssoError();
    if (e) setError(e);
    else if (elsewhere) setError(t('Signed in on another computer.'));
  }, [elsewhere]);

  const submit = async (e: React.FormEvent) => {
    e.preventDefault();
    if (busy) return;
    if (setup && password !== repeat) {
      setError(t('The passwords do not match.'));
      return;
    }
    setBusy(true);
    setError(null);
    try {
      if (setup) await useApp.getState().setup(username.trim(), password, displayName.trim());
      else await useApp.getState().login(username.trim(), password);
    } catch (err) {
      const status = err instanceof ApiError ? err.status : 0;
      const raw = (err as Error).message;
      if (!setup && status === 401) setError(t('Wrong user name or password.'));
      else if (status === 429) setError(t('Too many tries: wait a moment, then try again.'));
      else if (!setup && status === 403 && sso) setError(t('Sign in with {name}', { name: sso }));
      else setError(friendlyError(raw));
      setBusy(false);
    }
  };

  const next = encodeURIComponent(location.pathname + location.search);
  const name = auth?.serverName && auth.serverName !== BRAND ? auth.serverName : null;
  const empty = !username.trim() || !password || (setup && !repeat);

  return (
    <div className="login-page">
      <div className="lang-switch login-lang" role="group" aria-label={t('Language')}>
        {LANGS.map((l) => (
          <button key={l.id} type="button" className={lang === l.id ? 'on' : ''} aria-pressed={lang === l.id} title={l.name} onClick={() => pickLang(l.id)}>
            {l.label}
          </button>
        ))}
      </div>
      <form className="login-card" onSubmit={submit}>
        <div className="login-head">
          <Brand stacked />
          {name && <p className="muted">{name}</p>}
        </div>
        {setup ? (
          <div className="login-intro">
            <ShieldCheck size={16} />
            <span>{t('Welcome! Create the administrator account.')}</span>
          </div>
        ) : (
          sso && (
            <>
              <a className="btn primary block sso-btn" href={`/api/auth/sso/start?next=${next}`}>
                <KeyRound size={15} /> {t('Sign in with {name}', { name: sso })}
              </a>
              <div className="login-sep">{t('local account')}</div>
            </>
          )
        )}
        <label className="set-field">
          <span className="set-label">{t('User name')}</span>
          <input value={username} autoFocus={!setup} autoComplete="username" autoCapitalize="none" spellCheck={false} onChange={(e) => setUsername(e.target.value)} />
        </label>
        {setup && (
          <label className="set-field">
            <span className="set-label">{t('Display name')}</span>
            <input value={displayName} placeholder={t('Optional')} autoComplete="name" onChange={(e) => setDisplayName(e.target.value)} />
          </label>
        )}
        <label className="set-field">
          <span className="set-label">{t('Password')}</span>
          <input type="password" value={password} autoFocus={setup} autoComplete={setup ? 'new-password' : 'current-password'} onChange={(e) => setPassword(e.target.value)} />
        </label>
        {setup && (
          <label className="set-field">
            <span className="set-label">{t('Repeat the password')}</span>
            <input type="password" value={repeat} autoComplete="new-password" onChange={(e) => setRepeat(e.target.value)} />
            <span className="set-hint">{t('At least 8 characters.')}</span>
          </label>
        )}
        <button type="submit" className={`btn block ${setup || !sso ? 'primary' : ''}`} disabled={empty || busy}>
          {setup ? <UserPlus size={15} /> : <LogIn size={15} />} {setup ? t('Create the administrator account') : t('Sign in')}
        </button>
        {error && <div className="login-error" role="alert">{error}</div>}
        {setup && (
          <p className="set-hint">
            {t('For safety, this page only works on the server itself (http://localhost). Remotely, run this on the server:')} <code>strata-cut user add admin</code>
          </p>
        )}
      </form>
    </div>
  );
}
