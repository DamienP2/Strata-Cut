/*
 * The « … » / right-click menu: a `.menu` in a portal, under the pointer (above
 * it when it does not fit), always inside the window (8 px margin). Escape, a
 * press elsewhere or a resize close it.
 *
 *   onContextMenu={(e) => openContextMenu(e, [{ label: t('Rename'), icon: <Pencil size={14} />, onClick: … }, 'sep', { label: t('Delete'), danger: true, onClick: … }])}
 */
import { useEffect, useLayoutEffect, useRef, useState, type ReactNode } from 'react';
import { createPortal } from 'react-dom';
import { create } from 'zustand';

export interface MenuItem {
  label: string;
  icon?: ReactNode;
  onClick: () => void;
  danger?: boolean;
  disabled?: boolean;
  /** Key shown on the right (`.ux1-cap`). */
  keys?: string;
}
export type MenuEntry = MenuItem | 'sep';

export function ContextMenu({ x, y, items, onClose }: { x: number; y: number; items: MenuEntry[]; onClose: () => void }) {
  const ref = useRef<HTMLDivElement>(null);
  const [pos, setPos] = useState({ left: x, top: y, ready: false });

  useLayoutEffect(() => {
    const el = ref.current;
    if (!el) return;
    const w = el.offsetWidth;
    const h = el.offsetHeight;
    let top = y + 2;
    if (top + h > innerHeight - 8) top = y - h - 2 >= 8 ? y - h - 2 : Math.max(8, innerHeight - 8 - h);
    const left = Math.max(8, Math.min(x, innerWidth - 8 - w));
    setPos({ left, top, ready: true });
  }, [x, y]);

  useEffect(() => {
    const away = (e: Event) => {
      if (!ref.current?.contains(e.target as Node)) onClose();
    };
    const key = (e: KeyboardEvent) => {
      if (e.key === 'Escape') {
        e.preventDefault();
        e.stopPropagation();
        onClose();
      } else if (e.key === 'ArrowDown' || e.key === 'ArrowUp') {
        e.preventDefault();
        const list = [...(ref.current?.querySelectorAll<HTMLButtonElement>('button:not(:disabled)') ?? [])];
        const i = list.indexOf(document.activeElement as HTMLButtonElement);
        list[(i + (e.key === 'ArrowDown' ? 1 : list.length - 1)) % list.length]?.focus();
      }
    };
    const close = () => onClose();
    document.addEventListener('pointerdown', away, true);
    document.addEventListener('keydown', key, true);
    addEventListener('resize', close);
    addEventListener('blur', close);
    return () => {
      document.removeEventListener('pointerdown', away, true);
      document.removeEventListener('keydown', key, true);
      removeEventListener('resize', close);
      removeEventListener('blur', close);
    };
  }, [onClose]);

  return createPortal(
    <div
      ref={ref}
      className="menu ctx-menu"
      role="menu"
      style={{ position: 'fixed', left: pos.left, top: pos.top, visibility: pos.ready ? 'visible' : 'hidden' }}
      onPointerDown={(e) => e.stopPropagation()}
      onContextMenu={(e) => e.preventDefault()}
    >
      {items.map((it, i) =>
        it === 'sep' ? (
          <div key={i} className="menu-sep" />
        ) : (
          <button
            key={i}
            type="button"
            role="menuitem"
            className={`ux1-mi ${it.danger ? 'danger' : ''}`}
            disabled={it.disabled}
            onClick={() => {
              onClose();
              it.onClick();
            }}
          >
            {it.icon}
            <span>{it.label}</span>
            {it.keys && <span className="ux1-cap">{it.keys}</span>}
          </button>
        ),
      )}
    </div>,
    document.body,
  );
}

const useCtx = create<{ menu: { x: number; y: number; items: MenuEntry[]; id: number } | null }>(() => ({ menu: null }));
let ctxId = 0;

/** Opens the menu at the pointer (right click) or under the « … » button that was clicked. */
export function openContextMenu(e: { clientX: number; clientY: number; preventDefault?: () => void; stopPropagation?: () => void; currentTarget?: EventTarget | null; type?: string }, items: MenuEntry[]) {
  e.preventDefault?.();
  e.stopPropagation?.();
  let { clientX: x, clientY: y } = e;
  // A click on a button (keyboard included): under the button.
  if (e.type === 'click' && e.currentTarget instanceof HTMLElement) {
    const r = e.currentTarget.getBoundingClientRect();
    x = r.left;
    y = r.bottom + 4;
  }
  useCtx.setState({ menu: { x, y, items, id: ++ctxId } });
}

export function closeContextMenu() {
  useCtx.setState({ menu: null });
}

/** Mounted once in App. */
export function ContextMenuHost() {
  const menu = useCtx((s) => s.menu);
  if (!menu) return null;
  return <ContextMenu key={menu.id} x={menu.x} y={menu.y} items={menu.items} onClose={closeContextMenu} />;
}
