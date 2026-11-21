/*
 * Programmation: the slots of the recorders in "programmed" mode. Such a recorder records
 * only during its slots (one day, or days of the week) and keeps its recordings some days,
 * instead of a rolling loop. A slot can be taken from the source's programme guide.
 */
import { useCallback, useEffect, useMemo, useRef, useState } from 'react';
import { useDisplayPref } from '../displayPrefs';
import { useShallow } from 'zustand/react/shallow';
import { CalendarClock, Check, ChevronLeft, ChevronRight, Pause, Play, Plus, Square, Trash2, TriangleAlert, Tv, WifiOff, X } from 'lucide-react';
import { Empty } from '../ui/Empty';
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
  const admin = can.has(user, 'schedule.edit');
  const [slots, setSlots] = useState<Slot[] | null>(null);
  const [only, setOnly] = useDisplayPref<string | null>('schedule.recorder', null);
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

  // Every recorder can be programmed: a stopped one records during its slots.
  const programmed = recorders;
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
            <Calendar recorders={programmed} slots={slots} timezone={timezone} admin={admin} onOpen={(slot) => setEdit({ slot })} onNew={(initial) => setEdit({ slot: null, initial })} />
          </>
        )}
      </div>
      <SlotPanel slots={slots} recorders={recorders} names={names} timezone={timezone} admin={admin} only={only} setOnly={setOnly} onOpen={(slot) => setEdit({ slot })} onNew={() => setEdit({ slot: null })} />
      {edit && <SlotModal slot={edit.slot} initial={edit.initial} recorders={programmed} preset={only} names={names} timezone={timezone} admin={admin} onClose={() => setEdit(null)} />}
    </div>
  );
}

/** "every day", "Mon → Fri", "Mon, Wed", "once". */
function repeatOf(s: Slot, names: string[]): string {
  if (s.date) return t('once');
  const d = [...s.days].sort();
  if (d.length === 7) return t('every day');
  if (d.length > 2 && d.every((x, i) => i === 0 || x === d[i - 1] + 1)) return `${names[d[0] - 1]} → ${names[d[d.length - 1] - 1]}`;
  return d.map((x) => names[x - 1]).join(', ');
}

/**
 * On the right: the slots programmed, by day (today, tomorrow, later), each with its state;
 * then those finished over the last days (what was recorded of each, to extract).
 */
function SlotPanel({ slots, recorders, names, timezone, admin, only, setOnly, onOpen, onNew }: { slots: Slot[]; recorders: Recorder[]; names: string[]; timezone: string; admin: boolean; only: string | null; setOnly: (id: string | null) => void; onOpen: (s: Slot) => void; onNew: () => void }) {
  const toast = useApp((s) => s.toast);
  const done = useFinished();
  const now = serverNow();
  const nameOf = (id: string) => recorders.find((r) => r.id === id)?.name ?? '—';
  const mine = (id: string) => !only || id === only;
  const coming = slots.filter((s) => s.next && mine(s.recorder)).sort((a, b) => a.next!.start - b.next!.start);
  const today = startOfDay(now, timezone);
  const dayOf = (ms: number) => Math.round((startOfDay(ms, timezone) - today) / DAY);
  const groups: [string, Slot[]][] = [];
  for (const s of coming) {
    const g = dayOf(s.next!.start);
    const label = g <= 0 ? t('Today') : g === 1 ? t('Tomorrow') : t('Later');
    const last = groups[groups.length - 1];
    if (last && last[0] === label) last[1].push(s);
    else groups.push([label, [s]]);
  }
  // Ended today (the days before: in the timeline, Day or Week).
  const finished = (done ?? []).filter((x) => mine(x.recorder) && x.start >= today);
  // The sources recording all the time (started here or in Sources).
  const status = useApp((s) => s.status);
  const continuous = recorders.filter((r) => r.enabled && mine(r.id));
  const count = (id: string) => slots.filter((s) => s.next && s.recorder === id).length + (recorders.find((r) => r.id === id)?.enabled ? 1 : 0);
  const stopContinuous = async (r: Recorder) => {
    try {
      await api('POST', `/api/recorders/${r.id}/recording`, { mode: 'slots' });
    } catch (e) {
      toast(t((e as Error).message), 'error');
    }
  };

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
    <aside className="sc-plan-slots">
      <div className="sc-plan-slots-head">
        <div className="sc-plan-slots-title">
          <h2>{t('Recordings')}</h2>
          {admin && recorders.length > 0 && (
            <button className="btn primary small" onClick={onNew}>
              <Plus size={13} /> {t('New recording')}
            </button>
          )}
        </div>
        {recorders.length > 1 && (
          <div className="asr-chips">
            <button className={`asr-chip ${only === null ? 'on' : ''}`} onClick={() => setOnly(null)}>
              {t('All')} <span className="asr-chip-n">{slots.filter((s) => s.next).length + recorders.filter((r) => r.enabled).length}</span>
            </button>
            {recorders.map((r) => (
              <button key={r.id} className={`asr-chip ${only === r.id ? 'on' : ''}`} onClick={() => setOnly(only === r.id ? null : r.id)}>
                {r.name} {count(r.id) > 0 && <span className="asr-chip-n">{count(r.id)}</span>}
              </button>
            ))}
          </div>
        )}
      </div>
      <div className="sc-plan-slots-list">
        {coming.length === 0 && finished.length === 0 && continuous.length === 0 && <Empty icon={CalendarClock} text={t('No slot yet')} />}
        {continuous.length > 0 && (
          <div className="sc-plan-slots-group">
            <div className="sc-plan-slots-day">{tc('rec', 'Continuous')}</div>
            {continuous.map((r) => {
              const rec = status[r.id]?.state === 'recording';
              return (
                <div key={r.id} className={`sc-slot-it ${rec ? 'live' : ''}`} onClick={() => void navigate(`/extract/${r.id}`)} title={t('Click: watch it in Extraction')}>
                  <span className="sc-slot-bar" />
                  <div className="sc-slot-txt">
                    <span className="sc-slot-hm">{rec && status[r.id]?.since ? t('since {h}', { h: hhmm(status[r.id]!.since, timezone) }) : '—'}</span>
                    <b>{r.name}</b>
                    <small>{t('Records all the time')}</small>
                  </div>
                  <div className="sc-slot-side">
                    {rec ? (
                      <span className="sc-pill live">
                        <span className="sc-rec" /> REC
                      </span>
                    ) : (
                      <span className="sc-pill">{t('No signal')}</span>
                    )}
                    {admin && (
                      <span className="sc-slot-acts" onClick={(e) => e.stopPropagation()}>
                        <button className="icon-btn ghost" data-tip={t('Stop')} aria-label={t('Stop')} onClick={() => void stopContinuous(r)}>
                          <Square size={12} />
                        </button>
                      </span>
                    )}
                  </div>
                </div>
              );
            })}
          </div>
        )}
        {groups.map(([label, list]) => (
          <div key={label} className="sc-plan-slots-group">
            <div className="sc-plan-slots-day">{label}</div>
            {list.map((s) => {
              const n = s.next!;
              const live = s.enabled && !!s.active;
              return (
                <div key={s.id} className={`sc-slot-it ${live ? 'live' : ''} ${s.enabled ? '' : 'paused'}`} onClick={() => (live ? void navigate(`/extract/${s.recorder}`) : onOpen(s))}>
                  <span className="sc-slot-bar" />
                  <div className="sc-slot-txt">
                    <span className="sc-slot-hm">
                      {dayOf(n.start) > 1 ? `${day(n.start, timezone, locale())} · ` : ''}
                      {hhmm(n.start, timezone)} → {hhmm(n.end, timezone)}
                    </span>
                    <b>{s.name || t('Programmed slot')}</b>
                    <small>
                      {nameOf(s.recorder)} · {repeatOf(s, names)}
                    </small>
                  </div>
                  <div className="sc-slot-side">
                    {!s.enabled ? (
                      <span className="sc-pill">{t('Paused')}</span>
                    ) : live ? (
                      <span className="sc-pill live">
                        <span className="sc-rec" /> {t('In progress')}
                      </span>
                    ) : (
                      <span className="sc-pill soon">{t('in {d}', { d: until(n.start - now) })}</span>
                    )}
                    {admin && (
                      <span className="sc-slot-acts" onClick={(e) => e.stopPropagation()}>
                        <button className="icon-btn ghost" data-tip={s.enabled ? t('Pause') : t('Resume')} aria-label={s.enabled ? t('Pause') : t('Resume')} onClick={() => void toggle(s)}>
                          {s.enabled ? <Pause size={13} /> : <Play size={13} />}
                        </button>
                        <button className="icon-btn ghost danger" data-tip={t('Delete')} aria-label={t('Delete')} onClick={() => void remove(s)}>
                          <Trash2 size={13} />
                        </button>
                      </span>
                    )}
                  </div>
                </div>
              );
            })}
          </div>
        ))}
        {finished.length > 0 && (
          <div className="sc-plan-slots-group">
            <div className="sc-plan-slots-day">{t('Finished today')}</div>
            {finished.map((x) => {
              const usable = x.share > 0;
              return (
                <div key={`${x.slot}-${x.start}`} className={`sc-slot-it done ${usable ? '' : 'gone'}`} onClick={() => usable && void navigate(`/extract/${x.recorder}?at=${Math.round(x.start)}`)} title={usable ? t('Click: open it in Extraction') : undefined}>
                  <span className="sc-slot-bar" />
                  <div className="sc-slot-txt">
                    <span className="sc-slot-hm">
                      {hhmm(x.start, timezone)} → {hhmm(x.end, timezone)}
                    </span>
                    <b>{x.name || t('Programmed slot')}</b>
                    <small>{nameOf(x.recorder)}</small>
                  </div>
                  <div className="sc-slot-side">
                    {x.share >= 0.98 ? (
                      <span className="sc-pill ok">
                        <Check size={11} /> {t('Recorded')}
                      </span>
                    ) : usable ? (
                      <span className="sc-pill soon">{t('Partly recorded {p} %', { p: Math.round(x.share * 100) })}</span>
                    ) : (
                      <span className="sc-pill">{x.gone ? t('No longer in the loop') : t('Not recorded')}</span>
                    )}
                  </div>
                </div>
              );
            })}
          </div>
        )}
      </div>
    </aside>
  );
}

/**
 * The slots that ended over the last 7 days, the newest first: what was recorded of each
 * (from the loop).
 */
function useFinished() {
  const [done, setDone] = useState<(Occurrence & { share: number; gone: boolean })[] | null>(null);
  useEffect(() => {
    let dead = false;
    const load = async () => {
      const now = serverNow();
      try {
        const o = await api<{ occurrences: Occurrence[] }>('GET', `/api/schedules/occurrences?from=${Math.round(now - 7 * DAY)}&to=${Math.round(now)}`);
        const ended = o.occurrences.filter((x) => x.end <= now && x.enabled !== false);
        // What each recorder holds now, to say what is left of each slot.
        const ids = [...new Set(ended.map((x) => x.recorder))];
        const spans = new Map<string, { from: number; to: number }[]>();
        await Promise.all(
          ids.map(async (id) => {
            try {
              spans.set(id, (await api<{ spans: { from: number; to: number }[] }>('GET', `/api/recorders/${id}/coverage`)).spans);
            } catch {
              spans.set(id, []);
            }
          }),
        );
        const list = ended.map((x) => {
          const sp = spans.get(x.recorder) ?? [];
          const got = sp.reduce((a, r) => a + Math.max(0, Math.min(x.end, r.to) - Math.max(x.start, r.from)), 0);
          const oldest = sp.length ? Math.min(...sp.map((r) => r.from)) : Infinity;
          return { ...x, share: got / (x.end - x.start), gone: oldest > x.start && got === 0 };
        });
        if (!dead) setDone(list.sort((a, b) => b.start - a.start));
      } catch {
        if (!dead) setDone([]);
      }
    };
    void load();
    const i = setInterval(() => void load(), 60_000);
    return () => {
      dead = true;
      clearInterval(i);
    };
  }, []);
  return done;
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
      toast(t((e as Error).message), 'error');
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

/** A source at the head of its line: its picture (REC on it while it records), its logo and name,
 * and (as the role allows) REC to record it all the time, or Stop. */
function SourceHead({ rec, admin }: { rec: Recorder; admin: boolean }) {
  const st = useApp((s) => s.status[rec.id]);
  const toast = useApp((s) => s.toast);
  const [busy, setBusy] = useState(false);
  const setMode = async (mode: 'continuous' | 'slots') => {
    setBusy(true);
    try {
      await api('POST', `/api/recorders/${rec.id}/recording`, { mode });
    } catch (e) {
      if (!explainGuard(e)) toast(t((e as Error).message), 'error');
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
        {recording && (
          <span className="sc-onpic tl">
            <span className="sc-rec" />
            REC
          </span>
        )}
        {recording && <ContentFlags st={st} compact />}
        {state === 'nosignal' && (
          <span className="sc-cal-pic-msg">
            <WifiOff size={13} />
          </span>
        )}
      </div>
      <span className="sc-cal-info-name">
        <RecLogo rec={rec} size="sm" />
        <b title={rec.name}>{rec.name}</b>
      </span>
      {admin &&
        (rec.enabled ? (
          <button type="button" className="sc-head-rec stop" disabled={busy} data-tip={t('Stop recording all the time')} onClick={() => void setMode('slots')}>
            <Square size={9} /> {t('Stop')}
          </button>
        ) : (
          <button type="button" className="sc-head-rec" disabled={busy} data-tip={t('Record all the time')} onClick={() => void setMode('continuous')}>
            <span className="sc-rec" /> REC
          </button>
        ))}
    </div>
  );
}

function Calendar({ recorders, slots, timezone, admin, onOpen, onNew }: { recorders: Recorder[]; slots: Slot[]; timezone: string; admin: boolean; onOpen: (s: Slot) => void; onNew: (initial: Partial<Slot>) => void }) {
  // 6 h by default (from an hour ago), 12 h, a day, a week.
  const [view, setView] = useDisplayPref<View>('schedule.view', '6h');
  const [anchor, setAnchor] = useState(() => anchorFor(view, serverNow(), timezone));
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
              <SourceHead rec={r} admin={admin} />
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
                    title={`${g.name}\n${hhmm(g.start, timezone)}–${hhmm(g.end, timezone)}${g.text ? `\n${g.text}` : ''}${admin ? `\n\n${t('Click: programme it')}` : ''}`}
                    onClick={() => admin && g.end > serverNow() && open(r.id, g.start, g.end, g.name)}
                  >
                    {g.name}
                  </button>
                ))}
                {/* What it really recorded (its stills, as in Extraction), the holes in its loop, and, recording
                    all the time, what is to come. */}
                <Recorded spans={cover[r.id] ?? []} recId={r.id} from={from} to={to} now={now} loopFrom={now - r.loopHours * 3_600_000} tileMs={tileMs} x={x} />
                {r.enabled && now < to && <span className="sc-cal-cont todo" style={{ left: `${Math.max(0, x(now))}%`, width: `${100 - Math.max(0, x(now))}%` }} />}
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
      toast(t((e as Error).message), 'error');
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
