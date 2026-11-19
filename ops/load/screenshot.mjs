import { crc32, deflateSync } from 'node:zlib'

function chunk(type, data) {
  const length = Buffer.alloc(4)
  length.writeUInt32BE(data.length)
  const body = Buffer.concat([Buffer.from(type), data])
  const checksum = Buffer.alloc(4)
  checksum.writeUInt32BE(crc32(body))
  return Buffer.concat([length, body, checksum])
}

// Synthetic 1280x720 RGB screenshot. Blocks mimic window chrome and a noisy
// region prevents the upload from collapsing to a few bytes. No real pixels.
export function screenshot() {
  const width = 1280, height = 720
  const header = Buffer.alloc(13)
  header.writeUInt32BE(width)
  header.writeUInt32BE(height, 4)
  header[8] = 8
  header[9] = 2
  const stride = 1 + 3 * width
  const pixels = Buffer.alloc(stride * height)
  let noise = 7529
  for (let y = 0; y < height; y++) {
    for (let x = 0; x < width; x++) {
      const offset = y * stride + 1 + 3 * x
      noise = (Math.imul(noise, 1664525) + 1013904223) >>> 0
      const color = y < 96 ? 30 : x < 320 ? 96 : 215 - (y % 33)
      pixels[offset] = color
      pixels[offset + 1] = x > 880 && y % 4 === 0 ? noise >>> 24 : color + 8
      pixels[offset + 2] = x > 880 && y % 4 === 0 ? noise >>> 16 & 255 : color + 20
    }
  }
  return Buffer.concat([
    Buffer.from([137, 80, 78, 71, 13, 10, 26, 10]),
    chunk('IHDR', header), chunk('IDAT', deflateSync(pixels)), chunk('IEND', Buffer.alloc(0)),
  ])
}
