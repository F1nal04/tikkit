import { delay, percentiles, stack } from './client.mjs'

async function capture(args, optional = false) {
  const child = Bun.spawn(args, { stdout: 'pipe', stderr: optional ? 'ignore' : 'inherit' })
  const output = await new Response(child.stdout).text()
  if (await child.exited !== 0) {
    if (optional) return null
    throw new Error(`sampling failed: ${args[0]}`)
  }
  return output.trim()
}
const compose = (helper, args, optional) => capture(['bash', '-c', `. ops/load/stack.sh; ${helper} "$@"`, helper, ...args], optional)
const peak = (result, key, n) => { result[key] = Math.max(result[key] ?? 0, n) }

const interval = timestamps => percentiles(timestamps.slice(1).map((n, i) => n - timestamps[i]))

export function sampler({ allowDatabaseOutage = false } = {}) {
  let running = true
  const db = {}, workers = {}, containers = {}, residence = [], dbTimes = []
  const ids = stack(['ps', '-q']).split('\n')
  const postgres = stack(['ps', '-q', 'postgres'])
  const connections = (async () => {
    while (running) {
      const start = performance.now()
      const output = await capture(['docker', 'exec', postgres, 'psql', '-U', 'postgres',
        '-d', 'tikkit', '-Atq', '-c',
        `SELECT count(*) FILTER (WHERE backend_type='client backend' AND usename='tikkit' AND state <> 'idle'),
          count(*) FILTER (WHERE backend_type='client backend' AND usename='tikkit'),
          count(*) FILTER (WHERE backend_type='client backend' AND pid <> pg_backend_pid()),
          current_setting('max_connections') FROM pg_stat_activity`], allowDatabaseOutage)
      dbTimes.push(performance.now())
      if (output === null) {
        db.unavailable_samples = (db.unavailable_samples ?? 0) + 1
      } else {
        const [inUse, poolOpen, clientBackends, maxConnections] = output.split('|').map(Number)
        peak(db, 'active_or_in_transaction', inUse)
        peak(db, 'pool_open', poolOpen)
        peak(db, 'client_backends', clientBackends)
        db.max_connections = maxConnections
        db.api_pool_cap = 10
      }
      await delay(Math.max(0, 250 - (performance.now() - start)))
    }
  })()
  connections.catch(() => {})
  const samples = (async () => {
    while (running) {
      const [metrics, stats] = await Promise.all([
        compose('stack', ['exec', '-T', 'api', 'curl', '-fsS', 'http://localhost:9091/metrics'], allowDatabaseOutage),
        capture(['docker', 'stats', '--no-stream', '--format', '{{json .}}', ...ids]),
      ])
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
    await Promise.all([samples, connections])
    const inspect = JSON.parse(await capture(['docker', 'inspect', ...ids]))
    for (const container of inspect) {
      const service = container.Config.Labels['com.docker.compose.service']
      const stats = containers[service] ??= {}
      stats.configured_memory_bytes = container.HostConfig.Memory
      stats.configured_nanocpus = container.HostConfig.NanoCpus
    }
    return { samples: residence.length, db, workers, containers,
      sampling_interval_ms: interval(residence), db_samples: dbTimes.length, db_sampling_interval_ms: interval(dbTimes) }
  }
}
