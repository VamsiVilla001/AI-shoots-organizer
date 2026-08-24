// Copies the built `teo-server` next to the staged resources, so the installer
// ships it and the shell can find it beside the application.
//
// The desktop app is a client of that server: without it the app starts, says
// the local server did not start, and can do nothing else. That makes this a
// packaging step, not an optimisation.
//
//   node scripts/stage-sidecar.mjs      # after cargo build --release -p teo-server
import { copyFileSync, existsSync, mkdirSync, readdirSync, statSync } from 'node:fs'
import { dirname, join, resolve } from 'node:path'
import { fileURLToPath } from 'node:url'

const root = resolve(dirname(fileURLToPath(import.meta.url)), '..')
const staged = join(root, 'dist-resources')
const name = process.platform === 'win32' ? 'teo-server.exe' : 'teo-server'

// `cargo build --target <triple>` writes to `target/<triple>/release`, not
// `target/release`, and CI passes a triple on every platform — so the triples
// are discovered rather than listed. Hard-coding them meant a Windows CI build
// left the sidecar unstaged and the installer shipped an app with no server.
const targetDir = join(root, 'target')
const triples = existsSync(targetDir)
  ? readdirSync(targetDir, { withFileTypes: true })
      .filter((entry) => entry.isDirectory() && entry.name !== 'release' && entry.name !== 'debug')
      .map((entry) => join(targetDir, entry.name, 'release', name))
  : []

// A plain `--release` build first: it is what a local build produces, and what
// the build scripts document.
const candidates = [join(targetDir, 'release', name), ...triples]

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
