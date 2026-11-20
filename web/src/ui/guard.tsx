/*
 * The resources' guard: when the server refuses a new recording or stream (it would take the
 * machine past its limits, Système › Général), a window says why, with the numbers, and what
 * can be done.
 */
import { ApiError, can, useApp } from '../store';
import { locale, t } from '../i18n';
import { alertDialog } from './dialogs';

export interface Guard {
  reason: 'cpu' | 'memory' | 'upload';
  what: 'recording' | 'stream';
  label: string;
  why: string;
  now: number;
  adds: number;
  limit: number;
  cores: number;
  machineCores: number;
}

export function guardOf(e: unknown): Guard | null {
  if (!(e instanceof ApiError) || e.status !== 409) return null;
  try {
    return (JSON.parse(e.message) as { guard?: Guard }).guard ?? null;
  } catch {
    return null;
  }
}

/** The guard's window when it refused (true), else false: the caller says the error its way. */
export function explainGuard(e: unknown): boolean {
  const g = guardOf(e);
  if (!g) return false;
  void alertDialog({ title: g.what === 'stream' ? t('This stream cannot start') : t('This recording cannot start'), message: <GuardMessage g={g} /> });
  return true;
}

const num = (v: number) => v.toLocaleString(locale(), { maximumFractionDigits: 1 });

function GuardMessage({ g }: { g: Guard }) {
  const admin = can.admin(useApp.getState().auth?.user);
  const upload = g.reason === 'upload';
  const unit = upload ? ' Mb/s' : ' %';
  const scale = upload ? Math.max(g.limit, g.now + g.adds) * 1.15 : 100;
  const pct = (v: number) => `${Math.min(100, (v / scale) * 100)}%`;
  const what = g.what === 'stream' ? t('This stream ({label})', { label: g.label }) : t('Recording {label}', { label: g.label });
  return (
    <div className="guard-msg">
      <p>
        {g.reason === 'cpu'
          ? t('The processor is used at {now} %. {what} would take about {adds} % more ({why}): past the limit of {limit} %.', { now: num(g.now), what, adds: num(g.adds), why: t(g.why), limit: num(g.limit) })
          : g.reason === 'memory'
            ? t('The memory is used at {now} %: past the limit of {limit} %. {what} would need more.', { now: num(g.now), limit: num(g.limit), what })
            : t('The streams already send {now} Mb/s. {what} would add {adds} Mb/s: past the limit of {limit} Mb/s.', { now: num(g.now), what, adds: num(g.adds), limit: num(g.limit) })}
      </p>
      <div className="guard-gauge" aria-hidden="true">
        <span className="now" style={{ width: pct(g.now) }} />
        {g.adds > 0 && <span className="adds" style={{ left: pct(g.now), width: `calc(${pct(g.now + g.adds)} - ${pct(g.now)})` }} />}
        <i style={{ left: pct(g.limit) }} />
      </div>
      <div className="guard-legend">
        <span>
          <b className="now" /> {t('now')} {num(g.now)}
          {unit}
        </span>
        {g.adds > 0 && (
          <span>
            <b className="adds" /> {t('would add')} {num(g.adds)}
            {unit}
          </span>
        )}
        <span>
          <b className="limit" /> {t('limit')} {num(g.limit)}
          {unit}
        </span>
      </div>
      <p className="muted">{t('What already runs goes on, and the programmed slots start all the same.')}</p>
      <p className="muted">
        {g.reason === 'upload'
          ? t('To start it: stop another stream, lower its bitrate, or raise the limit.')
          : g.what === 'stream'
            ? t('To start it: stop another stream or a recording, send a copy of the stream rather than encoding it again, or raise the limit.')
            : t('To start it: stop a stream or another recording, record by slots, or raise the limit.')}{' '}
        {admin ? t('The limits: Système › Général › Resources’ guard.') : t('The limits are set by an administrator.')}
      </p>
    </div>
  );
}
