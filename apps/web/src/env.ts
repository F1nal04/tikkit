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
  PROTOCOL_HEADER: {
    description:
      'adapter-node: the header the reverse proxy sets to the request protocol (e.g. `x-forwarded-proto`). ' +
      'The page URL takes its protocol from it; HSTS is sent only when it is set and says https.',
    schema: (value) => (value ?? '').trim().toLowerCase(),
  },
})
