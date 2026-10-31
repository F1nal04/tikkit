/** A same-origin path from `?next=`, or null. Rejects `//host`, backslashes, control characters, and /login loops. */
export function safeNext(value: string | null | undefined): string | null {
  if (!value || !/^\/(?![/\\])[^\\\u0000-\u001f]*$/.test(value)) return null
  return value === '/login' || value.startsWith('/login?') ? null : value
}
