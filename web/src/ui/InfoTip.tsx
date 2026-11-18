import { Info } from 'lucide-react';

/** The « i » after a label: its text shows as a drawn tooltip. Use sparingly. */
export function InfoTip({ text }: { text: string }) {
  return (
    <span className="ux3-info" title={text} aria-label={text} role="img" tabIndex={0}>
      <Info size={13} />
    </span>
  );
}
