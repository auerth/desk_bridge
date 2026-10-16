import react from '@vitejs/plugin-react';
import { defineConfig } from 'vite';

// Tauri expects a fixed dev port (see src-tauri/tauri.conf.json -> build.devUrl).
export default defineConfig({
  plugins: [react()],
  clearScreen: false,
  server: { port: 1420, strictPort: true, host: '127.0.0.1' },
  envPrefix: ['VITE_', 'TAURI_ENV_*'],
  build: {
    target: 'chrome105', // WebView2 (Chromium) is always evergreen
    sourcemap: false,
    chunkSizeWarningLimit: 900,
  },
});
