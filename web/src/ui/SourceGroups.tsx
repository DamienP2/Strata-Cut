/*
 * The sources on the left of Extraction and Streamer (V26b): one line each (picture, state dot,
 * logo, name), in groups that say what can be done with them. Extraction: recording / a loop to
 * cut in (stopped) / nothing to cut. Streamer: on air / a picture / no picture. The last group
 * (the ones not to be chosen) is folded; only its lines say why.
 */
import { useEffect, useState } from 'react';
import { useShallow } from 'zustand/react/shallow';
import { ChevronDown, ChevronRight } from 'lucide-react';
import { useDisplayPref } from '../displayPrefs';
import { t, tc } from '../i18n';
import { navigate } from '../router';
import { useApp } from '../store';
import type { Recorder, RecorderStatus } from '../types';
import { RecLogo } from './RecLogo';

const WHY: Record<string, string> = { nosignal: 'No signal', error: 'Error', starting: 'Connecting…', waiting: 'Programmed', off: 'Stopped' };

interface Group {
  key: string;
  label: string;
  items: Recorder[];
  /** The ones not to be chosen: folded, each saying why. */
  quiet?: boolean;
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
  const [open, setOpen] = useDisplayPref(`sources.${kind}.quiet`, false);
  const tick = useTick(30_000);
  const live = (r: Recorder) => status[r.id]?.state === 'recording';
  const loop = (r: Recorder) => status[r.id]?.oldest != null;
  const air = (r: Recorder) => (onAir?.(r) ?? 0) > 0;
  const groups: Group[] =
    kind === 'extract'
      ? [
          { key: 'rec', label: t('Recording'), items: recorders.filter(live) },
          { key: 'loop', label: t('Loop to cut in'), items: recorders.filter((r) => !live(r) && loop(r)) },
          { key: 'none', label: t('Nothing to cut'), items: recorders.filter((r) => !live(r) && !loop(r)), quiet: true },
        ]
      : [
          { key: 'air', label: t('On air'), items: recorders.filter((r) => live(r) && air(r)) },
          { key: 'pic', label: t('Picture available'), items: recorders.filter((r) => live(r) && !air(r)) },
          { key: 'none', label: t('No picture'), items: recorders.filter((r) => !live(r)), quiet: true },
        ];
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
      {groups
        .filter((g) => g.items.length > 0)
        .map((g) => {
          // Folded, the source open now stays in sight.
          const shown = g.quiet && !open ? g.items.filter((r) => r.id === current) : g.items;
          return (
            <div key={g.key} className={`sc-grp ${g.quiet ? 'quiet' : ''}`}>
              {g.quiet ? (
                <button type="button" className="sc-grp-title fold" aria-expanded={open} onClick={() => setOpen(!open)}>
                  {open ? <ChevronDown size={11} /> : <ChevronRight size={11} />}
                  {g.label} <span className="sc-grp-n">{g.items.length}</span>
                </button>
              ) : (
                <div className="sc-grp-title">
                  {g.label} <span className="sc-grp-n">{g.items.length}</span>
                </div>
              )}
              {shown.map((r) => {
                const st = status[r.id];
                const locked = !!g.quiet;
                return (
                  <div key={r.id} className={`lib-nav-item sc-grp-it ${r.id === current ? 'on' : ''} ${locked ? 'dim locked' : ''}`} aria-disabled={locked || undefined} onClick={() => !locked && void navigate(`${base}/${r.id}`)}>
                    <span className="sc-grp-pic">{live(r) && <img src={`/api/recorders/${r.id}/thumb.jpg?v=${tick}`} alt="" onError={(e) => (e.currentTarget.style.display = 'none')} />}</span>
                    <span className={`sc-grp-dot ${dot(r, st)}`} />
                    <RecLogo rec={r} size="sm" />
                    <span className="sc-grp-name" title={r.name}>
                      {r.name}
                    </span>
                    {locked && <span className="sc-grp-why">{tc('source', WHY[st?.state ?? 'off'] ?? 'Stopped')}</span>}
                  </div>
                );
              })}
            </div>
          );
        })}
    </aside>
  );
}
