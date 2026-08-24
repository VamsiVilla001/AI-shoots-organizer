// Copies the built `teo-server` next to the staged resources, so the installer
// ships it and the shell can find it beside the application.
//
// The desktop app is a client of that server: without it the app starts, says
// the local server did not start, and can do nothing else. That makes this a
// packaging step, not an optimisation.
//
//   node scripts/stage-sidecar.mjs      # after cargo build --release -p teo-server
import { copyFileSync, existsSync, mkdirSync, statSync } from 'node:fs'
import { dirname, join, resolve } from 'node:path'
import { fileURLToPath } from 'node:url'

const root = resolve(dirname(fileURLToPath(import.meta.url)), '..')
const staged = join(root, 'dist-resources')
const name = process.platform === 'win32' ? 'teo-server.exe' : 'teo-server'

const candidates = [
  join(root, 'target', 'release', name),
  join(root, 'target', 'aarch64-apple-darwin', 'release', name),
  join(root, 'target', 'x86_64-apple-darwin', 'release', name),
]

const source = candidates.find(existsSync)
if (!source) {
  console.error(
    `Could not find ${name}.\n` +
      'Build it first: cargo build --release -p teo-server',
  )
  process.exit(1)
}

mkdirSync(staged, { recursive: true })
const target = join(staged, name)
copyFileSync(source, target)
console.log(`Staged ${name} (${(statSync(target).size / 1024 / 1024).toFixed(1)} MB)`)
console.log(`  from ${source}`)
console.log(`  to   ${target}`)
