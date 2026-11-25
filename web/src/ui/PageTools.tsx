/*
 * A web page source in the Streamer and the Extraction: its browser's bar (back, forward, reload or
 * stop, the page's title and address, a line while it loads) and two states. On air: the picture
 * as it goes out; a click in it (or P) takes the hand. Taking the hand: the whole page as it is now,
 * the mouse (its pointer as the page draws it), the wheel and the keyboard sent to it, and the
 * frame that goes on air laid over it, moved by its tab, sized by its corners, applied when asked.
 * Escape (or two minutes without a gesture) gives the hand back.
 */
import { useCallback, useEffect, useRef, useState, type CSSProperties, type ReactNode } from 'react';
import { ArrowLeft, ArrowRight, Globe, Hand, Move, RotateCw, X } from 'lucide-react';
import { t } from '../i18n';
import { api } from '../store';
import { showError } from './serverError';

export type PageMode = 'view' | 'pilot';

export interface Crop {
  x: number;
  y: number;
  w: number;
}

interface PageInfo {
  running: boolean;
  url: string;
  title?: string;
  loading?: boolean;
  crop: Crop | null;
  width: number;
  height: number;
}

/** The hand given back after this long without a gesture. */
const IDLE_MS = 120_000;

/** The page of a source: its address, its title, whether it loads, its frame (read every 2 s,
 * every 0.4 s while it loads). */
function usePageInfo(recId: string, on: boolean) {
  const [info, setInfo] = useState<PageInfo | null>(null);
  const [fast, setFast] = useState(false);
  const load = useCallback(async () => {
    try {
      setInfo(await api<PageInfo>('GET', `/api/pages/${recId}`));
    } catch {
      /* the next try */
    }
  }, [recId]);
  useEffect(() => {
    if (!on) return setInfo(null);
    void load();
    const i = window.setInterval(() => void load(), fast || info?.loading ? 400 : 2000);
    return () => clearInterval(i);
  }, [load, on, fast, info?.loading]);
  return { info, load, setFast };
}

const live = (recId: string) => `/api/pages/${recId}/live`;
const short = (url: string) => url.replace(/^https?:\/\/(www\.)?/, '').replace(/\/$/, '');

/**
 * The page's tools: `bar` above the picture, `catcher` just over the picture (on air: a click takes
 * the hand), `over` over everything (the page, while the hand is taken), `below` under it (while
 * the hand is taken); none for a source that is not a page (`on`). `can`: the hand may be taken
 * (the Extraction: at the live).
 */
export function usePageTools(recId: string, on: boolean, can: boolean): { mode: PageMode; bar: ReactNode; catcher: ReactNode; over: ReactNode; below: ReactNode } {
  const { info, load, setFast } = usePageInfo(recId, on);
  const [mode, setMode] = useState<PageMode>('view');
  const [typed, setTyped] = useState<string | null>(null);
  // An address asked for: shown until the page has it (never the old one in between).
  const [asked, setAsked] = useState<{ url: string; from: string; at: number } | null>(null);
  const [frame, setFrame] = useState<Crop | null>(null);
  const W = info?.width ?? 1920;
  const H = info?.height ?? 1080;
  const crop = info?.crop ?? { x: 0, y: 0, w: W };
  const off = !info?.running;
  useEffect(() => {
    if (!can && mode !== 'view') setMode('view');
  }, [can, mode]);
  useEffect(() => setMode('view'), [recId]);
  // The address asked for, until the page moves to it (or 15 s).
  useEffect(() => {
    if (!asked || !info) return;
    if ((info.url !== asked.from && !info.loading) || Date.now() - asked.at > 15_000) {
      setAsked(null);
      setFast(false);
    }
  }, [info, asked, setFast]);
  const fail = (e: unknown) => showError(e);
  const go = async (url: string) => {
    try {
      setAsked({ url, from: info?.url ?? '', at: Date.now() });
      setFast(true);
      const r = await api<{ url: string }>('POST', `/api/pages/${recId}/go`, { url });
      setAsked((a) => (a ? { ...a, url: r.url } : a));
      setTyped(null);
      void load();
    } catch (e) {
      setAsked(null);
      setFast(false);
      fail(e);
    }
  };
  const nav = (action: string) => {
    setFast(action !== 'stop');
    void api('POST', `/api/pages/${recId}/nav`, { action }).then(() => void load(), fail);
  };
  const take = useCallback(() => {
    if (!can || off) return;
    setFrame(crop);
    setMode('pilot');
  }, [can, off, crop]);
  const giveBack = useCallback(() => setMode('view'), []);
  // P takes the hand (not while typing).
  useEffect(() => {
    if (!on || mode !== 'view') return;
    const key = (e: KeyboardEvent) => {
      const el = e.target as HTMLElement;
      if ((e.key !== 'p' && e.key !== 'P') || e.ctrlKey || e.metaKey || e.altKey || el.closest('input, textarea, select, [contenteditable="true"], .modal-back')) return;
      e.preventDefault();
      take();
    };
    addEventListener('keydown', key);
    return () => removeEventListener('keydown', key);
  }, [on, mode, take]);
  const apply = async () => {
    if (!frame) return;
    try {
      await api('PUT', `/api/pages/${recId}/crop`, frame);
      await load();
    } catch (e) {
      fail(e);
    }
  };

  const loading = !!info?.loading || !!asked;
  const title = loading ? t('Loading…') : info?.title || '';
  const address = typed ?? (asked ? short(asked.url) : info?.url ? short(info.url) : '');
  const bar = (
    <div className={`pt-bar ${loading ? 'loading' : ''}`}>
      <button type="button" className="pt-ib" data-tip={t('Previous page')} disabled={!can || off} onClick={() => nav('back')}>
        <ArrowLeft size={15} />
      </button>
      <button type="button" className="pt-ib" data-tip={t('Next page')} disabled={!can || off} onClick={() => nav('forward')}>
        <ArrowRight size={15} />
      </button>
      {loading ? (
        <button type="button" className="pt-ib" data-tip={t('Stop loading')} disabled={!can || off} onClick={() => { setAsked(null); nav('stop'); }}>
          <X size={15} />
        </button>
      ) : (
        <button type="button" className="pt-ib" data-tip={t('Reload the page')} disabled={!can || off} onClick={() => nav('reload')}>
          <RotateCw size={14} />
        </button>
      )}
      <form
        className={`pt-url ${typed !== null ? 'editing' : ''}`}
        onSubmit={(e) => {
          e.preventDefault();
          if (typed?.trim()) void go(typed.trim());
        }}
      >
        {loading ? <span className="pt-spin" /> : <Globe size={14} className="pt-globe" />}
        {title && typed === null && <b className="pt-title">{title}</b>}
        <input
          value={address}
          placeholder={off ? t('The page opens when the source records') : t('Address of the page')}
          spellCheck={false}
          disabled={!can || off}
          onFocus={(e) => {
            setTyped(info?.url ?? '');
            requestAnimationFrame(() => e.target.select());
          }}
          onChange={(e) => setTyped(e.target.value)}
          onBlur={() => window.setTimeout(() => setTyped(null), 150)}
          onKeyDown={(e) => {
            e.stopPropagation();
            if (e.key === 'Escape') {
              setTyped(null);
              e.currentTarget.blur();
            }
          }}
        />
      </form>
      {mode === 'pilot' ? (
        <button type="button" className="pt-take on" onClick={giveBack}>
          <Hand size={14} /> {t('Give the hand back')} <kbd>{t('Esc')}</kbd>
        </button>
      ) : (
        <button type="button" className="pt-take" disabled={!can || off} onClick={take} data-tip={t('Click, scroll, type in the page; reframe it')}>
          <Hand size={14} /> {t('Take the hand')} <kbd>P</kbd>
        </button>
      )}
      <span className="pt-load" />
    </div>
  );
  const catcher =
    mode === 'view' && can && !off ? (
      <div className="pt-catch" onClick={take}>
        <span className="pt-hint">
          <Hand size={15} /> {t('Click to take the hand')}
        </span>
      </div>
    ) : null;
  const changed = !!frame && Math.abs(frame.x - crop.x) + Math.abs(frame.y - crop.y) + Math.abs(frame.w - crop.w) > 0.5;
  const over = mode === 'pilot' && frame ? <Pilot recId={recId} W={W} H={H} frame={frame} setFrame={setFrame} loading={loading} onExit={giveBack} /> : null;
  const below =
    mode === 'pilot' && frame ? (
      <div className="pt-below">
        <span className="muted">{t('Click, scroll, type: as on any site. Move the frame by its tab or its corners to reframe.')}</span>
        <span className="spacer" />
        {changed && (
          <span className="pt-frame-act">
            {t('Frame changed')}
            <button type="button" className="btn small ghost" onClick={() => setFrame(crop)}>
              {t('Cancel')}
            </button>
            <button type="button" className="btn small primary" onClick={() => void apply()}>
              {t('Apply')}
            </button>
          </span>
        )}
      </div>
    ) : null;
  if (!on) return { mode: 'view', bar: null, catcher: null, over: null, below: null };
  return { mode, bar, catcher, over, below };
}

/** Modifiers as the browser takes them: Alt 1, Ctrl 2, Meta 4, Shift 8. */
const mods = (e: { altKey: boolean; ctrlKey: boolean; metaKey: boolean; shiftKey: boolean }) => (e.altKey ? 1 : 0) | (e.ctrlKey ? 2 : 0) | (e.metaKey ? 4 : 0) | (e.shiftKey ? 8 : 0);
const BUTTONS = ['left', 'middle', 'right'];

/** The whole page as it is now; the mouse, the wheel and the keyboard sent to it; the frame on air over it. */
function Pilot({ recId, W, H, frame, setFrame, loading, onExit }: { recId: string; W: number; H: number; frame: Crop; setFrame: (c: Crop) => void; loading: boolean; onExit: () => void }) {
  const box = useRef<HTMLDivElement>(null);
  const queue = useRef<object[]>([]);
  const moved = useRef<object | null>(null);
  const [cursor, setCursor] = useState('default');
  const [ripples, setRipples] = useState<{ id: number; x: number; y: number }[]>([]);
  const lastAct = useRef(Date.now());
  const [left, setLeft] = useState(IDLE_MS);
  // What is done, sent in batches (a move: only the last of each batch); the answer: the pointer there.
  useEffect(() => {
    let busy = false;
    const i = window.setInterval(() => {
      if (moved.current) queue.current.push(moved.current);
      moved.current = null;
      if (!queue.current.length || busy) return;
      const events = queue.current;
      queue.current = [];
      busy = true;
      void api<{ cursor: string | null }>('POST', `/api/pages/${recId}/input`, { events })
        .then((r) => r?.cursor && setCursor(r.cursor))
        .catch(() => {})
        .finally(() => (busy = false));
    }, 40);
    return () => clearInterval(i);
  }, [recId]);
  // The hand given back after two minutes without a gesture (the last 30 s said).
  useEffect(() => {
    const i = window.setInterval(() => {
      const l = IDLE_MS - (Date.now() - lastAct.current);
      setLeft(l);
      if (l <= 0) onExit();
    }, 500);
    return () => clearInterval(i);
  }, [onExit]);
  const acted = () => (lastAct.current = Date.now());
  const at = (e: { clientX: number; clientY: number }) => {
    const r = box.current!.getBoundingClientRect();
    return { x: ((e.clientX - r.left) / r.width) * W, y: ((e.clientY - r.top) / r.height) * H };
  };
  // The keyboard goes to the page (its own keys too), Escape gives the hand back.
  useEffect(() => {
    const key = (type: 'down' | 'up') => (e: KeyboardEvent) => {
      const el = e.target as HTMLElement;
      if (el.closest('input, textarea, select, .modal-back')) return;
      e.preventDefault();
      e.stopPropagation();
      acted();
      if (e.key === 'Escape') {
        if (type === 'down') onExit();
        return;
      }
      const text = e.key.length === 1 && !e.ctrlKey && !e.metaKey ? e.key : e.key === 'Enter' ? '\r' : '';
      queue.current.push({ kind: 'key', type, key: e.key, code: e.code, text: type === 'down' ? text : '', keyCode: e.keyCode, mods: mods(e) });
    };
    const down = key('down');
    const up = key('up');
    addEventListener('keydown', down, true);
    addEventListener('keyup', up, true);
    return () => {
      removeEventListener('keydown', down, true);
      removeEventListener('keyup', up, true);
    };
  }, [onExit]);
  // The frame: its tab moves it, a corner sizes it (16:9 kept, a quarter of the width at least).
  const drag = useRef<{ c: string; x0: number; y0: number; f: Crop } | null>(null);
  const fit = (n: Crop): Crop => {
    const w = Math.max(W / 4, Math.min(W, n.w, (H * 16) / 9));
    const h = (w * 9) / 16;
    return { w: Math.round(w), x: Math.round(Math.max(0, Math.min(W - w, n.x))), y: Math.round(Math.max(0, Math.min(H - h, n.y))) };
  };
  const onFrameDown = (e: React.PointerEvent, c: string) => {
    e.stopPropagation();
    e.preventDefault();
    acted();
    (e.target as HTMLElement).setPointerCapture(e.pointerId);
    drag.current = { c, x0: e.clientX, y0: e.clientY, f: frame };
  };
  const onFrameMove = (e: React.PointerEvent) => {
    const d = drag.current;
    if (!d) return;
    e.stopPropagation();
    const k = (box.current?.clientWidth ?? 1) / W;
    const dx = (e.clientX - d.x0) / k;
    const dy = (e.clientY - d.y0) / k;
    const f = d.f;
    if (d.c === 'move') return setFrame(fit({ ...f, x: f.x + dx, y: f.y + dy }));
    const w = Math.max(W / 4, Math.min(W, f.w + (d.c.includes('e') ? dx : -dx)));
    const h0 = (f.w * 9) / 16;
    setFrame(fit({ w, x: d.c.includes('w') ? f.x + f.w - w : f.x, y: d.c.includes('n') ? f.y + h0 - (w * 9) / 16 : f.y }));
  };
  const fh = (frame.w * 9) / 16;
  const rect: CSSProperties = { left: `${(frame.x / W) * 100}%`, top: `${(frame.y / H) * 100}%`, width: `${(frame.w / W) * 100}%`, height: `${(fh / H) * 100}%` };
  const ripple = (e: React.PointerEvent) => {
    const r = box.current!.getBoundingClientRect();
    const id = Date.now() + Math.random();
    setRipples((x) => [...x, { id, x: e.clientX - r.left, y: e.clientY - r.top }]);
    window.setTimeout(() => setRipples((x) => x.filter((p) => p.id !== id)), 650);
  };
  return (
    <div
      className="pt-over pilot"
      ref={box}
      style={{ cursor }}
      onContextMenu={(e) => e.preventDefault()}
      onPointerDown={(e) => {
        acted();
        e.currentTarget.setPointerCapture(e.pointerId);
        ripple(e);
        queue.current.push({ kind: 'mouse', type: 'down', ...at(e), button: BUTTONS[e.button] ?? 'left', clicks: e.detail || 1, mods: mods(e) });
      }}
      onPointerUp={(e) => {
        if (drag.current) return void (drag.current = null);
        queue.current.push({ kind: 'mouse', type: 'up', ...at(e), button: BUTTONS[e.button] ?? 'left', clicks: e.detail || 1, mods: mods(e) });
      }}
      onPointerMove={(e) => {
        if (drag.current) return onFrameMove(e);
        acted();
        moved.current = { kind: 'mouse', type: 'move', ...at(e), mods: mods(e) };
      }}
      onWheel={(e) => {
        acted();
        queue.current.push({ kind: 'mouse', type: 'wheel', ...at(e), dx: e.deltaX, dy: e.deltaY, mods: mods(e) });
      }}
    >
      <img className="pt-live" src={live(recId)} alt="" draggable={false} />
      {loading && <span className="pt-veil" />}
      <div className="pt-frame" style={rect}>
        <span className="pt-grip" onPointerDown={(e) => onFrameDown(e, 'move')} onPointerMove={onFrameMove} onPointerUp={() => (drag.current = null)}>
          <Move size={11} /> {t('Frame on air')}
        </span>
        {['nw', 'ne', 'sw', 'se'].map((c) => (
          <i key={c} className={c} onPointerDown={(e) => onFrameDown(e, c)} onPointerMove={onFrameMove} onPointerUp={() => (drag.current = null)} />
        ))}
        <span className="pt-lab">
          {Math.round(frame.w)}×{Math.round(fh)} · ×{(W / frame.w).toFixed(2).replace('.', ',')}
        </span>
      </div>
      {ripples.map((r) => (
        <span key={r.id} className="pt-ripple" style={{ left: r.x, top: r.y }} />
      ))}
      <span className="pt-badge">
        {t('You pilot the page')} <small>· {left <= 30_000 ? t('given back in {n} s', { n: Math.max(0, Math.ceil(left / 1000)) }) : t('live')}</small>
      </span>
    </div>
  );
}
