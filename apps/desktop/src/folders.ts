/**
 * Folder-name preview.
 *
 * Mirrors `sanitise_component` in `crates/export-engine/src/naming.rs` so the
 * app can show the exact folder a group will produce *before* the export runs.
 * The Rust side remains the authority — if the two ever disagree, fix this one.
 */

const ILLEGAL = /[<>:"/\\|?*\u0000-\u001f]/g

/** Device names Windows refuses regardless of extension. */
const RESERVED = new Set([
  'CON', 'PRN', 'AUX', 'NUL',
  'COM1', 'COM2', 'COM3', 'COM4', 'COM5', 'COM6', 'COM7', 'COM8', 'COM9',
  'LPT1', 'LPT2', 'LPT3', 'LPT4', 'LPT5', 'LPT6', 'LPT7', 'LPT8', 'LPT9',
])

const MAX_COMPONENT = 80

export function sanitiseComponent(input: string): string {
  let out = input.replace(ILLEGAL, '_')

  // Windows strips trailing dots and spaces, so a name ending in one would not
  // match what was recorded.
  out = out.trim().replace(/[. ]+$/, '').trim()

  if ([...out].length > MAX_COMPONENT) {
    out = [...out].slice(0, MAX_COMPONENT).join('').trimEnd()
  }

  const stem = (out.split('.')[0] ?? '').toUpperCase()
  if (RESERVED.has(stem)) out = `${out}_`

  return out || 'Unnamed'
}

/** The folder a group exports to: its override if set, otherwise its name. */
export function folderNameFor(group: { name: string; folderName: string | null }): string {
  const override = group.folderName?.trim()
  return sanitiseComponent(override && override.length > 0 ? override : group.name)
}
