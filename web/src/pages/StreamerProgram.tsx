/*
 * Streamer: the programme of a source — what its lives send. The source or its image, its PNG
 * overlay, passed together at once or by a fade (made ready in `StreamerPass`), the sound of its
 * tracks; its pictures (image, overlay) in the settings' window. The window on air shows the
 * programme as it goes out (the image and the overlay drawn here over the source, with the same
 * fades).
 */
import { useCallback, useEffect, useRef, useState } from "react";
import { Headphones, Volume2, VolumeX, Wand2, X } from "lucide-react";
import { t, tc, tn } from "../i18n";
import { api, can, useApp, useChanged } from "../store";
import type { Recorder } from "../types";
import { DefaultSlate, Gallery, pictureUrl } from "./StreamerSlate";
import { trackLabel } from "../audioLabel";
import { showError } from "../ui/serverError";

export interface Track {
  on: boolean;
  gain: number;
}
export interface Program {
  image: string | null;
  imageOn: boolean;
  /** The image's AUTO, s. */
  fadeSecs: number;
  /** The overlay's AUTO, s. */
  overlayFadeSecs?: number;
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

const CHANNEL_LABEL: Record<Program["channels"], string> = { lr: "L+R", l: "L", r: "R", mono: "Mono" };

const trackOf = (p: Program, k: number): Track => p.tracks[k] ?? { on: k === 0, gain: 0 };

/** The programme of a source: read, changed (sent a moment after the last change), switched. */
export function useProgram(recId: string) {
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
    showError(e);
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
  /** On air: the source or the image, the overlay (on or not, where), by a fade of `fade` s (0: at once). */
  const pass = (next: Next, fade: number) => {
    if (!state) return;
    const p = state.program;
    setLastFade(fade);
    const o = next.overlay;
    setState({ ...state, program: { ...p, imageOn: next.image, overlayOn: o.on && !!p.overlay, x: o.x, y: o.y, size: o.size } });
    api("POST", `/api/program/${recId}/take`, { image: next.image, overlay: o, fade }).catch(fail);
  };
  return { state, set, pass, lastFade };
}
export type ProgramApi = ReturnType<typeof useProgram>;

/** What is made ready to go on air: the source or the image, the overlay (on or not, where). */
export interface Next {
  image: boolean;
  overlay: { on: boolean; x: number; y: number; size: number };
}

/** Over the source's picture: the image, as it goes out (with its fade). */
export function ProgramLayer({ rec, prog }: { rec: Recorder; prog: ProgramApi }) {
  if (!prog.state) return null;
  const p = prog.state.program;
  return (
    <div className="pg-layer">
      <div className="pg-image" style={{ opacity: p.imageOn ? 1 : 0, transition: `opacity ${prog.lastFade}s linear` }}>
        {p.image ? <img src={pictureUrl(p.image)} alt="" /> : <DefaultSlate name={rec.name} />}
      </div>
    </div>
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

export type Tab = "image" | "overlay";

/** The source's pictures: its image, its overlay. */
export function PicturesModal({ rec, prog, use, onClose, onPlace }: { rec: Recorder; prog: ProgramApi; use: Tab; onClose: () => void; onPlace: (p: { x: number; y: number; size: number }) => void }) {
  const run = useApp((s) => can.has(s.auth?.user, "stream.run"));
  const width = useApp((s) => s.status[rec.id]?.media?.width) || 1920;
  if (!prog.state) return null;
  const p = prog.state.program;
  // The overlay at its own size (its pixels on the programme's), or in the middle.
  const original = () => {
    if (!p.overlay) return;
    const i = new Image();
    i.onload = () => {
      const size = Math.min(100, Math.max(2, (i.naturalWidth / width) * 100));
      onPlace({ x: Math.min(p.x, 1 - size / 100), y: p.y, size });
    };
    i.src = pictureUrl(p.overlay);
  };
  return (
    <div className="modal-back" onPointerDown={onClose}>
      <div className="modal pg-settings" onPointerDown={(e) => e.stopPropagation()}>
        <header>
          <h2>{use === "image" ? t("Image") : t("Overlay")}</h2>
          <button type="button" className="icon-btn ghost" title={t("Close")} onClick={onClose}>
            <X size={16} />
          </button>
        </header>
        <div className="pg-settings-body">
          {use === "image" ? (
            <Gallery use="image" source={rec.id} chosen={p.image} onChoose={(image) => prog.set({ image })} fallback={{ name: t("Default"), pic: <DefaultSlate name={rec.name} /> }} disabled={!run} />
          ) : (
            <>
              <Gallery use="overlay" source={rec.id} chosen={p.overlay} onChoose={(overlay) => prog.set({ overlay, ...(overlay === p.overlay ? {} : { overlayOn: false }) })} fallback={{ name: tc("overlay", "None"), pic: null }} disabled={!run} />
              {p.overlay && (
                <div className="pg-ovl-set">
                  <label>
                    {t("Opacity")}
                    <input type="range" min={10} max={100} value={p.opacity} disabled={!run} onChange={(e) => prog.set({ opacity: Number(e.target.value) })} />
                    <b>{Math.round(p.opacity)} %</b>
                  </label>
                  <button type="button" className="btn small" disabled={!run} onClick={() => onPlace({ x: 0.5 - p.size / 200, y: p.y, size: p.size })}>
                    {t("Centre")}
                  </button>
                  <button type="button" className="btn small" disabled={!run} onClick={original}>
                    {t("Original size")}
                  </button>
                </div>
              )}
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
