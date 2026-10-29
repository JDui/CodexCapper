import * as React from "react";

export interface UsageBarProps {
  /** Fill percentage, 0 to 100. */
  value: number;
  palette: "five-hour" | "weekly";
  /** Draw the bar as inactive when the snapshot is older than its window. */
  stale?: boolean;
}

/** The reference meter distinguishes short and weekly windows by colour. */
export function UsageBar({ value, palette, stale }: UsageBarProps) {
  const p = Math.min(100, Math.max(0, Number.isFinite(value) ? value : 0));

  const background = stale
    ? "var(--text-secondary)"
    : `var(--${palette}-fill)`;

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
