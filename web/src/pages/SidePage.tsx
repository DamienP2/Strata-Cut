/*
 * The page layout: a list on the left (`aside.lib-nav`), the page on the
 * right (`main.library`), with its head (title, factual subtitle, main action)
 * and optional sub-tabs.
 */
import type { ComponentType, ReactNode } from 'react';

export function SidePage({ nav, children, className, wide = true }: { nav: ReactNode; children: ReactNode; className?: string; wide?: boolean }) {
  return (
    <div className={`projects-page ${className ?? ''}`}>
      <aside className="lib-nav">{nav}</aside>
      <main className="library">
        <div className={`library-inner ${wide ? 'wide' : ''}`}>{children}</div>
      </main>
    </div>
  );
}

/** A section title of the left list, with optional actions. */
export function NavSection({ title, children }: { title: string; children?: ReactNode }) {
  return (
    <div className="lib-nav-section">
      <span className="grow">{title}</span>
      {children}
    </div>
  );
}

export interface NavItemProps {
  icon: ComponentType<{ size?: number }>;
  label: string;
  on?: boolean;
  onClick: () => void;
  /** A dot: `ok` (green), `warn` (orange). */
  status?: 'ok' | 'warn' | null;
  count?: number;
  /** Icon buttons shown on hover. */
  actions?: ReactNode;
  title?: string;
}

export function NavItem({ icon: Icon, label, on, onClick, status, count, actions, title }: NavItemProps) {
  return (
    <div
      className={`lib-nav-item ${on ? 'on' : ''}`}
      role="link"
      tabIndex={0}
      aria-current={on ? 'page' : undefined}
      title={title}
      onClick={onClick}
      onKeyDown={(e) => {
        if (e.key === 'Enter' || e.key === ' ') {
          e.preventDefault();
          onClick();
        }
      }}
    >
      <Icon size={15} />
      <span className="grow">{label}</span>
      {actions && <span className="lib-nav-actions">{actions}</span>}
      {status && <span className={`nav-dot ${status}`} />}
      {count !== undefined && <span className="count">{count}</span>}
    </div>
  );
}

/** Page title, factual subtitle (a count, a version, a state), main action on the right. */
export function PageHead({ title, sub, children }: { title: ReactNode; sub?: ReactNode; children?: ReactNode }) {
  return (
    <div className="settings-head">
      <div>
        <h1>{title}</h1>
        {sub && <p className="muted">{sub}</p>}
      </div>
      <div className="spacer" />
      {children}
    </div>
  );
}

export interface SubTab<T extends string> {
  id: T;
  label: string;
  icon?: ReactNode;
  count?: number;
}

/** The pages of a section, under its title; `children` = the actions of the open page. */
export function SubTabs<T extends string>({ items, value, onChange, children }: { items: SubTab<T>[]; value: T; onChange: (v: T) => void; children?: ReactNode }) {
  return (
    <div className="sys-tabrow">
      <div className="tabs sys-tabs" role="tablist">
        {items.map((i) => (
          <button key={i.id} type="button" role="tab" aria-selected={i.id === value} className={i.id === value ? 'on' : ''} onClick={() => onChange(i.id)}>
            {i.icon}
            <span>{i.label}</span>
            {i.count !== undefined && <span className="sys-tab-n">{i.count}</span>}
          </button>
        ))}
      </div>
      {children}
    </div>
  );
}
