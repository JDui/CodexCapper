/**
 * Provider glyphs are drawn here rather than reusing vendor logos, so the
 * widget never implies endorsement by OpenAI.
 */

export function CodexMark() {
  return (
    <svg width="10" height="10" viewBox="0 0 10 10" aria-hidden>
      <rect
        x="0.6"
        y="1.2"
        width="8.8"
        height="7.6"
        rx="1.6"
        fill="none"
        stroke="var(--codex-to)"
        strokeWidth="1"
      />
      <path
        d="M2.6 4 L4.1 5 L2.6 6"
        fill="none"
        stroke="var(--codex-to)"
        strokeWidth="1"
        strokeLinecap="round"
        strokeLinejoin="round"
      />
      <path d="M5.2 6.4 H7.4" stroke="var(--codex-to)" strokeWidth="1" strokeLinecap="round" />
    </svg>
  );
}
