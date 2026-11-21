/*
 * Fields: Segmented (instead of radio buttons and short lists), NumberField
 * (drag the label, wheel, arrows, simple expressions), TextInput (local value,
 * commit on blur).
 */
import { useEffect, useLayoutEffect, useRef, useState, type ReactNode } from 'react';
import { lang, t } from '../i18n';

/* ---------- Segmented ---------- */

export interface SegOption<T extends string> {
  value: T;
  label: ReactNode;
  title?: string;
  disabled?: boolean;
}

export function Segmented<T extends string>({ value, options, onChange, className }: { value: T; options: SegOption<T>[]; onChange: (v: T) => void; className?: string }) {
  return (
    <div className={className ? `segmented ${className}` : 'segmented'} role="group">
      {options.map((o) => (
        <button key={o.value} type="button" title={o.title} disabled={o.disabled} aria-pressed={o.value === value} className={o.value === value ? 'on' : ''} onClick={() => onChange(o.value)}>
          {o.label}
        </button>
      ))}
    </div>
  );
}

/* ---------- Wheel and trackpad ---------- */

let lastScroll = 0;
let scrollWatch = false;
function watchScroll() {
  if (scrollWatch) return;
  scrollWatch = true;
  addEventListener('scroll', () => (lastScroll = performance.now()), { capture: true, passive: true });
}

/**
 * Steps for one wheel event over a field, or 0 when the page should scroll:
 * the field arms once the pointer rested on it 120 ms and the page did not
 * scroll for 300 ms. A wheel notch is one step, a trackpad adds up 16 px per step.
 */
function wheelSteps(e: WheelEvent, enteredAt: number, acc: { v: number }): number {
  const now = performance.now();
  if (now - enteredAt < 120 || now - lastScroll < 300) return 0;
  const notch = e.deltaMode !== 0 || (Math.abs(e.deltaY) >= 50 && Number.isInteger(e.deltaY));
  if (notch) return e.deltaY < 0 ? 1 : e.deltaY > 0 ? -1 : 0;
  acc.v -= e.deltaY + e.deltaX;
  const n = Math.trunc(acc.v / 16);
  acc.v -= n * 16;
  return n || NaN; // NaN: armed but not a full step yet (still no page scroll)
}

const factor = (e: { shiftKey: boolean; altKey: boolean }) => (e.shiftKey ? 10 : e.altKey ? 0.1 : 1);

/** Applies the same wheel to every <input type="number">. */
export function installNumberWheel() {
  watchScroll();
  let el: HTMLInputElement | null = null;
  let enteredAt = 0;
  const acc = { v: 0 };
  document.addEventListener('pointerover', (e) => {
    const t = e.target as HTMLElement;
    const input = t instanceof HTMLInputElement && t.type === 'number' && !t.disabled && !t.readOnly ? t : null;
    if (input !== el) {
      el = input;
      enteredAt = performance.now();
      acc.v = 0;
    }
  });
  document.addEventListener(
    'wheel',
    (e) => {
      if (!el || e.target !== el) return;
      const n = wheelSteps(e, enteredAt, acc);
      if (n === 0) return;
      e.preventDefault();
      if (Number.isNaN(n)) return;
      const step = Number(el.step) || 1;
      let v = (Number(el.value) || 0) + n * step * factor(e);
      if (el.min !== '') v = Math.max(Number(el.min), v);
      if (el.max !== '') v = Math.min(Number(el.max), v);
      const set = Object.getOwnPropertyDescriptor(HTMLInputElement.prototype, 'value')?.set;
      set?.call(el, String(Math.round(v * 1e6) / 1e6));
      el.dispatchEvent(new Event('input', { bubbles: true }));
    },
    { passive: false },
  );
}

/* ---------- NumberField ---------- */

/** Evaluates a simple expression: numbers, + - * / and parentheses (« 1920/2 », « 100+20 »). */
export function evalExpr(src: string): number | null {
  const s = src.replace(/\s+/g, '').replace(/,/g, '.');
  if (!s || !/^[-+*/().\d]+$/.test(s)) return null;
  let i = 0;
  const num = (): number => {
    if (s[i] === '(') {
      i++;
      const v = sum();
      if (s[i] !== ')') throw new Error();
      i++;
      return v;
    }
    if (s[i] === '-') {
      i++;
      return -num();
    }
    if (s[i] === '+') {
      i++;
      return num();
    }
    const m = /^\d*\.?\d+(e[-+]?\d+)?|^\d+\.?/.exec(s.slice(i));
    if (!m) throw new Error();
    i += m[0].length;
    return parseFloat(m[0]);
  };
  const prod = (): number => {
    let v = num();
    while (s[i] === '*' || s[i] === '/') {
      const op = s[i++];
      const r = num();
      v = op === '*' ? v * r : v / r;
    }
    return v;
  };
  const sum = (): number => {
    let v = prod();
    while (s[i] === '+' || s[i] === '-') {
      const op = s[i++];
      const r = prod();
      v = op === '+' ? v + r : v - r;
    }
    return v;
  };
  try {
    const v = sum();
    return i === s.length && Number.isFinite(v) ? v : null;
  } catch {
    return null;
  }
}

export interface NumberFieldProps {
  label?: ReactNode;
  value: number;
  onChange: (v: number) => void;
  /** Step of the arrows, the wheel and the drag, in displayed units. */
  step?: number;
  /** Shown value = value × display (100: a 0..1 value as a percentage). */
  display?: number;
  /** Decimals shown. */
  precision?: number;
  suffix?: string;
  min?: number;
  max?: number;
  title?: string;
  disabled?: boolean;
  className?: string;
}

export function NumberField({ label, value, onChange, step = 1, display = 1, precision = 0, suffix, min, max, title, disabled, className }: NumberFieldProps) {
  const shown = (v: number) => {
    const s = (v * display).toFixed(precision);
    return lang() === 'fr' ? s.replace('.', ',') : s;
  };
  const [text, setText] = useState(() => shown(value));
  const [editing, setEditing] = useState(false);
  const input = useRef<HTMLInputElement>(null);
  const live = useRef({ value, onChange });
  live.current = { value, onChange };

  useLayoutEffect(() => {
    if (!editing) setText(shown(value));
  }, [value, editing, display, precision]);

  const clamp = (v: number) => Math.min(max ?? Infinity, Math.max(min ?? -Infinity, v));
  const round = (v: number) => {
    const p = 10 ** (precision + Math.round(Math.log10(display)));
    return Math.round(v * p) / p;
  };
  /** Sets a new value (in value units); unchanged = nothing sent. */
  const send = (v: number) => {
    const next = clamp(round(v));
    if (next !== live.current.value) live.current.onChange(next);
    return next;
  };
  const nudge = (steps: number) => send(live.current.value + (steps * step) / display);

  const commit = () => {
    const v = evalExpr(text);
    setEditing(false);
    if (v === null) setText(shown(value));
    else setText(shown(send(v / display)));
  };

  // Wheel / trackpad without clicking.
  useEffect(() => {
    watchScroll();
    const el = input.current?.closest('.num-field') as HTMLElement | null;
    if (!el || disabled) return;
    let enteredAt = 0;
    const acc = { v: 0 };
    const enter = () => {
      enteredAt = performance.now();
      acc.v = 0;
    };
    const wheel = (e: WheelEvent) => {
      const n = wheelSteps(e, enteredAt, acc);
      if (n === 0) return;
      e.preventDefault();
      if (!Number.isNaN(n)) nudge(n * factor(e));
    };
    el.addEventListener('pointerenter', enter);
    el.addEventListener('wheel', wheel, { passive: false });
    return () => {
      el.removeEventListener('pointerenter', enter);
      el.removeEventListener('wheel', wheel);
    };
  });

  // Drag the label horizontally.
  const drag = useRef<{ x: number; v: number; id: number } | null>(null);
  const onScrubDown = (e: React.PointerEvent<HTMLSpanElement>) => {
    if (disabled || e.button !== 0) return;
    e.preventDefault();
    e.currentTarget.setPointerCapture(e.pointerId);
    drag.current = { x: e.clientX, v: live.current.value, id: e.pointerId };
    document.body.classList.add('resizing-x');
  };
  const onScrubMove = (e: React.PointerEvent<HTMLSpanElement>) => {
    const d = drag.current;
    if (!d || d.id !== e.pointerId) return;
    const dx = e.clientX - d.x;
    send(d.v + (dx * step * factor(e)) / display);
  };
  const onScrubUp = () => {
    drag.current = null;
    document.body.classList.remove('resizing-x');
  };

  const hint = t('Wheel or trackpad to adjust · Shift ×10 · Alt ×0.1');
  return (
    <label className={`num-field ${className ?? ''}`} title={title ? `${title}\n${hint}` : hint}>
      {label !== undefined && (
        <span className="scrub" onPointerDown={onScrubDown} onPointerMove={onScrubMove} onPointerUp={onScrubUp} onPointerCancel={onScrubUp} onClick={(e) => e.preventDefault()}>
          {label}
        </span>
      )}
      <input
        ref={input}
        type="text"
        inputMode="decimal"
        value={text}
        disabled={disabled}
        onFocus={(e) => {
          setEditing(true);
          e.currentTarget.select();
        }}
        onChange={(e) => {
          setEditing(true);
          setText(e.target.value);
        }}
        onBlur={commit}
        onKeyDown={(e) => {
          if (e.key !== 'Escape' && e.key !== 'Tab') e.stopPropagation();
          if (e.key === 'Enter') {
            e.preventDefault();
            e.currentTarget.blur();
          } else if (e.key === 'ArrowUp' || e.key === 'ArrowDown') {
            e.preventDefault();
            setEditing(false);
            nudge((e.key === 'ArrowUp' ? 1 : -1) * factor(e));
          } else if (e.key === 'Escape') {
            setText(shown(value));
            setEditing(false);
            e.currentTarget.blur();
          }
        }}
      />
      {suffix !== undefined && <span className="suffix">{suffix}</span>}
    </label>
  );
}

/* ---------- TextInput ---------- */

export interface TextInputProps extends Omit<React.InputHTMLAttributes<HTMLInputElement>, 'value' | 'onChange'> {
  value: string;
  onChange: (v: string) => void;
  /** Send the value only when leaving the field (Enter leaves it). */
  commitOnBlur?: boolean;
}

/** A text field whose keys never reach the shortcuts. */
export function TextInput({ value, onChange, commitOnBlur, onKeyDown, onBlur, ...rest }: TextInputProps) {
  const [local, setLocal] = useState(value);
  const focused = useRef(false);
  useEffect(() => {
    if (!focused.current || !commitOnBlur) setLocal(value);
  }, [value, commitOnBlur]);

  const commit = () => {
    if (commitOnBlur && local !== value) onChange(local);
  };
  const change = (v: string) => {
    setLocal(v);
    if (!commitOnBlur) onChange(v);
  };
  const keys = (e: React.KeyboardEvent<HTMLInputElement>) => {
    if (e.key !== 'Escape' && e.key !== 'Tab') e.stopPropagation();
    if (e.key === 'Enter' && commitOnBlur) {
      e.preventDefault();
      e.currentTarget.blur();
    }
    onKeyDown?.(e);
  };

  return (
    <input
      {...rest}
      value={local}
      onFocus={(e) => {
        focused.current = true;
        rest.onFocus?.(e);
      }}
      onChange={(e) => change(e.target.value)}
      onBlur={(e) => {
        focused.current = false;
        commit();
        onBlur?.(e);
      }}
      onKeyDown={keys}
    />
  );
}
