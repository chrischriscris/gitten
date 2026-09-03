import { state } from '../state';
import { showFeedback } from './feedback';
import {
  closeModals,
  openAskModal,
  openExtensionsModal,
  openHelpModal,
  toggleExplain,
} from './modals';
import { focusPane, selectCommit, toggleStageCurrent } from './panes';

// Faithful port of the legacy keydown dispatcher: the mockup answers the
// same keys the app does so the guide stays an honest preview.

function stepCommit(dir: 1 | -1): void {
  const rows = Array.from(document.querySelectorAll('.commit-row'));
  const selIdx = rows.findIndex((r) => r.classList.contains('selected'));
  const next = rows[selIdx + dir];
  if (selIdx >= 0 && next) selectCommit(next, 'next');
}

function stepFile(dir: 1 | -1): void {
  const files = Array.from(document.querySelectorAll('.file-item'));
  const selIdx = files.findIndex((f) => f.classList.contains('selected'));
  const next = files[selIdx + dir];
  if (selIdx >= 0 && next) (next as HTMLElement).click();
}

export function initKeyboard(): void {
  window.addEventListener('keydown', (e) => {
    if (e.key === 'Escape') {
      const openModal = document.querySelector('.mock-modal.show');
      if (openModal) {
        closeModals();
        return;
      }
      if (state.currentFocusedPane === 5) {
        focusPane(state.previousFocusedPane || 1);
        return;
      }
      return;
    }

    if ((e.metaKey || e.ctrlKey) && e.key.toLowerCase() === 'k') {
      e.preventDefault();
      openAskModal();
      return;
    }

    if (
      document.activeElement &&
      document.activeElement.tagName === 'INPUT'
    ) {
      return;
    }

    if (state.keymapMode === 'lazygit') {
      if (e.key === '1') {
        focusPane(1);
        showFeedback('[1] Status');
      } else if (e.key === '2') {
        focusPane(1);
        showFeedback('[2] Files');
      } else if (e.key === '3') {
        focusPane(1);
        showFeedback('[3] Branches');
      } else if (e.key === '4') {
        focusPane(4);
        showFeedback('[4] Commits');
      } else if (e.key === '5') {
        focusPane(1);
        showFeedback('[5] Stash');
      } else if (e.key === '0' || e.key === 'Enter') {
        e.preventDefault();
        focusPane(5);
        showFeedback('[0] Diff Focus');
      }
    } else {
      if (e.key === '1') focusPane(1);
      else if (e.key === '2') focusPane(1);
      else if (e.key === '3') focusPane(1);
      else if (e.key === '4') focusPane(4);
      else if (e.key === '5') focusPane(5);
    }

    if (e.key === 'h' || e.key === 'ArrowLeft') {
      if (state.currentFocusedPane === 5) focusPane(4);
      else if (state.currentFocusedPane === 4) focusPane(1);
    } else if (e.key === 'l' || e.key === 'ArrowRight') {
      if (state.currentFocusedPane === 1) focusPane(4);
      else if (state.currentFocusedPane === 4) focusPane(5);
    }

    if (e.key === 'j' || e.key === 'ArrowDown') {
      if (state.currentFocusedPane === 4) stepCommit(1);
      else if (state.currentFocusedPane === 1) stepFile(1);
    } else if (e.key === 'k' || e.key === 'ArrowUp') {
      if (state.currentFocusedPane === 4) stepCommit(-1);
      else if (state.currentFocusedPane === 1) stepFile(-1);
    }

    if (e.key === ' ') {
      e.preventDefault();
      toggleStageCurrent();
    }

    if (e.key === 'c' && !e.metaKey && !e.ctrlKey) openAskModal();
    if (e.key === 'a' && !e.metaKey && !e.ctrlKey) {
      showFeedback('All files staged');
    }
    if (e.key === 'P') showFeedback('Pushing to origin/main...');
    if (e.key === 'p') showFeedback('Pulling latest from upstream...');
    if (e.key === 'f') showFeedback('Fetching remotes...');
    if (e.key === 'R') showFeedback('Refreshed git repository state');
    if (e.key === 'y') showFeedback('Copied to clipboard');
    if (e.key === '?') openHelpModal();
    if (e.key === 'e') toggleExplain();
    if (e.key === 'x') openExtensionsModal();
  });
}
