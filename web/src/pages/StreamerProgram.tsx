/*
 * Streamer: the programme of a source — what its lives send. The source or its image (a fade of
 * 1 s between them, or at once), its PNG overlay (placed and sized on the picture itself), the
 * sound of its tracks; its pictures (image, overlay) in the settings' window. The picture
 * shows the programme as it goes out (the image and the overlay drawn here over the source, with
 * the same fades).
 */
import { useCallback, useEffect, useRef, useState } from "react";
import { Headphones, RotateCcw, Settings, Volume2, VolumeX, Wand2, X } from "lucide-react";
import { t, tc, tn } from "../i18n";
import { api, can, useApp, useChanged } from "../store";
import { Switch } from "../settings/ui";
import type { Recorder } from "../types";
import { DefaultSlate, Gallery, pictureUrl } from "./StreamerSlate";
import { trackLabel } from "../audioLabel";

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
  /** The level of what goes out, dB. */
  master: number;
}
interface ProgramState {
  program: Program;
  /** The source's tracks: their language ("" when not said), and their kind. */
  tracks: string[];
  kinds?: string[];
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
    </div>
  );
}

/** The two inputs, the fade, the overlay; the lives of the source. */
export function Antenne({ rec, prog, onSettings }: { rec: Recorder; prog: ProgramApi; onSettings: () => void }) {
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
        <button type="button" className="btn small" onClick={onSettings}>
          <Settings size={13} /> {t("Settings")}
        </button>
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
      </div>
    </section>
  );
}

/** Where a meter's level is (a share of its height): dB from -60 to 0. */
const vuShare = (db: number) => Math.max(0, Math.min(1, (db + 60) / 60));
/**
 * A good level, for whom is not a sound engineer: peaks between these (dBFS), the band drawn on
 * the meters; above `LOUD`, too loud; at `CLIP`, saturated. AUTO puts a track's peaks at `TARGET`.
 * (What goes out is limited at -1 dBFS anyway: never saturated.)
 */
const GOOD_LOW = -20;
const GOOD_HIGH = -6;
const LOUD = -3;
const CLIP = -1;
const TARGET = -9;
const SILENT = -55;

type Verdict = "silent" | "low" | "good" | "loud" | "clip";
const verdictOf = (peak: number): Verdict => (peak <= SILENT ? "silent" : peak >= CLIP ? "clip" : peak > LOUD ? "loud" : peak < GOOD_LOW ? "low" : "good");
const VERDICT_LABEL: Record<Verdict, string> = { silent: "Silent", low: "Too low", good: "Good", loud: "Too loud", clip: "Saturated" };

/** Says how a level is, in words and a colour. */
function Verdict({ peak }: { peak: number }) {
  const v = verdictOf(peak);
  return <span className={`pg-verdict ${v}`}>{t(VERDICT_LABEL[v])}</span>;
}

/** The meter's marks (dB). */
const VU_MARKS = [0, -6, -12, -18, -24, -36, -48];

/** A level meter (two channels), dB from -60 to 0, with its marks. */
function Vu({ levels }: { levels: number[] }) {
  // A bar for each channel (4 px, 2 px between).
  const width = Math.max(10, levels.length * 6 - 2);
  return (
    <span className="pg-vu" aria-hidden="true" style={{ width }}>
      {levels.map((db, c) => (
        <i key={c} style={{ height: `${vuShare(db) * 100}%` }} />
      ))}
      <em className="pg-vu-zone" style={{ bottom: `${vuShare(GOOD_LOW) * 100}%`, height: `${(vuShare(GOOD_HIGH) - vuShare(GOOD_LOW)) * 100}%` }} />
      {VU_MARKS.map((m) => (
        <b key={m} className="pg-vu-mark" style={{ bottom: `${vuShare(m) * 100}%` }} />
      ))}
    </span>
  );
}

/**
 * A scale beside a fader or a meter: its marks where the thumb or the level stands (`share`: 0 at
 * the bottom, 1 at the top). A fader's thumb travels its height less its own size.
 */
function Scale({ marks, share, fader }: { marks: number[]; share: (v: number) => number; fader?: boolean }) {
  return (
    <span className={`pg-scale ${fader ? "fader" : ""}`} aria-hidden="true">
      {marks.map((m) => (
        <i key={m} style={{ bottom: fader ? `calc(8px + (100% - 16px) * ${share(m)})` : `${share(m) * 100}%` }}>
          {m > 0 ? `+${m}` : m}
        </i>
      ))}
    </span>
  );
}

/** The track faders' marks (dB of gain, -40 to +12). */
const GAIN_MARKS = [12, 6, 0, -6, -12, -20, -30, -40];
const gainShare = (db: number) => (db + 40) / 52;

/**
 * The levels of the source's tracks, measured by the server (10 a second while shown), of the frame
 * the picture shows: as a meter shows them (a peak falls back at 60 dB a second: a sound that stops
 * is seen to stop at once), and the highest of each track over its last readings.
 */
function useLevels(recId: string, n: number, shown: () => number | null): { lv: number[][]; recent: (k: number, readings: number) => number } {
  const [lv, setLv] = useState<number[][]>([]);
  const history = useRef<number[][]>([]);
  useEffect(() => {
    if (n === 0) return;
    let on = true;
    let busy = false;
    history.current = [];
    const get = () => {
      if (busy) return;
      busy = true;
      void api<{ tracks: number[][] }>("GET", `/api/program/${recId}/levels${shown() !== null ? `?t=${shown()}` : ""}`).then(
        (r) => {
          if (!on) return;
          r.tracks.forEach((tr, k) => {
            const h = (history.current[k] ??= []);
            h.push(Math.max(-90, ...tr));
            if (h.length > 100) h.shift();
          });
          // A peak falls back at 6 dB a reading (60 dB a second).
          setLv((prev) => r.tracks.map((tr, k) => tr.map((db, c) => Math.max(db, (prev[k]?.[c] ?? -90) - 6))));
        },
        () => {},
      ).finally(() => {
        busy = false;
      });
    };
    get();
    const i = window.setInterval(get, 100);
    return () => {
      on = false;
      window.clearInterval(i);
    };
  }, [recId, n]);
  const recent = (k: number, readings: number) => Math.max(-90, ...(history.current[k] ?? []).slice(-readings));
  return { lv, recent };
}

/** The browser's listening (headphones): which track, how loud; nothing of it goes out. */
export interface Monitor {
  volume: number;
  setVolume: (v: number) => void;
  muted: boolean;
  setMuted: (m: boolean) => void;
  /** The track heard alone (solo), or none: the output. */
  solo: number | null;
  setSolo: (k: number | null) => void;
  /** The track played (the solo, else the first on). */
  heard: number;
}

/** A vertical fader. */
function Fader({ value, min, max, step, disabled, label, onChange, onReset }: { value: number; min: number; max: number; step: number; disabled?: boolean; label: string; onChange: (v: number) => void; onReset?: () => void }) {
  return (
    <input
      type="range"
      className="pg-fader"
      min={min}
      max={max}
      step={step}
      value={value}
      disabled={disabled}
      aria-label={label}
      onChange={(e) => onChange(Number(e.target.value))}
      onDoubleClick={onReset}
      onKeyDown={(e) => e.stopPropagation()}
    />
  );
}

/** The sound: each track's level, meter and switch (left); what goes out, and the headphones (right). */
export function Mixer({ rec, prog, monitor, shown }: { rec: Recorder; prog: ProgramApi; monitor: Monitor; shown: () => number | null }) {
  const run = useApp((s) => can.has(s.auth?.user, "stream.run"));
  const n = prog.state?.tracks.length ?? 0;
  const { lv: levels, recent } = useLevels(rec.id, n, shown);
  const toast = useApp((s) => s.toast);
  if (!prog.state) return <section className="pg-mixer" />;
  const { program: p, tracks } = prog.state;
  const all = () => Array.from({ length: Math.max(tracks.length, p.tracks.length) }, (_, i) => trackOf(p, i));
  const setTrack = (k: number, x: Partial<Track>) => {
    const next = all();
    next[k] = { ...next[k], ...x };
    prog.set({ tracks: next });
  };
  // The source's own levels again (each track at 0 dB; on and off kept).
  const reset = () => prog.set({ tracks: all().map((tr) => ({ ...tr, gain: 0 })), master: 0 });
  const changed = all().some((tr) => tr.gain !== 0) || p.master !== 0;
  // AUTO: the track's peaks of the last 10 s brought to the target.
  const auto = (k: number) => {
    const peak = recent(k, 100);
    if (peak <= SILENT) {
      toast(t("This track is silent: nothing to set"), "info");
      return;
    }
    setTrack(k, { gain: Math.max(-40, Math.min(12, Math.round(TARGET - peak))) });
  };
  // What goes out, as heard: the loudest track on, at its level, on the channels chosen; none
  // under the image.
  const out = [0, 1].map((c) => {
    if (p.imageOn) return -90;
    let best = -90;
    tracks.forEach((_, k) => {
      const tr = trackOf(p, k);
      const lv = levels[k];
      if (!tr.on || !lv?.length) return;
      // Its left and right (a mono track: the same; more channels: the first two, near enough).
      const [l, r] = [lv[0], lv[1] ?? lv[0]];
      const src = p.channels === "l" ? l : p.channels === "r" ? r : p.channels === "mono" ? Math.max(l, r) - 3 : c === 0 ? l : r;
      best = Math.max(best, src + tr.gain);
    });
    return best <= -90 ? -90 : Math.min(-1, best + p.master);
  });
  // The same over the last 3 s, for its verdict (limited at -1 dBFS when it goes out).
  let outPeak = -90;
  tracks.forEach((_, k) => {
    const tr = trackOf(p, k);
    if (tr.on) outPeak = Math.max(outPeak, recent(k, 30) + tr.gain);
  });
  outPeak = outPeak <= -90 ? -90 : outPeak + p.master;
  return (
    <section className="pg-mixer" aria-label={t("Sound")}>
      <header>
        <span className="segmented">
          {CHANNELS.map((c) => (
            <button key={c} type="button" className={p.channels === c ? "on" : ""} disabled={!run || n === 0} onClick={() => prog.set({ channels: c })}>
              {tc("channels", CHANNEL_LABEL[c])}
            </button>
          ))}
        </span>
        <button type="button" className="btn small" disabled={!run || !changed} onClick={reset}>
          <RotateCcw size={12} /> {t("Reset")}
        </button>
      </header>
      <div className="pg-strips">
        <div className="pg-strip-group tracks">
          {n === 0 && <span className="muted pg-nosound">{t("No sound")}</span>}
          {tracks.map((lang, k) => {
            const tr = trackOf(p, k);
            // As it goes into the mix: the source's level with this track's.
            // Each of its channels (none measured yet: two).
            const lv = (levels[k]?.length ? levels[k] : [-90, -90]).map((db) => (db <= -90 ? -90 : db + tr.gain));
            const peak = recent(k, 30) + tr.gain;
            const solo = monitor.solo === k;
            const label = trackLabel(lang, prog.state?.kinds?.[k]);
            return (
              <div key={k} className={`pg-strip ${tr.on ? "" : "off"}`} style={lv.length > 2 ? { width: 76 + (lv.length - 2) * 6 } : undefined}>
                <span className="pg-strip-name" title={[label.full, lv.length > 2 ? tn(lv.length, "{n} channel", "{n} channels") : ""].filter(Boolean).join(" · ")}>
                  {k + 1}
                  <small>{label.short}</small>
                </span>
                {tr.on ? <Verdict peak={peak} /> : <span className="pg-verdict off">{tc("track", "Off")}</span>}
                <span className="pg-strip-body">
                  <Fader value={tr.gain} min={-40} max={12} step={1} disabled={!run || !tr.on} label={t("Track {n}", { n: k + 1 })} onChange={(gain) => setTrack(k, { gain })} onReset={() => setTrack(k, { gain: 0 })} />
                  <Scale marks={GAIN_MARKS} share={gainShare} fader />
                  <Vu levels={lv} />
                </span>
                <b className="pg-db">
                  {tr.gain > 0 ? "+" : ""}
                  {tr.gain}
                </b>
                <span className="pg-strip-tools">
                  <button type="button" className={`pg-tool ${solo ? "on" : ""}`} aria-pressed={solo} data-tip={solo ? t("Back to the output in the headphones") : t("Hear alone in the headphones")} onClick={() => monitor.setSolo(solo ? null : k)}>
                    <Headphones size={12} />
                  </button>
                  <button type="button" className="pg-tool" disabled={!run || !tr.on} data-tip={t("Set the level by itself")} onClick={() => auto(k)}>
                    <Wand2 size={12} />
                  </button>
                </span>
                <button type="button" className={`pg-on ${tr.on ? "on" : ""}`} disabled={!run} aria-pressed={tr.on} onClick={() => setTrack(k, { on: !tr.on })}>
                  {tr.on ? "ON" : "OFF"}
                </button>
              </div>
            );
          })}
        </div>
        <div className="pg-strip-group end">
          {n > 0 && (
            <div className="pg-strip master">
              <span className="pg-strip-name">
                {t("Output")}
                <small>{tc("channels", CHANNEL_LABEL[p.channels])}</small>
              </span>
              {p.imageOn ? <span className="pg-verdict off">{t("Image")}</span> : <Verdict peak={outPeak} />}
              <span className="pg-strip-body">
                <Fader value={p.master} min={-40} max={12} step={1} disabled={!run} label={t("Output")} onChange={(master) => prog.set({ master })} onReset={() => prog.set({ master: 0 })} />
                <Scale marks={GAIN_MARKS} share={gainShare} fader />
                <Vu levels={out} />
              </span>
              <b className="pg-db">
                {p.master > 0 ? "+" : ""}
                {p.master}
              </b>
              <span className="pg-on-space" />
            </div>
          )}
          <div className="pg-strip phones">
            <span className="pg-strip-name">
              <span className="pg-phones-title">
                <Headphones size={13} />
                {t("Headphones")}
              </span>
              <small>{monitor.solo !== null ? t("Track {n}", { n: monitor.solo + 1 }) : t("Output")}</small>
            </span>
            <span className="pg-strip-body">
              <Fader value={monitor.muted ? 0 : monitor.volume} min={0} max={1} step={0.01} label={t("Headphones")} onChange={(v) => {
                monitor.setVolume(v);
                if (monitor.muted) monitor.setMuted(false);
              }} />
            </span>
            <b className="pg-db">{Math.round((monitor.muted ? 0 : monitor.volume) * 100)} %</b>
            <button type="button" className={`pg-on ${monitor.muted ? "mute" : ""}`} aria-pressed={monitor.muted} onClick={() => monitor.setMuted(!monitor.muted)}>
              {monitor.muted ? <VolumeX size={12} /> : <Volume2 size={12} />}
            </button>
          </div>
        </div>
      </div>
    </section>
  );
}

type Tab = "image" | "overlay";

/** The source's pictures: its image, its overlay. */
export function SettingsModal({ rec, prog, onClose }: { rec: Recorder; prog: ProgramApi; onClose: () => void }) {
  const run = useApp((s) => can.has(s.auth?.user, "stream.run"));
  const [tab, setTab] = useState<Tab>("image");
  useEffect(() => {
    const key = (e: KeyboardEvent) => e.key === "Escape" && onClose();
    window.addEventListener("keydown", key);
    return () => window.removeEventListener("keydown", key);
  }, [onClose]);
  if (!prog.state) return null;
  const p = prog.state.program;
  const tabs: [Tab, string][] = [
    ["image", t("Image")],
    ["overlay", t("Overlay")],
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
          {tab === "image" && <Gallery use="image" source={rec.id} chosen={p.image} onChoose={(image) => prog.set({ image })} fallback={fallback} disabled={!run} />}
          {tab === "overlay" && <Gallery use="overlay" source={rec.id} chosen={p.overlay} onChoose={(overlay) => prog.set({ overlay, ...(overlay ? {} : { overlayOn: false }) })} fallback={{ name: tc("overlay", "None"), pic: null }} disabled={!run} />}
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
