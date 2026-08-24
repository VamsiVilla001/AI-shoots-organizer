/**
 * The browser transport: `fetch` for commands, `EventSource` for events.
 *
 * Two details drive the shape of this file:
 *
 * * **`EventSource` cannot send headers.** So can `<img>` and `<video>`, which
 *   is why the first thing a connection does is trade the token for a cookie at
 *   `POST /api/auth/session`. After that every request authenticates either way.
 * * **One stream, many listeners.** The event bridge subscribes to several event
 *   names; opening an `EventSource` per name would be several connections
 *   carrying identical traffic, so one is shared and dispatched by name.
 */

import type { MediaKind, Transport } from './types'
import { DESKTOP_ONLY, specFor } from './routes'
import { UnsupportedByTransport } from './types'

export interface HttpConnection {
  /** Origin of the server, without a trailing slash. Empty means same-origin. */
  baseUrl: string
  token: string
  /**
   * Put the token in media and event URLs rather than relying on the session
   * cookie.
   *
   * The desktop shell needs this: its page is `tauri.localhost` and its server
   * is `127.0.0.1`, so a `SameSite` cookie is never sent and `EventSource` and
   * `<img>` cannot set a header. Loopback only, with a token that lasts one
   * launch — which is why a token in a URL is acceptable there and not used for
   * the browser edition, where everything is same-origin.
   */
  tokenInUrl?: boolean
}

const STORAGE_KEY = 'teo.connection'

/** What the connection screen last used, so a reload does not ask again. */
export function loadConnection(): HttpConnection | null {
  try {
    const raw = window.localStorage.getItem(STORAGE_KEY)
    if (!raw) return null
    const parsed = JSON.parse(raw) as Partial<HttpConnection>
    if (typeof parsed.token !== 'string' || !parsed.token) return null
    return { baseUrl: (parsed.baseUrl ?? '').replace(/\/+$/, ''), token: parsed.token }
  } catch {
    return null
  }
}

export function saveConnection(connection: HttpConnection) {
  window.localStorage.setItem(STORAGE_KEY, JSON.stringify(connection))
}

export function forgetConnection() {
  window.localStorage.removeItem(STORAGE_KEY)
}

/** Raised when the server answered 401 — a wrong or expired token. */
export class NotAuthorised extends Error {
  constructor(message: string) {
    super(message)
    this.name = 'NotAuthorised'
  }
}

export interface HttpTransport extends Transport {
  readonly kind: 'http'
  readonly connection: HttpConnection
  /** Exchanges the token for a session cookie so media tags load. */
  openSession(): Promise<void>
  close(): void
}

export function createHttpTransport(connection: HttpConnection): HttpTransport {
  const base = connection.baseUrl.replace(/\/+$/, '')
  const url = (path: string) => `${base}${path}`

  let source: EventSource | null = null
  const handlers = new Map<string, Set<(payload: unknown) => void>>()

  /** Opens the shared stream on first subscription. */
  const ensureStream = () => {
    if (source) return
    // `withCredentials` so the session cookie travels when the bundle is served
    // from a different origin than the API during development.
    const stream = connection.tokenInUrl
      ? url(`/api/events?token=${encodeURIComponent(connection.token)}`)
      : url('/api/events')
    source = new EventSource(stream, { withCredentials: true })
    source.onerror = () => {
      // The browser reconnects on its own; a log line is enough, and throwing
      // here would take down whatever triggered the subscription.
      console.warn('event stream interrupted; the browser will retry')
    }
    for (const name of handlers.keys()) attach(name)
  }

  const attached = new Set<string>()
  const attach = (name: string) => {
    if (!source || attached.has(name)) return
    attached.add(name)
    source.addEventListener(name, (event) => {
      const message = event as MessageEvent<string>
      let payload: unknown
      try {
        payload = JSON.parse(message.data)
      } catch {
        payload = message.data
      }
      for (const handler of handlers.get(name) ?? []) handler(payload)
    })
  }

  return {
    kind: 'http',
    connection: { baseUrl: base, token: connection.token, tokenInUrl: connection.tokenInUrl },

    async call<T>(command: string, args?: Record<string, unknown>): Promise<T> {
      const reason = DESKTOP_ONLY[command]
      if (reason) throw new UnsupportedByTransport(reason)

      const spec = specFor(command, args ?? {})

      const search = new URLSearchParams()
      for (const [key, value] of Object.entries(spec.query ?? {})) {
        if (value !== null && value !== undefined) search.append(key, String(value))
      }
      const query = search.toString()

      const response = await fetch(url(spec.path) + (query ? `?${query}` : ''), {
        method: spec.method,
        headers: {
          Authorization: `Bearer ${connection.token}`,
          ...(spec.body !== undefined ? { 'Content-Type': 'application/json' } : {}),
        },
        credentials: 'include',
        body: spec.body !== undefined ? JSON.stringify(spec.body) : undefined,
      })

      if (response.status === 404 && spec.nullOn404) {
        return null as T
      }

      if (!response.ok) {
        // Errors come back as `{ message }`, the same shape the Tauri layer
        // produces, so callers cannot tell the two apart.
        const message = await response
          .json()
          .then((body: { message?: string }) => body?.message)
          .catch(() => undefined)
        const text = message ?? `${response.status} ${response.statusText}`
        throw response.status === 401 ? new NotAuthorised(text) : new Error(text)
      }

      if (response.status === 204) return undefined as T
      const text = await response.text()
      return (text ? JSON.parse(text) : undefined) as T
    },

    async listen<T>(event: string, handler: (payload: T) => void) {
      const set = handlers.get(event) ?? new Set()
      set.add(handler as (payload: unknown) => void)
      handlers.set(event, set)

      ensureStream()
      attach(event)

      return () => {
        set.delete(handler as (payload: unknown) => void)
        if (set.size === 0) handlers.delete(event)
      }
    },

    mediaUrl(mediaId: number, kind: MediaKind) {
      // The HTTP shape differs from the protocol's `/<kind>/<id>`: video is
      // served by a ranged handler at a different name.
      const route = kind === 'video' ? 'stream' : kind
      const query = connection.tokenInUrl ? `?token=${encodeURIComponent(connection.token)}` : ''
      return url(`/media/${mediaId}/${route}${query}`)
    },

    setMediaBase() {
      // Ignored on purpose. The server reports `/media`, but the URL also needs
      // the origin the client is talking to, which only this transport knows.
    },

    async openSession() {
      const response = await fetch(url('/api/auth/session'), {
        method: 'POST',
        headers: { 'Content-Type': 'application/json' },
        credentials: 'include',
        body: JSON.stringify({ token: connection.token }),
      })
      if (!response.ok) {
        throw new NotAuthorised('the server rejected that token')
      }
    },

    close() {
      source?.close()
      source = null
      attached.clear()
      handlers.clear()
    },
  }
}
