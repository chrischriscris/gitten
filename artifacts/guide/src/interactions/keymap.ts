import { state } from '../state';

function setKeycap(id: string, text: string): void {
  const el = document.getElementById(id);
  if (el) el.innerText = text;
}

export function setKeymapMode(mode: string): void {
  state.keymapMode = mode === 'mockup' ? 'mockup' : 'lazygit';
  if (state.keymapMode === 'lazygit') {
    setKeycap('keycap-files', '2');
    setKeycap('keycap-branches', '3');
    setKeycap('keycap-commits', '4');
    setKeycap('keycap-stash', '5');
    setKeycap('keycap-diff', '0');
    const kStatus = document.getElementById('keycap-status');
    if (kStatus) kStatus.style.display = 'inline-flex';
  } else {
    setKeycap('keycap-files', '1');
    setKeycap('keycap-branches', '2');
    setKeycap('keycap-stash', '3');
    setKeycap('keycap-commits', '4');
    setKeycap('keycap-diff', '5');
    const kStatus = document.getElementById('keycap-status');
    if (kStatus) kStatus.style.display = 'none';
  }
}
