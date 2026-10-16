import react from '@vitejs/plugin-react'
import { defineConfig } from 'vitest/config'

// Deliberately independent of vite.config.ts: component tests render React
// directly and must not load the TanStack Start or Nitro plugins.
export default defineConfig({
  plugins: [react()],
  test: {
    environment: 'jsdom',
    include: ['src/**/*.test.{ts,tsx}'],
    setupFiles: ['./src/test/setup.ts'],
    restoreMocks: true,
  },
})
