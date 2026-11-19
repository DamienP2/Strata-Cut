/* Système › Serveur › Matériel: the capture cards (Blackmagic DeckLink), drawn with their inputs. */
import { useEffect, useState } from 'react';
import { Check, Cpu, Pencil, Plus, RefreshCw, Scissors, Search, Trash2, X } from 'lucide-react';
import { api, useApp } from '../store';
import { lang, t, tn } from '../i18n';
import { formatName } from '../tc';
import { navigate } from '../router';
import { PageHead } from '../pages/SidePage';
import { modelOf, type ConnKind, type Connector } from './decklinkModels';
import { Card } from './ui';

/** `absent`: an adopted input the driver did not list at the last search. */
type PortState = 'signal' | 'nosignal' | 'busy' | 'unknown' | 'absent' | 'checking';

interface Port {
  number: number;
  name: string;
  persistentId: number | null;
  state: PortState;
  /** "1080i50", "720p5994", "pal" (empty when not known). */
  format: string;
  recorder: { id: string; name: string; running: boolean; connection: string } | null;
}

interface CaptureCard {
  /** The card, for its name: its serial number, else its model. */
  key: string;
  /** The name given to it (empty: none). */
  name: string;
  model: string;
  serial: string;
  connector: 'sdi' | 'hdmi';
  /** Listed by the driver at the last search (an adopted card stays shown when it is not). */
  present: boolean;
  inputs: Port[];
}

interface Hardware {
  gstreamer: boolean;
  driver: boolean;
  driverVersion: string;
  cards: CaptureCard[];
}

const STATE_LABEL = (s: PortState) => ({ signal: t('Signal'), nosignal: t('No signal'), busy: t('Used by another program'), unknown: t('Starting…'), absent: t('Not detected'), checking: t('Checking…') })[s];

/** The last report, kept while the application is open: the page shows it at once when it comes back. */
let last: Hardware | null = null;

export function Hardware() {
  const toast = useApp((s) => s.toast);
  const [hw, setHwState] = useState<Hardware | null>(last);
  const [loading, setLoading] = useState(false);
  const [searching, setSearching] = useState(false);
  const [failed, setFailed] = useState(false);
  const setHw = (h: Hardware) => {
    last = h;
    setHwState(h);
  };
  const load = async (shown = false) => {
    if (shown) setLoading(true);
    try {
      setHw(await api<Hardware>('GET', '/api/system/hardware'));
      setFailed(false);
    } catch {
      setFailed(true);
    } finally {
      setLoading(false);
    }
  };
  // New cards are looked for only when asked (and once when the server starts).
  const search = async () => {
    setSearching(true);
    try {
      setHw(await api<Hardware>('POST', '/api/system/hardware/search'));
      setFailed(false);
    } catch (e) {
      toast((e as Error).message, 'error');
    } finally {
      setSearching(false);
    }
  };
  const forget = async (c: CaptureCard) => {
    if (!window.confirm(t('Forget the card “{name}”? It comes back if a search finds it again.', { name: c.name || c.model }))) return;
    try {
      await api('POST', '/api/system/hardware/forget', { key: c.key });
      await load();
    } catch (e) {
      toast((e as Error).message, 'error');
    }
  };
  // The inputs' signal changes: asked again every 3 s while the page is open. The server answers
  // at once: the inputs its recorders use as they are now, the free ones as last probed (it
  // probes them again in the background, about every 10 s: a probe holds the input a moment).
  useEffect(() => {
    void load();
    const i = setInterval(() => void load(), 3_000);
    return () => clearInterval(i);
  }, []);

  const inputs = hw?.cards.reduce((a, c) => a + c.inputs.length, 0) ?? 0;
  return (
    <>
      <PageHead
        title={t('Hardware')}
        sub={hw ? (hw.cards.length ? `${tn(hw.cards.length, '{n} capture card', '{n} capture cards')} · ${tn(inputs, '{n} input', '{n} inputs')}` : t('No capture card')) : undefined}
      >
        <button type="button" className="btn" disabled={loading || searching} onClick={() => void load(true)}>
          <RefreshCw size={14} className={loading ? 'spin' : ''} /> {t('Refresh')}
        </button>
        <button type="button" className="btn" disabled={searching} data-tip={t('Looks for cards added to the server (the cards found stay adopted)')} onClick={() => void search()}>
          <Search size={14} className={searching ? 'spin' : ''} /> {searching ? t('Searching…') : t('Search for cards')}
        </button>
      </PageHead>
      {!hw && !failed && <p className="muted">{t('Looking for the cards and the signal of their inputs…')}</p>}
      {failed && !hw && <p className="muted">{t('The server did not answer.')}</p>}
      {hw && (!hw.gstreamer || !hw.driver) && (
        <Card icon={<Cpu size={16} />} title={t('Blackmagic DeckLink cards')}>
          <div className="sc-space bad">
            <strong>{!hw.driver ? t('Blackmagic’s Desktop Video driver is not installed on the server.') : t('GStreamer’s DeckLink module is missing on the server.')}</strong>
            <span>{!hw.driver ? t('Download Desktop Video for Linux from Blackmagic’s site on your computer, copy it to the server (scp … :~/) and run the install script again: it installs it, step by step. In a virtual machine, the card must first be passed through to it (PCI passthrough).') : t('Run the install script again: it installs GStreamer and its modules.')}</span>
          </div>
        </Card>
      )}
      {hw && hw.gstreamer && hw.driver && hw.cards.length === 0 && (
        <Card icon={<Cpu size={16} />} title={t('Blackmagic DeckLink cards')}>
          <p className="muted">{t('The driver sees no card. Is it in place, and in a virtual machine passed through to it? After installing the driver, the server must be restarted.')} {t('Then use “Search for cards”.')}</p>
        </Card>
      )}
      {hw?.cards.map((c, i) => (
        <Card
          key={`${c.model}-${c.serial}-${i}`}
          className={c.present === false ? 'hw-absent' : undefined}
          icon={<Cpu size={16} />}
          title={<CardName card={c} onSaved={() => void load()} />}
          actions={
            <>
              {c.present === false && (
                <>
                  <span className="hw-badge" data-tip={t('Adopted: the driver did not list it at the last search. Its recorders wait for it.')}>
                    {t('Not detected')}
                  </span>
                  <button type="button" className="btn small ghost" onClick={() => void forget(c)}>
                    <Trash2 size={13} /> {t('Forget')}
                  </button>
                </>
              )}
              <span className="muted mono">{[c.serial && t('Serial {s}', { s: c.serial }), hw.driverVersion && `Desktop Video ${hw.driverVersion}`].filter(Boolean).join(' · ')}</span>
            </>
          }
        >
          <CardPicture card={c} />
          <div className="hw-ports">
            {c.inputs.map((p, k) => (
              <div key={p.number} className={`hw-port ${p.state}`}>
                <span className="hw-dot" />
                <span className="hw-port-name">
                  <b>{t('Input {n}', { n: k + 1 })}</b>
                  <span className="muted mono">
                    {p.name}
                    {p.persistentId != null && ` · ID ${p.persistentId.toString(16).toUpperCase()}`}
                  </span>
                </span>
                <span className="hw-port-state">
                  {STATE_LABEL(p.state)}
                  {p.state === 'signal' && p.format && <span className="mono"> · {formatName(p.format, lang() === 'fr')}</span>}
                </span>
                <span className="hw-port-rec">
                  {p.recorder ? (
                    <button type="button" className="btn small ghost" data-tip={t('Open it in Extraction')} onClick={() => void navigate(`/extract/${p.recorder!.id}`)}>
                      <Scissors size={13} /> {p.recorder.name}
                    </button>
                  ) : (
                    <button type="button" className="btn small" onClick={() => void navigate(`/recorders?decklink=${p.number}${p.format ? `&format=${p.format}` : ''}&name=${encodeURIComponent(`${c.model} ${k + 1}`)}`)}>
                      <Plus size={13} /> {t('Create a recorder')}
                    </button>
                  )}
                </span>
              </div>
            ))}
          </div>
        </Card>
      ))}
    </>
  );
}

/** A card's title: the name given to it (its model under it), renamed in place. */
function CardName({ card, onSaved }: { card: CaptureCard; onSaved: () => void }) {
  const toast = useApp((s) => s.toast);
  const model = modelOf(card.model, card.inputs.length, card.connector === 'hdmi').name;
  const [edit, setEdit] = useState<string | null>(null);
  const save = async () => {
    if (edit === null) return;
    try {
      await api('PUT', '/api/system/hardware/name', { key: card.key, name: edit.trim() });
      setEdit(null);
      onSaved();
    } catch (e) {
      toast(t((e as Error).message), 'error');
    }
  };
  if (edit !== null)
    return (
      <span className="hw-name-edit">
        <input
          autoFocus
          value={edit}
          maxLength={60}
          placeholder={model}
          aria-label={t('Name of the card')}
          onChange={(e) => setEdit(e.target.value)}
          onKeyDown={(e) => {
            e.stopPropagation();
            if (e.key === 'Enter') void save();
            if (e.key === 'Escape') setEdit(null);
          }}
        />
        <button type="button" className="icon-btn" data-tip={t('Save')} onClick={() => void save()}>
          <Check size={14} />
        </button>
        <button type="button" className="icon-btn ghost" data-tip={t('Cancel')} onClick={() => setEdit(null)}>
          <X size={14} />
        </button>
      </span>
    );
  return (
    <span className="hw-name">
      <span>{card.name || model}</span>
      {card.name && <span className="muted hw-name-model">{model}</span>}
      <button type="button" className="icon-btn ghost" data-tip={t('Name this card')} onClick={() => setEdit(card.name)}>
        <Pencil size={13} />
      </button>
    </span>
  );
}

/** What is printed on the bracket above each connector. */
const SILK: Record<ConnKind, string> = { sdi: 'SDI IN', 'sdi-out': 'SDI OUT', hdmi: 'HDMI IN', 'hdmi-out': 'HDMI OUT', ref: 'REF IN', optical: 'FIBRE', sfp: 'SFP', analog: 'AUDIO' };
/** The room a connector takes on the bracket (px). */
const pitch = (c: Connector) => (c.kind === 'analog' ? 96 : c.mini ? 54 : 74);

/**
 * The device's bracket, seen from the back of the server: the metal plate with its mounting tab,
 * the labels printed on it and its connectors as they look (BNC, mini-BNC, HDMI, SFP cage, sub-D).
 * Under the plate, each input's state: signal and format, no signal, busy.
 */
function CardPicture({ card }: { card: CaptureCard }) {
  const model = modelOf(card.model, card.inputs.length, card.connector === 'hdmi');
  const conns = model.connectors;
  const box = model.form === 'box';
  const tab = box ? 18 : 46;
  const inner = conns.reduce((a, c) => a + pitch(c), 0);
  // A full-height bracket is longer than its connectors need; a low-profile one is just enough.
  const plateW = inner + 28 + (model.form === 'full' ? 70 : 0);
  const w = tab + plateW + 24;
  const plateY = 18;
  const plateH = 74;
  const cy = plateY + plateH / 2 + 7;
  const perInput = (i: number) => conns.filter((c) => c.input === i).length;
  let x = tab + 14;
  const placed = conns.map((c) => {
    const cx = x + pitch(c) / 2;
    x += pitch(c);
    return { c, cx };
  });
  const first = (i: number) => placed.findIndex((p) => p.c.input === i);
  const middle = (i: number) => {
    const xs = placed.filter((p) => p.c.input === i).map((p) => p.cx);
    return (Math.min(...xs) + Math.max(...xs)) / 2;
  };
  return (
    <svg className="hw-card" viewBox={`0 0 ${w} 158`} role="img" aria-label={model.name} style={{ maxWidth: `${w * 1.25}px` }}>
      <defs>
        <linearGradient id="hw-steel" x1="0" y1="0" x2="0" y2="1">
          <stop offset="0" stopColor="#d7dae0" />
          <stop offset="0.5" stopColor="#bfc3cb" />
          <stop offset="1" stopColor="#a4a9b3" />
        </linearGradient>
        <linearGradient id="hw-anod" x1="0" y1="0" x2="0" y2="1">
          <stop offset="0" stopColor="#3b3f48" />
          <stop offset="1" stopColor="#24272e" />
        </linearGradient>
        <radialGradient id="hw-nickel" cx="0.4" cy="0.35" r="0.75">
          <stop offset="0" stopColor="#f2f3f5" />
          <stop offset="0.6" stopColor="#b9bdc5" />
          <stop offset="1" stopColor="#80858f" />
        </radialGradient>
      </defs>
      {/* The plate: a PCIe bracket (mounting tab on the left, tongue on the right) or a box's panel. */}
      {box ? (
        <rect x={6} y={plateY - 4} width={tab + plateW + 6} height={plateH + 8} rx={10} fill="url(#hw-anod)" stroke="#15171c" />
      ) : (
        <>
          <path d={`M6 ${plateY + 6} h${tab - 2} v${plateH - 12} h-${tab - 2} z`} fill="url(#hw-steel)" stroke="#8a8f99" />
          <rect x={18} y={plateY + plateH / 2 - 6} width={16} height={12} rx={6} fill="#5d626c" />
          <path d={`M${tab + 4} ${plateY} h${plateW} l14 ${plateH / 2 - 8} v16 l-14 ${plateH / 2 - 8} h-${plateW} z`} fill="url(#hw-steel)" stroke="#8a8f99" />
        </>
      )}
      {placed.map(({ c, cx }, k) => {
        const port = c.input !== undefined ? card.inputs[c.input] : undefined;
        const chosen = port?.recorder?.connection;
        const active = !!port && (perInput(c.input!) === 1 || !chosen || chosen === 'auto' || chosen === c.kind);
        const state = port ? (active ? port.state : 'idle') : 'other';
        const silk = c.input !== undefined && c.kind === 'sdi' ? `SDI ${c.input + 1}` : SILK[c.kind];
        return (
          <g key={k} className={`hw-in ${state}`}>
            <text className={`hw-silk ${box ? 'on-dark' : ''}`} x={cx} y={plateY + 14} textAnchor="middle">
              {silk}
            </text>
            <Conn c={c} x={cx} y={cy} />
            {port && <circle className="hw-led" cx={cx} cy={plateH + plateY + 18} r={3.5} />}
            {/* One caption per input, under its connector (between them when it has an SDI and an HDMI). */}
            {port && first(c.input!) === k && (
              <>
                <text className="hw-cap" x={middle(c.input!)} y={plateH + plateY + 38} textAnchor="middle">
                  {t('Input {n}', { n: c.input! + 1 })}
                </text>
                <text className={`hw-cap2 ${port.state}`} x={middle(c.input!)} y={plateH + plateY + 54} textAnchor="middle">
                  {port.state === 'signal' ? formatName(port.format, lang() === 'fr') || t('Signal') : port.state === 'nosignal' ? '—' : port.state === 'busy' ? t('Busy') : port.state === 'absent' ? '×' : '…'}
                </text>
              </>
            )}
          </g>
        );
      })}
    </svg>
  );
}

/** A connector drawn as it looks, centred on (x, y). */
function Conn({ c, x, y }: { c: Connector; x: number; y: number }) {
  if (c.kind === 'hdmi' || c.kind === 'hdmi-out')
    return (
      <g>
        <path d={`M${x - 16} ${y - 7} h32 v6 l-4 6 h-24 l-4 -6 z`} fill="url(#hw-nickel)" stroke="#6f747e" />
        <path d={`M${x - 12} ${y - 4} h24 v3 l-3 4 h-18 l-3 -4 z`} fill="#16181d" />
        <rect x={x - 9} y={y - 3} width={18} height={2.4} fill="#2c3038" />
        <circle className="hw-ring" cx={x} cy={y} r={21} />
      </g>
    );
  if (c.kind === 'sfp' || c.kind === 'optical')
    return (
      <g>
        <rect x={x - 15} y={y - 10} width={30} height={20} rx={1.5} fill="url(#hw-nickel)" stroke="#6f747e" />
        <rect x={x - 11} y={y - 6} width={22} height={12} fill="#121418" />
        <circle cx={x - 5} cy={y} r={2.2} fill="#3d424c" />
        <circle cx={x + 5} cy={y} r={2.2} fill="#3d424c" />
        <circle className="hw-ring" cx={x} cy={y} r={21} />
      </g>
    );
  if (c.kind === 'analog')
    return (
      <g>
        <path d={`M${x - 30} ${y - 9} h60 l-5 18 h-50 z`} fill="url(#hw-nickel)" stroke="#6f747e" />
        <path d={`M${x - 26} ${y - 6} h52 l-4 12 h-44 z`} fill="#16181d" />
        {Array.from({ length: 12 }, (_, i) => (
          <circle key={i} cx={x - 22 + i * 4} cy={y - 2} r={0.9} fill="#c9a227" />
        ))}
        {Array.from({ length: 11 }, (_, i) => (
          <circle key={i} cx={x - 20 + i * 4} cy={y + 2.5} r={0.9} fill="#c9a227" />
        ))}
        <circle cx={x - 38} cy={y} r={2.5} fill="#6f747e" />
        <circle cx={x + 38} cy={y} r={2.5} fill="#6f747e" />
      </g>
    );
  // BNC or mini-BNC (DIN 1.0/2.3): hex nut, barrel with its bayonet lugs, white insulator, gold pin.
  const k = c.mini ? 0.62 : 1;
  const hex = Array.from({ length: 6 }, (_, i) => {
    const a = (Math.PI / 3) * i + Math.PI / 6;
    return `${(x + Math.cos(a) * 17 * k).toFixed(1)},${(y + Math.sin(a) * 17 * k).toFixed(1)}`;
  }).join(' ');
  return (
    <g>
      <polygon points={hex} fill="url(#hw-nickel)" stroke="#6f747e" />
      <circle cx={x} cy={y} r={11.5 * k} fill="#cfd2d8" stroke="#7d828c" />
      {!c.mini && (
        <>
          <rect x={x - 14.5} y={y - 1.6} width={3.6} height={3.2} rx={1} fill="#9ea3ad" />
          <rect x={x + 10.9} y={y - 1.6} width={3.6} height={3.2} rx={1} fill="#9ea3ad" />
        </>
      )}
      <circle cx={x} cy={y} r={7.5 * k} fill="#2a2d33" />
      <circle cx={x} cy={y} r={5.2 * k} fill="#ece8dc" />
      <circle cx={x} cy={y} r={1.6 * k + 0.4} fill="#c9a227" />
      <circle className="hw-ring" cx={x} cy={y} r={21 * k} />
    </g>
  );
}
