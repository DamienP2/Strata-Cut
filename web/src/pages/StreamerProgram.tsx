/*
 * Streamer: the programme of a source — what its lives send (those not set to the clean
 * source). Its image passed by CUT or AUTO, its PNG overlay (the HABILLAGE button puts it on
 * air), the sound of its tracks, and what goes out while the source is lost. The picture above
 * shows it as it goes out (drawn here over the source: the same image, overlay and fades).
 */
import { useCallback, useEffect, useRef, useState } from "react";
import { ImagePlus, Layers } from "lucide-react";
import { t, tc } from "../i18n";
import { api, can, useApp, useChanged } from "../store";
import { Switch } from "../settings/ui";
import type { Recorder } from "../types";
import { DefaultSlate, PictureModal, PictureThumb, SlateModal, pictureUrl, usePictures } from "./StreamerSlate";

export interface Track {
  on: boolean;
  gain: number;
}
export interface Program {
  image: string | null;
  imageOn: boolean;
  fade: number;
  overlay: string | null;
  overlayOn: boolean;
  position: number;
  size: number;
  margin: number;
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

const FADES = [0.5, 1, 2];
const CHANNELS: { value: Program["channels"]; label: string }[] = [
  { value: "lr", label: "L+R" },
  { value: "l", label: "L" },
  { value: "r", label: "R" },
  { value: "mono", label: "Mono" },
];

const trackOf = (p: Program, k: number): Track => p.tracks[k] ?? { on: k === 0, gain: 0 };

/** The programme of a source: read, changed (settings sent a moment after the last change), switched. */
export function useProgram(recId: string) {
  const toast = useApp((s) => s.toast);
  const [state, setState] = useState<ProgramState | null>(null);
  // The fade of the last switch (s; 0: a cut), for the picture above.
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
  const take = (image: boolean, auto: boolean) => {
    if (!state) return;
    setLastFade(auto ? state.program.fade : 0);
    setState({ ...state, program: { ...state.program, imageOn: image } });
    api("POST", `/api/program/${recId}/take`, { image, auto }).catch(fail);
  };
  const overlayOn = (on: boolean) => {
    if (!state) return;
    setState({ ...state, program: { ...state.program, overlayOn: on } });
    api("POST", `/api/program/${recId}/overlay`, { on }).catch(fail);
  };
  return { state, set, take, overlayOn, lastFade };
}

/** Where the overlay goes in the picture (percentages of its box). */
function overlayStyle(p: Program): React.CSSProperties {
  const col = p.position % 3;
  const row = Math.floor(p.position / 3);
  // The margin is a share of the height: across, that many of the width's 16 ninths.
  const mx = `${(p.margin * 9) / 16}%`;
  const my = `${p.margin}%`;
  return {
    width: `${p.size}%`,
    left: col === 0 ? mx : col === 1 ? `${(100 - p.size) / 2}%` : undefined,
    right: col === 2 ? mx : undefined,
    top: row === 0 ? my : row === 1 ? "50%" : undefined,
    bottom: row === 2 ? my : undefined,
    transform: row === 1 ? "translateY(-50%)" : undefined,
  };
}

/** Over the source's picture: the image and the overlay, as they go out. */
export function ProgramLayer({ rec, program, lastFade }: { rec: Recorder; program: Program; lastFade: number }) {
  const p = program;
  return (
    <div className="pg-layer">
      <div className="pg-image" style={{ opacity: p.imageOn ? 1 : 0, transition: `opacity ${lastFade}s linear` }}>
        {p.image ? <img src={pictureUrl(p.image)} alt="" /> : <DefaultSlate name={rec.name} />}
      </div>
      {p.overlay && <img className="pg-ovl" src={pictureUrl(p.overlay)} alt="" style={{ ...overlayStyle(p), opacity: p.overlayOn ? p.opacity / 100 : 0 }} />}
      <span className="pg-tag">{t("PROGRAMME")}</span>
    </div>
  );
}

/** The 9 places of the overlay. */
function Corners({ value, onChange, disabled }: { value: number; onChange: (v: number) => void; disabled?: boolean }) {
  return (
    <span className="pg-corners" role="radiogroup" aria-label={t("Position")}>
      {Array.from({ length: 9 }, (_, i) => (
        <button key={i} type="button" role="radio" aria-checked={value === i} className={value === i ? "on" : ""} disabled={disabled} onClick={() => onChange(i)} />
      ))}
    </span>
  );
}

function Slider({ value, min, max, step, unit, onChange, disabled }: { value: number; min: number; max: number; step: number; unit: string; onChange: (v: number) => void; disabled?: boolean }) {
  return (
    <span className="pg-slider">
      <input type="range" min={min} max={max} step={step} value={value} disabled={disabled} onChange={(e) => onChange(Number(e.target.value))} />
      <b>
        {value > 0 && unit === "dB" ? "+" : ""}
        {value.toLocaleString(undefined, { maximumFractionDigits: 1 })} {unit}
      </b>
    </span>
  );
}

/** Under the picture: the source and the image (the one on air in red, the other ready in green), CUT and AUTO, the overlay's button. */
export function Switcher({ rec, prog }: { rec: Recorder; prog: ReturnType<typeof useProgram> }) {
  const run = useApp((s) => can.has(s.auth?.user, "stream.run"));
  const [picking, setPicking] = useState(false);
  const [tick, setTick] = useState(0);
  useEffect(() => {
    const i = window.setInterval(() => setTick((n) => n + 1), 2000);
    return () => window.clearInterval(i);
  }, []);
  if (!prog.state) return null;
  const p = prog.state.program;
  const tile = (image: boolean) => {
    const air = p.imageOn === image;
    return (
      <div className={`pg-in ${air ? "pgm" : "pvw"}`}>
        <span className="pg-thumb">{image ? <PictureThumb id={p.image} name={rec.name} /> : <img src={`/api/recorders/${rec.id}/thumb.jpg?v=${tick}`} alt="" onError={(e) => (e.currentTarget.style.visibility = "hidden")} />}</span>
        <span className="pg-in-text">
          <b>{image ? t("Image") : rec.name}</b>
          {image && run ? (
            <button type="button" className="link" onClick={() => setPicking(true)}>
              {t("Change")}
            </button>
          ) : (
            <span>{image ? "" : t("Source")}</span>
          )}
        </span>
        <span className="pg-k">{air ? "PGM" : "PVW"}</span>
      </div>
    );
  };
  return (
    <div className="pg-switcher">
      {tile(false)}
      {tile(true)}
      <div className="pg-trans">
        <button type="button" className="pg-cut" disabled={!run} onClick={() => prog.take(!p.imageOn, false)}>
          CUT
        </button>
        <button type="button" className="pg-auto" disabled={!run} onClick={() => prog.take(!p.imageOn, true)}>
          AUTO
        </button>
        <span className="segmented pg-fade" title={t("Fade")}>
          {FADES.map((f) => (
            <button key={f} type="button" className={p.fade === f ? "on" : ""} disabled={!run} onClick={() => prog.set({ fade: f })}>
              {f.toLocaleString()} s
            </button>
          ))}
        </span>
      </div>
      <button type="button" className={`pg-dsk ${p.overlayOn && p.overlay ? "on" : ""}`} disabled={!run || !p.overlay} onClick={() => prog.overlayOn(!p.overlayOn)}>
        <Layers size={18} />
        {t("OVERLAY")}
      </button>
      {picking && (
        <PictureModal title={t("Image of “{name}”", { name: rec.name })} chosen={p.image} onChoose={(id) => prog.set({ image: id })} fallback={<DefaultSlate name={rec.name} />} onClose={() => setPicking(false)} />
      )}
    </div>
  );
}

/** The overlay's settings: where, how large, how far from the edge, how opaque. */
function OverlaySettings({ p, set, disabled }: { p: Program; set: (x: Partial<Program>) => void; disabled?: boolean }) {
  return (
    <>
      <span className="label">{t("Position")}</span>
      <Corners value={p.position} onChange={(position) => set({ position })} disabled={disabled} />
      <span className="label">{t("Size")}</span>
      <Slider value={p.size} min={2} max={100} step={1} unit="%" onChange={(size) => set({ size })} disabled={disabled} />
      <span className="label">{t("Margin")}</span>
      <Slider value={p.margin} min={0} max={20} step={0.5} unit="%" onChange={(margin) => set({ margin })} disabled={disabled} />
      <span className="label">{t("Opacity")}</span>
      <Slider value={p.opacity} min={0} max={100} step={5} unit="%" onChange={(opacity) => set({ opacity })} disabled={disabled} />
    </>
  );
}

/** Under the switcher: the overlay, the sound, the source lost. */
export function ProgramCards({ rec, prog }: { rec: Recorder; prog: ReturnType<typeof useProgram> }) {
  const run = useApp((s) => can.has(s.auth?.user, "stream.run"));
  const [pics] = usePictures();
  const [modal, setModal] = useState<"overlay" | "slate" | null>(null);
  if (!prog.state) return null;
  const { program: p, tracks } = prog.state;
  const name = (id: string | null | undefined) => (id ? (pics?.images.find((x) => x.id === id)?.name ?? "—") : null);
  const slate = pics?.sources[rec.id] ?? null;
  const setTrack = (k: number, x: Partial<Track>) => {
    const all = Array.from({ length: Math.max(tracks.length, p.tracks.length) }, (_, i) => trackOf(p, i));
    all[k] = { ...all[k], ...x };
    prog.set({ tracks: all });
  };
  return (
    <div className="pg-cards">
      <section className="pg-card">
        <header>
          <span>{t("Overlay")}</span>
          {run && (
            <button type="button" className="btn small" onClick={() => setModal("overlay")}>
              {t("Change")}
            </button>
          )}
        </header>
        {p.overlay ? (
          <div className="pg-card-body">
            <div className="pg-asset">
              <PictureThumb id={p.overlay} name={rec.name} overlay />
              <b title={name(p.overlay) ?? ""}>{name(p.overlay)}</b>
            </div>
            <div className="pg-row">
              <Corners value={p.position} onChange={(position) => prog.set({ position })} disabled={!run} />
              <Slider value={p.size} min={2} max={100} step={1} unit="%" onChange={(size) => prog.set({ size })} disabled={!run} />
            </div>
          </div>
        ) : (
          <button type="button" className="pg-empty" disabled={!run} onClick={() => setModal("overlay")}>
            <ImagePlus size={18} />
            {t("Add a PNG")}
          </button>
        )}
      </section>
      <section className="pg-card">
        <header>
          <span>{t("Sound")}</span>
          <span className="segmented">
            {CHANNELS.map((c) => (
              <button key={c.value} type="button" className={p.channels === c.value ? "on" : ""} disabled={!run} onClick={() => prog.set({ channels: c.value })}>
                {tc("channels", c.label)}
              </button>
            ))}
          </span>
        </header>
        <div className="pg-card-body pg-tracks">
          {tracks.length === 0 && <span className="muted">{t("No sound")}</span>}
          {tracks.map((lang, k) => {
            const tr = trackOf(p, k);
            return (
              <div key={k} className={`pg-track ${tr.on ? "" : "off"}`}>
                <Switch checked={tr.on} disabled={!run} onChange={(on) => setTrack(k, { on })} />
                <span className="pg-track-name">
                  {t("Track {n}", { n: k + 1 })}
                  {lang ? ` · ${lang}` : ""}
                </span>
                <Slider value={tr.gain} min={-40} max={12} step={1} unit="dB" onChange={(gain) => setTrack(k, { gain })} disabled={!run || !tr.on} />
              </div>
            );
          })}
        </div>
      </section>
      <section className="pg-card">
        <header>
          <span>{t("Source lost")}</span>
          <Switch checked={p.slate} disabled={!run} onChange={(slate) => prog.set({ slate })} />
        </header>
        <div className={`pg-card-body ${p.slate ? "" : "dim"}`}>
          <div className="pg-asset">
            <PictureThumb id={slate} name={rec.name} />
            <b>{slate ? name(slate) : t("Default")}</b>
          </div>
          {run && (
            <button type="button" className="btn small" onClick={() => setModal("slate")}>
              {t("Change")}
            </button>
          )}
        </div>
      </section>
      {modal === "overlay" && (
        <PictureModal title={t("Overlay of “{name}”", { name: rec.name })} overlay chosen={p.overlay} onChoose={(id) => prog.set({ overlay: id })} fallback={null} onClose={() => setModal(null)}>
          <span className="label">{t("Preview")}</span>
          <div className="pg-preview">
            <img className="pg-preview-src" src={`/api/recorders/${rec.id}/thumb.jpg`} alt="" onError={(e) => (e.currentTarget.style.visibility = "hidden")} />
            {p.overlay && <img className="pg-ovl" src={pictureUrl(p.overlay)} alt="" style={{ ...overlayStyle(p), opacity: p.opacity / 100 }} />}
          </div>
          <OverlaySettings p={p} set={prog.set} disabled={!run} />
        </PictureModal>
      )}
      {modal === "slate" && <SlateModal recId={rec.id} name={rec.name} onClose={() => setModal(null)} />}
    </div>
  );
}
