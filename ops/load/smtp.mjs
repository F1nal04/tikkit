// Synthetic relay. Keep only envelope-independent counters and content hashes.
import { appendFileSync, readFileSync } from 'node:fs'
import { createHash } from 'node:crypto'
import { createServer } from 'node:tls'
import { createInterface } from 'node:readline'

const control = '/load-control/'
const record = (kind, hash = '') => appendFileSync(control + 'smtp-events', `${kind} ${hash}\n`)
createServer({
  cert: readFileSync('/workflow-tls/stub.crt'),
  key: readFileSync('/workflow-tls/stub.key'),
}, socket => {
  socket.on('error', () => {})
  const send = line => socket.write(line + '\r\n')
  const mode = () => readFileSync(control + 'smtp', 'utf8').trim()
  send('220 smtp.load.test')
  let data
  const lines = createInterface({ input: socket, crlfDelay: Infinity })
  lines.on('line', line => {
    if (data) {
      if (line !== '.') {
        // Message-ID, Date and transfer encoding may vary across retries.
        if (!/^(?:message-id|date):/i.test(line)) data.update(line + '\n')
        return
      }
      const hash = data.digest('hex')
      data = undefined
      if (mode() === 'hold') {
        record('held', hash)
        return
      }
      if (mode() === 'reject') {
        record('refused')
        send('451 Try again later')
        return
      }
      record('accepted', hash)
      send('250 Accepted')
    } else if (/^EHLO /i.test(line)) {
      send('250-smtp.load.test\r\n250 AUTH PLAIN')
    } else if (/^AUTH PLAIN /i.test(line)) {
      send('235 Authenticated')
    } else if (/^(MAIL FROM:|RCPT TO:)/i.test(line)) {
      send('250 OK')
    } else if (line === 'DATA') {
      data = createHash('sha256')
      send('354 End with dot')
    } else if (line === 'QUIT') {
      send('221 Bye')
      socket.end()
    } else {
      send('502 Unsupported')
    }
  })
}).listen(2465, '0.0.0.0')
