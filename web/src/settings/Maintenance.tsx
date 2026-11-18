/* Système › Serveur › Maintenance. */
import { useState } from 'react';
import { Cpu, RotateCcw } from 'lucide-react';
import { t } from '../i18n';
import { api, useApp } from '../store';
import { PageHead } from '../pages/SidePage';
import { formatDuration } from '../ui/format';
import { showError } from '../ui/serverError';
import { Card } from './ui';
import { useServerConfig } from './serverConfig';

export function Maintenance() {
  const { info, reload } = useServerConfig();
  const [busy, setBusy] = useState(false);

  const restart = async () => {
    setBusy(true);
    try {
      await api('POST', '/api/system/restart');
      useApp.getState().toast(t('The server is restarting'));
      setTimeout(() => void reload(), 2000);
    } catch (e) {
      showError(e);
    }
    setBusy(false);
  };

  return (
    <>
      <PageHead
        title={t('Maintenance')}
        sub={info && t('Strata Cut {version} · {machine} · {os}/{arch} · running for {time}', { version: info.version, machine: info.hostname, os: info.os, arch: info.arch, time: formatDuration(info.uptimeSecs) })}
      >
        <button type="button" className="btn" disabled={busy} onClick={() => void restart()}>
          <RotateCcw size={14} className={busy ? 'spin' : undefined} /> {t('Restart')}
        </button>
      </PageHead>
      {!info ? (
        <div className="loading">{t('Loading…')}</div>
      ) : (
        <Card icon={<Cpu size={16} />} title={t('System')}>
          <dl className="sys-list">
            <dt>{t('Version')}</dt>
            <dd>{info.version}</dd>
            <dt>{t('Machine')}</dt>
            <dd>
              {info.hostname} · {info.os}/{info.arch}
            </dd>
            <dt>{t('Data folder')}</dt>
            <dd>
              <code>{info.dataDir}</code>
            </dd>
            <dt>{t('Listening on')}</dt>
            <dd>
              <code>{info.bind}</code>
            </dd>
            <dt>{t('CPU threads')}</dt>
            <dd>{info.cpus}</dd>
            <dt>{t('Running for')}</dt>
            <dd>{formatDuration(info.uptimeSecs)}</dd>
          </dl>
        </Card>
      )}
    </>
  );
}
