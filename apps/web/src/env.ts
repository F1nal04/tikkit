import { defineEnvVars } from '@sveltejs/kit/env'

export const variables = defineEnvVars({
  API_ORIGIN: {
    description: 'Origin of the Rust API, used for `/api` requests made during SSR.',
    schema: (value) => {
      const url = new URL(value || 'http://127.0.0.1:3001')
      if (url.protocol !== 'http:' && url.protocol !== 'https:') {
        throw new Error(`API_ORIGIN must be an http(s) URL, got ${url.protocol}`)
      }
      return url.origin
    },
  },
})
