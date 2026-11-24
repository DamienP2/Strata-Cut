/* After a crash (admins): « Strata LiveCut s’est arrêté de façon inattendue le … puis a redémarré. » */
import { useState } from 'react';
import { TriangleAlert } from 'lucide-react';
import { t } from '../i18n';
import { navigate } from '../router';
import { formatDateTime } from './format';
import { healthSeen, useHealth } from './health';

export function CrashBanner({ hidden = false }: { hidden?: boolean }) {
  const health = useHealth((s) => s.health);
  const [gone, setGone] = useState(false);
  if (hidden || gone || !health?.unseenCrash || !health.lastCrash) return null;
  return (
    <div className="crash-banner" role="alert">
      <TriangleAlert size={16} />
      <span>{t('Strata LiveCut stopped unexpectedly on {date} and restarted.', { date: formatDateTime(health.lastCrash) })}</span>
      <button
        type="button"
        className="btn small"
        onClick={() => {
          setGone(true);
          void healthSeen();
          void navigate('/system/health');
        }}
      >
        {t('View')}
      </button>
      <button
        type="button"
        className="btn small ghost"
        onClick={() => {
          setGone(true);
          void healthSeen();
        }}
      >
        {t('OK')}
      </button>
    </div>
  );
}
