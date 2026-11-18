/* Système › Serveur › Général. */
import { useMemo } from 'react';
import { Server } from 'lucide-react';
import { t } from '../i18n';
import { PageHead } from '../pages/SidePage';
import { Dropdown, type DropOption } from '../ui/Dropdown';
import { Card, Field, SaveBar } from './ui';
import { useServerConfig } from './serverConfig';

/** Every IANA time zone the browser knows, grouped by region. */
function zones(current: string): DropOption[] {
  let list: string[] = [];
  try {
    list = Intl.supportedValuesOf('timeZone');
  } catch {
    /* old browser */
  }
  if (!list.includes('UTC')) list = ['UTC', ...list];
  if (current && !list.includes(current)) list = [current, ...list];
  return list.map((z) => ({ value: z, label: z.replace(/_/g, ' '), group: z.includes('/') ? z.split('/')[0] : undefined }));
}

export function General() {
  const { cfg, setCfg, dirty, saving, save, discard } = useServerConfig();
  const options = useMemo(() => zones(cfg?.timezone ?? ''), [cfg?.timezone]);
  return (
    <>
      <PageHead title={t('General')} />
      {!cfg ? (
        <div className="loading">{t('Loading…')}</div>
      ) : (
        <Card icon={<Server size={16} />} title={t('General')}>
          <div className="set-grid two">
            <Field label={t('Server name')}>
              <input value={cfg.name} aria-invalid={!cfg.name.trim()} onChange={(e) => setCfg((c) => ({ ...c, name: e.target.value }))} />
            </Field>
            <Field label={t('Time zone')}>
              <Dropdown value={cfg.timezone} options={options} onChange={(timezone) => setCfg((c) => ({ ...c, timezone }))} />
            </Field>
          </div>
        </Card>
      )}
      <SaveBar dirty={dirty} saving={saving} onSave={() => void save()} onDiscard={discard} />
    </>
  );
}
