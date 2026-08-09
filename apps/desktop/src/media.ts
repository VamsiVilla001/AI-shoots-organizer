/**
 * URLs for the teomedia:// protocol, plus a few formatting helpers.
 */

let base = 'http://teomedia.localhost'

/** Called once at startup with `AppInfo.mediaUrlBase`. */
export function setMediaBase(urlBase: string) {
  base = urlBase
}

export const thumbUrl = (mediaId: number) => `${base}/thumb/${mediaId}`
export const fullUrl = (mediaId: number) => `${base}/full/${mediaId}`
export const videoUrl = (mediaId: number) => `${base}/video/${mediaId}`

export function formatBytes(bytes: number): string {
  if (!Number.isFinite(bytes) || bytes < 0) return '—'
  if (bytes < 1024) return `${bytes} B`
  const units = ['KB', 'MB', 'GB', 'TB']
  let value = bytes
  let unit = -1
  do {
    value /= 1024
    unit += 1
  } while (value >= 1024 && unit < units.length - 1)
  return `${value.toFixed(value >= 100 ? 0 : 1)} ${units[unit]}`
}

/** Seconds → `m:ss` or `h:mm:ss`, for video durations and timestamps. */
export function formatTime(seconds: number): string {
  if (!Number.isFinite(seconds) || seconds < 0) return '0:00'
  const whole = Math.floor(seconds)
  const h = Math.floor(whole / 3600)
  const m = Math.floor((whole % 3600) / 60)
  const s = whole % 60
  const pad = (n: number) => String(n).padStart(2, '0')
  return h > 0 ? `${h}:${pad(m)}:${pad(s)}` : `${m}:${pad(s)}`
}

export function formatCount(n: number): string {
  return n.toLocaleString()
}

/** Confidence 0..1 → "98.4%" as the plan's mock-ups show it. */
export function formatConfidence(value: number | null): string {
  if (value === null || !Number.isFinite(value)) return '—'
  return `${(value * 100).toFixed(1)}%`
}

export function formatDate(iso: string | null): string {
  if (!iso) return '—'
  const date = new Date(iso)
  if (Number.isNaN(date.getTime())) return '—'
  return date.toLocaleDateString(undefined, { day: '2-digit', month: 'short', year: 'numeric' })
}
