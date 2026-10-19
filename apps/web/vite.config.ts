import adapter from '@sveltejs/adapter-node'
import { sveltekit } from '@sveltejs/kit/vite'
import { svelteTesting } from '@testing-library/svelte/vite'
import { defineConfig } from 'vitest/config'

export default defineConfig({
  plugins: [sveltekit({ adapter: adapter() }), svelteTesting()],
  server: {
    proxy: {
      '/api': 'http://127.0.0.1:3001',
    },
  },
  test: { environment: 'jsdom', include: ['src/**/*.test.ts'] },
})
