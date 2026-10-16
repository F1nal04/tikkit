// Typed client for the Tikkit API, generated types in ./schema.d.ts.
// Framework-independent: pass SvelteKit's `fetch` in `load` functions. A client
// holds no per-user state (cookies travel with the injected fetch), so create
// one per request on the server rather than sharing a module-level instance.
import type { components, paths } from "./schema";

export type { components, paths };
export type Schemas = components["schemas"];
export type ErrorEnvelope = Schemas["ErrorEnvelope"];
export type ErrorCode = Schemas["ErrorCode"];
export type Language = Schemas["Language"];

type Method = "get" | "post" | "put" | "patch" | "delete";
type MethodsOf<P extends keyof paths> = {
  [M in Method]: paths[P][M] extends undefined ? never : M;
}[Method];
type Operation<P extends keyof paths, M extends Method> = NonNullable<paths[P][M]>;

type Params<O> = O extends { parameters: infer Ps } ? Ps : never;
type Body<O> = O extends { requestBody?: { content: { "application/json": infer B } } } ? B : never;
type Success<O> = O extends { responses: infer R }
  ? {
      [S in keyof R]: S extends 200 | 201 | 202
        ? R[S] extends { content: { "application/json": infer T } }
          ? T
          : unknown
        : S extends 204
          ? undefined
          : never;
    }[keyof R]
  : never;

export type RequestOptions<O> = {
  path?: Params<O> extends { path?: infer T } ? T : never;
  query?: Params<O> extends { query?: infer T } ? T : never;
  body?: Body<O>;
  headers?: Record<string, string>;
  signal?: AbortSignal;
};

export type ClientOptions = {
  /** Prefix for request paths. Default "" (same origin). */
  baseUrl?: string;
  /** Use SvelteKit's `fetch` in load functions so cookies and SSR work. */
  fetch?: typeof globalThis.fetch;
  /** Sent on every request, e.g. forwarded headers. */
  headers?: Record<string, string>;
  /** Interface language, sent as Accept-Language. Never affects permissions. */
  language?: Language;
};

/** A non-2xx response. `body` is the error envelope when the API produced one. */
export class ApiError extends Error {
  readonly status: number;
  readonly body: ErrorEnvelope | null;

  constructor(status: number, body: ErrorEnvelope | null) {
    super(body ? `${body.error.code}: ${body.error.message}` : `HTTP ${status}`);
    this.name = "ApiError";
    this.status = status;
    this.body = body;
  }

  /** Stable machine-readable code; translate this rather than `message`. */
  get code(): ErrorCode | null {
    return this.body?.error.code ?? null;
  }

  get requestId(): string | null {
    return this.body?.request_id ?? null;
  }

  /** Field path -> field error codes, for validation errors. */
  get fields(): Record<string, string[]> {
    return this.body?.error.details?.fields ?? {};
  }
}

function isEnvelope(value: unknown): value is ErrorEnvelope {
  const error = (value as { error?: { code?: unknown } } | null)?.error;
  return typeof error?.code === "string";
}

export function createClient(options: ClientOptions = {}) {
  const baseUrl = options.baseUrl ?? "";

  async function request<P extends keyof paths, M extends MethodsOf<P>>(
    method: M,
    path: P,
    init: RequestOptions<Operation<P, M>> = {},
  ): Promise<Success<Operation<P, M>>> {
    const fetchImpl = options.fetch ?? globalThis.fetch;
    const pathParams = (init.path ?? {}) as Record<string, unknown>;
    let url =
      baseUrl +
      String(path).replace(/\{(\w+)\}/g, (_, name: string) =>
        encodeURIComponent(String(pathParams[name])),
      );
    const query = new URLSearchParams();
    for (const [key, value] of Object.entries((init.query ?? {}) as Record<string, unknown>)) {
      if (value !== undefined && value !== null) query.set(key, String(value));
    }
    if (query.size) url += `?${query}`;

    const headers: Record<string, string> = { accept: "application/json", ...options.headers };
    if (options.language) headers["accept-language"] = options.language;
    if (init.body !== undefined) headers["content-type"] = "application/json";
    Object.assign(headers, init.headers);

    const response = await fetchImpl(url, {
      method: method.toUpperCase(),
      headers,
      body: init.body === undefined ? undefined : JSON.stringify(init.body),
      credentials: "include",
      signal: init.signal,
    });
    if (response.status === 204) return undefined as Success<Operation<P, M>>;
    const text = await response.text();
    const data: unknown = text ? JSON.parse(text) : undefined;
    if (!response.ok) throw new ApiError(response.status, isEnvelope(data) ? data : null);
    return data as Success<Operation<P, M>>;
  }

  return { request };
}

export type Client = ReturnType<typeof createClient>;
