/**
 * Picks the transport once, at boot, and hands it to everything else.
 *
 * Inside the Tauri window `window.__TAURI_INTERNALS__` exists; served from
 * `teo-server` it does not. That single check is the whole decision — nothing
 * downstream branches on it again.
 */

import { createDesktopTransport } from './desktop'
import { createHttpTransport, loadConnection, type HttpConnection, type HttpTransport } from './http'
import { isTauri as detectTauri } from './native'
import type { Transport } from './types'

export type { Transport, MediaKind } from './types'
export { UnsupportedByTransport } from './types'
export { ENDPOINT_CHANGED, ServerUnavailable } from './desktop'
export { serverStatus, type ServerStatus } from './native'
export {
  forgetConnection,
  loadConnection,
  NotAuthorised,
  saveConnection,
  type HttpConnection,
} from './http'

export function isTauri(): boolean {
  return detectTauri()
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

/**
 * Desktop boot: the shell has already started a private server, so this asks
 * where it is and connects to it. Throws `ServerUnavailable` when the shell
 * could not start one, which the UI turns into a screen rather than a blank
 * window.
 */
export async function initDesktopTransport(): Promise<Transport> {
  active = await createDesktopTransport()
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

/** True once a transport exists and is pointed at something. */
export function activeKind(): 'tauri' | 'http' | null {
  return active?.kind ?? null
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
