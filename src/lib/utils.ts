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
  const anyUnderHour = resetTimes.some((t) => t - now > 0 && t - now < 3_600_000);
  return anyUnderHour ? 1000 : 60_000;
}
