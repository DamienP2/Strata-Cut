/*
 * The sends of the one signed in, at the bottom right of the Extraits page: each going (its
 * progress, its speed), waiting, or failed (why, and « Relancer »). Folded to its title.
 */
import { useState } from 'react';
import { ChevronDown, ChevronUp, RotateCcw, Upload, X } from 'lucide-react';
import { api } from '../store';
import { lang, t, tn } from '../i18n';
import { sendStateText, speedText, type SendJob } from '../sends';
import { showError } from './serverError';

export async function retrySend(j: SendJob) {
  try {
    await api('POST', `/api/sends/${j.id}/retry`);
  } catch (e) {
    showError(e);
  }
}

async function dismiss(j: SendJob) {
  try {
    await api('DELETE', `/api/sends/${j.id}`);
  } catch (e) {
    showError(e);
  }
}

export function SendsPanel({ sends }: { sends: SendJob[] }) {
  const [open, setOpen] = useState(true);
  const shown = sends.filter((j) => j.mine && j.state !== 'done').sort((a, b) => (a.state === 'running' ? -1 : b.state === 'running' ? 1 : a.at - b.at));
  if (shown.length === 0) return null;
  const going = shown.filter((j) => j.state === 'running').length;
  const waiting = shown.filter((j) => j.state === 'queued').length;
  const failed = shown.filter((j) => j.state === 'error').length;
  const fr = lang() === 'fr';
  return (
    <aside className="snd-panel">
      <header onClick={() => setOpen(!open)}>
        <Upload size={14} />
        <b>{t('Clip sends')}</b>
        <span className="muted">{[going && tn(going, '{n} going', '{n} going'), waiting && tn(waiting, '{n} waiting', '{n} waiting'), failed && tn(failed, '{n} failed', '{n} failed')].filter(Boolean).join(' · ')}</span>
        {open ? <ChevronDown size={14} /> : <ChevronUp size={14} />}
      </header>
      {open && (
        <div className="snd-panel-list">
          {shown.map((j) => (
            <div key={j.id} className={`snd-row ${j.state}`}>
              <div className="snd-row-t">
                <b title={j.clipName}>{j.clipName}</b>
                <small title={j.remote}>
                  → {j.targetName}
                  {j.state === 'error' ? ` · ${t(j.message || 'Failed')}` : j.state === 'queued' ? ` · ${t('waiting')}` : ` · ${j.remote}`}
                </small>
              </div>
              <div className="snd-row-r">
                {j.state === 'running' && (
                  <span className="mono">
                    {sendStateText(j)}
                    {j.speed > 0 && <small> {speedText(j.speed, fr)}</small>}
                  </span>
                )}
                {j.state === 'error' && (
                  <button type="button" className="btn small" onClick={() => void retrySend(j)}>
                    <RotateCcw size={12} /> {t('Retry')}
                  </button>
                )}
                <button type="button" className="icon-btn ghost" title={j.state === 'error' ? t('Dismiss') : t('Cancel')} onClick={() => void dismiss(j)}>
                  <X size={13} />
                </button>
              </div>
              {j.state === 'running' && (
                <div className="snd-bar">
                  <i className={j.progress == null ? 'busy' : undefined} style={{ width: j.progress == null ? '30%' : `${Math.round(j.progress * 100)}%` }} />
                </div>
              )}
            </div>
          ))}
        </div>
      )}
    </aside>
  );
}
