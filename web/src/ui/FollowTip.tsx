/*
 * The tooltip that follows the pointer: any element with `data-tip` (the action) and
 * optionally `data-keys` (its shortcut, keys separated by spaces: "⇧ I") shows them next
 * to the pointer, at once, moving with it. Mounted once for the whole app.
 */
import { useEffect, useRef, useState } from 'react';

interface Tip {
  text: string;
  keys: string[];
}

export function FollowTip() {
  const [tip, setTip] = useState<Tip | null>(null);
  const box = useRef<HTMLDivElement>(null);
  const at = useRef({ x: 0, y: 0 });
  const frame = useRef(0);

  useEffect(() => {
    const place = () => {
      frame.current = 0;
      const el = box.current;
      if (!el) return;
      const { x, y } = at.current;
      const w = el.offsetWidth;
      const h = el.offsetHeight;
      // Right of and under the pointer; on the other side near the window's edges.
      const left = x + 16 + w > innerWidth - 8 ? x - 12 - w : x + 16;
      const top = y + 20 + h > innerHeight - 8 ? y - 12 - h : y + 20;
      el.style.transform = `translate(${Math.max(8, left)}px, ${Math.max(8, top)}px)`;
    };
    const onMove = (e: PointerEvent) => {
      at.current = { x: e.clientX, y: e.clientY };
      const target = (e.target as Element | null)?.closest?.<HTMLElement>('[data-tip]');
      const text = target?.dataset.tip ?? '';
      const keys = (target?.dataset.keys ?? '').split(' ').filter(Boolean);
      setTip((old) => (!text ? null : old && old.text === text && old.keys.join() === keys.join() ? old : { text, keys }));
      if (!frame.current) frame.current = requestAnimationFrame(place);
    };
    const hide = () => setTip(null);
    addEventListener('pointermove', onMove, { passive: true });
    addEventListener('pointerdown', hide, { passive: true });
    addEventListener('blur', hide);
    document.addEventListener('pointerleave', hide);
    return () => {
      removeEventListener('pointermove', onMove);
      removeEventListener('pointerdown', hide);
      removeEventListener('blur', hide);
      document.removeEventListener('pointerleave', hide);
      cancelAnimationFrame(frame.current);
    };
  }, []);

  // Placed again once its size is known (a new text).
  useEffect(() => {
    if (tip && !frame.current) {
      frame.current = requestAnimationFrame(() => {
        frame.current = 0;
        const el = box.current;
        if (!el) return;
        const { x, y } = at.current;
        const left = x + 16 + el.offsetWidth > innerWidth - 8 ? x - 12 - el.offsetWidth : x + 16;
        const top = y + 20 + el.offsetHeight > innerHeight - 8 ? y - 12 - el.offsetHeight : y + 20;
        el.style.transform = `translate(${Math.max(8, left)}px, ${Math.max(8, top)}px)`;
      });
    }
  }, [tip]);

  if (!tip) return null;
  return (
    <div ref={box} className="follow-tip" role="tooltip">
      <span>{tip.text}</span>
      {tip.keys.length > 0 && (
        <span className="follow-keys">
          {tip.keys.map((k, i) => (
            <kbd key={i} className="keycap">
              {k}
            </kbd>
          ))}
        </span>
      )}
    </div>
  );
}
