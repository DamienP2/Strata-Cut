/*
 * The sources on the left of Extraction and Streamer: one line each (picture, state dot, logo,
 * name), only the ones that can be chosen. Extraction: recording / a loop to cut in (stopped).
 * Streamer: on air, then the others received (no title).
 */
import { useEffect, useState } from 'react';
import { useShallow } from 'zustand/react/shallow';
import { t } from '../i18n';
import { navigate } from '../router';
import { useApp } from '../store';
import type { Recorder, RecorderStatus } from '../types';
import { RecLogo } from './RecLogo';

interface Group {
  key: string;
  /** None: the list goes on under the group before. */
  label?: string;
  items: Recorder[];
}

/** A picture every 30 s for the sources received (the server keeps their last one). */
function useTick(ms: number) {
  const [n, setN] = useState(0);
  useEffect(() => {
    const i = setInterval(() => setN((x) => x + 1), ms);
    return () => clearInterval(i);
  }, [ms]);
  return n;
}

export function SourceGroups({ kind, current, base, onAir }: { kind: 'extract' | 'stream'; current?: string; base: string; onAir?: (r: Recorder) => number }) {
  const { recorders, status } = useApp(useShallow((s) => ({ recorders: s.recorders, status: s.status })));
  const tick = useTick(30_000);
  const live = (r: Recorder) => status[r.id]?.state === 'recording';
  const loop = (r: Recorder) => status[r.id]?.oldest != null;
  const air = (r: Recorder) => (onAir?.(r) ?? 0) > 0;
  const groups: Group[] = (
    kind === 'extract'
      ? [
          { key: 'rec', label: t('Recording'), items: recorders.filter(live) },
          { key: 'loop', label: t('Loop to cut in'), items: recorders.filter((r) => !live(r) && loop(r)) },
        ]
      : [
          { key: 'air', label: t('On air'), items: recorders.filter((r) => live(r) && air(r)) },
          { key: 'pic', items: recorders.filter((r) => live(r) && !air(r)) },
        ]
  ).filter((g) => g.items.length > 0);
  const dot = (r: Recorder, st?: RecorderStatus) => {
    const s = st?.state ?? 'off';
    if (s === 'recording') return kind === 'extract' || air(r) ? 'rec' : 'ok';
    return s === 'nosignal' || s === 'starting' ? 'warn' : s === 'error' ? 'bad' : s === 'waiting' ? 'plan' : '';
  };
  return (
    <aside className="lib-nav sc-grp-nav">
      <div className="lib-nav-section">
        <span className="grow">{t('Sources')}</span>
      </div>
      {groups.length === 0 && <div className="sc-grp-empty">{t('No source')}</div>}
      {groups.map((g) => (
        <div key={g.key} className="sc-grp">
          {g.label && (
            <div className="sc-grp-title">
              {g.label} <span className="sc-grp-n">{g.items.length}</span>
            </div>
          )}
          {g.items.map((r) => (
            <div key={r.id} className={`lib-nav-item sc-grp-it ${r.id === current ? 'on' : ''}`} onClick={() => void navigate(`${base}/${r.id}`)}>
              <span className="sc-grp-pic">{live(r) && <img src={`/api/recorders/${r.id}/thumb.jpg?v=${tick}`} alt="" onError={(e) => (e.currentTarget.style.display = 'none')} />}</span>
              <span className={`sc-grp-dot ${dot(r, status[r.id])}`} />
              <RecLogo rec={r} size="sm" />
              <span className="sc-grp-name" title={r.name}>
                {r.name}
              </span>
            </div>
          ))}
        </div>
      ))}
    </aside>
  );
}
