/*
 * Système › Serveur › YouTube: what yt-dlp uses so that YouTube lets the server read its lives
 * (and does not take it for a bot): a signed-in account's cookies for every YouTube recorder,
 * the client it passes for, a PO Token, a proxy. A recorder with its own cookies keeps them.
 */
import { Cookie, Globe, KeyRound, MonitorPlay, MonitorSmartphone, TriangleAlert } from 'lucide-react';
import { t, tn } from '../i18n';
import { useApp } from '../store';
import { useShallow } from 'zustand/react/shallow';
import { PageHead } from '../pages/SidePage';
import { Dropdown } from '../ui/Dropdown';
import { Card, Field, SaveBar } from './ui';
import { useServerConfig, type ServerConfig } from './serverConfig';

const MASK = '••••••••';

interface YoutubeConfig {
  cookies: string;
  client: string;
  poToken: string;
  visitorData: string;
  proxy: string;
}
const EMPTY: YoutubeConfig = { cookies: '', client: '', poToken: '', visitorData: '', proxy: '' };

/** The clients yt-dlp can pass for (as the server allows them), with what they are good for (English). */
const CLIENTS: { value: string; label: string; detail: string }[] = [
  { value: '', label: 'Automatic', detail: 'yt-dlp’s choice (advised to begin with)' },
  { value: 'tv', label: 'TV', detail: 'A television’s app: often let through without a PO Token' },
  { value: 'web_safari', label: 'Web (Safari)', detail: 'Safari on a computer: lives in HLS' },
  { value: 'mweb', label: 'Mobile web', detail: 'The mobile site: works with a PO Token' },
  { value: 'web', label: 'Web', detail: 'The computer site: needs a PO Token today' },
  { value: 'ios', label: 'iOS', detail: 'The iPhone app' },
  { value: 'android_vr', label: 'Android VR', detail: 'The headsets’ app: no account' },
];

export function Youtube() {
  const { cfg, setCfg, dirty, saving, save, discard } = useServerConfig();
  const { recorders, status } = useApp(useShallow((s) => ({ recorders: s.recorders, status: s.status })));
  const y: YoutubeConfig = { ...EMPTY, ...((cfg?.youtube as Partial<YoutubeConfig> | undefined) ?? {}) };
  const set = (patch: Partial<YoutubeConfig>) => setCfg((c: ServerConfig) => ({ ...c, youtube: { ...EMPTY, ...((c.youtube as Partial<YoutubeConfig>) ?? {}), ...patch } }));
  const importCookies = (file: File | undefined) => {
    if (file) void file.text().then((text) => set({ cookies: text }));
  };

  // The YouTube recorders, and those YouTube refuses now.
  const web = recorders.filter((r) => r.protocol === 'web');
  const refused = web.filter((r) => (status[r.id]?.message ?? '').includes('not a bot') || (status[r.id]?.message ?? '').includes('refuses these cookies'));
  const own = web.filter((r) => !!r.cookies);

  return (
    <>
      <PageHead title={t('YouTube')} sub={tn(web.length, '{n} YouTube recorder', '{n} YouTube recorders')} />
      {refused.length > 0 && (
        <div className="sc-space bad" style={{ marginBottom: 16 }}>
          <strong>
            <TriangleAlert size={14} /> {tn(refused.length, 'YouTube refuses {n} recorder now', 'YouTube refuses {n} recorders now')}: {refused.map((r) => r.name).join(', ')}
          </strong>
          <span>{t('It takes the server for a bot: give it a signed-in account’s cookies (below), then, if that is not enough, a PO Token or another client.')}</span>
        </div>
      )}
      {cfg && (
        <>
          <Card icon={<Cookie size={16} />} title={t('A YouTube account')}>
            <p className="muted sc-yt-note">{t('The most effective: YouTube lets a signed-in account through. In a private window of a browser, sign in to YouTube (an account made for this), export its cookies.txt (extension “Get cookies.txt LOCALLY”), close the window and import the file here. Every YouTube recorder uses them, unless it has its own.')}</p>
            <Field label={t('Cookies of a signed-in account')} group hint={own.length ? tn(own.length, '{n} recorder has its own cookies: it keeps them.', '{n} recorders have their own cookies: they keep them.') : undefined}>
              <div className="sc-cookies">
                {y.cookies === MASK ? <span className="sc-cookies-set">{t('Cookies saved')}</span> : <textarea rows={4} value={y.cookies} spellCheck={false} placeholder="# Netscape HTTP Cookie File" onChange={(e) => set({ cookies: e.target.value })} />}
                <div className="sc-cookies-btns">
                  <label className="btn small">
                    {t('Import cookies.txt')}
                    <input type="file" accept=".txt,text/plain" hidden onChange={(e) => importCookies(e.target.files?.[0])} />
                  </label>
                  {!!y.cookies && (
                    <button type="button" className="btn small ghost" onClick={() => set({ cookies: '' })}>
                      {t('Remove')}
                    </button>
                  )}
                </div>
              </div>
            </Field>
          </Card>
          <Card icon={<MonitorSmartphone size={16} />} title={t('YouTube client')}>
            <p className="muted sc-yt-note">{t('The application yt-dlp passes for. YouTube treats them differently: when it refuses one, another may be let through.')}</p>
            <Field label={t('Client')}>
              <Dropdown value={y.client} options={CLIENTS.map((c) => ({ value: c.value, label: t(c.label), detail: t(c.detail) }))} onChange={(v) => set({ client: v })} />
            </Field>
          </Card>
          <Card icon={<KeyRound size={16} />} title={t('PO Token')}>
            <p className="muted sc-yt-note">{t('A “Proof of Origin” token proves that requests come from a real page. It is made in a browser where YouTube plays (see yt-dlp’s guide “PO Token Guide”) and lasts a few hours to a few days. With cookies, the visitor data is not needed.')}</p>
            <div className="set-grid two">
              <Field label={t('PO Token')} hint={t('Given as is, or as “web.gvs+TOKEN”.')}>
                <input type="password" value={y.poToken} autoComplete="off" placeholder={t('Optional')} spellCheck={false} onChange={(e) => set({ poToken: e.target.value })} />
              </Field>
              <Field label={t('Visitor data')} hint={t('Only without cookies.')}>
                <input value={y.visitorData} placeholder={t('Optional')} spellCheck={false} onChange={(e) => set({ visitorData: e.target.value })} />
              </Field>
            </div>
          </Card>
          <Card icon={<Globe size={16} />} title={t('Proxy')}>
            <p className="muted sc-yt-note">{t('YouTube refuses some addresses (data centres especially): through a proxy, YouTube sees another address. For YouTube only; the other sources are not affected.')}</p>
            <Field label={t('Proxy address')}>
              <input value={y.proxy} placeholder="socks5://user:password@host:1080" spellCheck={false} onChange={(e) => set({ proxy: e.target.value })} />
            </Field>
          </Card>
          <p className="muted sc-yt-note">
            <MonitorPlay size={13} /> {t('The settings apply from the next reading of each live (a recorder refused is tried again within 10 minutes).')}
          </p>
        </>
      )}
      <SaveBar dirty={dirty} saving={saving} onSave={() => void save()} onDiscard={discard} />
    </>
  );
}
