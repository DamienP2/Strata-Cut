/* Système › Supervision › Santé: incidents and the server's log. */
import { useEffect, useLayoutEffect, useMemo, useRef, useState } from 'react';
import { create } from 'zustand';
import { Download, Eraser, HeartPulse, Pause, Play, ScrollText, Search, TriangleAlert } from 'lucide-react';
import { locale, t, tn } from '../i18n';
import { api, useApp } from '../store';
import { PageHead, SubTabs } from '../pages/SidePage';
import { useMetrics, watchMetrics } from '../ui/metrics';
import { useDisplayPref } from '../displayPrefs';
import { Dropdown } from '../ui/Dropdown';
import { Empty } from '../ui/Empty';
import { confirmDialog } from '../ui/dialogs';
import { formatDateTime, formatDuration, timeAgo } from '../ui/format';
import { healthSeen, loadHealth, useHealth, type Incident } from '../ui/health';
import { said, WhoIcon, type Said } from '../ui/logWho';
import { showError } from '../ui/serverError';

type Tab = 'incidents' | 'log';

export function Health() {
  const health = useHealth((s) => s.health);
  const toast = useApp((s) => s.toast);
  const clearJournal = async () => {
    if (!(await confirmDialog({ title: t('Empty the health journal?'), message: t('The incidents, the crash reports and the log are deleted. Export them first if the support needs them.'), confirm: t('Empty'), danger: true }))) return;
    try {
      await api('POST', '/api/system/health/clear');
      await loadHealth();
      toast(t('Journal emptied'));
    } catch (e) {
      showError(e);
    }
  };
  const [tab, setTab] = useDisplayPref<Tab>('health.tab', 'incidents');

  useEffect(() => {
    void loadHealth().then(() => {
      if (useHealth.getState().health?.unseenCrash) void healthSeen();
    });
  }, []);

  return (
    <>
      <PageHead title={t('Health')}>
        <button className="btn" onClick={() => void clearJournal()}>
          <Eraser size={14} /> {t('Empty the journal')}
        </button>
        <a className="btn" href="/api/system/health/export.zip" download>
          <Download size={14} /> {t('Export')}
        </a>
      </PageHead>
      <Tiles />
      <SubTabs<Tab>
        value={tab}
        onChange={setTab}
        items={[
          { id: 'incidents', label: t('Incidents'), icon: <TriangleAlert size={14} />, count: health?.incidents.length },
          { id: 'log', label: t('Log'), icon: <ScrollText size={14} /> },
        ]}
      >
        {tab === 'log' && <LogTools />}
      </SubTabs>
      {tab === 'incidents' ? <Incidents list={health?.incidents ?? null} /> : <LogConsole />}
    </>
  );
}

const LEVEL_CLASS: Record<Incident['level'], string> = { CRASH: 'crash', ERROR: 'bad', WARN: 'warn' };
const LEVEL_LABEL: Record<Incident['level'], string> = { CRASH: 'Crash', ERROR: 'Error', WARN: 'Warning' };

/** The server at a glance: how long it runs, what went wrong, its load. */
function Tiles() {
  const health = useHealth((s) => s.health);
  const m = useMetrics((s) => s.metrics?.machine);
  useEffect(() => watchMetrics(), []);
  const mem = m && m.memTotal ? (m.memUsed / m.memTotal) * 100 : null;
  const disk = health && health.disk.total ? ((health.disk.total - health.disk.free) / health.disk.total) * 100 : null;
  const tone = (pct: number | null) => (pct === null ? '' : pct >= 90 ? 'bad' : pct >= 75 ? 'warn' : 'ok');
  const gauge = (label: string, pct: number | null, sub?: string) => (
    <div className={`hl-tile ${tone(pct)}`}>
      <span className="hl-tile-l">{label}</span>
      <b>{pct === null ? '—' : `${Math.round(pct)} %`}</b>
      <span className="hl-bar">
        <span style={{ width: `${Math.min(100, pct ?? 0)}%` }} />
      </span>
      {sub && <span className="hl-tile-s">{sub}</span>}
    </div>
  );
  const count = (label: string, n: number | undefined, cls: string, sub?: string) => (
    <div className={`hl-tile ${n ? cls : 'ok'}`}>
      <span className="hl-tile-l">{label}</span>
      <b>{n ?? '—'}</b>
      {sub && <span className="hl-tile-s">{sub}</span>}
    </div>
  );
  return (
    <div className="hl-tiles">
      <div className="hl-tile">
        <span className="hl-tile-l">{t('Running for')}</span>
        <b>{health ? formatDuration(health.uptimeSecs) : '—'}</b>
        {health && <span className="hl-tile-s">{t('version {v}', { v: health.version })}</span>}
      </div>
      {count(t('Crashes'), health?.crashes, 'bad')}
      {count(t('Errors'), health?.errors, 'bad', health?.lastHourErrors ? t('{n} in the last hour', { n: health.lastHourErrors }) : undefined)}
      {count(t('Warnings'), health?.warnings, 'warn')}
      {gauge(t('Processor'), m ? m.cpu : null, m ? tn(m.cores, '{n} core', '{n} cores') : undefined)}
      {gauge(t('Memory'), mem)}
      {gauge(t('Disk'), disk)}
    </div>
  );
}

function Incidents({ list }: { list: Incident[] | null }) {
  const recs = useApp((s) => s.allRecorders);
  const [only, setOnly] = useState('');
  const items = useMemo(() => {
    if (!list) return null;
    // The same incident told with another memory address ("[tcp @ 0x55…]") is one; the clips
    // not sent to the same place for the same reason are one too.
    const merged: { i: Incident; names: string[] }[] = [];
    const at = new Map<string, number>();
    for (const i of list) {
      const send = i.title.match(/^clip (.+?) not sent to (.+?): (.+)$/);
      // Said again with other figures (the load, how many lost…): the same.
      const figures = /^processor over its limit|reception problems|damaged in the last 10 minutes|preview is \d+ s behind|^network card|dropped \d+ (UDP )?packets/.test(i.title);
      const title = i.title.replace(/0x[0-9a-f]+/gi, '');
      const key = send ? `${i.level}|send|${send[2]}|${send[3]}` : `${i.level}|${figures ? title.replace(/\d+([.,]\d+)?/g, '#') : title}`;
      const k = at.get(key);
      if (k === undefined) {
        at.set(key, merged.length);
        merged.push({ i: { ...i }, names: send ? [send[1]] : [] });
      } else {
        const m = merged[k].i;
        m.count += i.count;
        if (new Date(i.last).getTime() > new Date(m.last).getTime()) m.last = i.last;
        if (new Date(i.first).getTime() < new Date(m.first).getTime()) m.first = i.first;
        if (send && !merged[k].names.includes(send[1])) merged[k].names.push(send[1]);
      }
    }
    return merged.map(({ i, names }) => {
      let s: Said = said(i.title, i.cat, recs);
      if (names.length > 1) {
        const send = i.title.match(/^clip (.+?) not sent to (.+?): (.+)$/)!;
        s = { ...s, text: tn(names.length, '{n} clip not sent to {dest}', '{n} clips not sent to {dest}', { dest: send[2] }), hint: `${names.join(', ')} · ${s.hint ?? ''}` };
      }
      return { i, s };
    });
  }, [list, recs]);
  if (!items) return <div className="loading">{t('Loading…')}</div>;
  if (!items.length)
    return (
      <div className="set-card health-empty">
        <Empty icon={HeartPulse} text={t('No incident')} />
      </div>
    );
  const whos = [...new Set(items.map((x) => x.s.who.label))];
  const shown = only ? items.filter((x) => x.s.who.label === only) : items;
  return (
    <>
      <div className="lw-chips">
        <button type="button" className={`lw-chip ${only ? '' : 'on'}`} onClick={() => setOnly('')}>
          {t('All')} <b>{items.length}</b>
        </button>
        {whos.map((w) => (
          <button key={w} type="button" className={`lw-chip ${only === w ? 'on' : ''}`} onClick={() => setOnly(only === w ? '' : w)}>
            {w} <b>{items.filter((x) => x.s.who.label === w).length}</b>
          </button>
        ))}
      </div>
      <div className="hl-incs">
        {shown.map(({ i, s }, k) => {
          const detail = [i.title, i.detail].filter((x, n, a) => x && x.trim() && a.indexOf(x) === n).join('\n');
          return (
            <div key={k} className={`lw-inc ${LEVEL_CLASS[i.level] ?? ''}`}>
              <span className={`lw-dot ${LEVEL_CLASS[i.level] ?? ''}`} title={t(LEVEL_LABEL[i.level] ?? i.level)} />
              <span className="lw-who">
                <WhoIcon who={s.who} /> {s.who.label}
              </span>
              <div className="lw-main">
                <strong>{s.text}</strong>
                {s.hint && <span>{s.hint}</span>}
                <details>
                  <summary>{t('Technical details')}</summary>
                  <code>{detail}</code>
                </details>
              </div>
              <div className="hl-inc-side" title={i.first !== i.last ? t('First: {date}', { date: formatDateTime(i.first) }) : formatDateTime(i.last)}>
                {i.count > 1 && <b>×{i.count}</b>}
                <span>{timeAgo(i.last)}</span>
              </div>
            </div>
          );
        })}
      </div>
    </>
  );
}

/* ---------- Log ---------- */

interface LogEntry {
  seq: number;
  at: number;
  level: string;
  target: string;
  message: string;
  fields: string;
}

type Level = '' | 'INFO' | 'WARN' | 'ERROR';

const useLogView = create<{ level: Level; q: string; paused: boolean; tech: boolean; who: string }>(() => ({ level: '', q: '', paused: false, tech: false, who: '' }));

function LogTools() {
  const { level, q, paused, tech } = useLogView();
  return (
    <div className="media-toolbar log-tools">
      <label className="search">
        <Search size={14} />
        <input value={q} placeholder={t('Search the log')} onChange={(e) => useLogView.setState({ q: e.target.value })} onKeyDown={(e) => e.key !== 'Escape' && e.stopPropagation()} />
      </label>
      <div className="log-level">
        <Dropdown<Level>
          value={level}
          onChange={(v) => useLogView.setState({ level: v })}
          options={[
            { value: '', label: t('All levels') },
            { value: 'INFO', label: t('Information and above') },
            { value: 'WARN', label: t('Warnings and errors') },
            { value: 'ERROR', label: t('Errors only') },
          ]}
        />
      </div>
      <label className="lw-tech">
        <input type="checkbox" checked={tech} onChange={(e) => useLogView.setState({ tech: e.target.checked })} /> {t('Technical details')}
      </label>
      <button type="button" className={`btn ${paused ? 'on' : ''}`} onClick={() => useLogView.setState({ paused: !paused })}>
        {paused ? <Play size={14} /> : <Pause size={14} />} {paused ? t('Resume') : t('Pause')}
      </button>
      <a className="btn" href="/api/system/logs/download" download title={t('Download the log')}>
        <Download size={14} /> {t('Download')}
      </a>
    </div>
  );
}

const RANK: Record<string, number> = { TRACE: 0, DEBUG: 1, INFO: 2, WARN: 3, ERROR: 4 };
const LOG_LEVEL: Record<string, string> = { ERROR: 'Error', WARN: 'Warning', INFO: 'Info', DEBUG: 'Debug', TRACE: 'Debug' };

/** The part of a log target that says what wrote it ("strata_cut::recorder::run" → "recorder"). */
const catOf = (target: string) => {
  const p = target.split('::');
  return p[0] === 'strata_cut' ? (p[1] ?? 'server') : p[0];
};

function LogConsole() {
  const { level, q, paused, tech, who } = useLogView();
  const recs = useApp((s) => s.allRecorders);
  const tz = useApp((s) => s.timezone);
  const [entries, setEntries] = useState<LogEntry[] | null>(null);
  const last = useRef(0);
  const box = useRef<HTMLDivElement>(null);
  const stick = useRef(true);

  useEffect(() => {
    if (paused) return;
    let stop = false;
    const poll = async () => {
      try {
        const r = await api<{ entries: LogEntry[]; last: number }>('GET', `/api/system/logs?after=${last.current}&limit=1000`);
        if (stop) return;
        last.current = r.last ?? last.current;
        setEntries((old) => (r.entries.length ? [...(old ?? []), ...r.entries].slice(-5000) : (old ?? [])));
      } catch {
        if (!stop) setEntries((old) => old ?? []);
      }
    };
    void poll();
    const id = window.setInterval(() => void poll(), 2000);
    return () => {
      stop = true;
      clearInterval(id);
    };
    // Polls from the last line received; restarts only when paused / resumed.
  }, [paused]);

  // Each line: whom it is about, what it says.
  const lines = useMemo(() => (entries ?? []).map((e) => ({ e, s: said(e.message, catOf(e.target), recs) })), [entries, recs]);
  const whos = useMemo(() => [...new Set(lines.map((l) => l.s.who.label))], [lines]);
  const shown = useMemo(() => {
    const s = q.trim().toLowerCase();
    const min = level ? RANK[level] : -1;
    return lines.filter(({ e, s: x }) => (RANK[e.level] ?? 2) >= min && (!who || x.who.label === who) && (!s || `${x.who.label} ${x.text} ${e.target} ${e.message} ${e.fields}`.toLowerCase().includes(s)));
  }, [lines, q, level, who]);

  useLayoutEffect(() => {
    const b = box.current;
    if (b && stick.current) b.scrollTop = b.scrollHeight;
  }, [shown]);

  const time = (at: number) => {
    try {
      return new Date(at).toLocaleTimeString(locale(), { hour12: false, timeZone: tz });
    } catch {
      return new Date(at).toLocaleTimeString(locale(), { hour12: false });
    }
  };

  return (
    <>
    <div className="lw-chips">
      <button type="button" className={`lw-chip ${who ? '' : 'on'}`} onClick={() => useLogView.setState({ who: '' })}>
        {t('All the flows')}
      </button>
      {whos.map((w) => (
        <button key={w} type="button" className={`lw-chip ${who === w ? 'on' : ''}`} onClick={() => useLogView.setState({ who: who === w ? '' : w })}>
          {w}
        </button>
      ))}
    </div>
    <div
      ref={box}
      className="log-box"
      onScroll={(e) => {
        const b = e.currentTarget;
        stick.current = b.scrollHeight - b.scrollTop - b.clientHeight < 24;
      }}
    >
      {entries === null ? (
        <div className="loading">{t('Loading…')}</div>
      ) : shown.length === 0 ? (
        <div className="loading">{q || level ? t('No match') : t('Empty log')}</div>
      ) : (
        shown.map(({ e, s: x }) => (
          <div key={e.seq} className="log-line lw-line">
            <span className="log-at" title={formatDateTime(e.at)}>
              {time(e.at)}
            </span>
            <span className={`log-lvl ${e.level}`}>{t(LOG_LEVEL[e.level] ?? e.level)}</span>
            <span className="lw-who sm">
              <WhoIcon who={x.who} size={11} /> {x.who.label}
            </span>
            <span className="log-msg">
              {x.text}
              {tech && (
                <span className="log-fields">
                  {' '}
                  [{e.target}] {e.message} {e.fields}
                </span>
              )}
            </span>
          </div>
        ))
      )}
    </div>
    </>
  );
}
