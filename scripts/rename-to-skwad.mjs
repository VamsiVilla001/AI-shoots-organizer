// One-time mechanical product and package rename for the V2.0 branch.
// paths.rs is intentionally excluded because it retains the legacy identifier
// needed by the first-launch data migration.
import { readFileSync, writeFileSync, statSync } from 'node:fs'
import { execFileSync } from 'node:child_process'
import { join } from 'node:path'

const root = process.argv[2] ?? process.cwd()
const rules = [
  [/Esports AI Media Organiser/g, 'SKWAD Media Organiser'],
  [/TE Organiser/g, 'SKWAD Media Organiser'],
  [/com\.teorganiser\.desktop/g, 'com.skwad.mediaorganiser'],
  [/esports-media-ai/g, 'skwad-media-organiser'],
  [/@teo\//g, '@skwad/'],
  [/teomedia/g, 'skwadmedia'],
  [/teo:\/\//g, 'skwad://'],
  [/TEO_/g, 'SKWAD_'],
  [/teo\.log/g, 'skwad.log'],
  [/teo_/g, 'skwad_'],
  [/teo-/g, 'skwad-'],
]
const skip = new Set([
  'Cargo.lock',
  'package-lock.json',
  'apps/desktop/src-tauri/src/paths.rs',
  'scripts/rename-to-skwad.mjs',
])

const tracked = execFileSync('git', ['-C', root, 'ls-files'], { encoding: 'utf8' })
  .split('\n')
  .map((line) => line.trim())
  .filter(Boolean)

for (const relative of tracked) {
  if (skip.has(relative) || skip.has(relative.split('/').pop())) continue
  const path = join(root, relative)
  let before
  try {
    if (statSync(path).size > 8 * 1024 * 1024) continue
    before = readFileSync(path, 'utf8')
  } catch {
    continue
  }
  if (before.includes('\0')) continue
  let after = before
  for (const [pattern, replacement] of rules) after = after.replace(pattern, replacement)
  if (after !== before) writeFileSync(path, after)
}
