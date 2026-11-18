// Checks a clip in the page: image by image, with its sound and VU-meters.
import { useCallback, useEffect, useRef, useState } from 'react';
import { ChevronFirst, ChevronLast, Download, Pause, Play, StepBack, StepForward, X } from 'lucide-react';
import { useApp } from '../store';
import { locale, t } from '../i18n';
import { day, duration, fpsOf, size, timecode } from '../tc';
import { Meters, VolumeControl, useAudioOut } from '../ui/AudioUi';
import type { Clip } from '../types';

export function ClipPlayer({ clip, onClose }: { clip: Clip; onClose: () => void }) {
  const timezone = useApp((s) => s.timezone);
  const fps = fpsOf(clip.fpsNum, clip.fpsDen);
  const last = Math.max(0, clip.frames - 1);
  const video = useRef<HTMLVideoElement>(null);
  const audio = useAudioOut(video);
  const [frame, setFrame] = useState(0);
  const [playing, setPlaying] = useState(false);
  const frameOf = useCallback((time: number) => Math.min(last, Math.max(0, Math.floor(time * fps + 1e-3))), [fps, last]);

  // While playing, follow the frames shown.
  useEffect(() => {
    const v = video.current as (HTMLVideoElement & { requestVideoFrameCallback?: (cb: (n: number, m: { mediaTime: number }) => void) => number; cancelVideoFrameCallback?: (h: number) => void }) | null;
    if (!v) return;
    let h = 0;
    const tick = () => {
      if (v.requestVideoFrameCallback) h = v.requestVideoFrameCallback((_n, m) => {
        setFrame(frameOf(m.mediaTime));
        tick();
      });
    };
    tick();
    const onTime = () => !v.requestVideoFrameCallback && setFrame(frameOf(v.currentTime));
    const onPlay = () => setPlaying(true);
    const onPause = () => setPlaying(false);
    v.addEventListener('timeupdate', onTime);
    v.addEventListener('play', onPlay);
    v.addEventListener('pause', onPause);
    return () => {
      v.cancelVideoFrameCallback?.(h);
      v.removeEventListener('timeupdate', onTime);
      v.removeEventListener('play', onPlay);
      v.removeEventListener('pause', onPause);
    };
  }, [frameOf]);

  const seek = useCallback(
    (n: number) => {
      const v = video.current;
      if (!v) return;
      v.pause();
      const f = Math.min(last, Math.max(0, Math.round(n)));
      setFrame(f);
      // A hair after the frame's start so that this very frame is shown.
      v.currentTime = f / fps + 0.0005;
    },
    [fps, last],
  );
  const toggle = useCallback(() => {
    const v = video.current;
    if (!v) return;
    if (v.paused) {
      if (frame >= last) v.currentTime = 0;
      void v.play().catch(() => {});
    } else seek(frameOf(v.currentTime));
  }, [frame, last, seek, frameOf]);

  useEffect(() => {
    const onKey = (e: KeyboardEvent) => {
      if ((e.target as HTMLElement).closest('input, textarea') || e.ctrlKey || e.metaKey || e.altKey) return;
      const big = Math.round(fps);
      let done = true;
      if (e.key === ' ' || e.key.toLowerCase() === 'k') toggle();
      else if (e.key === 'ArrowLeft') seek(frame - (e.shiftKey ? big : 1));
      else if (e.key === 'ArrowRight') seek(frame + (e.shiftKey ? big : 1));
      else if (e.key === 'Home') seek(0);
      else if (e.key === 'End') seek(last);
      else if (e.key.toLowerCase() === 'm') audio.setMuted(!audio.muted);
      else done = false;
      if (done) {
        e.preventDefault();
        e.stopPropagation();
      }
    };
    addEventListener('keydown', onKey, true);
    return () => removeEventListener('keydown', onKey, true);
  }, [frame, last, fps, seek, toggle, audio]);

  return (
    <div className="modal-back" onPointerDown={onClose}>
      <div className="modal sc-clip-modal" role="dialog" aria-labelledby="clip-title" onPointerDown={(e) => e.stopPropagation()}>
        <header>
          <h2 id="clip-title">{clip.name}</h2>
          <a className="btn small" href={`/api/clips/${clip.id}/download`}>
            <Download size={13} /> {t('Download')}
          </a>
          <button type="button" className="icon-btn ghost" title={t('Close')} onClick={onClose}>
            <X size={16} />
          </button>
        </header>
        <div className="sc-clip-view" onClick={toggle}>
          <video ref={video} className="sc-video" src={`/api/clips/${clip.id}/download?inline=1`} preload="auto" playsInline onLoadedData={() => seek(0)} />
          <Meters out={audio.out} />
        </div>
        <input type="range" className="sc-scrub" min={0} max={last} step={1} value={frame} aria-label={t('Position')} onChange={(e) => seek(Number(e.target.value))} onKeyDown={(e) => e.stopPropagation()} />
        <div className="sc-clip-bar">
          <div className="sc-tc-big">{timecode(clip.start + (frame * 1000) / fps, fps, timezone)}</div>
          <span className="muted sc-clip-count">{t('Frame {n} of {total}', { n: frame + 1, total: clip.frames })}</span>
          <div className="sc-ctrls">
            <button className="icon-btn ghost" title={t('First frame  (Home)')} onClick={() => seek(0)}>
              <ChevronFirst size={16} />
            </button>
            <button className="icon-btn ghost" title={t('Previous frame  (←)')} onClick={() => seek(frame - 1)}>
              <StepBack size={16} />
            </button>
            <button className="icon-btn play" title={t('Play / pause  (K, Space)')} onClick={toggle}>
              {playing ? <Pause size={16} /> : <Play size={16} />}
            </button>
            <button className="icon-btn ghost" title={t('Next frame  (→)')} onClick={() => seek(frame + 1)}>
              <StepForward size={16} />
            </button>
            <button className="icon-btn ghost" title={t('Last frame  (End)')} onClick={() => seek(last)}>
              <ChevronLast size={16} />
            </button>
          </div>
          <div className="spacer" />
          <VolumeControl volume={audio.volume} setVolume={audio.setVolume} muted={audio.muted} setMuted={audio.setMuted} />
        </div>
        <div className="sc-clip-meta">
          <span>{clip.recorderName}</span>
          <span>{day(clip.start, timezone, locale())}</span>
          <span>
            {t('In')} {timecode(clip.start, fps, timezone)}
          </span>
          <span>
            {t('Duration')} {duration(clip.frames, fps)}
          </span>
          <span>{clip.format}</span>
          <span>{size(clip.size, locale(), locale() === 'fr-FR')}</span>
        </div>
      </div>
    </div>
  );
}
