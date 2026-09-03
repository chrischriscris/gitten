import { fileDiffs } from '../data/guide-data';
import { state } from '../state';
import { showFeedback } from './feedback';

// Legacy note: the original file wrapped focusPane after definition to track
// pane history for Esc. Here tracking lives inside the one function.

export function focusPane(paneId: number): void {
  if (state.currentFocusedPane !== paneId) {
    state.previousFocusedPane = state.currentFocusedPane;
    state.currentFocusedPane = paneId;
  }
  document
    .querySelectorAll('.pane')
    .forEach((p) => p.classList.remove('focused'));
  const target = document.getElementById('pane-' + paneId);
  if (target) target.classList.add('focused');
}

function setText(id: string, text: string): void {
  const el = document.getElementById(id);
  if (el) el.innerText = text;
}

export function selectFile(el: Element, fileKey: string): void {
  document
    .querySelectorAll('.file-item')
    .forEach((f) => f.classList.remove('selected'));
  el.classList.add('selected');

  const data = fileDiffs[fileKey] ?? fileDiffs['host.go'];
  if (!data) return;
  setText('diff-file-title', data.title);
  const hunkBar = document.querySelector('.diff-hunk-bar');
  if (hunkBar) hunkBar.textContent = data.hunk;
  setText('ai-card-text', data.explanation);
  const addsEl = document.querySelector('.stat-adds');
  if (addsEl) addsEl.textContent = data.adds;
  const delsEl = document.querySelector('.stat-dels');
  if (delsEl) delsEl.textContent = data.dels;

  focusPane(5);
}

export function selectCommit(el: Element, _hash: string): void {
  document
    .querySelectorAll('.commit-row')
    .forEach((r) => r.classList.remove('selected'));
  el.classList.add('selected');
  focusPane(4);
}

export function toggleStageCurrent(): void {
  const selectedFile = document.querySelector('.file-item.selected');
  if (!selectedFile) return;
  const flag = selectedFile.querySelector('.status-flag');
  if (!flag) return;
  if (flag.classList.contains('status-m-unstaged')) {
    flag.className = 'status-flag status-m-staged';
    showFeedback('Staged 1 file');
  } else if (flag.classList.contains('status-m-staged')) {
    flag.className = 'status-flag status-m-unstaged';
    showFeedback('Unstaged 1 file');
  }
}
