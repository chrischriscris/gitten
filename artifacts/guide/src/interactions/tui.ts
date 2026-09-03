import { state } from '../state';

export function toggleTuiHelp(): void {
  const box = document.getElementById('tui-help');
  const btn = document.getElementById('tbtn-help');
  if (!box || !btn) return;
  const show = !box.classList.contains('show');
  box.classList.toggle('show', show);
  btn.classList.toggle('active', show);
}

const ASCII_MAP: Record<string, string> = {
  '◉': '@',
  '●': '*',
  '│': '|',
  '─': '-',
  '┼': '+',
  '╮': '\\',
  '╯': '/',
  '╰': '\\',
  '╭': '/',
  '▐': '#',
  '▗': '#',
  '▝': '#',
};

export function toggleTuiAscii(): void {
  state.tuiAscii = !state.tuiAscii;
  const btn = document.getElementById('tbtn-ascii');
  if (btn) {
    btn.innerText = state.tuiAscii ? '--ascii: on' : '--ascii: off';
    btn.classList.toggle('active', state.tuiAscii);
  }
  const root = document.getElementById('tui-commits');
  if (!root) return;
  if (state.tuiAscii && state.tuiCommitsBackup === null) {
    state.tuiCommitsBackup = root.innerHTML;
  }
  if (state.tuiAscii) {
    let html = root.innerHTML;
    for (const [a, b] of Object.entries(ASCII_MAP)) {
      html = html.split(a).join(b);
    }
    root.innerHTML = html;
  } else if (state.tuiCommitsBackup !== null) {
    root.innerHTML = state.tuiCommitsBackup;
  }
  const st = document.getElementById('tui-status-text');
  if (st) {
    st.innerText = state.tuiAscii
      ? ' commits · 1/13 · 2 lanes · word · unified · ascii'
      : ' commits · 1/13 · 2 lanes · word · unified';
  }
}

export function toggleTuiNarrow(): void {
  state.tuiNarrow = !state.tuiNarrow;
  const win = document.getElementById('tui-window');
  const btn = document.getElementById('tbtn-narrow');
  if (win) win.classList.toggle('tui-narrow', state.tuiNarrow);
  if (btn) {
    btn.innerText = state.tuiNarrow ? 'narrow <96' : 'wide ≥96';
    btn.classList.toggle('active', state.tuiNarrow);
  }
}
