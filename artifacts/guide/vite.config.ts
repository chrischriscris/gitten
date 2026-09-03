import { defineConfig } from 'vite';

// Static showcase only: no SSR, no router. Relative base so dist/ also works opened from disk.
export default defineConfig({
  base: './',
  build: {
    outDir: 'dist',
  },
});
