/* Système › Serveur › Matériel: the capture cards (Blackmagic DeckLink), drawn with their inputs. */
import { useEffect, useState } from 'react';
import { Cpu, Plus, RefreshCw, Scissors } from 'lucide-react';
import { api } from '../store';
import { lang, t, tn } from '../i18n';
import { formatName } from '../tc';
import { navigate } from '../router';
import { PageHead } from '../pages/SidePage';
import { modelOf, type ConnKind } from './decklinkModels';
import { Card } from './ui';

type PortState = 'signal' | 'nosignal' | 'busy' | 'unknown';

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
  model: string;
  serial: string;
  connector: 'sdi' | 'hdmi';
  inputs: Port[];
}

interface Hardware {
  gstreamer: boolean;
  driver: boolean;
  driverVersion: string;
  cards: CaptureCard[];
}

const STATE_LABEL = (s: PortState) => ({ signal: t('Signal'), nosignal: t('No signal'), busy: t('Used by another program'), unknown: t('Starting…') })[s];

export function Hardware() {
  const [hw, setHw] = useState<Hardware | null>(null);
  const [loading, setLoading] = useState(false);
  const [failed, setFailed] = useState(false);
  const load = async () => {
    setLoading(true);
    try {
      setHw(await api<Hardware>('GET', '/api/system/hardware'));
      setFailed(false);
    } catch {
      setFailed(true);
    } finally {
      setLoading(false);
    }
  };
  // The inputs' signal changes: asked again every 15 s while the page is open.
  useEffect(() => {
    void load();
    const i = setInterval(() => void load(), 15_000);
    return () => clearInterval(i);
  }, []);

  const inputs = hw?.cards.reduce((a, c) => a + c.inputs.length, 0) ?? 0;
  return (
    <>
      <PageHead
        title={t('Hardware')}
        sub={hw ? (hw.cards.length ? `${tn(hw.cards.length, '{n} capture card', '{n} capture cards')} · ${tn(inputs, '{n} input', '{n} inputs')}` : t('No capture card')) : undefined}
      >
        <button type="button" className="btn" disabled={loading} onClick={() => void load()}>
          <RefreshCw size={14} className={loading ? 'spin' : ''} /> {t('Refresh')}
        </button>
      </PageHead>
      {!hw && !failed && <p className="muted">{t('Looking for the cards and the signal of their inputs…')}</p>}
      {failed && !hw && <p className="muted">{t('The server did not answer.')}</p>}
      {hw && (!hw.gstreamer || !hw.driver) && (
        <Card icon={<Cpu size={16} />} title={t('Blackmagic DeckLink cards')}>
          <div className="sc-space bad">
            <strong>{!hw.driver ? t('Blackmagic’s Desktop Video driver is not installed on the server.') : t('GStreamer’s DeckLink module is missing on the server.')}</strong>
            <span>{!hw.driver ? t('Install Desktop Video (Blackmagic’s site, Support page), restart the server, then run the install script again. In a virtual machine, the card must be passed through to it (PCI passthrough).') : t('Run the install script again: it installs GStreamer and its modules.')}</span>
          </div>
        </Card>
      )}
      {hw && hw.gstreamer && hw.driver && hw.cards.length === 0 && (
        <Card icon={<Cpu size={16} />} title={t('Blackmagic DeckLink cards')}>
          <p className="muted">{t('The driver sees no card. Is it in place, and in a virtual machine passed through to it? After installing the driver, the server must be restarted.')}</p>
        </Card>
      )}
      {hw?.cards.map((c, i) => (
        <Card
          key={`${c.model}-${c.serial}-${i}`}
          icon={<Cpu size={16} />}
          title={modelOf(c.model, c.inputs.length, c.connector === 'hdmi').name}
          actions={<span className="muted mono">{[c.serial && t('Serial {s}', { s: c.serial }), hw.driverVersion && `Desktop Video ${hw.driverVersion}`].filter(Boolean).join(' · ')}</span>}
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

const CONN_LABEL: Record<ConnKind, string> = { sdi: 'SDI', 'sdi-out': 'SDI OUT', hdmi: 'HDMI', 'hdmi-out': 'HDMI OUT', ref: 'REF', optical: 'OPTICAL', sfp: 'SFP', analog: 'ANALOG' };

/**
 * The device drawn simply, as its model is made: a full-height or low-profile PCIe card (board,
 * PCI Express connector, bracket) or an external box, with its connectors in order. Each input
 * connector shows its sub-device's signal; outputs, reference and analogue stay grey.
 */
function CardPicture({ card }: { card: CaptureCard }) {
  const model = modelOf(card.model, card.inputs.length, card.connector === 'hdmi');
  const conns = model.connectors;
  const step = 36;
  const box = model.form === 'box';
  const low = model.form === 'low';
  const bodyH = Math.max(low ? 140 : 150, 40 + conns.length * step);
  const h = bodyH + (box ? 16 : 34);
  const first = 12 + bodyH / 2 - ((conns.length - 1) * step) / 2;
  const boardW = low ? 250 : 340;
  const boardX = 356 - boardW;
  // The input connectors of a sub-device: several (SDI and HDMI of a Mini Recorder) say which one.
  const perInput = (i: number) => conns.filter((c) => c.input === i).length;
  return (
    <svg className="hw-card" viewBox={`0 0 680 ${h}`} role="img" aria-label={model.name}>
      {box ? (
        <>
          <rect className="hw-box" x={boardX} y={12} width={boardW} height={bodyH} rx={14} />
          <text className="hw-model" x={boardX + 18} y={36}>
            {model.name}
          </text>
          <text className="hw-chip-t" x={boardX + 18} y={12 + bodyH - 16}>
            Thunderbolt / USB
          </text>
        </>
      ) : (
        <>
          <rect className="hw-board" x={boardX} y={12 + (low ? 20 : 0)} width={boardW} height={bodyH - (low ? 20 : 0)} rx={8} />
          {/* The chip at the bottom right, under the model's name. */}
          <rect className="hw-chip" x={boardX + boardW - 104} y={12 + bodyH - 72} width={64} height={52} rx={4} />
          <text className="hw-chip-t" x={boardX + boardW - 72} y={12 + bodyH - 42} textAnchor="middle">
            BMD
          </text>
          {Array.from({ length: low ? 12 : 20 }, (_, i) => (
            <rect key={i} className="hw-finger" x={boardX + 40 + i * 9} y={12 + bodyH} width={6} height={12} rx={1} />
          ))}
          <text className="hw-model" x={boardX + 12} y={32 + (low ? 20 : 0)}>
            {model.name}
          </text>
        </>
      )}
      {!box && <rect className="hw-bracket" x={356} y={4} width={18} height={bodyH + 16} rx={2} />}
      {conns.map((c, k) => {
        const y = first + k * step;
        const port = c.input !== undefined ? card.inputs[c.input] : undefined;
        // An input used by a recorder on one of its connectors only: the other one stays grey.
        const chosen = port?.recorder?.connection;
        const active = port && (perInput(c.input!) === 1 || !chosen || chosen === 'auto' || chosen === c.kind);
        const state = port ? (active ? port.state : 'idle') : 'idle';
        const title = port ? `${t('Input {n}', { n: c.input! + 1 })}${perInput(c.input!) > 1 ? ` · ${CONN_LABEL[c.kind]}` : ''}` : CONN_LABEL[c.kind];
        return (
          <g key={k} className={`hw-in ${state} ${port ? '' : 'other'}`}>
            <Conn kind={c.kind} y={y} />
            <line className="hw-lead" x1={384} y1={y} x2={398} y2={y} />
            {port ? (
              <>
                <text className="hw-label" x={404} y={y - 3}>
                  {title}
                </text>
                <text className="hw-sub" x={404} y={y + 13}>
                  {[active ? (port.state === 'signal' && port.format ? formatName(port.format, lang() === 'fr') : STATE_LABEL(port.state)) : t('Not used'), active ? port.recorder?.name : undefined].filter(Boolean).join(' · ')}
                </text>
              </>
            ) : (
              <text className="hw-other" x={404} y={y + 4}>
                {title}
              </text>
            )}
          </g>
        );
      })}
    </svg>
  );
}

/** One connector, as it looks: BNC (SDI, reference), HDMI, optical / SFP cage, analogue breakout. */
function Conn({ kind, y }: { kind: ConnKind; y: number }) {
  const x = 365;
  if (kind === 'hdmi' || kind === 'hdmi-out') return <path className="hw-conn" d={`M${x - 13} ${y - 7} h26 l-4 14 h-18 z`} />;
  if (kind === 'optical' || kind === 'sfp')
    return (
      <>
        <rect className="hw-conn" x={x - 12} y={y - 8} width={24} height={16} rx={2} />
        <circle className="hw-pin" cx={x - 5} cy={y} r={2.5} />
        <circle className="hw-pin" cx={x + 5} cy={y} r={2.5} />
      </>
    );
  if (kind === 'analog') return <path className="hw-conn" d={`M${x - 15} ${y - 8} h30 l-4 16 h-22 z`} />;
  const r = kind === 'ref' ? 10 : 13;
  return (
    <>
      <circle className="hw-bnc" cx={x} cy={y} r={r} />
      <circle className="hw-conn" cx={x} cy={y} r={r - 5} />
      <circle className="hw-pin" cx={x} cy={y} r={2.5} />
    </>
  );
}
