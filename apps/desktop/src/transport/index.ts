/**
 * Picks the transport once, at boot, and hands it to everything else.
 *
 * Inside the Tauri window `window.__TAURI_INTERNALS__` exists; served from
 * `teo-server` it does not. That single check is the whole decision — nothing
 * downstream branches on it again.
 */

import { createTauriTransport } from './tauri'
import { createHttpTransport, loadConnection, type HttpConnection, type HttpTransport } from './http'
import type { Transport } from './types'

export type { Transport, MediaKind } from './types'
export { UnsupportedByTransport } from './types'
export {
  forgetConnection,
  loadConnection,
  NotAuthorised,
  saveConnection,
  type HttpConnection,
} from './http'

export function isTauri(): boolean {
  return typeof window !== 'undefined' && '__TAURI_INTERNALS__' in window
}

let active: Transport | null = null

/**
 * The transport in use. Throws before [`initTransport`] has run, which is a bug
 * rather than a state to handle: nothing should call the backend before boot.
 */
export function transport(): Transport {
  if (!active) {
    throw new Error('the transport has not been initialised yet')
  }
  return active
}

export function transportReady(): boolean {
  return active !== null
}

/** Desktop boot: always available, nothing to configure. */
export function initTauriTransport(): Transport {
  active = createTauriTransport()
  return active
}

/**
 * Browser boot. Opens the session so media tags and the event stream carry the
 * cookie, then leaves the transport in place for the rest of the session.
 */
export async function initHttpTransport(connection: HttpConnection): Promise<HttpTransport> {
  const next = createHttpTransport(connection)
  await next.openSession()
  active = next
  return next
}

export function resetTransport() {
  if (active && active.kind === 'http') {
    ;(active as HttpTransport).close()
  }
  active = null
}

/**
 * The connection a browser build should try first: whatever was saved, else the
 * origin serving the bundle with no token yet — which is the normal case when
 * the server itself served the page and a cookie may already be in place.
 */
export function initialConnection(): HttpConnection | null {
  const saved = loadConnection()
  if (saved) return saved
  return null
}

/** Same-origin default for the connection screen's first render. */
export function defaultBaseUrl(): string {
  if (typeof window === 'undefined') return ''
  return window.location.origin
}
