/*
 * A set of pages in the top bar: a button (icon + label + chevron), `on` when
 * one of its pages is open, and its menu. Keys: ↓ / Enter opens, ↑ ↓ Home End
 * move, Escape and Tab close; a press elsewhere closes.
 */
import { useEffect, useRef, useState, type ReactNode } from 'react';
import { ChevronDown } from 'lucide-react';

/** Arrow keys, Home and End over a menu's items; Escape closes. */
export function useMenuKeys(open: boolean, menu: React.RefObject<HTMLElement | null>, close: (focusButton: boolean) => void) {
  useEffect(() => {
    if (!open) return;
    const items = () => [...(menu.current?.querySelectorAll<HTMLElement>('[role="menuitem"]:not(:disabled), .lang-switch button') ?? [])];
    const key = (e: KeyboardEvent) => {
      const list = items();
      const i = list.indexOf(document.activeElement as HTMLElement);
      if (e.key === 'Escape') {
        e.preventDefault();
        close(true);
      } else if (e.key === 'Tab') close(false);
      else if (e.key === 'ArrowDown') {
        e.preventDefault();
        list[(i + 1) % list.length]?.focus();
      } else if (e.key === 'ArrowUp') {
        e.preventDefault();
        list[(i - 1 + list.length) % list.length]?.focus();
      } else if (e.key === 'Home') {
        e.preventDefault();
        list[0]?.focus();
      } else if (e.key === 'End') {
        e.preventDefault();
        list[list.length - 1]?.focus();
      }
    };
    const away = (e: Event) => {
      const root = menu.current?.parentElement;
      if (root && !root.contains(e.target as Node)) close(false);
    };
    document.addEventListener('keydown', key);
    document.addEventListener('pointerdown', away, true);
    return () => {
      document.removeEventListener('keydown', key);
      document.removeEventListener('pointerdown', away, true);
    };
  }, [open, menu, close]);
}

export interface NavItem {
  id: string;
  label: string;
  icon?: ReactNode;
  on?: boolean;
  onSelect: () => void;
}

export function NavGroup({ icon, label, items, on, dot }: { icon: ReactNode; label: string; items: NavItem[]; on: boolean; dot?: boolean }) {
  const [open, setOpen] = useState(false);
  const btn = useRef<HTMLButtonElement>(null);
  const menu = useRef<HTMLDivElement>(null);
  const close = useRef((focus: boolean) => {
    setOpen(false);
    if (focus) btn.current?.focus();
  }).current;
  useMenuKeys(open, menu, close);
  useEffect(() => {
    if (open) (menu.current?.querySelector('[aria-current="page"]') as HTMLElement | null ?? menu.current?.querySelector<HTMLElement>('[role="menuitem"]'))?.focus();
  }, [open]);

  return (
    <div className="ux1-navgroup">
      <button
        ref={btn}
        className={`${on ? 'on' : ''} ${open ? 'open' : ''}`}
        aria-haspopup="menu"
        aria-expanded={open}
        onClick={() => setOpen((o) => !o)}
        onKeyDown={(e) => {
          if (!open && (e.key === 'ArrowDown' || e.key === 'Enter')) {
            e.preventDefault();
            setOpen(true);
          }
        }}
      >
        {icon} <span className="tab-label">{label}</span>
        <ChevronDown size={13} className="ux1-chev" />
        {dot && <span className="nav-dot warn" />}
      </button>
      {open && (
        <div ref={menu} className="menu ux1-navmenu" role="menu">
          {items.map((i) => (
            <button
              key={i.id}
              role="menuitem"
              className={`ux1-mi ${i.on ? 'on' : ''}`}
              aria-current={i.on ? 'page' : undefined}
              onClick={() => {
                setOpen(false);
                i.onSelect();
              }}
            >
              {i.icon}
              {i.label}
            </button>
          ))}
        </div>
      )}
    </div>
  );
}
