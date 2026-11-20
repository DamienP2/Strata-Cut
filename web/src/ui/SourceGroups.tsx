/*
 * The sources on the left of Extraction and Streamer, in two groups (no state written on each
 * line: the group says it). Extraction: in progress / stopped. Streamer: available (received
 * now) / not available, to start in the Régie.
 */
import type { ReactNode } from 'react';
import { useShallow } from 'zustand/react/shallow';
import { t, tc } from '../i18n';
import { navigate } from '../router';
import { useApp } from '../store';
import type { Recorder } from '../types';
import { RecLogo } from './RecLogo';

export function SourceGroups({ kind, current, base, badge }: { kind: 'extract' | 'stream'; current?: string; base: string; badge?: (r: Recorder) => ReactNode }) {
  const { recorders, status } = useApp(useShallow((s) => ({ recorders: s.recorders, status: s.status })));
  const running = recorders.filter((r) => status[r.id]?.state === 'recording');
  const others = recorders.filter((r) => status[r.id]?.state !== 'recording');
  const groups: { label: string; items: Recorder[]; dim: boolean; hint?: ReactNode; live?: boolean }[] =
    kind === 'extract'
      ? [
          { label: tc('srcgroup', 'In progress'), items: running, dim: false, live: true },
          { label: tc('srcgroup', 'Stopped'), items: others, dim: true },
        ]
      : [
          { label: tc('srcgroup', 'Available'), items: running, dim: false, live: true },
          {
            label: tc('srcgroup', 'Not available'),
            items: others,
            dim: true,
            hint: (
              <button type="button" className="sc-grp-hint" onClick={() => void navigate('/schedule')}>
                {t('To start in {page}', { page: t('Schedule') })}
              </button>
            ),
          },
        ];
  return (
    <aside className="lib-nav sc-grp-nav">
      <div className="lib-nav-section">
        <span className="grow">{t('Sources')}</span>
      </div>
      {groups
        .filter((g) => g.items.length > 0)
        .map((g) => (
          <div key={g.label} className="sc-grp">
            <div className={`sc-grp-title ${g.live ? 'live' : ''}`}>
              {g.live && <span className="sc-grp-dot" />}
              {g.label} <span className="sc-grp-n">{g.items.length}</span>
            </div>
            {g.hint}
            {g.items.map((r) => (
              <div key={r.id} className={`lib-nav-item sc-grp-it ${r.id === current ? 'on' : ''} ${g.dim ? 'dim' : ''}`} onClick={() => void navigate(`${base}/${r.id}`)}>
                <RecLogo rec={r} size="sm" />
                <span className="sc-grp-name" title={r.name}>
                  {r.name}
                </span>
                {badge?.(r)}
              </div>
            ))}
          </div>
        ))}
    </aside>
  );
}
