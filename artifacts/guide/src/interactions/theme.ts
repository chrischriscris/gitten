import { accents } from '../data/guide-data';
import { state } from '../state';

export function changeTheme(theme: string): void {
  document.body.className = '';
  if (theme === 'slate') document.body.classList.add('theme-slate');
  if (theme === 'light') document.body.classList.add('theme-light');
}

export function cycleAccent(): void {
  state.accentIdx = (state.accentIdx + 1) % accents.length;
  const accent = accents[state.accentIdx];
  if (!accent) return;
  document.documentElement.style.setProperty('--c-accent', accent.hex);
  document.documentElement.style.setProperty('--lane-0', accent.hex);
  const tag = document.querySelector('.accent-tag span:last-child');
  if (tag) tag.textContent = accent.name.toLowerCase() + ' ▾';
}
