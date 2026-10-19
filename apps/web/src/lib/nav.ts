import type { Role } from '@tikkit/api-client'

const STAFF: readonly Role[] = ['operator', 'specialist', 'team_lead', 'admin']

/**
 * Who may open which path prefix. Navigation aid only: Rust enforces access (docs/authorization.md).
 * New staff or admin pages need no entry here, their prefix already covers them.
 */
const GUARDS: readonly (readonly [prefix: string, roles: readonly Role[]])[] = [
  ['/admin', ['admin']],
  ['/queue', STAFF],
  ['/team', ['team_lead']],
]

export type NavSection = 'portal' | 'workspace' | 'admin'
export type NavKey = 'myTickets' | 'newTicket' | 'queue' | 'team' | 'adminHome' | 'accounts' | 'teams' | 'settings' | 'email'

/** Add a page to the sidebar here; it shows only to roles that may open `href`. */
export const NAV: readonly { section: NavSection; key: NavKey; href: string }[] = [
  { section: 'portal', key: 'myTickets', href: '/' },
  { section: 'portal', key: 'newTicket', href: '/tickets/new' },
  { section: 'workspace', key: 'queue', href: '/queue' },
  { section: 'workspace', key: 'team', href: '/team' },
  { section: 'admin', key: 'adminHome', href: '/admin' },
  { section: 'admin', key: 'accounts', href: '/admin/accounts' },
  { section: 'admin', key: 'teams', href: '/admin/teams' },
  { section: 'admin', key: 'settings', href: '/admin/settings' },
  { section: 'admin', key: 'email', href: '/admin/email' },
]

const under = (path: string, prefix: string) => path === prefix || path.startsWith(`${prefix}/`)

export function canAccess(role: Role, path: string) {
  // Match the decoded path SvelteKit routes, including encoded unreserved characters.
  path = decodeURI(path)
  return GUARDS.every(([prefix, roles]) => !under(path, prefix) || roles.includes(role))
}

/** Sidebar entries for a role, grouped in section order; empty sections are dropped. */
export function navFor(role: Role) {
  const items = NAV.filter((item) => canAccess(role, item.href))
  return (['portal', 'workspace', 'admin'] as const)
    .map((section) => ({ section, items: items.filter((i) => i.section === section) }))
    .filter((group) => group.items.length > 0)
}

/** The current page's entry: the longest matching href (`/` only matches exactly). */
export function activeHref(path: string) {
  path = decodeURI(path)
  return NAV.filter(({ href }) => (href === '/' ? path === '/' : under(path, href)))
    .map(({ href }) => href)
    .sort((a, b) => b.length - a.length)[0]
}
