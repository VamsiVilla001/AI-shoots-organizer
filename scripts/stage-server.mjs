// Copies the release build of skwad-server into the desktop bundle's
// resources, so the installer ships the server next to the app (under
// `server/` in the install folder). `npm run stage:server` builds it first.
//
// The folder is declared in `tauri.server.conf.json`, an overlay the build
// scripts pass, rather than in `tauri.conf.json`: Tauri checks that a
// resource exists at compile time of the desktop crate, and a plain
// `cargo check` on a machine that has not built the server must keep working.

import { copyFileSync, existsSync, mkdirSync } from 'node:fs'
import { dirname, join } from 'node:path'
import { fileURLToPath } from 'node:url'

const root = join(dirname(fileURLToPath(import.meta.url)), '..')
const exe = process.platform === 'win32' ? 'skwad-server.exe' : 'skwad-server'
const built = join(root, 'target', 'release', exe)
const destination = join(root, 'apps', 'desktop', 'src-tauri', 'binaries')

if (!existsSync(built)) {
  console.error(`stage-server: ${built} is missing — run \`cargo build --release -p skwad-server\` first`)
  process.exit(1)
}
mkdirSync(destination, { recursive: true })
copyFileSync(built, join(destination, exe))
console.log(`stage-server: staged ${exe} into ${destination}`)
