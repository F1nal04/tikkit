// The one typed client for the Tikkit API. Types come from schema.gen.ts,
// generated from apps/api/openapi.json (`bun run generate:api`).
//
// Every non-2xx response and every network failure rejects with an
// `ApiError`, so callers only handle success data or one error type:
//
//   const health = await unwrap(api.GET('/api/health'))
//
// Authorization is enforced by the API; the client only forwards the
// same-origin session cookie and never decides what a user may do.
import createClient, { type Middleware } from 'openapi-fetch'
import type { components, paths } from './schema.gen'

export type Schemas = components['schemas']
export type ServerErrorCode = Schemas['ErrorCode']
export type FieldErrorCode = Schemas['FieldErrorCode']
export type FieldError = Schemas['FieldError']
/** Server codes plus client-side failures that never reach the API. */
export type ApiErrorCode = ServerErrorCode | 'network_error' | 'unknown_error'

/** Cursor page as returned by every list endpoint. */
export interface Page<T> {
  items: T[]
  nextCursor: string | null
}

export class ApiError extends Error {
  readonly code: ApiErrorCode
  readonly status: number
  readonly fields: FieldError[]
  readonly requestId: string | null

  constructor(init: {
    code: ApiErrorCode
    message: string
    status?: number
    fields?: FieldError[]
    requestId?: string | null
    cause?: unknown
  }) {
    super(init.message, { cause: init.cause })
    this.name = 'ApiError'
    this.code = init.code
    this.status = init.status ?? 0
    this.fields = init.fields ?? []
    this.requestId = init.requestId ?? null
  }

  /** First error for a field (camelCase name, e.g. `displayName`). */
  field(name: string): FieldError | undefined {
    return this.fields.find((field) => field.field === name)
  }
}

export function isApiError(error: unknown): error is ApiError {
  return error instanceof ApiError
}

// A Record (not a Set) so tsc fails when the server adds a code.
const SERVER_CODES: Record<ServerErrorCode, true> = {
  invalid_request: true,
  validation_failed: true,
  unauthenticated: true,
  forbidden: true,
  csrf_rejected: true,
  not_found: true,
  method_not_allowed: true,
  conflict: true,
  payload_too_large: true,
  rate_limited: true,
  internal_error: true,
  service_unavailable: true,
}

function isErrorBody(value: unknown): value is Schemas['ErrorBody'] {
  if (typeof value !== 'object' || value === null || !('error' in value)) return false
  const error = value.error
  return (
    typeof error === 'object' &&
    error !== null &&
    'code' in error &&
    typeof error.code === 'string' &&
    Object.hasOwn(SERVER_CODES, error.code)
  )
}

/** Converts a non-2xx response into an `ApiError`. */
export async function toApiError(response: Response): Promise<ApiError> {
  const headerId = response.headers.get('x-request-id')
  let body: unknown = null
  try {
    body = await response.clone().json()
  } catch {
    // Not JSON (e.g. a proxy error page); fall through.
  }
  if (isErrorBody(body)) {
    return new ApiError({
      code: body.error.code,
      message: body.error.message,
      status: response.status,
      fields: body.error.fields ?? [],
      requestId: body.error.requestId ?? headerId,
    })
  }
  return new ApiError({
    code: response.status === 503 ? 'service_unavailable' : 'unknown_error',
    message: `Unexpected response (HTTP ${response.status})`,
    status: response.status,
    requestId: headerId,
  })
}

function isAbort(error: unknown): boolean {
  return error instanceof DOMException && error.name === 'AbortError'
}

export const errorMiddleware: Middleware = {
  onRequest({ request }) {
    request.headers.set('accept', 'application/json')
    // Marks requests as coming from the app; CSRF protection itself is the
    // server's Origin check on unsafe methods.
    request.headers.set('x-requested-with', 'tikkit')
    return request
  },
  async onResponse({ response }) {
    if (!response.ok) throw await toApiError(response)
    return undefined
  },
  onError({ error }) {
    if (error instanceof ApiError || isAbort(error)) return error as Error
    return new ApiError({
      code: 'network_error',
      message: 'The server could not be reached.',
      cause: error,
    })
  },
}

export const api = createClient<paths>({
  // Same origin. Request needs an absolute URL outside real browsers
  // (jsdom, Node), so resolve against the current location when there is one.
  baseUrl: typeof window === 'undefined' ? '' : window.location.origin,
  credentials: 'same-origin',
  // Resolve fetch per call so tests (and polyfills) can replace it.
  fetch: (request) => globalThis.fetch(request),
})
api.use(errorMiddleware)

/**
 * Awaits a client call and returns its data. Errors are already thrown by
 * the middleware; this only narrows away `undefined` for typed bodies.
 */
export async function unwrap<T>(call: Promise<{ data?: T; response: Response }>): Promise<T> {
  const { data, response } = await call
  if (data === undefined && response.status !== 204) {
    throw new ApiError({
      code: 'unknown_error',
      message: 'The response had no body.',
      status: response.status,
      requestId: response.headers.get('x-request-id'),
    })
  }
  return data as T
}
