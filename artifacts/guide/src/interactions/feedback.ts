// Status-bar transient message. Own module so panes/ and modals/ can both
// use it without an import cycle.

export function showFeedback(text: string): void {
  const sb = document.querySelector('.statusbar-right');
  if (!sb) return;
  const orig = sb.textContent ?? '';
  sb.textContent = text;
  (sb as HTMLElement).style.color = 'var(--c-accent)';
  setTimeout(() => {
    sb.textContent = orig;
    (sb as HTMLElement).style.color = 'var(--c-ghost)';
  }, 1500);
}
