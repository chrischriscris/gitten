import { state } from '../state';

export function toggleAnnotations(): void {
  state.annotationsOn = !state.annotationsOn;
  const btn = document.getElementById('btn-annotations');
  const win = document.getElementById('app-window');
  if (!btn || !win) return;
  if (state.annotationsOn) {
    btn.innerText = 'Annotations: On';
    btn.classList.add('active');
    win.classList.add('blueprint-active');
  } else {
    btn.innerText = 'Annotations: Off';
    btn.classList.remove('active');
    win.classList.remove('blueprint-active');
  }
}
