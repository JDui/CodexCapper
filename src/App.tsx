import * as React from "react";
import { listen } from "@tauri-apps/api/event";
import { invoke } from "@tauri-apps/api/core";
import { getCurrentWindow } from "@tauri-apps/api/window";
import { openUrl } from "@tauri-apps/plugin-opener";

import UsageBar from "@/components/UsageBar";
import { CodexMark } from "@/components/Marks";
import type {
  AppearanceConfig,
  ProviderSnapshot,
  UsagePayload,
  WindowSnapshot,
} from "@/lib/types";
import { formatAbsoluteReset, formatClock, formatSubscription, formatUsageReset, minutesSince, tickInterval } from "@/lib/utils";

const DEFAULT_APPEARANCE: AppearanceConfig = {
  theme: "system",
  font_family: "system",
  compact_mode: true,
  show_model_weekly: false,
  placement: "float",
  reset_time_style: "auto",
  resolved_theme: "dark",
};

const CODEX_INSTALL_URL = "https://developers.openai.com/codex/cli/";

export default function App() {
  const [payload, setPayload] = React.useState<UsagePayload | null>(null);
  const [appearance, setAppearance] = React.useState<AppearanceConfig>(DEFAULT_APPEARANCE);
  const [now, setNow] = React.useState(() => Date.now());

  React.useEffect(() => {
    const unlisteners: Array<() => void> = [];

    listen<UsagePayload>("usage", (e) => setPayload(e.payload)).then((un) => unlisteners.push(un));
    listen<AppearanceConfig>("config", (e) => setAppearance(e.payload)).then((un) =>
      unlisteners.push(un),
    );

    invoke<UsagePayload>("get_usage").then(setPayload).catch(() => {});
    invoke<AppearanceConfig>("get_appearance").then(setAppearance).catch(() => {});

    return () => unlisteners.forEach((un) => un());
  }, []);

  React.useEffect(() => {
    document.documentElement.dataset.theme = appearance.resolved_theme;
  }, [appearance.resolved_theme]);

  React.useEffect(() => {
    document.documentElement.dataset.fontFamily =
      appearance.font_family === "serif" || appearance.font_family === "mono"
        ? appearance.font_family : "system";
  }, [appearance.font_family]);

  const providers = React.useMemo(
    () => (payload?.providers ?? []).filter((p) => p.enabled),
    [payload],
  );

  const resetTimes = React.useMemo(() => {
    const out: number[] = [];
    for (const p of providers) {
      for (const w of [p.five_hour, p.weekly]) {
        if (w?.resets_at) {
          const t = new Date(w.resets_at).getTime();
          if (!Number.isNaN(t)) out.push(t);
        }
      }
    }
    return out;
  }, [providers]);

  // Self rescheduling so the cadence tightens the moment a countdown drops
  // under an hour, without waiting for the next payload.
  React.useEffect(() => {
    let id = 0;
    setNow(Date.now());
    const schedule = () => {
      const period = tickInterval(resetTimes, Date.now());
      id = window.setTimeout(() => {
        setNow(Date.now());
        schedule();
      }, period);
    };
    schedule();
    return () => window.clearTimeout(id);
  }, [resetTimes]);

  // A window refilling to the full is a small event worth marking: the
  // surface burns a scrolling rainbow for a quarter minute. Only the jump
  // counts — a row that is already full when the widget starts stays quiet.
  const [celebrating, setCelebrating] = React.useState(false);
  const seenVals = React.useRef<Record<string, number>>({});
  React.useEffect(() => {
    if (!payload) return;
    const seen: Record<string, number> = {};
    let burst = false;
    for (const p of payload.providers) {
      if (!p.enabled) continue;
      const windows: Array<["5h" | "7d", WindowSnapshot | null]> = [
        ["5h", p.five_hour],
        ["7d", p.weekly],
      ];
      for (const [key, w] of windows) {
        if (!w || w.stale) continue;
        const val = Math.round(Math.min(100, Math.max(0, 100 - w.utilization)));
        const k = `${p.id}:${key}`;
        const prev = seenVals.current[k];
        if (prev !== undefined && prev < 100 && val >= 100) burst = true;
        seen[k] = val;
      }
    }
    seenVals.current = seen;
    if (burst) setCelebrating(true);
  }, [payload]);
  React.useEffect(() => {
    if (!celebrating) return;
    const t = window.setTimeout(() => setCelebrating(false), 15000);
    return () => window.clearTimeout(t);
  }, [celebrating]);

  React.useEffect(() => {
    const onContext = (e: MouseEvent) => {
      e.preventDefault();
      invoke("show_context_menu").catch(() => {});
    };
    window.addEventListener("contextmenu", onContext);
    return () => window.removeEventListener("contextmenu", onContext);
  }, []);

  const startDrag = React.useCallback((e: React.MouseEvent) => {
    if (e.button !== 0 || (e.target instanceof Element && e.target.closest("button, a, .cx-link"))) return;
    getCurrentWindow()
      .startDragging()
      .catch(() => {});
  }, []);

  const pinned = appearance.placement === "taskbar";

  const shellRef = React.useRef<HTMLDivElement | null>(null);
  const contentRef = React.useRef<HTMLDivElement | null>(null);

  // The window is sized to what is actually on screen, so an empty status line
  // does not sit in a window built for two bars. Pinned placement needs the
  // number even more than floating does: the strip it reserves beside the
  // taskbar is charged against the whole width of the display, so a row taller
  // than what is drawn inside it is paid for twice.
  React.useEffect(() => {
    const content = contentRef.current;
    const shell = shellRef.current;
    const bars = content?.parentElement;
    if (!content || !shell || !bars) return;

    let last = 0;
    const report = () => {
      const barsStyle = getComputedStyle(bars);
      const shellStyle = getComputedStyle(shell);
      // Read the frame from the stylesheet rather than repeating it here, so
      // changing the padding in one place cannot leave the window wrong.
      const frame =
        parseFloat(barsStyle.paddingTop) +
        parseFloat(barsStyle.paddingBottom) +
        parseFloat(shellStyle.borderTopWidth) +
        parseFloat(shellStyle.borderBottomWidth);
      const needed = Math.ceil(Math.max(content.scrollHeight, content.getBoundingClientRect().height) + frame);
      if (needed > 0 && needed !== last) {
        last = needed;
        invoke("set_content_height", { height: needed }).catch(() => {});
      }
    };

    report();
    const observer = new ResizeObserver(report);
    observer.observe(content);
    return () => observer.disconnect();
  }, [pinned, providers, payload?.onboarding, appearance.compact_mode]);

  return (
    <div
      className={celebrating ? "cx-shell cx-celebrate" : "cx-shell"}
      data-placement={appearance.placement}
      data-compact={appearance.compact_mode}
      onMouseDown={!pinned && appearance.compact_mode ? startDrag : undefined}
      ref={shellRef}
    >
      <div className="cx-bars">
        <div className="cx-content" ref={contentRef}>
          {!pinned && !appearance.compact_mode && (
            <div className="cx-header" onMouseDown={startDrag} title="拖动组件">
              <span className="cx-title">{formatSubscription(providers[0]?.plan_type)}</span>
              <button
                className="cx-settings"
                type="button"
                aria-label="显示设置"
                title="显示设置"
                onMouseDown={(e) => e.stopPropagation()}
                onClick={() => invoke("show_context_menu").catch(() => {})}
              >
                <svg viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="1.7" aria-hidden="true">
                  <path d="m9.5 3-.6 2.1-1.7 1-2.1-.5-2.5 4.3 1.5 1.6v2l-1.5 1.6 2.5 4.3 2.1-.5 1.7 1 .6 2.1h5l.6-2.1 1.7-1 2.1.5 2.5-4.3-1.5-1.6v-2l1.5-1.6-2.5-4.3-2.1.5-1.7-1L14.5 3z" />
                  <circle cx="12" cy="12" r="3.2" />
                </svg>
              </button>
            </div>
          )}
          {payload?.onboarding ? (
            <Onboarding pinned={pinned} />
          ) : pinned ? (
            <div className="cx-columns">
              {providers.map((p) => (
                <ProviderColumn
                  key={p.id}
                  provider={p}
                  now={now}
                  resetTimeStyle={appearance.reset_time_style}
                  compact={appearance.compact_mode}
                />
              ))}
            </div>
          ) : (
            providers.map((p) => (
              <ProviderGroup
                key={p.id}
                provider={p}
                now={now}
                resetTimeStyle={appearance.reset_time_style}
                compact={appearance.compact_mode}
              />
            ))
          )}
          {!payload && <div className="cx-status">加载中…</div>}
          {payload && !payload.onboarding && providers.length === 0 && (
            <div className="cx-status">尚未启用额度监测</div>
          )}
        </div>
      </div>
    </div>
  );
}

function Onboarding({ pinned }: { pinned: boolean }) {
  if (pinned) {
    return (
      <div className="cx-status">
        <span>未找到 Codex 数据</span>
      </div>
    );
  }
  return (
    <div className="cx-onboard">
      <div className="cx-onboard-title">未找到 Codex 数据</div>
      <div>登录 Codex CLI 或 ChatGPT 桌面版后，组件会自动识别其额度数据。</div>
      <div style={{ display: "flex", gap: 10 }}>
        <span className="cx-link" onClick={() => openUrl(CODEX_INSTALL_URL)}>
          了解 Codex
        </span>
      </div>
    </div>
  );
}

/** Long forms for the tooltip, where there is room for the full words. */
const WINDOW_NAMES: Record<string, string> = {
  "5h": "5 小时",
  "1d": "每日",
  "7d": "每周",
  "1M": "每月",
  "1y": "每年",
};

interface WindowRow {
  key: string;
  label: string;
  window: WindowSnapshot | null;
}

/**
 * Only the windows the account actually has. Codex sends whichever windows a
 * plan has and its first one is not always five hours, so the label comes from
 * the provider when it states how long the window is.
 */
function windowRows(provider: ProviderSnapshot): WindowRow[] {
  const rows: WindowRow[] = [];
  if (provider.five_hour)
    rows.push({ key: "5h", label: provider.five_hour.label ?? "5h", window: provider.five_hour });
  if (provider.weekly)
    rows.push({ key: "7d", label: provider.weekly.label ?? "7d", window: provider.weekly });
  return rows;
}

interface GroupProps {
  provider: ProviderSnapshot;
  now: number;
  resetTimeStyle: string;
  compact: boolean;
}

function ProviderGroup({ provider, now, resetTimeStyle, compact }: GroupProps) {
  const rows = windowRows(provider);

  // The rows themselves decide whether there is anything to draw, so the
  // provider can never end up as an empty box with a logo and no bars.
  if (
    provider.status === "cli_not_found" ||
    provider.status === "plan_unsupported" ||
    rows.length === 0
  ) {
    return (
      <div className="cx-group">
        <div className="cx-status">
          <span className="cx-logo">
            <CodexMark />
          </span>
          <span>{provider.detail ?? "暂无数据"}</span>
          {provider.install_url && (
            <span className="cx-link" onClick={() => openUrl(provider.install_url!)}>
              安装
            </span>
          )}
        </div>
      </div>
    );
  }

  return (
    <div className="cx-group">
      {rows.map((r) => compact ? (
        <UsageCapsule
          key={r.key}
          label={r.label}
          window={r.window}
          provider={provider}
          now={now}
          resetTimeStyle={resetTimeStyle}
          showLabel={rows.length > 1}
        />
      ) : (
        <Row
          key={r.key}
          label={r.label}
          window={r.window}
          provider={provider}
          now={now}
          resetTimeStyle={resetTimeStyle}
          showCountdown
        />
      ))}
      {provider.status === "reauth_needed" && (
        <div className="cx-status">
          <span className="cx-status-strip">需要重新登录，请运行 codex</span>
        </div>
      )}
    </div>
  );
}

/**
 * Taskbar layout. The provider becomes a narrow column with both windows
 * stacked so the strip stays short.
 */
function ProviderColumn({
  provider,
  now,
  resetTimeStyle,
  compact,
}: {
  provider: ProviderSnapshot;
  now: number;
  resetTimeStyle: string;
  compact: boolean;
}) {
  const rows = windowRows(provider);

  if (rows.length === 0) {
    return (
      <div className="cx-column cx-column-empty">
        <span className="cx-column-empty-text">{shortStatus(provider)}</span>
      </div>
    );
  }

  return (
    <div className="cx-column">
      {rows.map((r) => compact ? (
        <UsageCapsule
          key={r.key}
          label={r.label}
          window={r.window}
          provider={provider}
          now={now}
          resetTimeStyle={resetTimeStyle}
          showLabel={rows.length > 1}
        />
      ) : (
        <Row
          key={r.key}
          label={r.label}
          window={r.window}
          provider={provider}
          now={now}
          resetTimeStyle={resetTimeStyle}
          showCountdown
        />
      ))}
    </div>
  );
}

function shortStatus(provider: ProviderSnapshot): string {
  switch (provider.status) {
    case "cli_not_found":
      return "未安装";
    case "plan_unsupported":
      return "套餐不支持";
    case "reauth_needed":
      return "需重新登录";
    case "no_data_yet":
      return "暂无使用记录";
    default:
      return "无数据";
  }
}

interface RowProps {
  label: string;
  window: WindowSnapshot | null;
  provider: ProviderSnapshot;
  now: number;
  resetTimeStyle: string;
  showCountdown: boolean;
}

function Row({ label, window: w, provider, now, resetTimeStyle, showCountdown }: RowProps) {
  const used = w?.utilization ?? 0;
  // The widget tracks what is left, not what was spent, the same way the
  // ChatGPT desktop shows it.
  const remaining = Math.min(100, Math.max(0, 100 - used));
  const stale = w?.stale ?? false;
  const est = w ? w.source !== "official" : true;
  const localOnly = w?.source === "estimate_local_only";

  const resetText = formatUsageReset(w?.resets_at ?? null, label, resetTimeStyle, now);

  const pctClass =
    remaining <= 10 ? "cx-danger" : remaining <= 30 ? "cx-warn" : undefined;

  return (
    <div className="cx-row" data-window={label} title={tooltip(provider, label, w, now, used, remaining)}>
      <span className="cx-label">{label}</span>
      <UsageBar value={remaining} palette={label === "7d" ? "weekly" : "five-hour"} stale={stale} />
      <span className={`cx-pct cx-tabular ${pctClass ?? ""}`}>
        {Math.round(remaining)}%{est && <span className="cx-est">{localOnly ? "估*" : "估"}</span>}
      </span>
      {showCountdown && resetText !== null && (
        <span className="cx-countdown cx-tabular">{resetText}</span>
      )}
    </div>
  );
}

/** Keep the single/dual target size unless actual rendered text needs less room. */
function fitCapsuleText(node: HTMLElement): number {
  const target = node.closest<HTMLElement>(".cx-capsule")?.dataset.single === "true" ? 12 : 11;
  if (node.getBoundingClientRect().width <= 0) return target;
  const range = document.createRange();
  const overflow = () => Array.from(node.children).some((part) => {
    range.selectNodeContents(part);
    return range.getBoundingClientRect().width > part.getBoundingClientRect().width + 0.1;
  });
  const squeezeLabel = document.documentElement.dataset.fontFamily === "mono"
    && node.closest<HTMLElement>(".cx-capsule")?.dataset.labeled === "true";
  node.style.removeProperty("--capsule-gap");
  for (const size of target === 12 ? [12, 11.5, 11, 10.5, 10] : [11, 10.5, 10]) {
    node.style.setProperty("--capsule-font-size", `${size}px`);
    if (!overflow()) return size;
    if (squeezeLabel) {
      node.style.setProperty("--capsule-gap", "1px");
      if (!overflow()) return size;
    }
  }
  return 10;
}

function UsageCapsule({ label, window: w, provider, now, resetTimeStyle, showLabel }: Omit<RowProps, "showCountdown"> & { showLabel: boolean }) {
  const textRef = React.useRef<HTMLSpanElement>(null);
  const used = w?.utilization ?? 0;
  const remaining = Math.min(100, Math.max(0, 100 - used));
  const reset = formatUsageReset(w?.resets_at ?? null, label, resetTimeStyle, now, true) ?? "--";
  const estimate = !w || w.source === "official" ? "" : w.source === "estimate_local_only" ? "估*" : "估";
  React.useLayoutEffect(() => {
    const text = textRef.current;
    if (!text) return;
    const fit = () => { fitCapsuleText(text); };
    fit();
    const resize = new ResizeObserver(fit);
    resize.observe(text);
    const font = new MutationObserver(fit);
    font.observe(document.documentElement, { attributes: true, attributeFilter: ["data-font-family"] });
    document.fonts.addEventListener("loadingdone", fit);
    return () => {
      resize.disconnect();
      font.disconnect();
      document.fonts.removeEventListener("loadingdone", fit);
    };
  }, [label, showLabel, reset, estimate, Math.round(remaining)]);
  if (!w) return null;
  const content = <>
    {showLabel && <span className="cx-capsule-label">{label}</span>}
    <span className="cx-capsule-percent">{Math.round(remaining)}%{estimate}</span>
    <span className="cx-capsule-reset">{reset}</span>
  </>;
  return (
    <div
      className="cx-capsule"
      data-window={label}
      data-stale={w.stale}
      data-labeled={showLabel}
      data-single={!showLabel}
      role="progressbar"
      aria-label={`${WINDOW_NAMES[label] ?? label}剩余额度`}
      aria-valuemin={0}
      aria-valuemax={100}
      aria-valuenow={Math.round(remaining)}
      aria-valuetext={`剩余 ${Math.round(remaining)}%${estimate}，重置 ${reset}`}
      title={tooltip(provider, label, w, now, used, remaining)}
    >
      <div className="cx-capsule-track"><div className="cx-capsule-fill" style={{ width: `${remaining}%` }} /></div>
      <span className="cx-capsule-text" ref={textRef}>{content}</span>
    </div>
  );
}

function tooltip(
  p: ProviderSnapshot,
  label: string,
  w: WindowSnapshot | null,
  now: number,
  used: number,
  remaining: number,
): string {
  const windowName = WINDOW_NAMES[label] ?? label;
  const lines: string[] = [`${p.label} · ${windowName}窗口`];

  if (!w) {
    lines.push(p.detail ?? "无数据");
    return lines.join("\n");
  }

  lines.push(`剩余：${Math.round(remaining)}%（已用 ${Math.round(used)}%）`);
  lines.push(
    `数据来源：${
      w.source === "official"
        ? "官方"
        : w.source === "estimate_local_only"
          ? "估算（仅本机）"
          : "估算"
    }`,
  );
  lines.push(`测量时间：${formatClock(w.as_of)}（${minutesSince(w.as_of, now)} 分钟前）`);
  if (w.stale) lines.push("数据已过期，本窗口内暂无新数据");
  if (w.burn_rate !== null && w.burn_rate > 0)
    lines.push(`消耗速率：${w.burn_rate.toFixed(1)}%/小时（估）`);
  if (w.eta) lines.push(`按当前速率，预计 ${formatClock(w.eta)} 达到上限（估）`);
  if (w.resets_at) {
    lines.push(`重置时间：${formatAbsoluteReset(w.resets_at, now)}`);
  }
  if (p.plan_type) lines.push(`订阅：${formatSubscription(p.plan_type)}`);
  if (p.extra_usage !== null && p.extra_usage > 0) lines.push(`额外用量：${p.extra_usage}`);
  if (p.tokens)
    lines.push(
      `Token：输入 ${fmt(p.tokens.input)}，输出 ${fmt(p.tokens.output)}，缓存写/读 ${fmt(
        p.tokens.cache_write,
      )}/${fmt(p.tokens.cache_read)}`,
    );
  if (p.detail) lines.push(p.detail);

  return lines.join("\n");
}

function fmt(n: number): string {
  if (n >= 1_000_000) return `${(n / 1_000_000).toFixed(1)}M`;
  if (n >= 1_000) return `${(n / 1_000).toFixed(1)}k`;
  return String(n);
}
