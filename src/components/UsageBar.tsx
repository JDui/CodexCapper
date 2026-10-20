import * as React from "react";

export interface UsageBarProps {
  /** Fill percentage, 0 to 100. */
  value: number;
  palette: "codex";
  /** Draw the bar as inactive when the snapshot is older than its window. */
  stale?: boolean;
}

/**
 * A flat bar: one solid fill on a recessed track, half the width it used to
 * take. No gradients, highlights, glows or animations — the fill jumps
 * straight to its new width when the number changes. The track is its own
 * row item so the percentage sits tight against its right end.
 */
export function UsageBar({ value, palette, stale }: UsageBarProps) {
  const p = Math.min(100, Math.max(0, Number.isFinite(value) ? value : 0));

  const to = `var(--${palette}-to)`;
  const danger = p <= 10;
  const background = stale
    ? "var(--text-secondary)"
    : danger
      ? `color-mix(in srgb, ${to} 60%, var(--danger))`
      : to;

  return (
    <div
      className="cx-track"
      role="progressbar"
      aria-valuemin={0}
      aria-valuemax={100}
      aria-valuenow={Math.round(p)}
    >
      <div className="cx-fill" style={{ width: `${p}%`, background }} />
    </div>
  );
}

export default React.memo(UsageBar);
