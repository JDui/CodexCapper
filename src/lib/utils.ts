import { clsx, type ClassValue } from "clsx";
import { twMerge } from "tailwind-merge";

export function cn(...inputs: ClassValue[]) {
  return twMerge(clsx(inputs));
}

/** Match the compact countdown used by chatgpt-model-meter's quota rows. */
export function formatCountdown(msLeft: number): string {
  if (!Number.isFinite(msLeft) || msLeft <= 0) return "0m";
  const totalMinutes = Math.ceil(msLeft / 60_000);
  const days = Math.floor(totalMinutes / 1440);
  const hours = Math.floor((totalMinutes % 1440) / 60);
  const minutes = totalMinutes % 60;
  if (days > 0) return `${days}d ${hours}h ${minutes}m`;
  if (hours > 0) return `${hours}h ${minutes}m`;
  return `${minutes}m`;
}

/** Two units fit the miniature single-row widget; the tooltip keeps the date. */
export function formatCompactCountdown(msLeft: number): string {
  if (!Number.isFinite(msLeft) || msLeft <= 0) return "0m";
  const totalMinutes = Math.ceil(msLeft / 60_000);
  const days = Math.floor(totalMinutes / 1440);
  const hours = Math.floor((totalMinutes % 1440) / 60);
  const minutes = totalMinutes % 60;
  if (days >= 100) return `${days}d`;
  if (days > 0) return `${days}d ${hours}h`;
  if (hours > 0) return `${hours}h ${minutes}m`;
  return `${minutes}m`;
}

/** Absolute reset time always includes the clock; include the date tomorrow. */
export function formatAbsoluteReset(iso: string, now: number): string {
  const reset = new Date(iso);
  if (!Number.isFinite(reset.getTime())) return "--";
  const current = new Date(now);
  const clock = `${String(reset.getHours()).padStart(2, "0")}:${String(reset.getMinutes()).padStart(2, "0")}`;
  if (reset.getFullYear() === current.getFullYear()
    && reset.getMonth() === current.getMonth()
    && reset.getDate() === current.getDate()) return clock;
  return `${String(reset.getMonth() + 1).padStart(2, "0")}/${String(reset.getDate()).padStart(2, "0")} ${clock}`;
}

/** Auto switches exactly at the window's threshold, including the boundary. */
export function formatUsageReset(
  iso: string | null,
  label: string,
  style: string,
  now: number,
  compact = false,
): string | null {
  if (!iso) return null;
  const at = new Date(iso).getTime();
  if (!Number.isFinite(at)) return "--";
  const left = at - now;
  const threshold = label === "7d" ? 12 * 3_600_000 : 3_600_000;
  const countdown = style === "countdown" || (style !== "absolute" && left <= threshold);
  if (!countdown) return formatAbsoluteReset(iso, now);
  if (left <= 0) return compact ? "0m" : "即将重置";
  return compact ? formatCompactCountdown(left) : formatCountdown(left);
}

export function formatSubscription(plan?: string | null): string {
  const raw = plan?.trim();
  if (!raw) return "订阅";
  if (/^pro[_\s-]?lite$/i.test(raw)) return "Pro Lite";
  return raw.toLowerCase().split(/[\s_-]+/).map((word) =>
    word.charAt(0).toUpperCase() + word.slice(1),
  ).join(" ");
}

export function formatClock(iso: string): string {
  const d = new Date(iso);
  if (Number.isNaN(d.getTime())) return "--:--";
  return d.toLocaleTimeString("zh-CN", { hour: "2-digit", minute: "2-digit" });
}

export function minutesSince(iso: string, now: number): number {
  const t = new Date(iso).getTime();
  if (Number.isNaN(t)) return 0;
  return Math.max(0, Math.floor((now - t) / 60000));
}

/**
 * Ticks once a second only while a countdown is inside its final hour, so an
 * idle widget wakes the webview once a minute instead.
 */
export function tickInterval(resetTimes: number[], now: number): number {
  const anyUnderHour = resetTimes.some((t) => t - now > 0 && t - now <= 3_600_000);
  let period = anyUnderHour ? 1000 : 60_000;
  // Wake at either auto boundary instead of showing the old format for up to
  // another minute. Unused boundaries cost only an occasional extra tick.
  for (const at of resetTimes) {
    for (const threshold of [3_600_000, 12 * 3_600_000]) {
      const until = at - now - threshold;
      if (until > 0) period = Math.min(period, Math.ceil(until));
    }
  }
  return period;
}
