/* Système › Serveur › Maintenance: the updates, the restart, the system. */
import { useState } from 'react';
import { RotateCcw } from 'lucide-react';
import { t } from '../i18n';
import { api, useApp } from '../store';
import { PageHead } from '../pages/SidePage';
import { formatDuration } from '../ui/format';
import { showError } from '../ui/serverError';
import { useServerConfig } from './serverConfig';
import { Updates } from './Updates';
import { confirmDialog } from '../ui/dialogs';
import { restartCuts } from '../ui/restartNote';

export function Maintenance() {
  const { info, reload } = useServerConfig();
  const [busy, setBusy] = useState(false);

  const restart = async () => {
    // Something live or being made: said before it is cut.
    const cuts = await restartCuts();
    if (cuts && !(await confirmDialog({ title: t('Restart the server?'), message: cuts, confirm: t('Restart'), danger: true }))) return;
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
      <PageHead title={t('Maintenance')}>
        <button type="button" className="btn" disabled={busy} onClick={() => void restart()}>
          <RotateCcw size={14} className={busy ? 'spin' : undefined} /> {t('Restart')}
        </button>
      </PageHead>
      {/* The server at a glance, then its updates. */}
      {info && (
        <div className="hl-tiles mt-tiles">
          <div className="hl-tile">
            <span className="hl-tile-l">{t('Version')}</span>
            <b>{info.version}</b>
            <span className="hl-tile-s">
              {info.os}/{info.arch}
            </span>
          </div>
          <div className="hl-tile">
            <span className="hl-tile-l">{t('Running for')}</span>
            <b>{formatDuration(info.uptimeSecs)}</b>
          </div>
          <div className="hl-tile">
            <span className="hl-tile-l">{t('Machine')}</span>
            <b title={info.hostname}>{info.hostname}</b>
            <span className="hl-tile-s">{t('{n} CPU threads', { n: info.cpus })}</span>
          </div>
          <div className="hl-tile wide">
            <span className="hl-tile-l">{t('Listening on')}</span>
            <b className="mono" title={info.bind}>
              {info.bind}
            </b>
          </div>
          <div className="hl-tile wide">
            <span className="hl-tile-l">{t('Data folder')}</span>
            <b className="mono" title={info.dataDir}>
              {info.dataDir}
            </b>
          </div>
        </div>
      )}
      <Updates />
    </>
  );
}
