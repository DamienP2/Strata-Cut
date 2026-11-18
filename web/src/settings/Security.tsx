/* Système › Accès › Sécurité: sign-in through the company directory (OpenID Connect). */
import { useState } from 'react';
import { CircleCheck, Copy, KeyRound, PlugZap, Plus, X } from 'lucide-react';
import { t, tc } from '../i18n';
import { api, ROLES, useApp, type Role } from '../store';
import { PageHead } from '../pages/SidePage';
import { Dropdown } from '../ui/Dropdown';
import { Segmented } from '../ui/fields';
import { errorText } from '../ui/serverError';
import { Card, Field, SaveBar, Switch } from './ui';
import { useServerConfig, type SsoConfig, type SsoProvider } from './serverConfig';

const DEFAULT_SSO: SsoConfig = { enabled: false, provider: 'entra', issuer: '', clientId: '', clientSecret: '', groupsClaim: 'groups', groups: [], defaultRole: null, localAccounts: 'adminOnly' };

const PROVIDERS: { value: SsoProvider; label: string }[] = [
  { value: 'entra', label: 'Microsoft Entra' },
  { value: 'okta', label: 'Okta' },
  { value: 'keycloak', label: 'Keycloak' },
  { value: 'google', label: 'Google' },
  { value: 'other', label: 'Others' },
];

const ISSUER: Record<SsoProvider, string> = {
  entra: 'https://login.microsoftonline.com/<tenant>/v2.0',
  okta: 'https://<domain>.okta.com/oauth2/default',
  keycloak: 'https://<host>/realms/<realm>',
  google: 'https://accounts.google.com',
  other: 'https://…',
};

interface TestResult {
  issuer: string;
  authorizationEndpoint: string;
  tokenEndpoint: string;
  keys: number | unknown[];
}

export function Security() {
  const { cfg, setCfg, dirty, saving, save, discard } = useServerConfig();
  const [test, setTest] = useState<{ busy: boolean; ok?: TestResult; error?: string; raw?: string }>({ busy: false });
  const sso: SsoConfig = { ...DEFAULT_SSO, ...(cfg?.security?.sso ?? {}) };
  const set = (patch: Partial<SsoConfig>) => setCfg((c) => ({ ...c, security: { ...c.security, sso: { ...DEFAULT_SSO, ...(c.security?.sso ?? {}), ...patch } } }));
  const callback = `${location.origin}/api/auth/sso/callback`;

  const runTest = async () => {
    setTest({ busy: true });
    try {
      setTest({ busy: false, ok: await api<TestResult>('POST', '/api/security/sso/test', { issuer: sso.issuer.trim() }) });
    } catch (e) {
      setTest({ busy: false, error: errorText(e), raw: (e as Error).message });
    }
  };

  const copy = async () => {
    try {
      await navigator.clipboard.writeText(callback);
      useApp.getState().toast(t('Copied'));
    } catch {
      useApp.getState().toast(t('Could not copy'), 'error');
    }
  };

  const keys = (k: number | unknown[] | undefined) => (Array.isArray(k) ? k.length : (k ?? 0));
  const roleOptions = ROLES.map((r) => ({ value: r, label: tc('role', r) }));

  return (
    <>
      <PageHead title={t('Security')} />
      {!cfg ? (
        <div className="loading">{t('Loading…')}</div>
      ) : (
        <Card
          icon={<KeyRound size={16} />}
          title={t('Sign-in through the company directory (SSO)')}
          actions={<Switch checked={sso.enabled} onChange={(enabled) => set({ enabled })} label={sso.enabled ? t('Enabled') : t('Disabled')} />}
        >
          {sso.enabled && (
            <>
              <div className="set-grid two">
                <Field label={t('Provider')} group>
                  <Segmented value={sso.provider} options={PROVIDERS.map((p) => ({ value: p.value, label: p.value === 'other' ? t(p.label) : p.label }))} onChange={(provider) => set({ provider })} />
                </Field>
                <Field label={t('Return URL')} group>
                  <div className="row sso-callback">
                    <code>{callback}</code>
                    <button type="button" className="btn" onClick={() => void copy()}>
                      <Copy size={14} /> {t('Copy')}
                    </button>
                  </div>
                </Field>
                <Field label={t('Tenant / Issuer')}>
                  <input value={sso.issuer} placeholder={ISSUER[sso.provider]} spellCheck={false} onChange={(e) => set({ issuer: e.target.value })} />
                </Field>
                <Field label={t('Client ID')}>
                  <input value={sso.clientId} spellCheck={false} onChange={(e) => set({ clientId: e.target.value })} />
                </Field>
                <Field label={t('Secret')}>
                  <input type="password" value={sso.clientSecret} autoComplete="new-password" onChange={(e) => set({ clientSecret: e.target.value })} />
                </Field>
                <Field label={t('Groups claim')}>
                  <input value={sso.groupsClaim} spellCheck={false} onChange={(e) => set({ groupsClaim: e.target.value })} />
                </Field>
              </div>
              <Field label={t('Roles by group')} group>
                <div className="sso-groups">
                  {sso.groups.map((g, i) => (
                    <div key={i} className="row sso-group-row">
                      <input
                        value={g.group}
                        placeholder={t('Group')}
                        spellCheck={false}
                        onChange={(e) => set({ groups: sso.groups.map((x, k) => (k === i ? { ...x, group: e.target.value } : x)) })}
                      />
                      <div className="sso-group-role">
                        <Dropdown value={g.role} options={roleOptions} onChange={(role: Role) => set({ groups: sso.groups.map((x, k) => (k === i ? { ...x, role } : x)) })} />
                      </div>
                      <button type="button" className="icon-btn ghost" title={t('Remove')} onClick={() => set({ groups: sso.groups.filter((_, k) => k !== i) })}>
                        <X size={14} />
                      </button>
                    </div>
                  ))}
                  <div>
                    <button type="button" className="btn small" onClick={() => set({ groups: [...sso.groups, { group: '', role: 'viewer' }] })}>
                      <Plus size={13} /> {t('group')}
                    </button>
                  </div>
                </div>
              </Field>
              <div className="set-grid two">
                <Field label={t('No known group')} group>
                  <Segmented
                    value={sso.defaultRole ?? 'none'}
                    options={[{ value: 'none', label: t('Refused') }, ...ROLES.map((r) => ({ value: r, label: tc('role', r) }))]}
                    onChange={(v) => set({ defaultRole: v === 'none' ? null : (v as Role) })}
                  />
                </Field>
                <Field label={t('Local accounts')} group>
                  <Segmented
                    value={sso.localAccounts}
                    options={[
                      { value: 'none', label: t('Disabled') },
                      { value: 'adminOnly', label: t('Admins only (fallback)') },
                      { value: 'all', label: t('All') },
                    ]}
                    onChange={(localAccounts) => set({ localAccounts })}
                  />
                </Field>
              </div>
              <div className="row sso-test">
                <button type="button" className="btn" disabled={!sso.issuer.trim() || test.busy} onClick={() => void runTest()}>
                  <PlugZap size={14} className={test.busy ? 'spin' : undefined} /> {t('Test the connection')}
                </button>
                {test.ok && (
                  <span className="sso-test-ok" title={`${test.ok.authorizationEndpoint}\n${test.ok.tokenEndpoint}`}>
                    <CircleCheck size={14} /> {t('Directory reached · {n} keys', { n: keys(test.ok.keys) })}
                  </span>
                )}
                {test.error && (
                  <span className="update-error" title={test.raw}>
                    {test.error}
                  </span>
                )}
              </div>
            </>
          )}
        </Card>
      )}
      <SaveBar dirty={dirty} saving={saving} onSave={() => void save()} onDiscard={discard} />
    </>
  );
}
