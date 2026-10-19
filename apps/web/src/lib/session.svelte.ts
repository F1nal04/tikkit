// Set when a signed-in request is answered 401, e.g. the session expired while a form was open.
// Browser-only state: the server never writes it, so SSR cannot leak it between requests.
export const session = $state({ expired: false })
