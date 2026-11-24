/*
 * Streamer: one window, what is on air. The overlay is placed on it (dragged, sized by its
 * corner): faint while it is not on air, whole when it is (moved then, it goes on air where it is
 * let go). Under the window, two blocks: the image and the overlay, each with its picture (its
 * window of pictures), its state, its CUT and its AUTO (its length). Space: the image's AUTO;
 * Enter: its CUT; H: the overlay's AUTO.
 */
import { useRef, useState, type ReactNode } from "react";
import { t } from "../i18n";
import type { Recorder } from "../types";
import { DefaultSlate, pictureUrl } from "./StreamerSlate";
import type { ProgramApi } from "./StreamerProgram";

const DURATIONS = [0.5, 1, 2];
const secs = (s: number) => (s === 1 ? "1 s" : String(s).replace(".", ","));
const same = (a: number, b: number) => Math.abs(a - b) < 1e-4;

export type Place = { x: number; y: number; size: number };

/** The overlay over the window on air: dragged, sized by its corner (on air: sent where it is let go). */
export function OverlayEditor({ prog, editable, onPlace }: { prog: ProgramApi; editable: boolean; onPlace: (p: Place) => void }) {
  const box = useRef<HTMLDivElement>(null);
  const img = useRef<HTMLImageElement>(null);
  const [drag, setDrag] = useState<Place | null>(null);
  const [centred, setCentred] = useState(false);
  const p = prog.state?.program;
  if (!p?.overlay) return null;
  const at = drag ?? { x: p.x, y: p.y, size: p.size };
  // Its height, a share of the picture's (its own proportions).
  const hShare = (size: number) => {
    const i = img.current;
    return i?.naturalWidth ? ((size / 100) * i.naturalHeight) / i.naturalWidth / (9 / 16) : 0;
  };
  const clamp = (v: Place) => {
    const size = Math.min(100, Math.max(2, v.size));
    return { size, x: Math.min(Math.max(0, v.x), 1 - size / 100), y: Math.min(Math.max(0, v.y), Math.max(0, 1 - hShare(size))) };
  };
  const start = (e: React.PointerEvent, resize: boolean) => {
    const r = box.current?.getBoundingClientRect();
    if (!r || !editable) return;
    e.preventDefault();
    e.stopPropagation();
    const from = { px: e.clientX, py: e.clientY, ...at };
    let last = at;
    const move = (m: PointerEvent) => {
      const dx = (m.clientX - from.px) / r.width;
      const dy = (m.clientY - from.py) / r.height;
      let n = clamp(resize ? { x: from.x, y: from.y, size: from.size + dx * 100 } : { x: from.x + dx, y: from.y + dy, size: from.size });
      // Near the middle: on it.
      const mid = 0.5 - n.size / 200;
      const near = !resize && Math.abs(n.x - mid) < 0.01;
      if (near) n = { ...n, x: mid };
      setCentred(near);
      last = n;
      setDrag(n);
    };
    const up = () => {
      window.removeEventListener("pointermove", move);
      window.removeEventListener("pointerup", up);
      setCentred(false);
      setDrag(null);
      if (!(same(last.x, p.x) && same(last.y, p.y) && same(last.size, p.size))) onPlace(last);
    };
    window.addEventListener("pointermove", move);
    window.addEventListener("pointerup", up);
  };
  const on = p.overlayOn;
  return (
    <div ref={box} className="ps-ovl-box">
      {centred && <span className="ps-guide" />}
      <div
        className={`pg-ovl ${editable ? "edit" : ""} ${on ? "on" : "off"}`}
        style={{ left: `${at.x * 100}%`, top: `${at.y * 100}%`, width: `${at.size}%`, opacity: on ? p.opacity / 100 : 0.4, transition: drag ? "none" : `opacity ${prog.lastFade}s linear` }}
        onPointerDown={(e) => start(e, false)}
      >
        <img ref={img} src={pictureUrl(p.overlay)} alt="" draggable={false} />
        {editable && <span className="pg-handle" onPointerDown={(e) => start(e, true)} />}
      </div>
    </div>
  );
}

/** One block of the bar: its picture (its window), its state, CUT, AUTO (filling while it fades), its length. */
function Block({ title, pic, checker, onAir, airLabel, offLabel, editable, can, dur, setDur, passing, fade, onCut, onAuto, onPictures, keys }: { title: string; pic: ReactNode; checker?: boolean; onAir: boolean; airLabel: string; offLabel: string; editable: boolean; can: boolean; dur: number; setDur: (d: number) => void; passing: number; fade: number; onCut: () => void; onAuto: () => void; onPictures: () => void; keys: [string, string] }) {
  return (
    <div className={`ps-blk ${onAir ? "air" : ""}`}>
      <button type="button" className={`ps-thumb ${checker ? "checker" : ""}`} data-tip={t("Choose")} aria-label={t("Choose")} onClick={onPictures}>
        {pic}
      </button>
      <span className="ps-who">
        <b>{title}</b>
        <span className="ps-state">{onAir ? airLabel : offLabel}</span>
      </span>
      <button type="button" className="ps-tbtn" disabled={!editable || !can} data-tip={keys[0] || undefined} onClick={onCut}>
        CUT
      </button>
      <button type="button" className="ps-tbtn auto" disabled={!editable || !can} data-tip={keys[1]} onClick={onAuto}>
        {passing > 0 && fade > 0 && <i key={passing} className="ps-fill" style={{ animationDuration: `${fade}s` }} />}
        <span>AUTO</span>
      </button>
      <span className="ps-dur">
        {DURATIONS.map((d) => (
          <button key={d} type="button" className={same(dur, d) ? "on" : ""} disabled={!editable} onClick={() => setDur(d)}>
            {secs(d)}
          </button>
        ))}
      </span>
    </div>
  );
}

/** Under the window: the image's block and the overlay's. */
export function PassBar({ rec, prog, editable, passing, onImage, onOverlay, onPictures }: { rec: Recorder; prog: ProgramApi; editable: boolean; passing: { image: number; overlay: number }; onImage: (cut: boolean) => void; onOverlay: (cut: boolean) => void; onPictures: (use: "image" | "overlay") => void }) {
  const p = prog.state!.program;
  return (
    <div className="ps-bar2">
      <Block
        title={t("Image")}
        pic={p.image ? <img src={pictureUrl(p.image)} alt="" /> : <DefaultSlate name={rec.name} />}
        onAir={p.imageOn}
        airLabel={t("On air")}
        offLabel={t("Source")}
        editable={editable}
        can
        dur={p.fadeSecs}
        setDur={(d) => prog.set({ fadeSecs: d })}
        passing={passing.image}
        fade={prog.lastFade}
        onCut={() => onImage(true)}
        onAuto={() => onImage(false)}
        onPictures={() => onPictures("image")}
        keys={[t("Enter"), t("Space")]}
      />
      <Block
        title={t("Overlay")}
        checker
        pic={p.overlay ? <img className="ps-thumb-ovl" src={pictureUrl(p.overlay)} alt="" style={{ left: `${p.x * 100}%`, top: `${p.y * 100}%`, width: `${p.size}%` }} /> : null}
        onAir={p.overlayOn && !!p.overlay}
        airLabel={t("On air")}
        offLabel={t("Taken off")}
        editable={editable}
        can={!!p.overlay}
        dur={p.overlayFadeSecs ?? 1}
        setDur={(d) => prog.set({ overlayFadeSecs: d })}
        passing={passing.overlay}
        fade={prog.lastFade}
        onCut={() => onOverlay(true)}
        onAuto={() => onOverlay(false)}
        onPictures={() => onPictures("overlay")}
        keys={["", "H"]}
      />
    </div>
  );
}
