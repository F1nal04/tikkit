import { afterEach, expect, test, vi } from 'vitest'
import { poll } from './poll.ts'

afterEach(() => vi.useRealTimers())

test('ticks on the interval and on reconnect, never overlapping, and keeps going after failures', async () => {
  vi.useFakeTimers()
  let release = () => {}
  const tick = vi.fn(() => new Promise<void>((resolve, reject) => {
    release = tick.mock.calls.length === 1 ? () => reject(new Error('offline')) : resolve
  }))
  const stop = poll(tick, 1000)
  await vi.advanceTimersByTimeAsync(1000)
  expect(tick).toHaveBeenCalledTimes(1)
  window.dispatchEvent(new Event('online')) // still running: skipped
  expect(tick).toHaveBeenCalledTimes(1)
  release()
  await vi.advanceTimersByTimeAsync(0)
  window.dispatchEvent(new Event('online'))
  expect(tick).toHaveBeenCalledTimes(2)
  release()
  stop()
  await vi.advanceTimersByTimeAsync(5000)
  expect(tick).toHaveBeenCalledTimes(2)
})

test('does not tick while the tab is hidden, and a tick that outlives the page does not apply', async () => {
  vi.useFakeTimers()
  let visible = false
  vi.spyOn(document, 'visibilityState', 'get').mockImplementation(() => (visible ? 'visible' : 'hidden'))
  let release = () => {}
  const applied: boolean[] = []
  const tick = vi.fn((current: () => boolean) => new Promise<void>((resolve) => {
    release = () => {
      applied.push(current())
      resolve()
    }
  }))
  const stop = poll(tick, 1000)
  await vi.advanceTimersByTimeAsync(3000)
  expect(tick).not.toHaveBeenCalled()
  visible = true
  document.dispatchEvent(new Event('visibilitychange'))
  expect(tick).toHaveBeenCalledTimes(1)
  stop()
  release()
  await vi.advanceTimersByTimeAsync(0)
  expect(applied).toEqual([false])
})
