/* Système › Serveur › Matériel: the capture cards (Blackmagic DeckLink), drawn with their inputs. */
import { useEffect, useState } from 'react';
import { Cpu, Plus, RefreshCw, Scissors } from 'lucide-react';
import { api } from '../store';
import { lang, t, tn } from '../i18n';
import { formatName } from '../tc';
import { navigate } from '../router';
import { PageHead } from '../pages/SidePage';
import { Card } from './ui';

type PortState = 'signal' | 'nosignal' | 'busy' | 'unknown';

interface Port {
  number: number;
  name: string;
  persistentId: number | null;
  state: PortState;
  /** "1080i50", "720p5994", "pal" (empty when not known). */
  format: string;
  recorder: { id: string; name: string; running: boolean } | null;
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
          title={c.model}
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

/** The card drawn simply: its board, its connector and its bracket, each input with its signal. */
function CardPicture({ card }: { card: CaptureCard }) {
  const n = card.inputs.length;
  const step = 40;
  const h = Math.max(150, 56 + n * step);
  const boardH = h - 34;
  const first = 12 + boardH / 2 - ((n - 1) * step) / 2;
  const hdmi = card.connector === 'hdmi';
  return (
    <svg className="hw-card" viewBox={`0 0 640 ${h}`} role="img" aria-label={card.model}>
      {/* The board, its chip and its PCI Express connector. */}
      <rect className="hw-board" x={16} y={12} width={340} height={boardH} rx={8} />
      <rect className="hw-chip" x={140} y={12 + boardH / 2 - 26} width={64} height={52} rx={4} />
      <text className="hw-chip-t" x={172} y={12 + boardH / 2 + 4} textAnchor="middle">
        BMD
      </text>
      {Array.from({ length: 20 }, (_, i) => (
        <rect key={i} className="hw-finger" x={60 + i * 9} y={12 + boardH} width={6} height={12} rx={1} />
      ))}
      <text className="hw-model" x={28} y={32}>
        {card.model}
      </text>
      {/* The bracket and the inputs on it. */}
      <rect className="hw-bracket" x={356} y={4} width={18} height={boardH + 16} rx={2} />
      {card.inputs.map((p, k) => {
        const y = first + k * step;
        return (
          <g key={p.number} className={`hw-in ${p.state}`}>
            {hdmi ? (
              <path className="hw-conn" d={`M352 ${y - 7} h26 l-4 14 h-18 z`} />
            ) : (
              <>
                <circle className="hw-bnc" cx={365} cy={y} r={13} />
                <circle className="hw-conn" cx={365} cy={y} r={8} />
                <circle className="hw-pin" cx={365} cy={y} r={2.5} />
              </>
            )}
            <line className="hw-lead" x1={382} y1={y} x2={398} y2={y} />
            <text className="hw-label" x={404} y={y - 3}>
              {t('Input {n}', { n: k + 1 })}
            </text>
            <text className="hw-sub" x={404} y={y + 13}>
              {[p.state === 'signal' && p.format ? formatName(p.format, lang() === 'fr') : STATE_LABEL(p.state), p.recorder?.name].filter(Boolean).join(' · ')}
            </text>
          </g>
        );
      })}
    </svg>
  );
}
