/*
 * Extraction: the clip seen before it is made, in a window: played from its IN to its OUT
 * (stopped on the OUT), as it will be — the picture with its sound, or the sound alone.
 */
import { useEffect, useRef, useState } from 'react';
import { AudioLines, RotateCcw, X } from 'lucide-react';
import { serverNow } from '../store';
import { t } from '../i18n';
import { LoopPlayer } from '../player';
import { duration, timecode } from '../tc';
import { Meters, VolumeControl, useAudioOut } from '../ui/AudioUi';
import { Transport, useTransportKeys, type TransportActs } from '../ui/Transport';

interface Point {
  wall: number;
}

export function ClipPreview({ recId, name, from, to, fps, timezone, sound, onClose }: { recId: string; name: string; from: Point; to: Point; fps: number; timezone: string; sound: boolean; onClose: () => void }) {
  const video = useRef<HTMLVideoElement>(null);
  const player = useRef<LoopPlayer | null>(null);
  const audio = useAudioOut(video, 'extract');
  const [at, setAt] = useState(from.wall);
  const [playing, setPlaying] = useState(false);
  const end = to.wall + 1000 / fps;
  const length = Math.max(1, end - from.wall);

  // Its own player: from the IN, playing, stopped on the OUT.
  useEffect(() => {
    const v = video.current!;
    const p = new LoopPlayer(v, recId, serverNow);
    player.current = p;
    p.onFrame = (wall) => {
      setAt(wall);
      // Its ends: stopped on the OUT (forwards) or on the IN (backwards).
      const back = p.playing && p.video.paused;
      if (p.playing && (back ? wall <= from.wall : wall >= to.wall)) {
        p.pause();
        setPlaying(false);
      }
    };
    const onPlay = () => setPlaying(true);
    const onPause = () => setPlaying(p.playing);
    v.addEventListener('play', onPlay);
    v.addEventListener('pause', onPause);
    void p.seekWall(from.wall, true);
    return () => {
      v.removeEventListener('play', onPlay);
      v.removeEventListener('pause', onPause);
      p.destroy();
      player.current = null;
    };
  }, [recId, from.wall, to.wall]); // eslint-disable-line react-hooks/exhaustive-deps

  const replay = () => {
    setRate(1);
    void player.current?.seekWall(from.wall, true);
  };
  const toggle = () => {
    const p = player.current;
    if (!p) return;
    setRate(1);
    if (p.playing) p.pause();
    else if (at >= to.wall) replay();
    else p.play();
  };
  /** Shows a moment of the clip (kept within it), playing on if it played. */
  const go = (wall: number, play = false) => void player.current?.seekWall(Math.min(Math.max(wall, from.wall), to.wall), play);
  const [rate, setRate] = useState(1);
  const acts: TransportActs = {
    toggle,
    normal: () => {
      setRate(1);
      player.current?.play(1);
    },
    jump: (s) => {
      const p = player.current;
      if (p) go(at + s * 1000, !p.video.paused);
    },
    step: (n) => {
      setRate(1);
      player.current?.step(n);
    },
    fast: (r) => {
      const p = player.current;
      if (!p) return;
      setRate(r);
      if (r > 0) {
        if (at >= to.wall) void p.seekWall(from.wall).then(() => p.play(r));
        else p.play(r);
      } else {
        p.playReverse(-r);
        setPlaying(true);
      }
    },
    toIn: () => go(from.wall),
    toOut: () => go(to.wall),
    mute: () => audio.setMuted(!audio.muted),
  };
  useTransportKeys(acts, { rate, playing }, true, true);
  // Escape: closed.
  useEffect(() => {
    const onKey = (e: KeyboardEvent) => {
      if (e.key !== 'Escape') return;
      e.preventDefault();
      e.stopPropagation();
      onClose();
    };
    addEventListener('keydown', onKey, true);
    return () => removeEventListener('keydown', onKey, true);
  });
  // The playhead dragged: the picture follows, played on after if it played.
  const drag = useRef<{ was: boolean } | null>(null);
  const scrubTo = (e: React.PointerEvent<HTMLDivElement>) => {
    const r = e.currentTarget.getBoundingClientRect();
    go(from.wall + Math.min(1, Math.max(0, (e.clientX - r.left) / r.width)) * length);
  };

  const done = Math.min(1, Math.max(0, (at - from.wall) / length));
  const frames = Math.round(((end - from.wall) / 1000) * fps);
  const elapsed = Math.round(((Math.min(at, end) - from.wall) / 1000) * fps);
  return (
    <div className="modal-back" onPointerDown={onClose}>
      <div className="modal sc-clip-modal sc-preview-modal" role="dialog" aria-labelledby="preview-title" onPointerDown={(e) => e.stopPropagation()}>
        <header>
          <h2 id="preview-title">
            {t('Preview')} · {name}
          </h2>
          <span className={`sc-kind-tag ${sound ? 'audio' : 'video'}`}>{sound ? t('Sound') : t('Video')}</span>
          <button type="button" className="icon-btn ghost" title={t('Close')} onClick={onClose}>
            <X size={16} />
          </button>
        </header>
        <div className="sc-clip-view" onClick={toggle}>
          <video ref={video} className="sc-video" playsInline />
          {sound && (
            <span className="sc-clip-sound solid">
              <AudioLines size={56} strokeWidth={1.5} />
            </span>
          )}
          <Meters out={audio.out} />
        </div>
        {/* The playhead: dragged, or a click, goes there. */}
        <div
          className="sc-scrub"
          role="slider"
          aria-label={t('Playhead')}
          aria-valuemin={0}
          aria-valuemax={frames}
          aria-valuenow={elapsed}
          onPointerDown={(e) => {
            const p = player.current;
            e.currentTarget.setPointerCapture(e.pointerId);
            drag.current = { was: !!p && !p.video.paused };
            p?.halt();
            scrubTo(e);
          }}
          onPointerMove={(e) => drag.current && scrubTo(e)}
          onPointerUp={() => {
            if (drag.current?.was) player.current?.play(1);
            drag.current = null;
          }}
        >
          <span className="sc-scrub-done" style={{ width: `${done * 100}%` }} />
          <span className="sc-scrub-head" style={{ left: `${done * 100}%` }} />
        </div>
        <div className="sc-clip-bar">
          <div className="sc-tc-big">{timecode(Math.min(at, to.wall), fps, timezone)}</div>
          <span className="muted sc-clip-time">
            {duration(elapsed, fps)} / {duration(frames, fps)}
          </span>
          <div className="spacer" />
          <Transport acts={acts} rate={rate} playing={playing} hasIn hasOut />
          <button className="ctl" data-tip={t('From the IN again')} onClick={replay}>
            <RotateCcw size={17} />
          </button>
          <div className="spacer" />
          <VolumeControl volume={audio.volume} setVolume={audio.setVolume} muted={audio.muted} setMuted={audio.setMuted} />
        </div>
      </div>
    </div>
  );
}
