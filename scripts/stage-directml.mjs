// Copies the DirectML redistributable that ONNX Runtime downloaded into a
// stable folder, so the Windows installer can ship it beside the executable.
//
// Why this exists: Windows itself only carries DirectML 1.0 in System32, while
// ONNX Runtime wants the 1.15 build that `ort` fetches at compile time. Without
// the newer DLL an installed copy quietly falls back to the CPU provider. The
// DLL cannot be referenced straight out of `target/`, because that is the
// directory cargo is writing while the bundler reads it — hence a copy.
//
//   node scripts/stage-directml.mjs        # after at least one release build
//
// Then build with `--config src-tauri/tauri.directml.conf.json`, which is what
// `npm run package:win` does.
import { copyFileSync, existsSync, mkdirSync, readdirSync, realpathSync, statSync } from 'node:fs'
import { dirname, join, resolve } from 'node:path'
import { fileURLToPath } from 'node:url'

const root = resolve(dirname(fileURLToPath(import.meta.url)), '..')
const staged = join(root, 'dist-resources')
const name = 'DirectML.dll'

if (process.platform !== 'win32') {
  console.log(`${name} is a Windows component; nothing to stage on ${process.platform}.`)
  process.exit(0)
}

/** Newest DirectML.dll in ort's download cache, if the build ever ran. */
function fromOrtCache() {
  const cache = join(process.env.LOCALAPPDATA ?? '', 'ort.pyke.io', 'dfbin')
  if (!existsSync(cache)) return null

  const found = []
  const walk = (dir, depth) => {
    if (depth > 4) return
    for (const entry of readdirSync(dir, { withFileTypes: true })) {
      const path = join(dir, entry.name)
      if (entry.isDirectory()) walk(path, depth + 1)
      else if (entry.name === name) found.push(path)
    }
  }
  walk(cache, 0)

  return found.sort((a, b) => statSync(b).mtimeMs - statSync(a).mtimeMs)[0] ?? null
}

// The build leaves a symlink at target/release/DirectML.dll pointing into that
// cache; resolve it rather than copying a link.
const candidates = [join(root, 'target', 'release', name), join(root, 'target', 'debug', name)]
let source = candidates.find(existsSync)
source = source ? realpathSync(source) : fromOrtCache()

if (!source) {
  console.error(
    `Could not find ${name}.\n` +
      'ONNX Runtime downloads it during the first build, so run a build once — ' +
      '`cargo build --release -p teo-face-detection` is enough — then re-run this script.',
  )
  process.exit(1)
}

mkdirSync(staged, { recursive: true })
const target = join(staged, name)
copyFileSync(source, target)
console.log(`Staged ${name} (${(statSync(target).size / 1024 / 1024).toFixed(1)} MB)`)
console.log(`  from ${source}`)
console.log(`  to   ${target}`)
