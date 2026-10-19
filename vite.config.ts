import { defineConfig } from 'vite';
import react from '@vitejs/plugin-react';

/**
 * The desktop app has no HTTP backend: the UI talks to Rust through Tauri's
 * IPC (`invoke`), so there is no dev proxy and no API port.
 */
export default defineConfig({
  plugins: [react()],
  // Tauri opens the window on 5173 in dev; keep the port predictable.
  server: {
    port: 5173,
    strictPort: true,
  },
  clearScreen: false,
  build: {
    target: 'es2020',
    sourcemap: false,
    chunkSizeWarningLimit: 900,
    rollupOptions: {
      output: {
        manualChunks: {
          react: ['react', 'react-dom', 'react-router-dom'],
        },
      },
    },
  },
});
