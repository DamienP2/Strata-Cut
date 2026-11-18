/*
 * Programmation: the slots of the recorders in "programmed" mode. Such a recorder records
 * only during its slots (one day, or days of the week) and keeps its recordings some days,
 * instead of a rolling loop. A slot can be taken from the source's programme guide.
 */
import { useCallback, useEffect, useMemo, useRef, useState } from 'react';
import { useShallow } from 'zustand/react/shallow';
import { CalendarClock, ChevronLeft, ChevronRight, Pencil, Plus, Search, Trash2, Tv, X } from 'lucide-react';
import { api, can, serverEvents, serverNow, useApp } from '../store';
import { locale, t, tn } from '../i18n';
import { navigate } from '../router';
import { clock, day } from '../tc';
import { confirmDialog } from '../ui/dialogs';
import { Dropdown } from '../ui/Dropdown';
import { Segmented } from '../ui/fields';
import { Field } from '../settings/ui';
import type { Programme, Recorder, Slot } from '../types';

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
  const admin = can.admin(user);
  const [slots, setSlots] = useState<Slot[] | null>(null);
  const [q, setQ] = useState('');
  const [only, setOnly] = useState<string | null>(null);
  const [edit, setEdit] = useState<{ slot: Slot | null; initial?: Partial<Slot> } | null>(null);
  const load = useCallback(async () => {
    try {
      setSlots((await api<{ slots: Slot[] }>('GET', '/api/schedules')).slots);
    } catch (e) {
      toast(t((e as Error).message), 'error');
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

  const programmed = recorders.filter((r) => r.scheduled);
  const nameOf = (id: string) => recorders.find((r) => r.id === id)?.name ?? '—';
  const names = useMemo(dayNames, []);
  if (!slots) return <div className="loading">{t('Loading…')}</div>;
  const shown = slots
    .filter((s) => (!only || s.recorder === only) && (!q || `${s.name} ${nameOf(s.recorder)}`.toLowerCase().includes(q.toLowerCase())))
    .sort((a, b) => (a.next?.start ?? Infinity) - (b.next?.start ?? Infinity));
  const upcoming = slots.filter((s) => s.next && s.enabled).sort((a, b) => a.next!.start - b.next!.start)[0];
  const now = serverNow();

  const when = (s: Slot) => (s.date ? day(new Date(`${s.date}T12:00:00Z`).getTime(), 'UTC', locale()) : s.days.length === 7 ? t('Every day') : s.days.map((d) => names[d - 1]).join(' · '));
  const remove = async (s: Slot) => {
    if (!(await confirmDialog({ title: t('Delete the slot “{name}”?', { name: s.name || nameOf(s.recorder) }), message: t('What it recorded stays.'), confirm: t('Delete'), danger: true }))) return;
    try {
      await api('DELETE', `/api/schedules/${s.id}`);
    } catch (e) {
      toast(t((e as Error).message), 'error');
    }
  };
  const toggle = async (s: Slot) => {
    try {
      await api('PUT', `/api/schedules/${s.id}`, { ...s, enabled: !s.enabled });
    } catch (e) {
      toast(t((e as Error).message), 'error');
    }
  };

  return (
    <div className="library">
      <div className="library-inner wide">
        <div className="settings-head">
          <div>
            <h1>{t('Schedule')}</h1>
            <p className="muted">
              {tn(slots.length, '{n} slot', '{n} slots')}
              {upcoming?.next && ` · ${t('next: {when}', { when: `${day(upcoming.next.start, timezone, locale())} ${hhmm(upcoming.next.start, timezone)} ${upcoming.name || nameOf(upcoming.recorder)}` })}`}
            </p>
          </div>
          <div className="spacer" />
          {admin && programmed.length > 0 && (
            <button className="btn primary" onClick={() => setEdit({ slot: null })}>
              <Plus size={14} /> {t('New slot')}
            </button>
          )}
        </div>

        {programmed.length === 0 ? (
          <div className="empty big">
            <CalendarClock size={22} />
            <p>{t('No recorder is programmed.')}</p>
            <p className="muted">{t('In Recorders, edit a recorder and choose “Programmed”: it then records only during its slots and keeps its recordings.')}</p>
            <button className="btn" onClick={() => void navigate('/')}>
              {t('Recorders')}
            </button>
          </div>
        ) : (
          <>
            <div className="media-toolbar">
              <label className="search">
                <Search size={14} />
                <input value={q} placeholder={t('Search slots')} onChange={(e) => setQ(e.target.value)} onKeyDown={(e) => e.stopPropagation()} />
              </label>
              <div className="asr-chips">
                <button className={`asr-chip ${only === null ? 'on' : ''}`} onClick={() => setOnly(null)}>
                  {t('All recorders')} <span className="asr-chip-n">{slots.length}</span>
                </button>
                {programmed.map((r) => (
                  <button key={r.id} className={`asr-chip ${only === r.id ? 'on' : ''}`} onClick={() => setOnly(r.id)}>
                    {r.name} <span className="asr-chip-n">{slots.filter((s) => s.recorder === r.id).length}</span>
                  </button>
                ))}
              </div>
            </div>
            <Calendar
              recorders={programmed.filter((r) => !only || r.id === only)}
              slots={slots}
              timezone={timezone}
              admin={admin}
              onOpen={(slot) => setEdit({ slot })}
              onNew={(initial) => setEdit({ slot: null, initial })}
            />
            {shown.length === 0 ? (
              <div className="empty big">
                <CalendarClock size={22} />
                <p>{q || only ? t('No match') : t('No slot yet')}</p>
              </div>
            ) : (
              <table className="user-table sc-slots">
                <thead>
                  <tr>
                    <th>{t('Name')}</th>
                    <th>{t('Recorder')}</th>
                    <th>{t('When')}</th>
                    <th>{t('Hours')}</th>
                    <th>{t('Next')}</th>
                    <th />
                  </tr>
                </thead>
                <tbody>
                  {shown.map((s) => (
                    <tr key={s.id} className={s.enabled ? '' : 'dim'}>
                      <td>
                        <strong>{s.name || '—'}</strong>
                      </td>
                      <td>{nameOf(s.recorder)}</td>
                      <td>{when(s)}</td>
                      <td className="num">
                        {s.start} → {s.end}
                      </td>
                      <td>
                        {!s.enabled ? (
                          <span className="muted">{t('Paused')}</span>
                        ) : s.active && s.next ? (
                          <span className="sc-state ok">
                            <span className="sc-rec" /> {t('Recording until {h}', { h: hhmm(s.next.end, timezone) })}
                          </span>
                        ) : s.next ? (
                          <span title={t('in {d}', { d: until(s.next.start - now) })}>
                            {day(s.next.start, timezone, locale())} {hhmm(s.next.start, timezone)}
                          </span>
                        ) : (
                          <span className="muted">{t('Over')}</span>
                        )}
                      </td>
                      <td style={{ textAlign: 'right', whiteSpace: 'nowrap' }}>
                        {admin && (
                          <>
                            <label className="check sc-slot-on" title={s.enabled ? t('Pause') : t('Resume')}>
                              <input type="checkbox" checked={s.enabled} onChange={() => void toggle(s)} />
                            </label>{' '}
                            <button className="icon-btn ghost" title={t('Edit')} onClick={() => setEdit({ slot: s })}>
                              <Pencil size={14} />
                            </button>{' '}
                            <button className="icon-btn ghost" title={t('Delete')} onClick={() => void remove(s)}>
                              <Trash2 size={14} />
                            </button>
                          </>
                        )}
                      </td>
                    </tr>
                  ))}
                </tbody>
              </table>
            )}
          </>
        )}
      </div>
      {edit && <SlotModal slot={edit.slot} initial={edit.initial} recorders={programmed} preset={only} names={names} timezone={timezone} admin={admin} onClose={() => setEdit(null)} />}
    </div>
  );
}

/** "2 h 05", "12 min", "3 j" */
function until(ms: number): string {
  const m = Math.max(0, Math.round(ms / 60_000));
  if (m < 60) return `${m} min`;
  if (m < 48 * 60) return `${Math.floor(m / 60)} h ${String(m % 60).padStart(2, '0')}`;
  return `${Math.round(m / 1440)} j`;
}

function SlotModal({ slot, initial, recorders, preset, names, timezone, admin, onClose }: { slot: Slot | null; initial?: Partial<Slot>; recorders: Recorder[]; preset: string | null; names: string[]; timezone: string; admin: boolean; onClose: () => void }) {
  const toast = useApp((s) => s.toast);
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
      (o) => setComing(o.occurrences.filter((x) => x.slot === slot.id).sort((a, b) => a.start - b.start).slice(0, 6)),
      () => setComing([]),
    );
  }, [slot]);
  const remove = async () => {
    if (!slot || !(await confirmDialog({ title: t('Delete the slot “{name}”?', { name: slot.name || t('Programmed slot') }), message: t('What it recorded stays.'), confirm: t('Delete'), danger: true }))) return;
    try {
      await api('DELETE', `/api/schedules/${slot.id}`);
      onClose();
    } catch (e) {
      toast(t((e as Error).message), 'error');
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

  const save = async () => {
    setBusy(true);
    try {
      if (slot) await api('PUT', `/api/schedules/${slot.id}`, s);
      else await api('POST', '/api/schedules', s);
      onClose();
    } catch (e) {
      toast(t((e as Error).message), 'error');
    } finally {
      setBusy(false);
    }
  };

  return (
    <div className="modal-back" onPointerDown={onClose}>
      <form className="modal wide" onPointerDown={(e) => e.stopPropagation()} onSubmit={(e) => { e.preventDefault(); void save(); }}>
        <header>
          <h2>{slot ? t('Edit the slot') : t('New slot')}</h2>
          <button type="button" className="icon-btn ghost" title={t('Close')} onClick={onClose}>
            <X size={16} />
          </button>
        </header>
        <div className="set-grid two">
          <Field label={t('Recorder')} group>
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
          <button type="submit" className="btn primary" disabled={!admin || busy || !s.recorder || (repeated && s.days.length === 0) || s.start === s.end}>
            {slot ? t('Save') : (
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
}

const DAY = 86_400_000;

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
function Calendar({ recorders, slots, timezone, admin, onOpen, onNew }: { recorders: Recorder[]; slots: Slot[]; timezone: string; admin: boolean; onOpen: (s: Slot) => void; onNew: (initial: Partial<Slot>) => void }) {
  const [view, setView] = useState<'day' | 'week'>('week');
  const [anchor, setAnchor] = useState(() => startOfDay(serverNow(), timezone));
  const span = view === 'day' ? DAY : 7 * DAY;
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
  const days = Array.from({ length: view === 'day' ? 1 : 7 }, (_, i) => startOfDay(from + i * DAY + DAY / 2, timezone));
  const ticks = view === 'day' ? Array.from({ length: 24 }, (_, h) => from + h * 3_600_000) : days.flatMap((d) => [6, 12, 18].map((h) => d + h * 3_600_000));
  const label = view === 'day' ? day(from, timezone, locale()) : `${day(from, timezone, locale())} → ${day(to - DAY / 2, timezone, locale())}`;

  // A drag on an empty place: the new slot's hours (5 min steps).
  const [sel, setSel] = useState<{ recorder: string; a: number; b: number } | null>(null);
  const drag = useRef<{ recorder: string; x0: number; a: number; lane: DOMRect } | null>(null);
  const snap = (ms: number, step: number) => Math.round(ms / step) * step;
  const at = (clientX: number, r: DOMRect) => from + Math.min(1, Math.max(0, (clientX - r.left) / r.width)) * span;
  const step = view === 'day' ? 5 * 60_000 : 15 * 60_000;
  const open = (recorder: string, a: number, b: number, name = '') => onNew({ recorder, name, date: dateIn(a, timezone), days: [], start: hhmm(a, timezone), end: hhmm(b, timezone) });

  return (
    <div className="sc-cal">
      <div className="sc-cal-head">
        <button className="icon-btn ghost" title={t('Before')} onClick={() => setAnchor((a) => startOfDay(a - span + DAY / 2, timezone))}>
          <ChevronLeft size={16} />
        </button>
        <button className="btn small" onClick={() => setAnchor(startOfDay(serverNow(), timezone))}>
          {t('Today')}
        </button>
        <button className="icon-btn ghost" title={t('After')} onClick={() => setAnchor((a) => startOfDay(a + span + DAY / 2, timezone))}>
          <ChevronRight size={16} />
        </button>
        <strong className="sc-cal-range">{label}</strong>
        <div className="spacer" />
        {admin && <span className="muted sc-cal-tip">{t('Click or drag on a row to programme; click a slot for its details.')}</span>}
        <Segmented
          value={view}
          options={[
            { value: 'day', label: t('Day') },
            { value: 'week', label: t('Week') },
          ]}
          onChange={(v) => setView(v)}
        />
      </div>
      <div className="sc-cal-grid">
        <div className="sc-cal-row sc-cal-axis">
          <div className="sc-cal-name" />
          <div className="sc-cal-lane">
            {view === 'week'
              ? days.map((d) => (
                  <span key={d} className={`sc-cal-day ${startOfDay(now, timezone) === d ? 'today' : ''}`} style={{ left: `${x(d)}%`, width: `${(DAY / span) * 100}%` }}>
                    {day(d, timezone, locale())}
                  </span>
                ))
              : ticks.map((tk) => (
                  <span key={tk} className="sc-cal-hour" style={{ left: `${x(tk)}%` }}>
                    {hhmm(tk, timezone).slice(0, 2)}
                  </span>
                ))}
          </div>
        </div>
        {recorders.map((r) => {
          const mine = occ.filter((o) => o.recorder === r.id);
          const guide = guides[r.id] ?? [];
          return (
            <div key={r.id} className="sc-cal-row">
              <div className="sc-cal-name" title={r.name}>
                {r.name}
              </div>
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
                    title={`${g.name}\n${hhmm(g.start, timezone)}–${hhmm(g.end, timezone)}${g.text ? `\n${g.text}` : ''}${admin ? `\n\n${t('Click: programme it')}` : ''}`}
                    onClick={() => admin && g.end > serverNow() && open(r.id, g.start, g.end, g.name)}
                  >
                    {g.name}
                  </button>
                ))}
                {mine.map((o) => {
                  const slot = slots.find((s) => s.id === o.slot);
                  const live = o.start <= now && now < o.end;
                  return (
                    <button
                      key={`${o.slot}-${o.start}`}
                      type="button"
                      className={`sc-cal-slot ${live ? 'live' : ''} ${o.end < now ? 'past' : ''} ${slot?.date ? '' : 'repeat'}`}
                      style={{ left: `${Math.max(0, x(o.start))}%`, width: `${Math.max(0.4, Math.min(100, x(o.end)) - Math.max(0, x(o.start)))}%` }}
                      title={`${o.name || t('Programmed slot')}\n${day(o.start, timezone, locale())} ${hhmm(o.start, timezone)}–${hhmm(o.end, timezone)}`}
                      onClick={() => slot && onOpen(slot)}
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
    </div>
  );
}
