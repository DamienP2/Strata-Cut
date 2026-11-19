/*
 * Extraction: the clip seen before it is made, in a window: played from its IN to its OUT
 * (stopped on the OUT), as it will be — the picture with its sound, or the sound alone.
 */
import { useEffect, useRef, useState } from 'react';
import { AudioLines, Pause, Play, RotateCcw, X } from 'lucide-react';
import { serverNow } from '../store';
import { t } from '../i18n';
import { LoopPlayer } from '../player';
import { duration, timecode } from '../tc';
import { Meters, VolumeControl, useAudioOut } from '../ui/AudioUi';

interface Point {
  wall: number;
}

export function ClipPreview({ recId, name, from, to, fps, timezone, sound, onClose }: { recId: string; name: string; from: Point; to: Point; fps: number; timezone: string; sound: boolean; onClose: () => void }) {
  const video = useRef<HTMLVideoElement>(null);
  const player = useRef<LoopPlayer | null>(null);
  const audio = useAudioOut(video);
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
      if (wall >= to.wall && p.playing) {
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

  const replay = () => void player.current?.seekWall(from.wall, true);
  const toggle = () => {
    const p = player.current;
    if (!p) return;
    if (p.playing) p.pause();
    else if (at >= to.wall) replay();
    else p.play();
  };
  // Space / K: play or pause; Escape: close.
  useEffect(() => {
    const onKey = (e: KeyboardEvent) => {
      if (e.key === 'Escape') onClose();
      else if (e.key === ' ' || e.key.toLowerCase() === 'k') toggle();
      else return;
      e.preventDefault();
      e.stopPropagation();
    };
    addEventListener('keydown', onKey, true);
    return () => removeEventListener('keydown', onKey, true);
  });

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
        {/* Where it is in the clip: a click goes there. */}
        <div
          className="sc-preview-bar"
          onClick={(e) => {
            const r = e.currentTarget.getBoundingClientRect();
            void player.current?.seekWall(from.wall + ((e.clientX - r.left) / r.width) * length, true);
          }}
        >
          <span style={{ width: `${done * 100}%` }} />
        </div>
        <div className="sc-clip-bar">
          <div className="sc-tc-big">{timecode(Math.min(at, to.wall), fps, timezone)}</div>
          <span className="muted">
            {duration(elapsed, fps)} / {duration(frames, fps)}
          </span>
          <div className="sc-ctrls">
            <button className="icon-btn ghost" title={t('From the IN again')} onClick={replay}>
              <RotateCcw size={16} />
            </button>
            <button className="icon-btn play" title={t('Play / pause  (K, Space)')} onClick={toggle}>
              {playing ? <Pause size={16} /> : <Play size={16} />}
            </button>
          </div>
          <div className="spacer" />
          <VolumeControl volume={audio.volume} setVolume={audio.setVolume} muted={audio.muted} setMuted={audio.setMuted} />
        </div>
      </div>
    </div>
  );
}
