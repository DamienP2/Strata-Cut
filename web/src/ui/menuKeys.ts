/*
 * A menu's keys: ↑ ↓ Home End move over its items, Escape and Tab close; a press elsewhere closes.
 */
import { useEffect } from 'react';

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
