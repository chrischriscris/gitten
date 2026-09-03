import { toggleStageCurrent } from './panes';

export function openAskModal(): void {
  closeModals();
  document.getElementById('modal-ask')?.classList.add('show');
}

export function openHelpModal(): void {
  closeModals();
  document.getElementById('modal-help')?.classList.add('show');
}

export function openExtensionsModal(): void {
  closeModals();
  document.getElementById('modal-extensions')?.classList.add('show');
}

export function closeModals(): void {
  document
    .querySelectorAll('.mock-modal')
    .forEach((m) => m.classList.remove('show'));
}

export function toggleExplain(): void {
  const card = document.getElementById('ai-card') as HTMLElement | null;
  if (card) card.style.display = card.style.display === 'none' ? 'block' : 'none';
}

export function handleKeyHint(action: string): void {
  if (action === 'ask') openAskModal();
  else if (action === 'extensions') openExtensionsModal();
  else if (action === 'explain') toggleExplain();
  else if (action === 'stage') toggleStageCurrent();
}
