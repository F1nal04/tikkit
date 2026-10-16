import { defineEnvVars } from '@sveltejs/kit/env'

export const variables = defineEnvVars({
  API_ORIGIN: {
    description: 'Origin of the Rust API, used for `/api` requests made during SSR.',
    schema: (value) => new URL(value || 'http://127.0.0.1:3001').origin,
  },
})
