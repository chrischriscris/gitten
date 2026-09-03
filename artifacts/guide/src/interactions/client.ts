import { state } from '../state';
import { closeModals } from './modals';

function setDisplay(id: string, value: string): void {
  const el = document.getElementById(id);
  if (el) el.style.display = value;
}

export function setClient(which: string): void {
  state.currentClient = which === 'tui' ? 'tui' : 'desktop';
  const isTui = state.currentClient === 'tui';
  document
    .getElementById('btn-client-desktop')
    ?.classList.toggle('active', !isTui);
  document.getElementById('btn-client-tui')?.classList.toggle('active', isTui);
  setDisplay('app-window', isTui ? 'none' : 'flex');
  setDisplay('tui-window', isTui ? 'block' : 'none');
  setDisplay('tui-legend', isTui ? 'flex' : 'none');
  setDisplay('accent-tag', isTui ? 'none' : 'inline-flex');
  setDisplay('tui-tag', isTui ? 'inline-flex' : 'none');
  setDisplay('tui-toolbar', isTui ? 'inline-flex' : 'none');
  const caption = document.getElementById('mockup-caption');
  if (caption) {
    caption.innerText = isTui
      ? 'gitten-tui · cell grid · same core, same keymap, same tokens'
      : 'GPUI window · 3 panes · mouse + keyboard';
  }
  if (isTui) closeModals();
  if (window.history && window.history.replaceState) {
    const url = new URL(window.location.href);
    if (isTui) url.searchParams.set('client', 'tui');
    else url.searchParams.delete('client');
    window.history.replaceState(null, '', url.toString());
  }
}

export function initClient(): void {
  try {
    if (new URLSearchParams(window.location.search).get('client') === 'tui') {
      setClient('tui');
    }
  } catch {
    // Non-URL contexts (file:// edge cases): stay on desktop.
  }
}
