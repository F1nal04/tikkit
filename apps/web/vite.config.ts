import adapter from '@sveltejs/adapter-node'
import { sveltekit } from '@sveltejs/kit/vite'
import { svelteTesting } from '@testing-library/svelte/vite'
import { defineConfig } from 'vitest/config'

// Every page is rendered per request, so SvelteKit adds a fresh nonce to its own
// inline scripts. Nothing else inline, nothing from another origin. In dev,
// SvelteKit adds 'unsafe-inline' to style-src itself for Vite's injected styles.
// The origin check for form posts (`csrf`) keeps SvelteKit's default: on.
const csp = {
  mode: 'nonce' as const,
  directives: {
    'default-src': ['self' as const],
    'script-src': ['self' as const],
    'style-src': ['self' as const],
    // SvelteKit's own route announcer uses a style attribute. Attributes only:
    // inline <style> elements still need the nonce, and markup is never injected.
    'style-src-attr': ['unsafe-inline' as const],
    'img-src': ['self' as const],
    'font-src': ['self' as const],
    'connect-src': ['self' as const],
    'frame-src': ['none' as const],
    'object-src': ['none' as const],
    'base-uri': ['none' as const],
    'form-action': ['self' as const],
    'frame-ancestors': ['none' as const],
  },
}

export default defineConfig({
  plugins: [sveltekit({ adapter: adapter(), csp }), svelteTesting()],
  server: {
    proxy: {
      '/api': {
        target: 'http://127.0.0.1:3001',
        configure(proxy) {
          proxy.on('proxyReq', (request) => request.removeHeader('x-request-id'))
        },
      },
    },
  },
  test: { environment: 'jsdom', include: ['src/**/*.test.ts'] },
})
