/*
 * Streamer: the programme of a source — what its lives send. The source or its image (a fade of
 * 1 s between them, or at once), its PNG overlay (placed and sized on the picture itself), the
 * sound of its tracks; its pictures (image, overlay, slate) in the settings' window. The picture
 * shows the programme as it goes out (the image and the overlay drawn here over the source, with
 * the same fades).
 */
import { useCallback, useEffect, useRef, useState } from "react";
import { X } from "lucide-react";
import { t, tc } from "../i18n";
import { api, can, useApp, useChanged } from "../store";
import { Switch } from "../settings/ui";
import type { Recorder } from "../types";
import { DefaultSlate, Gallery, pictureUrl, usePictures } from "./StreamerSlate";

export interface Track {
  on: boolean;
  gain: number;
}
export interface Program {
  image: string | null;
  imageOn: boolean;
  fadeOn: boolean;
  overlay: string | null;
  overlayOn: boolean;
  /** The overlay's top left corner (shares of the picture) and width (% of it). */
  x: number;
  y: number;
  size: number;
  opacity: number;
  tracks: Track[];
  channels: "lr" | "l" | "r" | "mono";
  slate: boolean;
}
interface ProgramState {
  program: Program;
  /** The source's tracks: their language ("" when not said). */
  tracks: string[];
}

/** The fade between the source and the image (as the server's). */
const FADE_S = 1;
const CHANNELS: Program["channels"][] = ["lr", "l", "r", "mono"];
const CHANNEL_LABEL: Record<Program["channels"], string> = { lr: "L+R", l: "L", r: "R", mono: "Mono" };

const trackOf = (p: Program, k: number): Track => p.tracks[k] ?? { on: k === 0, gain: 0 };

/** The programme of a source: read, changed (sent a moment after the last change), switched. */
export function useProgram(recId: string) {
  const toast = useApp((s) => s.toast);
  const [state, setState] = useState<ProgramState | null>(null);
  // The fade of the last switch (s; 0: at once), for the picture.
  const [lastFade, setLastFade] = useState(0);
  const timer = useRef<number | undefined>(undefined);
  const load = useCallback(async () => {
    try {
      setState(await api<ProgramState>("GET", `/api/program/${recId}`));
    } catch {
      /* the next change */
    }
  }, [recId]);
  useEffect(() => void load(), [load]);
  useChanged("program", () => {
    if (!timer.current) void load();
  });
  const fail = (e: unknown) => {
    toast(t((e as Error).message), "error");
    void load();
  };
  const set = (patch: Partial<Program>) => {
    setState((s) => {
      if (!s) return s;
      const program = { ...s.program, ...patch };
      window.clearTimeout(timer.current);
      timer.current = window.setTimeout(() => {
        timer.current = undefined;
        api("PUT", `/api/program/${recId}`, program).catch(fail);
      }, 250);
      return { ...s, program };
    });
  };
  const take = (image: boolean) => {
    if (!state || state.program.imageOn === image) return;
    setLastFade(state.program.fadeOn ? FADE_S : 0);
    setState({ ...state, program: { ...state.program, imageOn: image } });
    api("POST", `/api/program/${recId}/take`, { image }).catch(fail);
  };
  const overlayOn = (on: boolean) => {
    if (!state) return;
    setState({ ...state, program: { ...state.program, overlayOn: on } });
    api("POST", `/api/program/${recId}/overlay`, { on }).catch(fail);
  };
  return { state, set, take, overlayOn, lastFade };
}
export type ProgramApi = ReturnType<typeof useProgram>;

/**
 * Over the source's picture: the image and the overlay, as they go out. On air and for whom may
 * change it, the overlay is moved by dragging it and sized by its corner.
 */
export function ProgramLayer({ rec, prog, editable }: { rec: Recorder; prog: ProgramApi; editable: boolean }) {
  const box = useRef<HTMLDivElement>(null);
  const ovl = useRef<HTMLImageElement>(null);
  // While dragged: where it is (not sent until released).
  const [drag, setDrag] = useState<{ x: number; y: number; size: number } | null>(null);
  if (!prog.state) return null;
  const p = prog.state.program;
  const at = drag ?? { x: p.x, y: p.y, size: p.size };
  const live = editable && p.overlayOn && !!p.overlay;
  // The overlay's height, a share of the picture's (its own proportions).
  const hShare = (size: number) => {
    const img = ovl.current;
    if (!img || !img.naturalWidth) return 0;
    return ((size / 100) * img.naturalHeight) / img.naturalWidth / (9 / 16);
  };
  const clamp = (v: { x: number; y: number; size: number }) => {
    const size = Math.min(100, Math.max(2, v.size));
    return { size, x: Math.min(Math.max(0, v.x), 1 - size / 100), y: Math.min(Math.max(0, v.y), Math.max(0, 1 - hShare(size))) };
  };
  const start = (e: React.PointerEvent, resize: boolean) => {
    const r = box.current?.getBoundingClientRect();
    if (!r || !live) return;
    e.preventDefault();
    e.stopPropagation();
    const from = { px: e.clientX, py: e.clientY, ...at };
    let last = at;
    const move = (m: PointerEvent) => {
      const dx = (m.clientX - from.px) / r.width;
      const dy = (m.clientY - from.py) / r.height;
      last = clamp(resize ? { x: from.x, y: from.y, size: from.size + dx * 100 } : { x: from.x + dx, y: from.y + dy, size: from.size });
      setDrag(last);
    };
    const up = () => {
      window.removeEventListener("pointermove", move);
      window.removeEventListener("pointerup", up);
      setDrag(null);
      prog.set(last);
    };
    window.addEventListener("pointermove", move);
    window.addEventListener("pointerup", up);
  };
  return (
    <div ref={box} className="pg-layer">
      <div className="pg-image" style={{ opacity: p.imageOn ? 1 : 0, transition: `opacity ${prog.lastFade}s linear` }}>
        {p.image ? <img src={pictureUrl(p.image)} alt="" /> : <DefaultSlate name={rec.name} />}
      </div>
      {p.overlay && (
        <div
          className={`pg-ovl ${live ? "edit" : ""}`}
          style={{ left: `${at.x * 100}%`, top: `${at.y * 100}%`, width: `${at.size}%`, opacity: p.overlayOn ? p.opacity / 100 : 0 }}
          onPointerDown={(e) => start(e, false)}
        >
          <img ref={ovl} src={pictureUrl(p.overlay)} alt="" draggable={false} />
          {live && <span className="pg-handle" onPointerDown={(e) => start(e, true)} />}
        </div>
      )}
      <span className="pg-tag">
        <i />
        {t("PROGRAMME")}
      </span>
    </div>
  );
}

/** The two inputs, the fade, the overlay; the lives of the source. */
export function Antenne({ rec, prog, live, kbps, onStopAll }: { rec: Recorder; prog: ProgramApi; live: number; kbps: string; onStopAll: () => void }) {
  const run = useApp((s) => can.has(s.auth?.user, "stream.run"));
  const [tick, setTick] = useState(0);
  useEffect(() => {
    const i = window.setInterval(() => setTick((n) => n + 1), 2000);
    return () => window.clearInterval(i);
  }, []);
  if (!prog.state) return <section className="pg-box pg-antenne" />;
  const p = prog.state.program;
  const input = (image: boolean) => (
    <button type="button" className={`pg-input ${p.imageOn === image ? "air" : ""}`} disabled={!run} onClick={() => prog.take(image)}>
      <span className="pg-input-pic">
        {image ? (
          p.image ? (
            <img src={pictureUrl(p.image)} alt="" />
          ) : (
            <DefaultSlate name={rec.name} />
          )
        ) : (
          <img src={`/api/recorders/${rec.id}/thumb.jpg?v=${tick}`} alt="" onError={(e) => (e.currentTarget.style.visibility = "hidden")} />
        )}
      </span>
      <span className="pg-input-name">
        {p.imageOn === image && <i />}
        {image ? t("Image") : t("Source")}
      </span>
    </button>
  );
  return (
    <section className="pg-box pg-antenne">
      <header>
        <span className="label">{t("On air")}</span>
      </header>
      <div className="pg-antenne-body">
        <div className="pg-inputs">
          {input(false)}
          {input(true)}
        </div>
        <label className="pg-row">
          <span>{t("Fade")}</span>
          <Switch checked={p.fadeOn} disabled={!run} onChange={(fadeOn) => prog.set({ fadeOn })} />
        </label>
        <label className="pg-row" data-tip={!p.overlay ? t("Choose an overlay in the settings") : undefined}>
          <span>{t("Overlay")}</span>
          <Switch checked={p.overlayOn && !!p.overlay} disabled={!run || !p.overlay} onChange={(on) => prog.overlayOn(on)} />
        </label>
        <div className="pg-live">
          {live > 0 ? (
            <>
              <i />
              <span>
                <b>{live}</b> {t("on air")} · <b>{kbps}</b>
              </span>
            </>
          ) : (
            <span className="muted">{t("Nothing sent")}</span>
          )}
          <span className="spacer" />
          {run && live > 0 && (
            <button type="button" className="btn" onClick={onStopAll}>
              {t("Stop all")}
            </button>
          )}
        </div>
      </div>
    </section>
  );
}

/** A level meter (two channels), dB from -60 to 0. */
function Vu({ l, r }: { l: number; r: number }) {
  const h = (db: number) => `${Math.max(0, Math.min(100, ((db + 60) / 60) * 100))}%`;
  return (
    <span className="pg-vu" aria-hidden="true">
      <i style={{ height: h(l) }} />
      <i style={{ height: h(r) }} />
    </span>
  );
}

/** The levels of the source's tracks, measured by the server (5 a second while shown). */
function useLevels(recId: string, n: number): number[][] {
  const [lv, setLv] = useState<number[][]>([]);
  useEffect(() => {
    if (n === 0) return;
    let on = true;
    const get = () =>
      api<{ tracks: number[][] }>("GET", `/api/program/${recId}/levels`).then(
        // A peak falls back slowly (as a meter's needle), 6 dB a reading.
        (r) => on && setLv((prev) => r.tracks.map((tr, k) => tr.map((db, c) => Math.max(db, (prev[k]?.[c] ?? -90) - 6)))),
        () => {},
      );
    void get();
    const i = window.setInterval(get, 200);
    return () => {
      on = false;
      window.clearInterval(i);
    };
  }, [recId, n]);
  return lv;
}

/** The sound: each track's switch, level and meter; the channels; what goes out. */
export function Mixer({ rec, prog }: { rec: Recorder; prog: ProgramApi }) {
  const run = useApp((s) => can.has(s.auth?.user, "stream.run"));
  const n = prog.state?.tracks.length ?? 0;
  const levels = useLevels(rec.id, n);
  if (!prog.state) return <section className="pg-box pg-mixer" />;
  const { program: p, tracks } = prog.state;
  const setTrack = (k: number, x: Partial<Track>) => {
    const all = Array.from({ length: Math.max(tracks.length, p.tracks.length) }, (_, i) => trackOf(p, i));
    all[k] = { ...all[k], ...x };
    prog.set({ tracks: all });
  };
  // What goes out, as heard: the loudest track on, at its level, on the channels chosen; none
  // under the image.
  const out = [0, 1].map((c) => {
    if (p.imageOn) return -90;
    let best = -90;
    tracks.forEach((_, k) => {
      const tr = trackOf(p, k);
      const lv = levels[k];
      if (!tr.on || !lv) return;
      const src = p.channels === "l" ? lv[0] : p.channels === "r" ? lv[1] : p.channels === "mono" ? Math.max(lv[0], lv[1]) - 3 : lv[c];
      best = Math.max(best, src + tr.gain);
    });
    return best;
  });
  return (
    <section className="pg-box pg-mixer">
      <header>
        <span>{t("Sound")}</span>
        <span className="segmented">
          {CHANNELS.map((c) => (
            <button key={c} type="button" className={p.channels === c ? "on" : ""} disabled={!run || n === 0} onClick={() => prog.set({ channels: c })}>
              {tc("channels", CHANNEL_LABEL[c])}
            </button>
          ))}
        </span>
      </header>
      <div className="pg-strips">
        {n === 0 && <span className="muted pg-nosound">{t("No sound")}</span>}
        {tracks.map((lang, k) => {
          const tr = trackOf(p, k);
          const lv = levels[k] ?? [-90, -90];
          return (
            <div key={k} className={`pg-strip ${tr.on ? "" : "off"}`}>
              <span className="pg-strip-name">
                {t("Track {n}", { n: k + 1 })}
                <small>{lang || "—"}</small>
              </span>
              <span className="pg-strip-body">
                <input
                  type="range"
                  className="pg-fader"
                  min={-40}
                  max={12}
                  step={1}
                  value={tr.gain}
                  disabled={!run || !tr.on}
                  aria-label={t("Level")}
                  onChange={(e) => setTrack(k, { gain: Number(e.target.value) })}
                  onDoubleClick={() => setTrack(k, { gain: 0 })}
                />
                <Vu l={lv[0]} r={lv[1]} />
              </span>
              <b className="pg-db">
                {tr.gain > 0 ? "+" : ""}
                {tr.gain} dB
              </b>
              <Switch checked={tr.on} disabled={!run} onChange={(on) => setTrack(k, { on })} />
            </div>
          );
        })}
        {n > 0 && (
          <div className="pg-strip master">
            <span className="pg-strip-name">
              {t("Output")}
              <small>{tc("channels", CHANNEL_LABEL[p.channels])}</small>
            </span>
            <span className="pg-strip-body">
              <Vu l={out[0]} r={out[1]} />
            </span>
          </div>
        )}
      </div>
    </section>
  );
}

type Tab = "image" | "overlay" | "slate";

/** The source's pictures: its image, its overlay, its slate (and whether one is sent). */
export function SettingsModal({ rec, prog, onClose }: { rec: Recorder; prog: ProgramApi; onClose: () => void }) {
  const run = useApp((s) => can.has(s.auth?.user, "stream.run"));
  const toast = useApp((s) => s.toast);
  const [pics, reload] = usePictures();
  const [tab, setTab] = useState<Tab>("image");
  useEffect(() => {
    const key = (e: KeyboardEvent) => e.key === "Escape" && onClose();
    window.addEventListener("keydown", key);
    return () => window.removeEventListener("keydown", key);
  }, [onClose]);
  if (!prog.state) return null;
  const p = prog.state.program;
  const chooseSlate = async (image: string | null) => {
    try {
      await api("PUT", `/api/slates/source/${rec.id}`, { image });
      await reload();
    } catch (e) {
      toast(t((e as Error).message), "error");
    }
  };
  const tabs: [Tab, string][] = [
    ["image", t("Image")],
    ["overlay", t("Overlay")],
    ["slate", t("Source lost")],
  ];
  const fallback = { name: t("Default"), pic: <DefaultSlate name={rec.name} /> };
  return (
    <div className="modal-back" onPointerDown={onClose}>
      <div className="modal pg-settings" onPointerDown={(e) => e.stopPropagation()}>
        <header>
          <h2>{t("Settings of “{name}”", { name: rec.name })}</h2>
          <button type="button" className="icon-btn ghost" title={t("Close")} onClick={onClose}>
            <X size={16} />
          </button>
        </header>
        <nav className="pg-tabs" role="tablist">
          {tabs.map(([k, label]) => (
            <button key={k} type="button" role="tab" aria-selected={tab === k} className={tab === k ? "on" : ""} onClick={() => setTab(k)}>
              {label}
            </button>
          ))}
        </nav>
        <div className="pg-settings-body">
          {tab === "image" && <Gallery chosen={p.image} onChoose={(image) => prog.set({ image })} fallback={fallback} disabled={!run} />}
          {tab === "overlay" && <Gallery overlay chosen={p.overlay} onChoose={(overlay) => prog.set({ overlay, ...(overlay ? {} : { overlayOn: false }) })} fallback={{ name: tc("overlay", "None"), pic: null }} disabled={!run} />}
          {tab === "slate" && (
            <>
              <label className="pg-row">
                <span>{t("Send a picture while the source is lost")}</span>
                <Switch checked={p.slate} disabled={!run} onChange={(slate) => prog.set({ slate })} />
              </label>
              <Gallery chosen={pics?.sources[rec.id] ?? null} onChoose={chooseSlate} fallback={fallback} disabled={!run || !p.slate} />
            </>
          )}
        </div>
        <footer>
          <button type="button" className="btn" onClick={onClose}>
            {t("Close")}
          </button>
        </footer>
      </div>
    </div>
  );
}
