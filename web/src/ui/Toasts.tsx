import { useApp } from '../store';

/** Stacked at the bottom right, gone after 4 s. */
export function Toasts() {
  const toasts = useApp((s) => s.toasts);
  return (
    <div className="toasts" role="status" aria-live="polite">
      {toasts.map((x) => (
        <div key={x.id} className={`toast ${x.kind}`}>
          {x.message}
        </div>
      ))}
    </div>
  );
}
