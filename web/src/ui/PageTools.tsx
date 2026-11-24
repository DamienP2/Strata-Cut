/*
 * A web page source in the Streamer and the Extraction: its browser's bar (the address, the
 * previous and next pages, a reload) and three modes. « Voir »: the usual preview. « Recadrer »: the
 * whole page as it is now, a 16:9 frame to move and resize over it, applied at « Valider » (the
 * recording and the lives change together). « Piloter »: the page as it is now (a tenth of a second
 * behind), the mouse, the wheel and the keyboard sent to it; Escape gives them back.
 */
import { useCallback, useEffect, useRef, useState, type CSSProperties, type ReactNode } from 'react';
import { ArrowLeft, ArrowRight, Hand, Lock, RotateCw, Scan } from 'lucide-react';
import { t } from '../i18n';
import { api, useApp } from '../store';

export type PageMode = 'view' | 'crop' | 'pilot';

export interface Crop {
  x: number;
  y: number;
  w: number;
}

interface PageInfo {
  running: boolean;
  url: string;
  crop: Crop | null;
  width: number;
  height: number;
}

/** The page of a source: its address, its frame, its window (read again every 2 s). */
function usePageInfo(recId: string, on: boolean) {
  const [info, setInfo] = useState<PageInfo | null>(null);
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
    const i = window.setInterval(() => void load(), 2000);
    return () => clearInterval(i);
  }, [load, on]);
  return { info, load };
}

const live = (recId: string) => `/api/pages/${recId}/live`;

/**
 * The page's tools: `bar` above the picture, `over` inside it (absolute, over the whole picture),
 * `below` under it; none for a source that is not a page (`on`). `can`: the modes that act on the
 * page are offered (the Extraction: at the live).
 */
export function usePageTools(recId: string, on: boolean, can: boolean): { mode: PageMode; bar: ReactNode; over: ReactNode; below: ReactNode } {
  const toast = useApp((s) => s.toast);
  const { info, load } = usePageInfo(recId, on);
  const [mode, setMode] = useState<PageMode>('view');
  const [typed, setTyped] = useState<string | null>(null);
  const [draft, setDraft] = useState<Crop | null>(null);
  useEffect(() => {
    if (!can && mode !== 'view') setMode('view');
  }, [can, mode]);
  useEffect(() => setMode('view'), [recId]);
  const W = info?.width ?? 1920;
  const H = info?.height ?? 1080;
  const crop = info?.crop ?? { x: 0, y: 0, w: W };
  const fail = (e: unknown) => toast(t((e as Error).message), 'error');
  const go = async (url: string) => {
    try {
      await api('POST', `/api/pages/${recId}/go`, { url });
      setTyped(null);
      window.setTimeout(() => void load(), 800);
    } catch (e) {
      fail(e);
    }
  };
  const nav = (action: string) => void api('POST', `/api/pages/${recId}/nav`, { action }).then(() => window.setTimeout(() => void load(), 800), fail);
  const enter = (m: PageMode) => {
    if (m === 'crop') setDraft(crop);
    setMode(m);
  };
  const apply = async () => {
    if (!draft) return;
    try {
      await api('PUT', `/api/pages/${recId}/crop`, draft);
      await load();
      setMode('view');
    } catch (e) {
      fail(e);
    }
  };
  const off = !info?.running;

  const bar = (
    <div className="pt-bar">
      <button type="button" className="pt-ib" data-tip={t('Previous page')} disabled={!can || off} onClick={() => nav('back')}>
        <ArrowLeft size={15} />
      </button>
      <button type="button" className="pt-ib" data-tip={t('Next page')} disabled={!can || off} onClick={() => nav('forward')}>
        <ArrowRight size={15} />
      </button>
      <button type="button" className="pt-ib" data-tip={t('Reload the page')} disabled={!can || off} onClick={() => nav('reload')}>
        <RotateCw size={14} />
      </button>
      <form
        className="pt-url"
        onSubmit={(e) => {
          e.preventDefault();
          if (typed?.trim()) void go(typed.trim());
        }}
      >
        <Lock size={12} />
        <input
          value={typed ?? info?.url ?? ''}
          placeholder={off ? t('The page opens when the source records') : t('Address of the page')}
          spellCheck={false}
          disabled={!can || off}
          onChange={(e) => setTyped(e.target.value)}
          onBlur={() => window.setTimeout(() => setTyped(null), 200)}
          onKeyDown={(e) => {
            e.stopPropagation();
            if (e.key === 'Escape') setTyped(null);
          }}
        />
      </form>
      <span className="pt-modes" role="radiogroup">
        <button type="button" role="radio" aria-checked={mode === 'view'} className={mode === 'view' ? 'on' : ''} onClick={() => setMode('view')}>
          {t('View')}
        </button>
        <button type="button" role="radio" aria-checked={mode === 'crop'} className={mode === 'crop' ? 'on' : ''} disabled={!can || off} onClick={() => enter('crop')}>
          <Scan size={13} /> {t('Reframe')}
        </button>
        <button type="button" role="radio" aria-checked={mode === 'pilot'} className={mode === 'pilot' ? 'on' : ''} disabled={!can || off} onClick={() => enter('pilot')}>
          <Hand size={13} /> {t('Pilot')}
        </button>
      </span>
    </div>
  );
  const over = mode === 'crop' && draft ? <Reframe recId={recId} W={W} H={H} crop={draft} setCrop={setDraft} /> : mode === 'pilot' ? <Pilot recId={recId} W={W} crop={crop} onExit={() => setMode('view')} /> : null;
  const below =
    mode === 'crop' && draft ? (
      <div className="pt-cropbar">
        <span className="pt-nums">{t('Zone {w}×{h} · zoom ×{z}', { w: Math.round(draft.w), h: Math.round((draft.w * 9) / 16), z: (W / draft.w).toFixed(2).replace('.', ',') })}</span>
        <span className="spacer" />
        <button type="button" className="btn small ghost" onClick={() => setDraft({ x: 0, y: 0, w: W })}>
          {t('Whole page')}
        </button>
        <button type="button" className="btn small ghost" onClick={() => setDraft({ ...draft, x: (W - draft.w) / 2 })}>
          {t('Center')}
        </button>
        <button type="button" className="btn small ghost" onClick={() => setMode('view')}>
          {t('Cancel')}
        </button>
        <button type="button" className="btn small primary" onClick={() => void apply()}>
          {t('Apply')}
        </button>
      </div>
    ) : null;
  if (!on) return { mode: 'view', bar: null, over: null, below: null };
  return { mode, bar, over, below };
}

/** The whole page, the frame over it: moved, or a corner pulled (16:9 kept). */
function Reframe({ recId, W, H, crop, setCrop }: { recId: string; W: number; H: number; crop: Crop; setCrop: (c: Crop) => void }) {
  const box = useRef<HTMLDivElement>(null);
  const drag = useRef<{ corner: string | null; x0: number; y0: number; c: Crop } | null>(null);
  const fit = (c: Crop): Crop => {
    const w = Math.max(W / 4, Math.min(W, c.w));
    const h = (w * 9) / 16;
    return { w, x: Math.max(0, Math.min(W - w, c.x)), y: Math.max(0, Math.min(H - h, c.y)) };
  };
  const k = () => (box.current?.clientWidth ?? 1) / W;
  const h = (crop.w * 9) / 16;
  const frame: CSSProperties = { left: `${(crop.x / W) * 100}%`, top: `${(crop.y / H) * 100}%`, width: `${(crop.w / W) * 100}%`, height: `${(h / H) * 100}%` };
  return (
    <div className="pt-over crop" ref={box}>
      <img className="pt-live" src={live(recId)} alt="" draggable={false} />
      <div
        className="pt-frame"
        style={frame}
        onPointerDown={(e) => {
          e.currentTarget.setPointerCapture(e.pointerId);
          const corner = (e.target as HTMLElement).dataset.corner ?? null;
          drag.current = { corner, x0: e.clientX, y0: e.clientY, c: crop };
        }}
        onPointerMove={(e) => {
          const d = drag.current;
          if (!d) return;
          const dx = (e.clientX - d.x0) / k();
          const dy = (e.clientY - d.y0) / k();
          const c = d.c;
          if (!d.corner) return setCrop(fit({ ...c, x: c.x + dx, y: c.y + dy }));
          // A corner: the opposite one stays.
          const w = Math.max(W / 4, Math.min(W, c.w + (d.corner.includes('e') ? dx : -dx)));
          const h0 = (c.w * 9) / 16;
          setCrop(fit({ w, x: d.corner.includes('w') ? c.x + c.w - w : c.x, y: d.corner.includes('n') ? c.y + h0 - (w * 9) / 16 : c.y }));
        }}
        onPointerUp={() => (drag.current = null)}
      >
        <span className="pt-third" />
        <span className="pt-lab">
          {Math.round(crop.w)}×{Math.round(h)}
        </span>
        {['nw', 'ne', 'sw', 'se'].map((c) => (
          <i key={c} className={c} data-corner={c} />
        ))}
      </div>
      <span className="pt-badge crop">{t('Reframing · the whole page')}</span>
    </div>
  );
}

/** Modifiers as the browser takes them: Alt 1, Ctrl 2, Meta 4, Shift 8. */
const mods = (e: { altKey: boolean; ctrlKey: boolean; metaKey: boolean; shiftKey: boolean }) => (e.altKey ? 1 : 0) | (e.ctrlKey ? 2 : 0) | (e.metaKey ? 4 : 0) | (e.shiftKey ? 8 : 0);
const BUTTONS = ['left', 'middle', 'right'];

/** The page as it is now, in its frame; the mouse, the wheel and the keyboard sent to it. */
function Pilot({ recId, W, crop, onExit }: { recId: string; W: number; crop: Crop; onExit: () => void }) {
  const box = useRef<HTMLDivElement>(null);
  const queue = useRef<object[]>([]);
  const moved = useRef<object | null>(null);
  // What is done, sent in batches (a move: only the last of each batch).
  useEffect(() => {
    const i = window.setInterval(() => {
      if (moved.current) queue.current.push(moved.current);
      moved.current = null;
      if (!queue.current.length) return;
      const events = queue.current;
      queue.current = [];
      void api('POST', `/api/pages/${recId}/input`, { events }).catch(() => {});
    }, 40);
    return () => clearInterval(i);
  }, [recId]);
  const at = (e: { clientX: number; clientY: number }) => {
    const r = box.current!.getBoundingClientRect();
    return { x: crop.x + ((e.clientX - r.left) / r.width) * crop.w, y: crop.y + ((e.clientY - r.top) / r.height) * ((crop.w * 9) / 16) };
  };
  // The keyboard goes to the page (the page's own keys too), Escape gives it back.
  useEffect(() => {
    const key = (type: 'down' | 'up') => (e: KeyboardEvent) => {
      const el = e.target as HTMLElement;
      if (el.closest('input, textarea, select')) return;
      e.preventDefault();
      e.stopPropagation();
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
  const k = W / crop.w;
  const img: CSSProperties = { width: `${k * 100}%`, left: `${(-crop.x / crop.w) * 100}%`, top: `${(-crop.y / ((crop.w * 9) / 16)) * 100}%` };
  return (
    <div
      className="pt-over pilot"
      ref={box}
      onContextMenu={(e) => e.preventDefault()}
      onPointerDown={(e) => {
        e.currentTarget.setPointerCapture(e.pointerId);
        queue.current.push({ kind: 'mouse', type: 'down', ...at(e), button: BUTTONS[e.button] ?? 'left', clicks: e.detail || 1, mods: mods(e) });
      }}
      onPointerUp={(e) => queue.current.push({ kind: 'mouse', type: 'up', ...at(e), button: BUTTONS[e.button] ?? 'left', clicks: e.detail || 1, mods: mods(e) })}
      onPointerMove={(e) => (moved.current = { kind: 'mouse', type: 'move', ...at(e), mods: mods(e) })}
      onWheel={(e) => queue.current.push({ kind: 'mouse', type: 'wheel', ...at(e), dx: e.deltaX, dy: e.deltaY, mods: mods(e) })}
    >
      <img className="pt-live framed" src={live(recId)} alt="" draggable={false} style={img} />
      <span className="pt-badge">{t('Piloting · direct view')}</span>
    </div>
  );
}
