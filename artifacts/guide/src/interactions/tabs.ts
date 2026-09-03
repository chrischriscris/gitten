export function switchGuideTab(tabId: string): void {
  document
    .querySelectorAll('.guide-tab-btn')
    .forEach((btn) => btn.classList.remove('active'));
  document
    .querySelectorAll('.guide-view')
    .forEach((view) => view.classList.remove('active'));

  const activeBtn = Array.from(
    document.querySelectorAll('.guide-tab-btn'),
  ).find((b) => b.getAttribute('onclick')?.includes(tabId));
  if (activeBtn) activeBtn.classList.add('active');

  const targetView = document.getElementById('view-' + tabId);
  if (targetView) targetView.classList.add('active');
  if (window.history && window.history.replaceState) {
    window.history.replaceState(null, '', '#' + tabId);
  }
}

const TABS = ['mockup', 'blueprint', 'tokens', 'keyboard', 'architecture'];

function applyHashOrParam(): void {
  const params = new URLSearchParams(window.location.search);
  const paramTab = params.get('tab');
  const hashTab = window.location.hash.replace('#', '');
  const tab = paramTab || hashTab;
  if (tab && TABS.includes(tab)) {
    switchGuideTab(tab);
  }
}

export function initTabs(): void {
  applyHashOrParam();
  window.addEventListener('hashchange', applyHashOrParam);
}
