import { tanstackStart } from '@tanstack/react-start/plugin/vite'
import react from '@vitejs/plugin-react'
import { nitro } from 'nitro/vite'
import { defineConfig } from 'vite'

export default defineConfig(({ command }) => ({
  // Nitro's dev middleware runs before Vite's API proxy.
  plugins: [tanstackStart(), command === 'build' && nitro(), react()],
  server: {
    proxy: {
      '/api': 'http://127.0.0.1:3001',
    },
  },
}))
