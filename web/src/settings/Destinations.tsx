/*
 * Système › Flux › Destinations: where a source can be sent live (RTMP, RTMPS, SRT encoders and
 * platforms). Each card says what it sends now and at what bitrate; a destination is tried
 * (two seconds of black) before going live. Its secrets stay masked; what the operator may
 * set in the Streamer (a key given for each live, the address, the bitrate) is chosen here.
 */
import { useState } from 'react';
import { AlertTriangle, Check, Eye, EyeOff, MoreHorizontal, Pencil, Plug, Plus, Radio, Trash2, X } from 'lucide-react';
import { useShallow } from 'zustand/react/shallow';
import { api, serverNow, useApp } from '../store';
import { lang, locale, t, tn } from '../i18n';
import { PageHead } from '../pages/SidePage';
import { Dropdown } from '../ui/Dropdown';
import { NumberField, Segmented } from '../ui/fields';
import { confirmDialog } from '../ui/dialogs';
import { openContextMenu } from '../ui/ContextMenu';
import { Field } from './ui';
import { blankDestination, clockOf, MASKED, mbps, useDestinations, useStreams, type DestProtocol, type Destination, type StreamInfo } from '../streams';

export const PROTO_LABEL: Record<DestProtocol, string> = { rtmp: 'RTMP', rtmps: 'RTMPS', srt: 'SRT' };
const PLACEHOLDER: Record<DestProtocol, string> = { rtmp: 'rtmp://live.example.com/app', rtmps: 'rtmps://live.example.com:443/app', srt: 'srt://encoder.example.com:9000' };
const HEIGHTS = [0, 1080, 720, 540, 360];

/** The encoding, in a few words. */
export function encodingOf(d: Destination): string {
  if (d.encode === 'copy') return t('Copy of the stream');
  const h = d.height ? `${d.height}p` : t('as the source');
  return t('Encoded again {h} · {b}', { h, b: mbps(d.videoKbps ?? 6000, locale()) });
}

export function ProtoTag({ p }: { p: DestProtocol }) {
  return <span className={`io-proto ${p === 'srt' ? 'srt' : ''}`}>{PROTO_LABEL[p]}</span>;
}

/** A small line of a bitrate's last minutes. */
export function Spark({ values }: { values: number[] }) {
  const v = values.slice(-120);
  if (v.length < 2) return <svg className="io-spark" />;
  const max = Math.max(1, ...v) * 1.15;
  const pts = v.map((x, i) => `${((i / (v.length - 1)) * 100).toFixed(1)},${(24 - (x / max) * 22).toFixed(1)}`).join(' ');
  return (
    <svg className="io-spark" viewBox="0 0 100 24" preserveAspectRatio="none" aria-hidden="true">
      <polyline points={pts} fill="none" stroke="currentColor" strokeWidth="1.5" vectorEffect="non-scaling-stroke" />
    </svg>
  );
}

export function Destinations() {
  const [list] = useDestinations();
  const streams = useStreams(2000);
  const recorders = useApp((s) => s.recorders);
  const [edit, setEdit] = useState<Destination | null>(null);
  const live = streams.filter((s) => s.state === 'live').length;
  return (
    <>
      <PageHead title={t('Destinations')} sub={list ? `${tn(list.length, '{n} destination', '{n} destinations')} · ${tn(live, '{n} live', '{n} live')}` : undefined}>
        <button className="btn primary" onClick={() => setEdit(blankDestination())}>
          <Plus size={14} /> {t('New destination')}
        </button>
      </PageHead>
      {list && list.length === 0 && (
        <div className="set-card health-empty">
          <div className="empty big">
            <Radio size={22} />
            <p>{t('No destination yet: an encoder or a platform a source can be sent to (RTMP, RTMPS, SRT).')}</p>
          </div>
        </div>
      )}
      <div className="io-grid">
        {(list ?? []).map((d) => (
          <DestCard key={d.id} d={d} stream={streams.find((s) => s.destination === d.id)} sourceName={(id) => recorders.find((r) => r.id === id)?.name ?? id} onEdit={() => setEdit(d)} />
        ))}
      </div>
      {edit && <DestModal dest={edit} onClose={() => setEdit(null)} />}
    </>
  );
}

function DestCard({ d, stream, sourceName, onEdit }: { d: Destination; stream?: StreamInfo; sourceName: (id: string) => string; onEdit: () => void }) {
  const toast = useApp((s) => s.toast);
  const [test, setTest] = useState<{ busy: boolean; ok?: boolean; text?: string }>({ busy: false });
  const live = stream?.state === 'live';
  const run = async () => {
    setTest({ busy: true });
    try {
      const r = await api<{ ms: number; keyChecked: boolean }>('POST', `/api/destinations/${d.id}/test`, {});
      setTest({ busy: false, ok: true, text: r.keyChecked ? t('Accepted · {ms} ms', { ms: r.ms }) : t('The server answers ({ms} ms); the key is given for each live', { ms: r.ms }) });
    } catch (e) {
      setTest({ busy: false, ok: false, text: t((e as Error).message) });
    }
  };
  const remove = async () => {
    if (!(await confirmDialog({ title: t('Delete the destination “{name}”?', { name: d.name }), message: stream ? t('It is sent to now: that live stops.') : undefined, confirm: t('Delete'), danger: true }))) return;
    try {
      await api('DELETE', `/api/destinations/${d.id}`);
    } catch (e) {
      toast(t((e as Error).message), 'error');
    }
  };
  return (
    <article className="project-card io-dest">
      <div className="project-body">
        <div className="io-dest-top">
          <ProtoTag p={d.protocol} />
          <strong title={d.name}>{d.name}</strong>
          {stream ? (
            live ? (
              <span className="sc-state rec">
                <span className="sc-rec" />
                {t('LIVE')}
              </span>
            ) : (
              <span className="sc-state warn">{stream.state === 'connecting' ? t('Connecting…') : t('Reconnecting')}</span>
            )
          ) : (
            <span className="sc-state">{t('Free')}</span>
          )}
        </div>
        <code className="io-addr">
          {d.url}
          {d.protocol !== 'srt' && (d.keyLive ? `/${t('‹key of the live›')}` : d.key ? `/${MASKED}` : '')}
        </code>
        {stream && <Spark values={stream.history} />}
        <div className="io-metrics">
          {stream ? (
            <>
              <div>
                <span>{t('Sends')}</span>
                <b>{sourceName(stream.source)}</b>
              </div>
              <div>
                <span>{t('For')}</span>
                <b>{stream.liveSince ? clockOf(serverNow() - stream.liveSince) : '—'}</b>
              </div>
              <div>
                <span>{t('Bitrate')}</span>
                <b className="live">{mbps(stream.kbps, locale())}</b>
              </div>
            </>
          ) : (
            <div className="wide">
              <span>{t('Encoding')}</span>
              <b>{encodingOf(d)}</b>
            </div>
          )}
        </div>
        {stream?.message && (
          <span className="io-err">
            <AlertTriangle size={12} /> {t(stream.message)}
          </span>
        )}
        {test.text && <span className={`io-test-line ${test.ok ? 'ok' : 'bad'}`}>{test.ok ? <Check size={12} /> : <AlertTriangle size={12} />} {test.text}</span>}
        {(d.keyLive || d.exposeUrl || d.exposeBitrate) && (
          <span className="io-handed">
            {t('Set by the operator')}&nbsp;: {[d.keyLive && t('the key of each live'), d.exposeUrl && t('the address'), d.exposeBitrate && t('the bitrate')].filter(Boolean).join(', ')}
          </span>
        )}
      </div>
      <div className="sc-card-foot">
        <button className="btn small" disabled={test.busy || live} onClick={() => void run()} data-tip={t('Sends two seconds of black: the link and the key are checked')}>
          <Plug size={13} className={test.busy ? 'spin' : undefined} /> {test.busy ? t('Testing…') : t('Test')}
        </button>
        <div className="spacer" />
        <button className="icon-btn ghost" title={t('Edit')} onClick={onEdit}>
          <Pencil size={14} />
        </button>
        <button className="icon-btn ghost" title={t('More')} onClick={(e) => openContextMenu(e, [{ label: t('Delete'), icon: <Trash2 size={14} />, danger: true, onClick: () => void remove() }])}>
          <MoreHorizontal size={14} />
        </button>
      </div>
    </article>
  );
}

function DestModal({ dest, onClose }: { dest: Destination; onClose: () => void }) {
  const toast = useApp(useShallow((s) => s.toast));
  const [d, setD] = useState<Destination>(dest);
  const [busy, setBusy] = useState(false);
  const [show, setShow] = useState(false);
  const set = (p: Partial<Destination>) => setD((x) => ({ ...x, ...p }));
  const srt = d.protocol === 'srt';
  const save = async () => {
    setBusy(true);
    try {
      if (d.id) await api('PUT', `/api/destinations/${d.id}`, d);
      else await api('POST', '/api/destinations', d);
      onClose();
    } catch (e) {
      toast(t((e as Error).message), 'error');
    } finally {
      setBusy(false);
    }
  };
  const fr = lang() === 'fr';
  return (
    <div className="modal-back" onPointerDown={onClose}>
      <form
        className="modal xwide sc-rm"
        onPointerDown={(e) => e.stopPropagation()}
        onSubmit={(e) => {
          e.preventDefault();
          void save();
        }}
      >
        <header>
          <h2>{d.id ? t('Edit {name}', { name: dest.name }) : t('New destination')}</h2>
          <button type="button" className="icon-btn ghost" title={t('Close')} onClick={onClose}>
            <X size={16} />
          </button>
        </header>
        <div className="sc-rm-body">
          <div className="sc-rm-form set-grid two">
            <Field label={t('Name')}>
              <input value={d.name} autoFocus placeholder="YouTube" onChange={(e) => set({ name: e.target.value })} />
            </Field>
            <Field label={t('Type')} group>
              <Segmented value={d.protocol} options={(['rtmp', 'rtmps', 'srt'] as DestProtocol[]).map((p) => ({ value: p, label: PROTO_LABEL[p] }))} onChange={(p) => set({ protocol: p, url: d.url?.replace(/^[a-z]+:\/\//, `${p}://`) ?? '' })} />
            </Field>
            <Field label={t('Server address')} className="span2">
              <input value={d.url ?? ''} placeholder={PLACEHOLDER[d.protocol]} spellCheck={false} onChange={(e) => set({ url: e.target.value })} />
            </Field>
            {!srt && (
              <Field label={t('Stream key')} group className="span2" hint={d.keyLive ? t('The operator pastes it in the Streamer before going live (a platform gives one for each live). It is forgotten at the end of the live.') : t('Given by the platform; kept secret (masked everywhere).')}>
                <div className="io-keyrow">
                  <Segmented value={d.keyLive ? 'live' : 'fixed'} options={[{ value: 'fixed', label: t('Fixed (set here)') }, { value: 'live', label: t('Given for each live') }]} onChange={(v) => set({ keyLive: v === 'live' })} />
                  {!d.keyLive && (
                    <span className="io-key">
                      <input type={show ? 'text' : 'password'} value={d.key ?? ''} autoComplete="new-password" spellCheck={false} placeholder={t('Stream key')} onFocus={() => d.key === MASKED && set({ key: '' })} onChange={(e) => set({ key: e.target.value })} />
                      <button type="button" className="icon-btn" title={show ? t('Hide') : t('Show')} onClick={() => setShow(!show)}>
                        {show ? <EyeOff size={14} /> : <Eye size={14} />}
                      </button>
                    </span>
                  )}
                </div>
              </Field>
            )}
            {srt && (
              <>
                <Field label={t('Latency')}>
                  <NumberField value={d.latency ?? 500} min={20} max={8000} step={10} suffix="ms" onChange={(v) => set({ latency: v })} />
                </Field>
                <Field label={t('Passphrase')}>
                  <input type="password" value={d.passphrase ?? ''} placeholder={t('Optional')} autoComplete="new-password" onFocus={() => d.passphrase === MASKED && set({ passphrase: '' })} onChange={(e) => set({ passphrase: e.target.value })} />
                </Field>
              </>
            )}
            <Field label={t('Encoding')} group className="span2" hint={d.encode === 'copy' ? t('The source goes as it is, almost without computing. A source RTMP cannot carry (MPEG-2, HEVC) is encoded again.') : t('Encoded again in H.264 / AAC: about one processor core while it is live.')}>
              <Segmented value={d.encode} options={[{ value: 'copy', label: t('Copy of the stream') }, { value: 'h264', label: t('Encode again') }]} onChange={(v) => set({ encode: v })} />
            </Field>
            {d.encode === 'h264' && (
              <>
                <Field label={t('Picture')}>
                  <Dropdown value={String(d.height)} options={HEIGHTS.map((h) => ({ value: String(h), label: h ? `${h}p` : t('As the source') }))} onChange={(v) => set({ height: Number(v) })} />
                </Field>
                <Field label={t('Rate')} group>
                  <Segmented value={d.vbr ? 'vbr' : 'cbr'} options={[{ value: 'cbr', label: t('Constant (CBR)') }, { value: 'vbr', label: t('Variable (VBR)') }]} onChange={(v) => set({ vbr: v === 'vbr' })} />
                </Field>
                <Field label={t('Video bitrate')}>
                  <NumberField value={(d.videoKbps ?? 6000) / 1000} min={0.3} max={50} step={0.5} precision={1} suffix={fr ? 'Mb/s' : 'Mb/s'} onChange={(v) => set({ videoKbps: Math.round(v * 1000) })} />
                </Field>
              </>
            )}
            {!(srt && d.encode === 'copy') && (
              <Field label={t('Audio bitrate')} group>
                <Segmented value={String(d.audioKbps || 160)} options={[128, 160, 192, 256].map((k) => ({ value: String(k), label: `${k} kb/s` }))} onChange={(v) => set({ audioKbps: Number(v) })} />
              </Field>
            )}
            <div className="set-field span2">
              <div className="io-expose">
                <h4>{t('What the operator may set in the Streamer')}</h4>
                <p>{t('The rest is not shown to them: neither the address nor the fixed keys.')}</p>
                <div className="io-checks">
                  <label className="check">
                    <input type="checkbox" checked={d.keyLive && !srt} disabled /> {t('Stream key (given for each live)')}
                  </label>
                  <label className="check">
                    <input type="checkbox" checked={d.exposeUrl} onChange={(e) => set({ exposeUrl: e.target.checked })} /> {t('Server address')}
                  </label>
                  <label className="check">
                    <input type="checkbox" checked={d.exposeBitrate} onChange={(e) => set({ exposeBitrate: e.target.checked })} /> {t('Video bitrate')}
                  </label>
                </div>
              </div>
            </div>
          </div>
          <aside className="sc-rm-side">
            <section className="sc-rm-sec">
              <h3>{t('Test')}</h3>
              <p className="muted sc-rm-note">{d.id ? t('Save, then “Test” on its card: two seconds of black are sent, the link and the key are checked.') : t('Once created, “Test” on its card sends two seconds of black: the link and the key are checked.')}</p>
            </section>
            <section className="sc-rm-sec">
              <h3>{t('Resources')}</h3>
              <p className="muted sc-rm-note">{t('Copy: about 1 % of a core, and the source’s bitrate sent out. Encoded again: one to four cores (720p to 1080p) while it is live.')}</p>
            </section>
          </aside>
        </div>
        <footer>
          <button type="button" className="btn" onClick={onClose}>
            {t('Cancel')}
          </button>
          <button type="submit" className="btn primary" disabled={busy || !d.name.trim() || !d.url?.trim()}>
            {d.id ? t('Save') : (<><Plus size={14} /> {t('Create the destination')}</>)}
          </button>
        </footer>
      </form>
    </div>
  );
}
