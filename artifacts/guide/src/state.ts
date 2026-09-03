// The guide's entire mutable state in one place. Everything else renders
// from this plus the static partials in src/views/.

export const state = {
  keymapMode: 'lazygit' as 'lazygit' | 'mockup',
  currentFocusedPane: 5,
  previousFocusedPane: 1,
  annotationsOn: false,
  currentClient: 'desktop' as 'desktop' | 'tui',
  tuiAscii: false,
  tuiCommitsBackup: null as string | null,
  tuiNarrow: false,
  accentIdx: 0,
};
