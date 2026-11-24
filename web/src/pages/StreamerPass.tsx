/*
 * Streamer: what goes on air next. Two windows side by side — on the left what is made ready (the
 * source or the image; the overlay placed on it), on the right what is on air (the overlay put on
 * or taken off under it) — and between them the passage: CUT or AUTO (a fade, its length), Space.
 * What was on air comes back on the left: a second passage goes back.
 */
import { useEffect, useRef, useState, type ReactNode, type RefObject } from "react";
import { Move } from "lucide-react";
import { t } from "../i18n";
import { Switch } from "../settings/ui";
import type { Recorder } from "../types";
import { DefaultSlate, pictureUrl } from "./StreamerSlate";
import type { Next, ProgramApi } from "./StreamerProgram";

const DURATIONS = [0.5, 1, 2];
const secs = (s: number) => `${String(s).replace(".", ",")} s`;
const same = (a: number, b: number) => Math.abs(a - b) < 1e-4;

/** What is made ready, kept while the page shows the source: first, the other input and the overlay as it is on air. */
export function useNext(prog: ProgramApi) {
  const [next, setNext] = useState<Next | null>(null);
  const p = prog.state?.program;
  useEffect(() => {
    if (p && !next) setNext({ image: !p.imageOn, overlay: { on: p.overlayOn, x: p.x, y: p.y, size: p.size } });
  }, [p, next]);
  return [next, setNext] as const;
}

/** What « Passer » would change on air, said in a few words (none: nothing to pass). */
function changes(prog: ProgramApi, next: Next): string[] {
  const p = prog.state!.program;
  const out: string[] = [];
  if (next.image !== p.imageOn) out.push(next.image ? t("Image") : t("Source"));
  const o = next.overlay;
  if (p.overlay && !(same(o.x, p.x) && same(o.y, p.y) && same(o.size, p.size))) out.push(t("Overlay moved"));
  return out;
}

/** The source's picture, drawn from the picture on air (one video for both windows). */
function Mirror({ video }: { video: RefObject<HTMLVideoElement | null> }) {
  const canvas = useRef<HTMLCanvasElement>(null);
  useEffect(() => {
    let on = true;
    let raf = 0;
    const draw = () => {
      if (!on) return;
      const v = video.current;
      const c = canvas.current;
      if (v && c && v.videoWidth) {
        const w = Math.round(c.clientWidth * devicePixelRatio);
        const h = Math.round(c.clientHeight * devicePixelRatio);
        if (c.width !== w || c.height !== h) Object.assign(c, { width: w, height: h });
        c.getContext("2d")?.drawImage(v, 0, 0, w, h);
      }
      raf = requestAnimationFrame(draw);
    };
    draw();
    return () => {
      on = false;
      cancelAnimationFrame(raf);
    };
  }, [video]);
  return <canvas ref={canvas} className="ps-mirror" />;
}

/** The window on the left: what is made ready; its overlay moved by dragging it, sized by its corner. */
export function ReadyWindow({ rec, prog, next, setNext, video, received, editable }: { rec: Recorder; prog: ProgramApi; next: Next; setNext: (n: Next) => void; video: RefObject<HTMLVideoElement | null>; received: boolean; editable: boolean }) {
  const box = useRef<HTMLDivElement>(null);
  const ovl = useRef<HTMLImageElement>(null);
  const p = prog.state!.program;
  const o = next.overlay;
  const shown = !!p.overlay;
  // The overlay's height, a share of the picture's (its own proportions).
  const hShare = (size: number) => {
    const img = ovl.current;
    return img?.naturalWidth ? ((size / 100) * img.naturalHeight) / img.naturalWidth / (9 / 16) : 0;
  };
  const clamp = (v: { x: number; y: number; size: number }) => {
    const size = Math.min(100, Math.max(2, v.size));
    return { size, x: Math.min(Math.max(0, v.x), 1 - size / 100), y: Math.min(Math.max(0, v.y), Math.max(0, 1 - hShare(size))) };
  };
  const [centred, setCentred] = useState(false);
  const start = (e: React.PointerEvent, resize: boolean) => {
    const r = box.current?.getBoundingClientRect();
    if (!r || !editable || !shown) return;
    e.preventDefault();
    e.stopPropagation();
    const from = { px: e.clientX, py: e.clientY, x: o.x, y: o.y, size: o.size };
    const move = (m: PointerEvent) => {
      const dx = (m.clientX - from.px) / r.width;
      const dy = (m.clientY - from.py) / r.height;
      let at = clamp(resize ? { x: from.x, y: from.y, size: from.size + dx * 100 } : { x: from.x + dx, y: from.y + dy, size: from.size });
      // Near the middle: on it.
      const mid = 0.5 - at.size / 200;
      const near = !resize && Math.abs(at.x - mid) < 0.01;
      if (near) at = { ...at, x: mid };
      setCentred(near);
      setNext({ ...next, overlay: { ...next.overlay, ...at } });
    };
    const up = () => {
      setCentred(false);
      window.removeEventListener("pointermove", move);
      window.removeEventListener("pointerup", up);
    };
    window.addEventListener("pointermove", move);
    window.addEventListener("pointerup", up);
  };
  return (
    <div ref={box} className="ps-win ready">
      {next.image ? <div className="ps-full">{p.image ? <img src={pictureUrl(p.image)} alt="" /> : <DefaultSlate name={rec.name} />}</div> : received ? <Mirror video={video} /> : <span className="sc-onpic mid">{t("Source stopped")}</span>}
      {centred && <span className="ps-guide" />}
      {p.overlay && (
        <div className={`pg-ovl ${editable && shown ? "edit" : ""}`} style={{ left: `${o.x * 100}%`, top: `${o.y * 100}%`, width: `${o.size}%`, opacity: (p.opacity / 100) * (p.overlayOn ? 1 : 0.4) }} onPointerDown={(e) => start(e, false)}>
          <img ref={ovl} src={pictureUrl(p.overlay)} alt="" draggable={false} />
          {editable && shown && <span className="pg-handle" onPointerDown={(e) => start(e, true)} />}
        </div>
      )}
    </div>
  );
}

/** What can be made ready: the source, the image. */
export function Bank({ rec, prog, next, setNext, editable }: { rec: Recorder; prog: ProgramApi; next: Next; setNext: (n: Next) => void; editable: boolean }) {
  const p = prog.state!.program;
  const [tick, setTick] = useState(0);
  useEffect(() => {
    const i = window.setInterval(() => setTick((n) => n + 1), 2000);
    return () => window.clearInterval(i);
  }, []);
  const key = (image: boolean, name: string, pic: ReactNode) => (
    <button type="button" className={`ps-key ${next.image === image ? "next" : ""}`} disabled={!editable} onClick={() => setNext({ ...next, image })}>
      <span className="ps-th">{pic}</span>
      <span className="ps-nm">{name}</span>
    </button>
  );
  return (
    <div className="ps-bank">
      {key(false, t("Source"), <img src={`/api/recorders/${rec.id}/thumb.jpg?v=${tick}`} alt="" onError={(e) => (e.currentTarget.style.visibility = "hidden")} />)}
      {key(true, t("Image"), p.image ? <img src={pictureUrl(p.image)} alt="" /> : <DefaultSlate name={rec.name} />)}
    </div>
  );
}

/** Between the windows: a cut or a fade (its length), « Passer » and what it would change. */
export function Passage({ prog, next, onPass, editable, passing }: { prog: ProgramApi; next: Next; onPass: () => void; editable: boolean; passing: number }) {
  const p = prog.state!.program;
  const what = changes(prog, next);
  return (
    <div className="ps-pass">
      <div className="ps-vseg" role="radiogroup">
        <button type="button" className={!p.fadeOn ? "on" : ""} disabled={!editable} onClick={() => prog.set({ fadeOn: false })}>
          {t("Cut")}
        </button>
        <button type="button" className={p.fadeOn ? "on" : ""} disabled={!editable} onClick={() => prog.set({ fadeOn: true })}>
          {t("Fade")}
        </button>
      </div>
      <div className={`ps-dur ${p.fadeOn ? "" : "off"}`}>
        {DURATIONS.map((d) => (
          <button key={d} type="button" className={same(p.fadeSecs, d) ? "on" : ""} disabled={!editable || !p.fadeOn} onClick={() => prog.set({ fadeSecs: d })}>
            {secs(d)}
          </button>
        ))}
      </div>
      <button type="button" className="ps-take" disabled={!editable || what.length === 0} onClick={onPass} data-tip={t("Space")}>
        {passing > 0 && (
          <svg key={passing} viewBox="0 0 88 88" aria-hidden="true">
            <circle cx="44" cy="44" r="42" style={{ animationDuration: `${prog.lastFade}s` }} />
          </svg>
        )}
        <span>{p.fadeOn ? "AUTO" : "CUT"}</span>
      </button>
    </div>
  );
}

/** Under the window on air: the overlay put on or taken off (on air at once, by the transition chosen), its settings. */
export function AirOverlay({ prog, next, setNext, editable, onSettings }: { prog: ProgramApi; next: Next; setNext: (n: Next) => void; editable: boolean; onSettings: () => void }) {
  const p = prog.state!.program;
  const on = p.overlayOn && !!p.overlay;
  const toggle = (want: boolean) => {
    // It goes on where it was placed on the left; what else is on air stays.
    const n = { ...next, overlay: { ...next.overlay, on: want } };
    setNext(n);
    prog.pass({ image: p.imageOn, overlay: n.overlay });
  };
  return (
    <div className="ps-ovlrow">
      <Switch checked={on} disabled={!editable || !p.overlay} onChange={toggle} label={t("Overlay")} />
      <button type="button" className="icon-btn ghost" data-tip={t("Overlay settings")} aria-label={t("Overlay settings")} onClick={onSettings}>
        <Move size={14} />
      </button>
    </div>
  );
}
