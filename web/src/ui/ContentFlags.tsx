/*
 * What the preview sees in a source: a black picture, a frozen one, silence (held 10 s, so that a
 * fade or a pause is not one). Small pills on its picture; `compact`: a single warning mark.
 */
import { MicOff, Pause, Square, TriangleAlert } from "lucide-react";
import { t } from "../i18n";
import { serverNow } from "../store";
import type { RecorderStatus } from "../types";

const HOLD_MS = 10_000;

export function contentFlags(
  st?: RecorderStatus,
): { key: string; label: string }[] {
  if (!st) return [];
  const now = serverNow();
  const on = (since?: number | null) => since != null && now - since >= HOLD_MS;
  const out: { key: string; label: string }[] = [];
  if (on(st.blackSince)) out.push({ key: "black", label: t("Black picture") });
  if (on(st.frozenSince))
    out.push({ key: "frozen", label: t("Frozen picture") });
  if (on(st.silentSince)) out.push({ key: "silent", label: t("Silence") });
  return out;
}

const ICON = { black: Square, frozen: Pause, silent: MicOff } as const;

export function ContentFlags({
  st,
  compact = false,
}: {
  st?: RecorderStatus;
  compact?: boolean;
}) {
  const flags = contentFlags(st);
  if (!flags.length) return null;
  if (compact)
    return (
      <span
        className="sc-content-mark"
        data-tip={flags.map((f) => f.label).join(" · ")}
      >
        <TriangleAlert size={10} />
      </span>
    );
  return (
    <span className="sc-content-flags">
      {flags.map((f) => {
        const I = ICON[f.key as keyof typeof ICON];
        return (
          <span key={f.key} className="sc-content-flag">
            <I size={11} /> {f.label}
          </span>
        );
      })}
    </span>
  );
}
