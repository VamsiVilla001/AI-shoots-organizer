import { defineConfig } from 'vite'
import react from '@vitejs/plugin-react'

// Port 1420 is what tauri.conf.json's devUrl expects.
export default defineConfig({
  plugins: [react()],
  clearScreen: false,
  server: {
    port: 1420,
    strictPort: true,
    watch: {
      // The Rust workspace triggers its own rebuilds; watching it from Vite
      // would just churn.
      ignored: ['**/src-tauri/**', '**/target/**'],
    },
  },
  build: {
    target: ['es2022', 'chrome110', 'safari15'],
    sourcemap: false,
  },
})
