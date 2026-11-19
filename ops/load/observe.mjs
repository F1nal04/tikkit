import { delay, percentiles, stack } from './client.mjs'

async function capture(args) {
  const child = Bun.spawn(args, { stdout: 'pipe', stderr: 'inherit' })
  const output = await new Response(child.stdout).text()
  if (await child.exited !== 0) throw new Error(`sampling failed: ${args[0]}`)
  return output.trim()
}
const compose = (helper, args) => capture(['bash', '-c', `. ops/load/stack.sh; ${helper} "$@"`, helper, ...args])
const peak = (result, key, n) => { result[key] = Math.max(result[key] ?? 0, n) }

export function sampler() {
  let running = true
  const db = {}, workers = {}, containers = {}, residence = []
  const ids = stack(['ps', '-q']).split('\n')
  const samples = (async () => {
    while (running) {
      const [connections, metrics, stats] = await Promise.all([
        compose('sql', ['-c', `SELECT count(*) FILTER (WHERE backend_type='client backend' AND usename='tikkit' AND state <> 'idle'),
          count(*) FILTER (WHERE backend_type='client backend' AND usename='tikkit'),
          count(*) FILTER (WHERE backend_type='client backend' AND pid <> pg_backend_pid()),
          current_setting('max_connections') FROM pg_stat_activity`]),
        compose('stack', ['exec', '-T', 'api', 'curl', '-fsS', 'http://localhost:9091/metrics']),
        capture(['docker', 'stats', '--no-stream', '--format', '{{json .}}', ...ids]),
      ])
      const [inUse, poolOpen, clientBackends, maxConnections] = connections.split('|').map(Number)
      peak(db, 'active_or_in_transaction', inUse)
      peak(db, 'pool_open', poolOpen)
      peak(db, 'client_backends', clientBackends)
      db.max_connections = maxConnections
      db.api_pool_cap = 10
      if (/^tikkit_worker_snapshot_up 1$/m.test(metrics)) {
        for (const [, metric, worker, value] of metrics.matchAll(
          /^tikkit_worker_(oldest_due_seconds|due_jobs|pending_jobs)\{worker="([^"]+)"\} ([\d.e+-]+)$/gm,
        )) peak(workers[worker] ??= {}, metric, Number(value))
      }
      for (const line of stats.split('\n').filter(Boolean)) {
        const row = JSON.parse(line)
        const service = row.Name.replace(/^tikkit-m4-50-/, '').replace(/-\d+$/, '')
        const stats = containers[service] ??= {}
        peak(stats, 'cpu_percent', parseFloat(row.CPUPerc))
        const [use, limit] = row.MemUsage.split(' / ')
        const memory = input => {
          const [, n, unit] = input.match(/^([\d.]+)([KMG]?i?B)$/)
          return Math.round(Number(n) * 1024 ** ['', 'K', 'M', 'G'].indexOf(unit[0] === 'B' ? '' : unit[0]))
        }
        peak(stats, 'memory_bytes', memory(use))
        peak(stats, 'memory_limit_bytes', memory(limit))
      }
      residence.push(performance.now())
      await delay(1200)
    }
  })()
  // Attach a rejection handler now, then pass the error to the caller at stop.
  samples.catch(() => {})
  return async () => {
    running = false
    await samples
    const inspect = JSON.parse(await capture(['docker', 'inspect', ...ids]))
    for (const container of inspect) {
      const service = container.Config.Labels['com.docker.compose.service']
      const stats = containers[service] ??= {}
      stats.configured_memory_bytes = container.HostConfig.Memory
      stats.configured_nanocpus = container.HostConfig.NanoCpus
    }
    return { samples: residence.length, db, workers, containers,
      sampling_interval_ms: percentiles(residence.slice(1).map((n, i) => n - residence[i])) }
  }
}
