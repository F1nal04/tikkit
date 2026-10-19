/** How often open staff pages check for changes made elsewhere. */
export const POLL_MS = 15_000

/**
 * Runs `tick` every `ms` while the tab is visible, and once when it becomes visible or
 * goes back online (a reconnect after sleep, a background tab, a dropped network).
 * Ticks never overlap. `current` is false after cleanup, including while a tick is
 * still in flight, so leaving the page cannot reload whatever replaced it. A failing
 * tick waits for the next one. Returns the cleanup.
 */
export function poll(tick: (current: () => boolean) => Promise<unknown>, ms = POLL_MS) {
  let stopped = false
  let running = false
  const current = () => !stopped
  const run = async () => {
    if (stopped || running || document.visibilityState !== 'visible') return
    running = true
    try {
      await tick(current)
    } catch {
      // Offline or API down: the next tick retries.
    } finally {
      running = false
    }
  }
  const timer = setInterval(run, ms)
  document.addEventListener('visibilitychange', run)
  window.addEventListener('online', run)
  return () => {
    stopped = true
    clearInterval(timer)
    document.removeEventListener('visibilitychange', run)
    window.removeEventListener('online', run)
  }
}
