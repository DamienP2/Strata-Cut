/*
 * The drawn list (never a native <select>). The list opens in a portal under
 * the button (above it when there is no room below), in "rich" mode (two
 * lines, check on the right) when an option has an icon or a sub-line.
 */
import { useEffect, useLayoutEffect, useRef, useState, type CSSProperties, type ReactNode } from 'react';
import { createPortal } from 'react-dom';
import { Check, ChevronDown } from 'lucide-react';
import { t } from '../i18n';

export interface DropOption<T extends string = string> {
  value: T;
  label: ReactNode;
  /** Group title, shown when it changes. */
  group?: string;
  /** Small mono text on the right. */
  note?: string;
  style?: CSSProperties;
  /** Second line (rich mode); for a disabled option, why. */
  sub?: string;
  icon?: ReactNode;
  /** Mono text after the label in the button. */
  detail?: string;
  disabled?: boolean;
}

export interface DropdownProps<T extends string> {
  value: T;
  options: DropOption<T>[];
  onChange: (v: T) => void;
  title?: string;
  placeholder?: string;
  className?: string;
  disabled?: boolean;
}

interface Place {
  left: number;
  top?: number;
  bottom?: number;
  minWidth: number;
  up: boolean;
}

const text = (n: ReactNode) => (typeof n === 'string' || typeof n === 'number' ? String(n) : '');

export function Dropdown<T extends string>({ value, options, onChange, title, placeholder = '—', className, disabled }: DropdownProps<T>) {
  const [open, setOpen] = useState(false);
  const [hover, setHover] = useState(-1);
  const [place, setPlace] = useState<Place | null>(null);
  const btn = useRef<HTMLButtonElement>(null);
  const list = useRef<HTMLDivElement>(null);
  const typed = useRef({ s: '', at: 0 });
  const rich = options.some((o) => o.icon !== undefined || o.sub !== undefined);
  const current = options.find((o) => o.value === value);

  const enabled = (i: number) => i >= 0 && i < options.length && !options[i].disabled;
  const step = (from: number, dir: 1 | -1) => {
    for (let i = from + dir; i >= 0 && i < options.length; i += dir) if (enabled(i)) return i;
    return from;
  };

  const show = () => {
    if (disabled || !btn.current) return;
    const r = btn.current.getBoundingClientRect();
    const below = innerHeight - r.bottom;
    const up = below < 240 && r.top > below;
    setPlace({ left: r.left, minWidth: Math.max(rich ? 260 : 160, r.width), up, ...(up ? { bottom: innerHeight - r.top + 4 } : { top: r.bottom + 4 }) });
    const i = options.findIndex((o) => o.value === value);
    setHover(enabled(i) ? i : step(-1, 1));
    setOpen(true);
  };

  const close = (focus = false) => {
    setOpen(false);
    if (focus) btn.current?.focus();
  };

  const pick = (i: number) => {
    if (!enabled(i)) return;
    close(true);
    if (options[i].value !== value) onChange(options[i].value);
  };

  // Keep the list inside the window (8 px margin).
  useLayoutEffect(() => {
    if (!open || !list.current || !place) return;
    const r = list.current.getBoundingClientRect();
    if (r.right > innerWidth - 8) list.current.style.left = `${Math.max(8, innerWidth - 8 - r.width)}px`;
  }, [open, place]);

  // The hovered option scrolls into view (centred when the list opens).
  const opened = useRef(false);
  useEffect(() => {
    if (!open) {
      opened.current = false;
      return;
    }
    if (hover < 0) return;
    list.current?.querySelector(`[data-i="${hover}"]`)?.scrollIntoView({ block: opened.current ? 'nearest' : 'center' });
    opened.current = true;
  }, [open, hover]);

  // Close on a press elsewhere, the wheel outside the list, a resize; keys while open.
  useEffect(() => {
    if (!open) return;
    const outside = (e: Event) => {
      const n = e.target as Node;
      if (list.current?.contains(n) || btn.current?.contains(n)) return;
      close();
    };
    const wheel = (e: Event) => {
      if (!list.current?.contains(e.target as Node)) close();
    };
    const resize = () => close();
    document.addEventListener('pointerdown', outside, true);
    document.addEventListener('wheel', wheel, { capture: true, passive: true });
    addEventListener('resize', resize);
    return () => {
      document.removeEventListener('pointerdown', outside, true);
      document.removeEventListener('wheel', wheel, true);
      removeEventListener('resize', resize);
    };
  });

  const onKey = (e: React.KeyboardEvent) => {
    if (!open) {
      if (e.key === 'ArrowDown' || e.key === 'Enter' || e.key === ' ') {
        e.preventDefault();
        e.stopPropagation();
        show();
      }
      return;
    }
    e.stopPropagation();
    if (e.key === 'Escape') {
      e.preventDefault();
      close(true);
    } else if (e.key === 'ArrowDown') {
      e.preventDefault();
      setHover((h) => step(h, 1));
    } else if (e.key === 'ArrowUp') {
      e.preventDefault();
      setHover((h) => step(h, -1));
    } else if (e.key === 'Home') {
      e.preventDefault();
      setHover(step(-1, 1));
    } else if (e.key === 'End') {
      e.preventDefault();
      setHover(step(options.length, -1));
    } else if (e.key === 'Enter' || e.key === ' ') {
      e.preventDefault();
      pick(hover);
    } else if (e.key === 'Tab') {
      close();
    } else if (e.key.length === 1) {
      // Type the first letters to jump to an option.
      const now = Date.now();
      typed.current = { s: (now - typed.current.at < 700 ? typed.current.s : '') + e.key.toLowerCase(), at: now };
      const i = options.findIndex((o, k) => enabled(k) && text(o.label).toLowerCase().startsWith(typed.current.s));
      if (i >= 0) setHover(i);
    }
  };

  let lastGroup: string | undefined;
  return (
    <>
      <button
        ref={btn}
        type="button"
        className={`dd-btn ${open ? 'open' : ''} ${className ?? ''}`}
        title={title}
        disabled={disabled}
        aria-haspopup="listbox"
        aria-expanded={open}
        onPointerDown={(e) => e.stopPropagation()}
        onClick={() => (open ? close() : show())}
        onKeyDown={onKey}
      >
        {current?.icon !== undefined && <span className="dd-icon">{current.icon}</span>}
        <span className="dd-label" style={current?.style}>
          {current ? current.label : placeholder}
        </span>
        {current?.detail && <span className="dd-detail">{current.detail}</span>}
        <ChevronDown size={12} />
      </button>
      {open &&
        place &&
        createPortal(
          <div
            ref={list}
            className={`dd-list ${rich ? 'rich' : ''} ${place.up ? 'up' : ''}`}
            role="listbox"
            style={{ left: place.left, top: place.top, bottom: place.bottom, minWidth: place.minWidth }}
            onPointerDown={(e) => {
              e.preventDefault(); // the focus stays on the button
              e.stopPropagation(); // React bubbles portals to their owners
            }}
          >
            {options.length === 0 && <div className="dd-empty">{t('Nothing to choose')}</div>}
            {options.map((o, i) => {
              const head = o.group !== undefined && o.group !== lastGroup ? o.group : null;
              lastGroup = o.group;
              const on = o.value === value;
              const check = <span className="dd-check">{on && <Check size={13} />}</span>;
              return (
                <div key={`${o.value}|${i}`}>
                  {head && <div className="dd-group">{head}</div>}
                  <div
                    data-i={i}
                    role="option"
                    aria-selected={on}
                    aria-disabled={o.disabled || undefined}
                    className={`dd-opt ${on ? 'on' : ''} ${i === hover ? 'hover' : ''} ${o.disabled ? 'off' : ''}`}
                    onPointerEnter={() => enabled(i) && setHover(i)}
                    onClick={() => pick(i)}
                  >
                    {!rich && check}
                    {o.icon !== undefined && <span className="dd-icon">{o.icon}</span>}
                    {rich ? (
                      <span className="dd-text">
                        <span className="dd-main" style={o.style}>
                          {o.label}
                        </span>
                        {o.sub && <span className="dd-sub">{o.sub}</span>}
                      </span>
                    ) : (
                      <span className="dd-text" style={o.style}>
                        {o.label}
                      </span>
                    )}
                    {o.note && <span className="dd-note">{o.note}</span>}
                    {rich && check}
                  </div>
                </div>
              );
            })}
          </div>,
          document.body,
        )}
    </>
  );
}
