// Mock data the interactions read. Was inline in the legacy <script>.

export interface Accent {
  name: string;
  hex: string;
}

export const accents: Accent[] = [
  { name: 'Amber', hex: '#dfa851' },
  { name: 'Sky Blue', hex: '#6f9ecf' },
  { name: 'Lavender', hex: '#a983c9' },
  { name: 'Teal', hex: '#5fa8a0' },
  { name: 'Coral', hex: '#c97d6f' },
  { name: 'Olive', hex: '#8fb35e' },
];

export interface FileDiff {
  title: string;
  adds: string;
  dels: string;
  hunk: string;
  explanation: string;
}

export const fileDiffs: Record<string, FileDiff> = {
  'commit.go': {
    title: 'internal/ai/commit.go',
    adds: '+34',
    dels: '-2',
    hunk: '@@ -12,8 +12,14 @@ func SynthesizeCommit(diff *Diff) string {',
    explanation:
      'Generates structured Conventional Commit messages using local heuristic models when offline, falling back to streaming API when connected.',
  },
  'pool.go': {
    title: 'internal/extension/pool.go',
    adds: '+118',
    dels: '-0',
    hunk: '@@ -0,0 +1,48 @@ package extension',
    explanation:
      'New worker pool isolating concurrent extension dispatches with bounded goroutines and token budget pre-allocation.',
  },
  'host.go': {
    title: 'internal/extension/host.go',
    adds: '+18',
    dels: '-6',
    hunk: '@@ -41,9 +41,11 @@ func (h *Host) Dispatch(ev Event) error {',
    explanation:
      'The goroutine became a pool submission, so a dispatch failure now surfaces to the caller instead of dying silently in the background. The budget check moved into the same guard as Handles — which means an exhausted budget skips the extension quietly rather than erroring. Worth confirming that is the intent.',
  },
  'budget.go': {
    title: 'internal/extension/budget.go',
    adds: '+14',
    dels: '-8',
    hunk: '@@ -22,6 +22,8 @@ func (b *Budget) Exhausted() bool {',
    explanation:
      'Adds atomic token decrement tracking to prevent runaway LLM context consumption during multi-file diff analyses.',
  },
  'extensions.md': {
    title: 'docs/extensions.md',
    adds: '+62',
    dels: '-0',
    hunk: '@@ -0,0 +1,24 @@ # Extension Architecture',
    explanation:
      'Architecture document defining the zero-overhead boundary between core algorithms and third-party AI assistants.',
  },
};
