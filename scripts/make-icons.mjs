// Generates the application icon set with no external dependencies.
//
// Tauri needs PNG/ICO/ICNS files present before it will build a bundle, and
// pulling in an image toolchain just for a placeholder mark is not worth it —
// Node already ships zlib, which is the only hard part of writing a PNG.
//
//   node scripts/make-icons.mjs

import { deflateSync } from 'node:zlib'
import { mkdirSync, writeFileSync } from 'node:fs'
import { dirname, join } from 'node:path'
import { fileURLToPath } from 'node:url'

const OUT_DIR = join(dirname(fileURLToPath(import.meta.url)), '..', 'apps', 'desktop', 'src-tauri', 'icons')

// ---------------------------------------------------------------------------
// PNG encoding
// ---------------------------------------------------------------------------

const CRC_TABLE = (() => {
  const table = new Int32Array(256)
  for (let n = 0; n < 256; n++) {
    let c = n
    for (let k = 0; k < 8; k++) c = c & 1 ? 0xedb88320 ^ (c >>> 1) : c >>> 1
    table[n] = c
  }
  return table
})()

function crc32(buffer) {
  let c = 0xffffffff
  for (const byte of buffer) c = CRC_TABLE[(c ^ byte) & 0xff] ^ (c >>> 8)
  return (c ^ 0xffffffff) >>> 0
}

function chunk(type, data) {
  const length = Buffer.alloc(4)
  length.writeUInt32BE(data.length)
  const body = Buffer.concat([Buffer.from(type, 'ascii'), data])
  const crc = Buffer.alloc(4)
  crc.writeUInt32BE(crc32(body))
  return Buffer.concat([length, body, crc])
}

/** Encodes RGBA pixel data as a PNG. */
function encodePng(width, height, rgba) {
  const header = Buffer.alloc(13)
  header.writeUInt32BE(width, 0)
  header.writeUInt32BE(height, 4)
  header[8] = 8 // bit depth
  header[9] = 6 // colour type: RGBA
  // 10-12: compression, filter and interlace methods, all zero.

  // Each scanline is prefixed with its filter type; 0 means "none", which
  // compresses well enough for flat artwork.
  const raw = Buffer.alloc(height * (width * 4 + 1))
  for (let y = 0; y < height; y++) {
    const rowStart = y * (width * 4 + 1)
    raw[rowStart] = 0
    rgba.copy(raw, rowStart + 1, y * width * 4, (y + 1) * width * 4)
  }

  return Buffer.concat([
    Buffer.from([0x89, 0x50, 0x4e, 0x47, 0x0d, 0x0a, 0x1a, 0x0a]),
    chunk('IHDR', header),
    chunk('IDAT', deflateSync(raw, { level: 9 })),
    chunk('IEND', Buffer.alloc(0)),
  ])
}

// ---------------------------------------------------------------------------
// The mark: a rounded tile with three overlapping "player" rings
// ---------------------------------------------------------------------------

const BACKGROUND = [16, 18, 27]
const RINGS = [
  { cx: 0.36, cy: 0.42, r: 0.2, colour: [124, 92, 255] },
  { cx: 0.64, cy: 0.42, r: 0.2, colour: [0, 209, 178] },
  { cx: 0.5, cy: 0.66, r: 0.2, colour: [255, 92, 141] },
]

function renderIcon(size) {
  const rgba = Buffer.alloc(size * size * 4)
  const radius = size * 0.22

  for (let y = 0; y < size; y++) {
    for (let x = 0; x < size; x++) {
      const i = (y * size + x) * 4
      const inside = insideRoundedSquare(x + 0.5, y + 0.5, size, radius)
      if (inside <= 0) continue

      let [r, g, b] = BACKGROUND
      for (const ring of RINGS) {
        const dx = (x + 0.5) / size - ring.cx
        const dy = (y + 0.5) / size - ring.cy
        const distance = Math.sqrt(dx * dx + dy * dy)
        // A soft annulus, so the rings read at 32px as well as 512px.
        const band = 1 - Math.min(1, Math.abs(distance - ring.r) / (ring.r * 0.42))
        if (band > 0) {
          const weight = band * band
          r = r + (ring.colour[0] - r) * weight
          g = g + (ring.colour[1] - g) * weight
          b = b + (ring.colour[2] - b) * weight
        }
      }

      rgba[i] = Math.round(r)
      rgba[i + 1] = Math.round(g)
      rgba[i + 2] = Math.round(b)
      rgba[i + 3] = Math.round(255 * inside)
    }
  }
  return rgba
}

/** Coverage in 0..1 for a rounded square, giving the edge a little antialiasing. */
function insideRoundedSquare(x, y, size, radius) {
  const nx = Math.max(radius - x, x - (size - radius), 0)
  const ny = Math.max(radius - y, y - (size - radius), 0)
  const distance = Math.sqrt(nx * nx + ny * ny)
  return Math.max(0, Math.min(1, radius - distance + 0.5))
}

function png(size) {
  return encodePng(size, size, renderIcon(size))
}

// ---------------------------------------------------------------------------
// Container formats
// ---------------------------------------------------------------------------

/** ICO with PNG-compressed entries, which every supported Windows version reads. */
function encodeIco(sizes) {
  const images = sizes.map((size) => ({ size, data: png(size) }))

  const header = Buffer.alloc(6)
  header.writeUInt16LE(0, 0) // reserved
  header.writeUInt16LE(1, 2) // type: icon
  header.writeUInt16LE(images.length, 4)

  let offset = 6 + images.length * 16
  const entries = []
  for (const image of images) {
    const entry = Buffer.alloc(16)
    entry[0] = image.size >= 256 ? 0 : image.size // 0 means 256
    entry[1] = image.size >= 256 ? 0 : image.size
    entry.writeUInt16LE(1, 4) // colour planes
    entry.writeUInt16LE(32, 6) // bits per pixel
    entry.writeUInt32LE(image.data.length, 8)
    entry.writeUInt32LE(offset, 12)
    entries.push(entry)
    offset += image.data.length
  }

  return Buffer.concat([header, ...entries, ...images.map((i) => i.data)])
}

/** ICNS carrying PNG entries for the sizes macOS actually asks for. */
function encodeIcns() {
  const entries = [
    ['ic07', 128],
    ['ic08', 256],
    ['ic09', 512],
    ['ic10', 1024],
  ].map(([type, size]) => {
    const data = png(size)
    const length = Buffer.alloc(4)
    length.writeUInt32BE(data.length + 8)
    return Buffer.concat([Buffer.from(type, 'ascii'), length, data])
  })

  const body = Buffer.concat(entries)
  const length = Buffer.alloc(4)
  length.writeUInt32BE(body.length + 8)
  return Buffer.concat([Buffer.from('icns', 'ascii'), length, body])
}

// ---------------------------------------------------------------------------

mkdirSync(OUT_DIR, { recursive: true })

const files = {
  '32x32.png': png(32),
  '128x128.png': png(128),
  '128x128@2x.png': png(256),
  'icon.png': png(512),
  'Square30x30Logo.png': png(30),
  'Square44x44Logo.png': png(44),
  'Square71x71Logo.png': png(71),
  'Square89x89Logo.png': png(89),
  'Square107x107Logo.png': png(107),
  'Square142x142Logo.png': png(142),
  'Square150x150Logo.png': png(150),
  'Square284x284Logo.png': png(284),
  'Square310x310Logo.png': png(310),
  'StoreLogo.png': png(50),
  'icon.ico': encodeIco([16, 32, 48, 64, 128, 256]),
  'icon.icns': encodeIcns(),
}

for (const [name, data] of Object.entries(files)) {
  writeFileSync(join(OUT_DIR, name), data)
}

console.log(`Wrote ${Object.keys(files).length} icon files to ${OUT_DIR}`)
