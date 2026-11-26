/*
 * Programmation: the slots of the recorders in "programmed" mode. Such a recorder records
 * only during its slots (one day, or days of the week) and keeps its recordings some days,
 * instead of a rolling loop. A slot can be taken from the source's programme guide.
 */
import { useCallback, useEffect, useLayoutEffect, useMemo, useRef, useState } from 'react';
import { createPortal } from 'react-dom';
import { useDisplayPref } from '../displayPrefs';
import { kindOf, SOURCE_KINDS } from '../ui/sourceKinds';
import { useShallow } from 'zustand/react/shallow';
import { CalendarClock, ChevronLeft, ChevronRight, Plus, Trash2, TriangleAlert, Tv, WifiOff, X } from 'lucide-react';
import { api, can, serverEvents, serverNow, useApp } from '../store';
import { locale, t, tc } from '../i18n';
import { navigate } from '../router';
import { clock, day } from '../tc';
import { confirmDialog } from '../ui/dialogs';
import { Dropdown } from '../ui/Dropdown';
import { Segmented } from '../ui/fields';
import { Field } from '../settings/ui';
import { RecLogo } from '../ui/RecLogo';
import { ContentFlags } from '../ui/ContentFlags';
import type { Programme, Recorder, Slot } from '../types';
import { explainGuard } from '../ui/guard';
import { showError } from '../ui/serverError';
import { recWithLoop } from '../ui/RecLoop';

/** 1 = Monday … 7 = Sunday: their short names in the interface's language. */
function dayNames(): string[] {
  const f = new Intl.DateTimeFormat(locale(), { weekday: 'short', timeZone: 'UTC' });
  // 2024-01-01 was a Monday.
  return [1, 2, 3, 4, 5, 6, 7].map((d) => f.format(Date.UTC(2024, 0, d)).replace('.', ''));
}

/** A server time as the slot's date and hour, in the server's time zone. */
function dateIn(ms: number, tz: string): string {
  return new Intl.DateTimeFormat('en-CA', { timeZone: tz, year: 'numeric', month: '2-digit', day: '2-digit' }).format(ms);
}
const hhmm = (ms: number, tz: string) => clock(ms, tz).slice(0, 5);

export function Schedule() {
  const { recorders, user, timezone, toast } = useApp(useShallow((s) => ({ recorders: s.recorders, user: s.auth?.user, timezone: s.timezone, toast: s.toast })));
  // Starting, stopping, programming: as the role allows.
  // Programming: its own recordings, or the others' too.
  const admin = can.has(user, 'control.slots.own') || can.has(user, 'control.slots.all');
  const mayEdit = (slot: Slot) => can.has(user, 'control.slots.all') || (can.has(user, 'control.slots.own') && slot.by === user?.id);
  // REC / Stop on a source: their own rights (stopping only what it started, when so).
  const canRec = can.has(user, 'control.rec.start');
  const mayStop = (r: Recorder) => can.has(user, 'control.rec.stop') || (can.has(user, 'control.rec.stop.own') && r.recBy === user?.id);
  const [slots, setSlots] = useState<Slot[] | null>(null);
  const [edit, setEdit] = useState<{ slot: Slot | null; initial?: Partial<Slot> } | null>(null);
  const load = useCallback(async () => {
    try {
      setSlots((await api<{ slots: Slot[] }>('GET', '/api/schedules')).slots);
    } catch (e) {
      showError(e);
    }
  }, [toast]);
  useEffect(() => void load(), [load, recorders]);
  useEffect(() => {
    const on = (e: Event) => setSlots((e as CustomEvent<{ slots: Slot[] }>).detail.slots);
    serverEvents.addEventListener('schedules', on);
    // The next occurrences move with time.
    const i = setInterval(() => void load(), 60_000);
    return () => {
      serverEvents.removeEventListener('schedules', on);
      clearInterval(i);
    };
  }, [load]);

  // Every recorder can be programmed: a stopped one records during its slots.
  // A test pattern is not recorded (made live in the Streamer): not in the Régie.
  const programmed = recorders.filter((r) => r.protocol !== 'pattern');
  // One kind of source only (its key), or all.
  const [kind, setKind] = useDisplayPref<string>('schedule.kind', 'all');
  const kinds = SOURCE_KINDS.filter((k) => programmed.some((r) => kindOf(r) === k.key));
  const shown = kind === 'all' || !kinds.some((k) => k.key === kind) ? programmed : programmed.filter((r) => kindOf(r) === kind);
  const names = useMemo(dayNames, []);
  if (!slots) return <div className="loading">{t('Loading…')}</div>;

  return (
    <div className="sc-plan">
      <div className="sc-plan-main">
        {programmed.length === 0 ? (
          <div className="empty big">
            <CalendarClock size={22} />
            <p>{t('No recorder yet.')}</p>
          </div>
        ) : (
          <>
            <TzWarning timezone={timezone} admin={admin} />
            {(kinds.length > 1 || admin) && (
              <div className="asr-chips sc-kinds">
                <button className={`asr-chip ${kind === 'all' || shown === programmed ? 'on' : ''}`} onClick={() => setKind('all')}>
                  {t('All kinds')}
                </button>
                {kinds.map((k) => (
                  <button key={k.key} className={`asr-chip ${kind === k.key && shown !== programmed ? 'on' : ''}`} onClick={() => setKind(kind === k.key ? 'all' : k.key)}>
                    {k.icon} {t(k.label)} <span className="asr-chip-n">{programmed.filter((r) => kindOf(r) === k.key).length}</span>
                  </button>
                ))}
                <span className="spacer" />
                {admin && (
                  <button type="button" className="btn primary small" onClick={() => setEdit({ slot: null })}>
                    <Plus size={13} /> {t('New recording')}
                  </button>
                )}
              </div>
            )}
            <Calendar recorders={shown} slots={slots} timezone={timezone} admin={admin} canRec={canRec} mayStop={mayStop} onOpen={(slot) => setEdit({ slot })} onNew={(initial) => setEdit({ slot: null, initial })} />
          </>
        )}
      </div>
      {edit && <SlotModal slot={edit.slot} initial={edit.initial} recorders={programmed} preset={null} names={names} timezone={timezone} admin={edit.slot ? mayEdit(edit.slot) : admin} onClose={() => setEdit(null)} />}
    </div>
  );
}

function SlotModal({ slot, initial, recorders, preset, names, timezone, admin, onClose }: { slot: Slot | null; initial?: Partial<Slot>; recorders: Recorder[]; preset: string | null; names: string[]; timezone: string; admin: boolean; onClose: () => void }) {
  const [s, setS] = useState<Slot>(
    () =>
      slot ?? {
        id: '',
        recorder: preset && recorders.some((r) => r.id === preset) ? preset : (recorders[0]?.id ?? ''),
        name: '',
        date: dateIn(serverNow(), timezone),
        days: [],
        start: '20:00',
        end: '21:00',
        enabled: true,
        ...initial,
      },
  );
  // The coming occurrences of a slot already saved (its details).
  const [coming, setComing] = useState<Occurrence[]>([]);
  useEffect(() => {
    if (!slot) return;
    const now = serverNow();
    api<{ occurrences: Occurrence[] }>('GET', `/api/schedules/occurrences?from=${Math.round(now)}&to=${Math.round(now + 21 * 86_400_000)}`).then(
      (o) => setComing(o.occurrences.filter((x) => x.slot === slot.id && x.enabled !== false).sort((a, b) => a.start - b.start).slice(0, 6)),
      () => setComing([]),
    );
  }, [slot]);
  const remove = async () => {
    if (!slot || !(await confirmDialog({ title: t('Delete the slot “{name}”?', { name: slot.name || t('Programmed slot') }), message: t('What it recorded stays.'), confirm: t('Delete'), danger: true }))) return;
    try {
      await api('DELETE', `/api/schedules/${slot.id}`);
      onClose();
    } catch (e) {
      showError(e);
    }
  };
  const [busy, setBusy] = useState(false);
  const set = (patch: Partial<Slot>) => setS((x) => ({ ...x, ...patch }));
  const repeated = !s.date;
  // The source's guide: the coming programmes, to programme one in a click.
  const [guide, setGuide] = useState<Programme[]>([]);
  useEffect(() => {
    const now = serverNow();
    api<{ events: Programme[] }>('GET', `/api/recorders/${s.recorder}/epg?from=${Math.round(now)}&to=${Math.round(now + 48 * 3_600_000)}`).then(
      (g) => setGuide(g.events.slice(0, 30)),
      () => setGuide([]),
    );
  }, [s.recorder]);
  const fromGuide = (g: Programme) => set({ name: g.name, date: dateIn(g.start, timezone), days: [], start: hhmm(g.start, timezone), end: hhmm(g.end, timezone) });

  // A new recording: programmed (its hours), or continuous (from now on, until stopped).
  const [mode, setMode] = useState<'programme' | 'continuous'>('programme');
  const continuous = !slot && mode === 'continuous';
  const free = recorders.filter((r) => !r.enabled);
  const save = async () => {
    setBusy(true);
    try {
      if (continuous) {
        try {
          await api('POST', `/api/recorders/${s.recorder}/recording`, { mode: 'continuous' });
        } catch (e) {
          if (explainGuard(e)) return;
          throw e;
        }
      } else if (slot) await api('PUT', `/api/schedules/${slot.id}`, s);
      else await api('POST', '/api/schedules', s);
      onClose();
    } catch (e) {
      showError(e);
    } finally {
      setBusy(false);
    }
  };

  return (
    <div className="modal-back" onPointerDown={onClose}>
      <form className="modal wide" onPointerDown={(e) => e.stopPropagation()} onSubmit={(e) => { e.preventDefault(); void save(); }}>
        <header>
          <h2>{slot ? t('Edit the slot') : t('New recording')}</h2>
          <button type="button" className="icon-btn ghost" title={t('Close')} onClick={onClose}>
            <X size={16} />
          </button>
        </header>
        {!slot && (
          <Segmented
            className="sc-rec-mode"
            value={mode}
            options={[
              { value: 'programme', label: t('Programme') },
              { value: 'continuous', label: tc('rec', 'Continuous') },
            ]}
            onChange={(v) => {
              setMode(v);
              // Continuous: a source not already recording all the time.
              if (v === 'continuous' && !free.some((r) => r.id === s.recorder) && free[0]) set({ recorder: free[0].id });
            }}
          />
        )}
        {continuous ? (
          <div className="set-grid two">
            {free.length > 0 ? (
              <Field label={t('Recorder')} group>
                <Dropdown value={s.recorder} options={free.map((r) => ({ value: r.id, label: r.name }))} onChange={(v) => set({ recorder: v })} />
              </Field>
            ) : (
              <span className="muted span2">{t('Every source already records all the time.')}</span>
            )}
          </div>
        ) : (
        <div className="set-grid two">
          <Field label={t('Recorder')} group hint={recorders.find((r) => r.id === s.recorder)?.enabled ? t('It already records all the time: the slot counts when it is stopped.') : t('Stopped: it starts a minute before and stops at the end.')}>
            <Dropdown value={s.recorder} options={recorders.map((r) => ({ value: r.id, label: r.name }))} onChange={(v) => set({ recorder: v })} />
          </Field>
          <Field label={t('Name')}>
            <input value={s.name} placeholder={t('Optional')} onChange={(e) => set({ name: e.target.value })} />
          </Field>
          <Field label={t('Repeat')} group>
            <Segmented
              value={repeated ? 'days' : 'once'}
              options={[
                { value: 'once', label: t('Once') },
                { value: 'days', label: t('Every week') },
              ]}
              onChange={(v) => set(v === 'once' ? { date: s.date || dateIn(serverNow(), timezone), days: [] } : { date: '', days: s.days.length ? s.days : [1, 2, 3, 4, 5] })}
            />
          </Field>
          {repeated ? (
            <Field label={t('Days')} group>
              <div className="sc-days">
                {names.map((n, i) => {
                  const d = i + 1;
                  const on = s.days.includes(d);
                  return (
                    <button key={d} type="button" className={`btn small ${on ? 'on' : ''}`} onClick={() => set({ days: on ? s.days.filter((x) => x !== d) : [...s.days, d].sort() })}>
                      {n}
                    </button>
                  );
                })}
              </div>
            </Field>
          ) : (
            <Field label={t('Date')}>
              <input type="date" value={s.date} onChange={(e) => set({ date: e.target.value })} />
            </Field>
          )}
          <Field label={t('Start')}>
            <input type="time" value={s.start} onChange={(e) => set({ start: e.target.value })} />
          </Field>
          <Field label={t('End')} hint={s.end <= s.start ? t('The next day') : undefined}>
            <input type="time" value={s.end} onChange={(e) => set({ end: e.target.value })} />
          </Field>
          {guide.length > 0 && (
            <Field label={t('From the programme guide')} group className="span2">
              <div className="sc-guide-list">
                {guide.map((g) => (
                  <button key={`${g.id}-${g.start}`} type="button" className="sc-guide-item" onClick={() => fromGuide(g)} title={g.text}>
                    <Tv size={12} />
                    <span className="sc-guide-time">
                      {day(g.start, timezone, locale())} {hhmm(g.start, timezone)}–{hhmm(g.end, timezone)}
                    </span>
                    <span className="sc-guide-name">{g.name}</span>
                  </button>
                ))}
              </div>
            </Field>
          )}
          {coming.length > 0 && (
            <Field label={t('Coming recordings')} group className="span2">
              <div className="sc-coming">
                {coming.map((o) => (
                  <span key={o.start} className={o.start <= serverNow() ? 'on' : ''}>
                    {day(o.start, timezone, locale())} {hhmm(o.start, timezone)}–{hhmm(o.end, timezone)}
                  </span>
                ))}
              </div>
            </Field>
          )}
          <span className="set-hint span2">{t('Hours in the server’s time zone ({tz}).', { tz: timezone })}</span>
        </div>
        )}
        <footer>
          {slot && admin && (
            <button type="button" className="btn ghost danger-text" onClick={() => void remove()}>
              <Trash2 size={14} /> {t('Delete')}
            </button>
          )}
          <div className="spacer" />
          <button type="button" className="btn" onClick={onClose}>
            {t('Cancel')}
          </button>
          <button type="submit" className="btn primary" disabled={!admin || busy || !s.recorder || (continuous ? !free.some((r) => r.id === s.recorder) : (repeated && s.days.length === 0) || s.start === s.end)}>
            {continuous ? (
              <>
                <span className="sc-rec" /> {t('Start recording')}
              </>
            ) : slot ? t('Save') : (
              <>
                <Plus size={14} /> {t('Programme')}
              </>
            )}
          </button>
        </footer>
      </form>
    </div>
  );
}

/** An occurrence of a slot (the calendar). */
interface Occurrence {
  slot: string;
  recorder: string;
  name: string;
  start: number;
  end: number;
  /** false: the slot is paused (shown, records nothing). */
  enabled?: boolean;
}

const DAY = 86_400_000;

type View = '6h' | '12h' | 'day' | 'week';
const SPANS: Record<View, number> = { '6h': 6 * 3_600_000, '12h': 12 * 3_600_000, day: DAY, week: 7 * DAY };

/** Where a view starts around `ms`: an hour before (6 h, 12 h: on the hour), else at midnight. */
function anchorFor(v: View, ms: number, tz: string): number {
  if (v === 'day' || v === 'week') return startOfDay(ms, tz);
  const back = v === '6h' ? 3_600_000 : 2 * 3_600_000;
  return Math.floor((ms - back) / 3_600_000) * 3_600_000;
}

/** The time zone's offset at `ms` (ms to add to UTC to read its clock). */
function tzOffset(ms: number, tz: string): number {
  const p = new Intl.DateTimeFormat('en-US', { timeZone: tz, hourCycle: 'h23', year: 'numeric', month: '2-digit', day: '2-digit', hour: '2-digit', minute: '2-digit', second: '2-digit' }).formatToParts(ms);
  const g = (k: string) => Number(p.find((x) => x.type === k)?.value ?? 0);
  return Date.UTC(g('year'), g('month') - 1, g('day'), g('hour') % 24, g('minute'), g('second')) - Math.floor(ms / 1000) * 1000;
}

/** Midnight of the day holding `ms`, in the time zone. */
function startOfDay(ms: number, tz: string): number {
  const [y, m, d] = dateIn(ms, tz).split('-').map(Number);
  const utc = Date.UTC(y, m - 1, d);
  const first = utc - tzOffset(utc, tz);
  return utc - tzOffset(first, tz);
}

/**
 * The programme as a timeline: one row per recorder, a day or a week. A slot opens its
 * details; a click or a drag on an empty place makes a new one there; the source's guide
 * shows above each row (a click on a programme makes a slot for it).
 */
type RecMode = 'continuous' | 'slots' | 'off';

/** A source's line: what it really recorded, with its stills on a grid of times; the holes in its loop. */
function Recorded({ spans, recId, from, to, now, loopFrom, tileMs, x }: { spans: { from: number; to: number }[]; recId: string; from: number; to: number; now: number; loopFrom: number; tileMs: number; x: (ms: number) => number }) {
  const end = Math.min(to, now);
  const shown = spans.filter((s) => s.to > from && s.from < end);
  const tiles: number[] = [];
  for (const s of shown) {
    for (let t0 = Math.floor(Math.max(s.from, from) / tileMs) * tileMs; t0 < Math.min(s.to, end); t0 += tileMs) {
      const mid = t0 + tileMs / 2;
      if (mid >= s.from && mid < s.to) tiles.push(t0);
    }
  }
  // The holes, within the loop (before, nothing is kept anyway).
  const holes: [number, number][] = [];
  let cursor = Math.max(from, loopFrom);
  for (const s of [...shown].sort((a, b) => a.from - b.from)) {
    if (s.from > cursor) holes.push([cursor, Math.min(s.from, end)]);
    cursor = Math.max(cursor, s.to);
  }
  if (cursor < end && shown.length) holes.push([cursor, end]);
  return (
    <>
      {shown.map((s) => (
        <span key={s.from} className="sc-cal-rec" style={{ left: `${Math.max(0, x(s.from))}%`, width: `${Math.min(100, x(Math.min(s.to, end))) - Math.max(0, x(s.from))}%` }} />
      ))}
      {tiles.map((t0) => (
        <img key={t0} className="sc-cal-tile" loading="lazy" alt="" style={{ left: `${x(t0)}%`, width: `${x(t0 + tileMs) - x(t0)}%` }} src={`/api/recorders/${recId}/frame.jpg?at=${Math.round(t0 + tileMs / 2)}&h=72&fast=1`} onError={(e) => (e.currentTarget.style.visibility = 'hidden')} />
      ))}
      {holes.filter(([a, b]) => b > a).map(([a, b]) => (
        <span key={a} className="sc-cal-hole" style={{ left: `${x(a)}%`, width: `${x(b) - x(a)}%` }} />
      ))}
    </>
  );
}
/** The stills' durations (as in Extraction): 30 s to 4 h. */
const TILE_STEPS = [30_000, 60_000, 120_000, 300_000, 600_000, 900_000, 1_800_000, 3_600_000, 7_200_000, 14_400_000];
const recMode = (r: Recorder): RecMode => (r.enabled ? 'continuous' : r.slotsOff ? 'off' : 'slots');

/** A source at the head of its line: its picture (OFF on it while it does not record), its logo
 * and name, and (as the role allows) REC | OFF: record it all the time, or stop. */
function SourceHead({ rec, canRec, mayStop }: { rec: Recorder; canRec: boolean; mayStop: (r: Recorder) => boolean }) {
  const st = useApp((s) => s.status[rec.id]);
  const [busy, setBusy] = useState(false);
  const setMode = async (mode: 'slots') => {
    if (mode === 'slots' && !(await confirmDialog({ title: t('Stop recording “{name}”?', { name: rec.name }), message: t('What it recorded stays in its loop.'), confirm: t('Stop'), danger: true }))) return;
    setBusy(true);
    try {
      await api('POST', `/api/recorders/${rec.id}/recording`, { mode });
    } catch (e) {
      if (!explainGuard(e)) showError(e);
    }
    setBusy(false);
  };
  const state = st?.state ?? 'off';
  const recording = state === 'recording';
  const [n, setN] = useState(0);
  const [img, setImg] = useState(true);
  useEffect(() => {
    if (!recording) return;
    const i = setInterval(() => {
      setN((x) => x + 1);
      setImg(true);
    }, 4000);
    return () => clearInterval(i);
  }, [recording]);
  return (
    <div className="sc-cal-name sc-cal-src">
      <div className="sc-cal-pic">
        {img && (recording || state === 'starting') ? <img src={`/api/recorders/${rec.id}/thumb.jpg?v=${n}`} alt="" onError={() => setImg(false)} /> : <div className="sc-pic lost" />}
        {!recording && !rec.enabled && state !== 'starting' && <span className="sc-pic-off">OFF</span>}
        {recording && <ContentFlags st={st} compact />}
        {state === 'nosignal' && (
          <span className="sc-cal-pic-msg">
            <WifiOff size={13} />
          </span>
        )}
      </div>
      <span className="sc-cal-info">
      <span className="sc-cal-info-name">
        <RecLogo rec={rec} size="sm" />
        <b title={rec.name}>{rec.name}</b>
      </span>
      {(rec.enabled ? mayStop(rec) : canRec) && (
        <span className="sc-recseg" role="group" aria-label="REC">
          <button type="button" className={`rec ${rec.enabled ? 'on' : ''}`} aria-pressed={rec.enabled} disabled={busy} data-tip={rec.enabled ? undefined : t('Record all the time: choose the loop')} onClick={(e) => !rec.enabled && recWithLoop(e, rec, (st?.measured ?? 0) / 1e6)}>
            <i className="dot" />
            REC
          </button>
          <button type="button" className={`off ${rec.enabled ? '' : 'on'}`} aria-pressed={!rec.enabled} disabled={busy} data-tip={rec.enabled ? t('Stop recording all the time') : undefined} onClick={() => rec.enabled && void setMode('slots')}>
            <i className="sq" />
            OFF
          </button>
        </span>
      )}
      </span>
    </div>
  );
}

/** A programme of the guide, under the pointer: its time (start, end, length) on the left, its
 * name, source and words on the right; one to come can be programmed (a click on it). */
function EpgCard({ g, r, x, y, timezone, admin }: { g: Programme; r: Recorder; x: number; y: number; timezone: string; admin: boolean }) {
  const ref = useRef<HTMLDivElement>(null);
  const [pos, setPos] = useState({ left: x + 14, top: y + 18 });
  useLayoutEffect(() => {
    const el = ref.current;
    if (!el) return;
    const w = el.offsetWidth;
    const h = el.offsetHeight;
    setPos({ left: Math.max(8, Math.min(innerWidth - w - 12, x + 14)), top: y + 18 + h > innerHeight - 8 ? y - h - 12 : y + 18 });
  }, [x, y]);
  const now = serverNow();
  const live = g.start <= now && now < g.end;
  const mins = Math.round((g.end - g.start) / 60_000);
  const length = mins >= 60 ? `${Math.floor(mins / 60)} h ${String(mins % 60).padStart(2, '0')}` : `${mins} min`;
  return createPortal(
    <div ref={ref} className="sc-epg-card" style={{ left: pos.left, top: pos.top }} role="tooltip">
      <div className="sc-epg-time">
        <b>{hhmm(g.start, timezone)}</b>
        <span>→ {hhmm(g.end, timezone)}</span>
        <em>{length}</em>
      </div>
      <div className="sc-epg-body">
        <strong>{g.name}</strong>
        <span className="sc-epg-sub">
          {r.name}
          {live ? ` · ${t('on now')}` : g.end <= now ? ` · ${t('over')}` : ''}
        </span>
        {g.text && <p>{g.text}</p>}
        {admin && g.end > now && (
          <span className="sc-epg-act">
            <Plus size={12} /> {t('Programme')}
          </span>
        )}
      </div>
    </div>,
    document.body,
  );
}

function Calendar({ recorders, slots, timezone, admin, canRec, mayStop, onOpen, onNew }: { recorders: Recorder[]; slots: Slot[]; timezone: string; admin: boolean; canRec: boolean; mayStop: (r: Recorder) => boolean; onOpen: (s: Slot) => void; onNew: (initial: Partial<Slot>) => void }) {
  // 6 h by default (from an hour ago), 12 h, a day, a week.
  const [view, setView] = useDisplayPref<View>('schedule.view', '6h');
  const [anchor, setAnchor] = useState(() => anchorFor(view, serverNow(), timezone));
  // The programme under the pointer: its card.
  const [tip, setTip] = useState<{ g: Programme; r: Recorder; x: number; y: number } | null>(null);
  const span = SPANS[view];
  const hourly = view !== 'week';
  const from = anchor;
  const to = anchor + span;
  const [occ, setOcc] = useState<Occurrence[]>([]);
  const [guides, setGuides] = useState<Record<string, Programme[]>>({});
  const [now, setNow] = useState(serverNow());
  useEffect(() => {
    const i = setInterval(() => setNow(serverNow()), 30_000);
    return () => clearInterval(i);
  }, []);
  useEffect(() => {
    let dead = false;
    api<{ occurrences: Occurrence[] }>('GET', `/api/schedules/occurrences?from=${from}&to=${to}`).then(
      (o) => !dead && setOcc(o.occurrences),
      () => {},
    );
    return () => {
      dead = true;
    };
  }, [from, to, slots]);
  const ids = recorders.map((r) => r.id).join(',');
  // What each source really recorded (as in Extraction), asked again every 30 s.
  const [cover, setCover] = useState<Record<string, { from: number; to: number }[]>>({});
  useEffect(() => {
    let dead = false;
    const load = () =>
      void Promise.all(
        recorders.map((r) =>
          api<{ spans: { from: number; to: number }[] }>('GET', `/api/recorders/${r.id}/coverage`).then(
            (c) => [r.id, c.spans] as const,
            () => [r.id, [] as { from: number; to: number }[]] as const,
          ),
        ),
      ).then((all) => !dead && setCover(Object.fromEntries(all)));
    load();
    const i = setInterval(load, 30_000);
    return () => {
      dead = true;
      clearInterval(i);
    };
  }, [ids]); // eslint-disable-line react-hooks/exhaustive-deps
  // The lanes' width: stills about 68 px wide, on a grid of times (the same ones from one draw to the next).
  const grid = useRef<HTMLDivElement>(null);
  const [laneW, setLaneW] = useState(1000);
  useEffect(() => {
    const el = grid.current;
    if (!el) return;
    const ro = new ResizeObserver(() => setLaneW(Math.max(200, el.clientWidth - 300)));
    ro.observe(el);
    return () => ro.disconnect();
  }, []);
  const tileMs = TILE_STEPS.find((s) => (s / span) * laneW >= 64) ?? TILE_STEPS[TILE_STEPS.length - 1];
  useEffect(() => {
    let dead = false;
    void Promise.all(
      recorders.map((r) =>
        api<{ events: Programme[] }>('GET', `/api/recorders/${r.id}/epg?from=${from}&to=${to}`).then(
          (g) => [r.id, g.events] as const,
          () => [r.id, [] as Programme[]] as const,
        ),
      ),
    ).then((all) => !dead && setGuides(Object.fromEntries(all)));
    return () => {
      dead = true;
    };
  }, [from, to, ids]); // eslint-disable-line react-hooks/exhaustive-deps

  const x = (ms: number) => ((ms - from) / span) * 100;
  // Midnights in view, the lines (every 30 min on 6 h, every hour on 12 h and a day) and their labels.
  const days: number[] = [];
  for (let d = startOfDay(from, timezone); d < to; d = startOfDay(d + DAY + 3_600_000, timezone)) if (d >= from) days.push(d);
  const weekDays = view === 'week' ? Array.from({ length: 7 }, (_, i) => startOfDay(from + i * DAY + DAY / 2, timezone)) : [];
  const every = view === '6h' ? 1_800_000 : 3_600_000;
  const ticks = hourly ? Array.from({ length: Math.round(span / every) }, (_, i) => from + i * every) : weekDays.flatMap((d) => [6, 12, 18].map((h) => d + h * 3_600_000));
  const label = view === 'day'
    ? day(from, timezone, locale())
    : hourly
    ? `${day(from, timezone, locale())} ${hhmm(from, timezone)} → ${dateIn(to - 1, timezone) === dateIn(from, timezone) ? '' : `${day(to - 1, timezone, locale())} `}${hhmm(to, timezone)}`
    : `${day(from, timezone, locale())} → ${day(to - DAY / 2, timezone, locale())}`;
  const go = (dir: number) => setAnchor((a) => (view === 'week' || view === 'day' ? startOfDay(a + dir * span + DAY / 2, timezone) : a + dir * span));
  const pick = (v: View) => {
    setAnchor(anchorFor(v, now >= from && now < to ? now : from + span / 2, timezone));
    setView(v);
  };

  // A drag on an empty place: the new slot's hours (5 min steps).
  const [sel, setSel] = useState<{ recorder: string; a: number; b: number } | null>(null);
  const drag = useRef<{ recorder: string; x0: number; a: number; lane: DOMRect } | null>(null);
  const snap = (ms: number, step: number) => Math.round(ms / step) * step;
  const at = (clientX: number, r: DOMRect) => from + Math.min(1, Math.max(0, (clientX - r.left) / r.width)) * span;
  const step = hourly ? 5 * 60_000 : 15 * 60_000;
  const open = (recorder: string, a: number, b: number, name = '') => onNew({ recorder, name, date: dateIn(a, timezone), days: [], start: hhmm(a, timezone), end: hhmm(b, timezone) });

  return (
    <div className="sc-cal">
      <div className="sc-cal-head">
        <h1 className="sc-cal-title">{t('Schedule')}</h1>
        <button className="icon-btn ghost" title={t('Before')} onClick={() => go(-1)}>
          <ChevronLeft size={16} />
        </button>
        <button className="btn small" onClick={() => setAnchor(anchorFor(view, serverNow(), timezone))}>
          {t('Now')}
        </button>
        <button className="icon-btn ghost" title={t('After')} onClick={() => go(1)}>
          <ChevronRight size={16} />
        </button>
        <strong className="sc-cal-range">{label}</strong>
        <div className="spacer" />
        {/* What the marks in the rows are. */}
        <span className="sc-cal-key">
          <span><i className="rec" />{t('Recorded')}</span>
          <span><i className="hole" />{t('Not recorded')}</span>
          <span><i className="slot" />{t('Programmed')}</span>
        </span>
        <Segmented
          value={view}
          options={[
            { value: '6h', label: '6 h' },
            { value: '12h', label: '12 h' },
            { value: 'day', label: t('Day') },
            { value: 'week', label: t('Week') },
          ]}
          onChange={pick}
        />
      </div>
      <div className="sc-cal-grid" ref={grid}>
        <div className="sc-cal-row sc-cal-axis">
          <div className="sc-cal-name" />
          <div className="sc-cal-lane">
            {view === 'week'
              ? weekDays.map((d) => (
                  <span key={d} className={`sc-cal-day ${startOfDay(now, timezone) === d ? 'today' : ''}`} style={{ left: `${x(d)}%`, width: `${(DAY / span) * 100}%` }}>
                    {day(d, timezone, locale())}
                  </span>
                ))
              : ticks
                  .filter((tk) => hhmm(tk, timezone).endsWith(':00'))
                  .map((tk) => (
                    <span key={tk} className={`sc-cal-hour ${days.includes(tk) && tk > from ? 'midnight' : ''}`} style={{ left: `${x(tk)}%` }}>
                      {/* A new day inside the view shows its date (the first one is in the title). */}
                      {days.includes(tk) && tk > from ? day(tk, timezone, locale()) : view === '6h' ? hhmm(tk, timezone) : hhmm(tk, timezone).slice(0, 2)}
                    </span>
                  ))}
          </div>
        </div>
        {recorders.map((r) => {
          // Overlapping slots go one under the other (each keeps its own place to click).
          const mine = occ.filter((o) => o.recorder === r.id).sort((a, b) => a.start - b.start);
          const ends: number[] = [];
          const track = mine.map((o) => {
            let k = ends.findIndex((e) => e <= o.start);
            if (k < 0) k = ends.push(0) - 1;
            ends[k] = o.end;
            return k;
          });
          const tracks = Math.max(1, ends.length);
          const guide = guides[r.id] ?? [];
          return (
            <div key={r.id} className="sc-cal-row">
              <SourceHead rec={r} canRec={canRec} mayStop={mayStop} />
              <div
                className={`sc-cal-lane ${admin ? 'edit' : ''}`}
                onPointerDown={(e) => {
                  if (!admin || e.button !== 0 || (e.target as HTMLElement).closest('.sc-cal-slot, .sc-cal-prog')) return;
                  const lane = e.currentTarget.getBoundingClientRect();
                  e.currentTarget.setPointerCapture(e.pointerId);
                  const a = snap(at(e.clientX, lane), step);
                  drag.current = { recorder: r.id, x0: e.clientX, a, lane };
                  setSel({ recorder: r.id, a, b: a });
                }}
                onPointerMove={(e) => {
                  const d = drag.current;
                  if (!d) return;
                  const b = snap(at(e.clientX, d.lane), step);
                  setSel({ recorder: d.recorder, a: Math.min(d.a, b), b: Math.max(d.a, b) });
                }}
                onPointerUp={(e) => {
                  const d = drag.current;
                  drag.current = null;
                  setSel(null);
                  if (!d) return;
                  const b = snap(at(e.clientX, d.lane), step);
                  // A click: one hour from there (on the quarter); a drag: what was drawn.
                  if (Math.abs(e.clientX - d.x0) < 5 || b === d.a) {
                    const a = snap(at(d.x0, d.lane) - 7.5 * 60_000, 15 * 60_000);
                    open(d.recorder, a, a + 3_600_000);
                  } else open(d.recorder, Math.min(d.a, b), Math.max(d.a, b));
                }}
                onPointerCancel={() => {
                  drag.current = null;
                  setSel(null);
                }}
              >
                {/* Time gone by: shaded. */}
                {now > from && <span className="sc-cal-past" style={{ width: `${Math.min(100, x(now))}%` }} />}
                {days.map((d) => (
                  <span key={d} className="sc-cal-line day" style={{ left: `${x(d)}%` }} />
                ))}
                {ticks.map((tk) => (
                  <span key={tk} className="sc-cal-line" style={{ left: `${x(tk)}%` }} />
                ))}
                {guide.map((g) => (
                  <button
                    key={`${g.id}-${g.start}`}
                    type="button"
                    className="sc-cal-prog"
                    style={{ left: `${Math.max(0, x(g.start))}%`, width: `${Math.max(0.2, Math.min(100, x(g.end)) - Math.max(0, x(g.start)))}%` }}
                    aria-label={`${g.name} ${hhmm(g.start, timezone)}–${hhmm(g.end, timezone)}`}
                    onPointerEnter={(e) => setTip({ g, r, x: e.clientX, y: e.clientY })}
                    onPointerMove={(e) => setTip({ g, r, x: e.clientX, y: e.clientY })}
                    onPointerLeave={() => setTip(null)}
                    onClick={() => admin && g.end > serverNow() && open(r.id, g.start, g.end, g.name)}
                  >
                    {g.name}
                  </button>
                ))}
                {/* What it really recorded (its stills, as in Extraction), the holes in its loop, and, recording
                    all the time, what is to come. */}
                <Recorded spans={cover[r.id] ?? []} recId={r.id} from={from} to={to} now={now} loopFrom={now - r.loopHours * 3_600_000} tileMs={tileMs} x={x} />
                {r.enabled && now < to && (
                  <span className="sc-cal-cont todo" style={{ left: `${Math.max(0, x(now))}%`, width: `${100 - Math.max(0, x(now))}%` }}>
                    <i className="sc-cal-pulse" />
                    {tc('cal', 'Continuous')}
                  </span>
                )}
                {mine.map((o, i) => {
                  const slot = slots.find((s) => s.id === o.slot);
                  const live = o.enabled !== false && o.start <= now && now < o.end;
                  // Recorded and still in the loop: it opens in Extraction.
                  const kept = o.enabled !== false && o.end < now && o.end > now - r.loopHours * 3_600_000;
                  return (
                    <button
                      key={`${o.slot}-${o.start}`}
                      type="button"
                      className={`sc-cal-slot ${live ? 'live' : ''} ${o.end < now ? 'past' : ''} ${slot?.date ? '' : 'repeat'} ${o.enabled === false || recMode(r) !== 'slots' ? 'paused' : ''}`}
                      style={{
                        left: `${Math.max(0, x(o.start))}%`,
                        width: `${Math.max(0.4, Math.min(100, x(o.end)) - Math.max(0, x(o.start)))}%`,
                        top: `calc(19px + (100% - 23px) * ${track[i] / tracks})`,
                        height: `calc((100% - 23px) / ${tracks} - ${tracks > 1 ? 2 : 0}px)`,
                        bottom: 'auto',
                      }}
                      title={`${o.name || t('Programmed slot')}\n${day(o.start, timezone, locale())} ${hhmm(o.start, timezone)}–${hhmm(o.end, timezone)}\n\n${live ? t('Click: watch it in Extraction') : kept ? t('Click: open it in Extraction') : t('Click: its details')}`}
                      onClick={() => {
                        // Recording now: the live in Extraction; recorded: its start there; to come: its details.
                        if (live) void navigate(`/extract/${o.recorder}`);
                        else if (kept) void navigate(`/extract/${o.recorder}?at=${o.start}`);
                        else if (slot) onOpen(slot);
                      }}
                    >
                      {live && <span className="sc-rec" />}
                      <span>{o.name || `${hhmm(o.start, timezone)}–${hhmm(o.end, timezone)}`}</span>
                    </button>
                  );
                })}
                {sel && sel.recorder === r.id && sel.b > sel.a && (
                  <span className="sc-cal-sel" style={{ left: `${x(sel.a)}%`, width: `${x(sel.b) - x(sel.a)}%` }}>
                    {hhmm(sel.a, timezone)}–{hhmm(sel.b, timezone)}
                  </span>
                )}
                {now >= from && now < to && <span className="sc-cal-now" style={{ left: `${x(now)}%` }} />}
              </div>
            </div>
          );
        })}
      </div>
      {tip && <EpgCard {...tip} timezone={timezone} admin={admin} />}
    </div>
  );
}

/**
 * The slots follow the server's clock: when the browser's differs (a server left in UTC),
 * say so, and offer to set the server to the browser's time zone.
 */
function TzWarning({ timezone, admin }: { timezone: string; admin: boolean }) {
  const toast = useApp((s) => s.toast);
  const mine = Intl.DateTimeFormat().resolvedOptions().timeZone;
  const now = serverNow();
  const [busy, setBusy] = useState(false);
  if (!mine || tzOffset(now, mine) === tzOffset(now, timezone)) return null;
  const use = async () => {
    setBusy(true);
    try {
      const sys = await api<{ config?: Record<string, unknown> }>('GET', '/api/system');
      if (!sys.config) return;
      await api('PUT', '/api/system/config', { ...sys.config, timezone: mine });
      toast(t('Time zone set: {tz}', { tz: mine }));
      // The server restarts its web part; the page reads the new clock.
      setTimeout(() => location.reload(), 2500);
    } catch (e) {
      showError(e);
      setBusy(false);
    }
  };
  return (
    <div className="sc-alert sc-tz-alert">
      <TriangleAlert size={14} />
      <span>
        {t('The slots follow the server’s clock: {server} ({sh}), while this computer is in {mine} ({mh}).', {
          server: timezone,
          sh: hhmm(now, timezone),
          mine,
          mh: hhmm(now, mine),
        })}
      </span>
      {admin && (
        <button className="btn primary small" disabled={busy} onClick={() => void use()}>
          {t('Set the server to {tz}', { tz: mine })}
        </button>
      )}
    </div>
  );
}
