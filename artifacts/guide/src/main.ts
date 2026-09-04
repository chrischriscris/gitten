// Entry: styles in legacy cascade order, views mounted into the shell,
// interactions wired, legacy inline-handler names exposed on window.

import './styles/tokens.css';
import './styles/guide-chrome.css';
import './styles/desktop.css';
import './styles/modals.css';
import './styles/docs.css';
import './styles/settings-study.css';
import './styles/tui.css';

import modalsHtml from './components/modals.html?raw';
import settingsStudyHtml from './views/settings-study.html?raw';
import architectureHtml from './views/architecture.html?raw';
import blueprintHtml from './views/blueprint.html?raw';
import keyboardHtml from './views/keyboard.html?raw';
import mockupHtml from './views/mockup.html?raw';
import tokensHtml from './views/tokens.html?raw';
import { toggleAnnotations } from './interactions/annotations';
import { initClient, setClient } from './interactions/client';
import { initKeyboard } from './interactions/keyboard';
import { setKeymapMode } from './interactions/keymap';
import {
  closeModals,
  handleKeyHint,
  openAskModal,
  openExtensionsModal,
  openHelpModal,
  toggleExplain,
} from './interactions/modals';
import { focusPane, selectCommit, selectFile } from './interactions/panes';
import { changeTheme, cycleAccent } from './interactions/theme';
import { initTabs, switchGuideTab } from './interactions/tabs';
import { toggleTuiAscii, toggleTuiHelp, toggleTuiNarrow } from './interactions/tui';

function mount(id: string, html: string): void {
  const el = document.getElementById(id);
  if (el) el.innerHTML = html;
}

mount('view-mockup', mockupHtml + modalsHtml);
mount('view-blueprint', blueprintHtml);
mount('view-tokens', tokensHtml);
mount('view-keyboard', keyboardHtml);
mount('view-architecture', architectureHtml);
mount('view-settings', settingsStudyHtml);

// Bridge: the partials keep their legacy inline handlers (onclick="..."),
// so the functions they name must resolve on window. New code should use
// addEventListener instead; this table shrinks as partials get rewritten.
const globals: Record<string, unknown> = {
  switchGuideTab,
  toggleAnnotations,
  setKeymapMode,
  changeTheme,
  setClient,
  toggleTuiAscii,
  toggleTuiNarrow,
  toggleTuiHelp,
  cycleAccent,
  focusPane,
  selectFile,
  selectCommit,
  openAskModal,
  openHelpModal,
  openExtensionsModal,
  closeModals,
  toggleExplain,
  handleKeyHint,
};
for (const [name, fn] of Object.entries(globals)) {
  (window as unknown as Record<string, unknown>)[name] = fn;
}

initTabs();
initClient();
initKeyboard();
