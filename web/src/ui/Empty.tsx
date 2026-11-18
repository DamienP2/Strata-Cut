import type { ComponentType, ReactNode } from 'react';

/** An empty area in a panel: icon, one short text, an optional button. */
export function Empty({ icon: Icon, text, children }: { icon?: ComponentType<{ size?: number }>; text: string; children?: ReactNode }) {
  return (
    <div className="opx-void">
      {Icon && (
        <span className="opx-void-ic">
          <Icon size={22} />
        </span>
      )}
      <span className="opx-void-t">{text}</span>
      {children}
    </div>
  );
}
