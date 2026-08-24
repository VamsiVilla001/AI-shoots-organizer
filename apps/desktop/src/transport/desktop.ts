/**
 * The desktop transport: HTTP to the shell's own loopback server, plus the
 * native actions Tauri still owns.
 *
 * The desktop is a client of the same server the NAS edition runs, so there is
 * one implementation of every command instead of two. Two wrinkles come with
 * that, and both are handled here rather than in the screens:
 *
 * * **The port changes if the server restarts.** Every call is retried once
 *   after re-asking the shell where the server went, and live event
 *   subscriptions are moved onto the new connection.
 * * **A few commands are not the server's to answer.** Revealing a file in the
 *   file manager happens on this machine, so it goes over IPC.
 */

import { createHttpTransport, type HttpTransport } from './http'
import { nativeInvoke, pickFolder, serverStatus } from './native'
import { DESKTOP_ONLY } from './routes'
import type { MediaKind, Transport } from './types'

/** Raised when the shell says the server is not running and will not start. */
export class ServerUnavailable extends Error {
  constructor(message: string) {
    super(message)
    this.name = 'ServerUnavailable'
  }
}

/** Fired after the transport moves to a new port, so caches can be refreshed. */
export const ENDPOINT_CHANGED = 'teo:endpoint-changed'

/** A failed connection rather than a rejected request. */
function isConnectionFailure(error: unknown): boolean {
  // `fetch` rejects with a TypeError when it cannot reach the host at all;
  // anything the server answered arrives as our own Error with its message.
  return error instanceof TypeError
}

export async function createDesktopTransport(): Promise<Transport> {
  let http = await connect()
  /** Live subscriptions, so they can be re-attached to a new connection. */
  const subscriptions = new Map<string, Set<(payload: never) => void>>()

  async function connect(): Promise<HttpTransport> {
    const status = await serverStatus()
    if (!status.endpoint) {
      throw new ServerUnavailable(
        status.error ?? 'the local server is not running',
      )
    }
    // Cross-origin from the webview, so media tags and the event stream carry
    // the token in the URL; `fetch` still uses the Authorization header.
    const next = createHttpTransport({ ...status.endpoint, tokenInUrl: true })
    return next
  }

  /** Re-points at the server after a restart and restores the event stream. */
  async function reconnect(): Promise<void> {
    const previous = http
    http = await connect()
    previous.close()

    for (const [event, handlers] of subscriptions) {
      for (const handler of handlers) {
        await http.listen(event, handler)
      }
    }

    // Anything already fetched came from the old connection; whoever is holding
    // a query cache needs to know to refill it.
    window.dispatchEvent(new CustomEvent(ENDPOINT_CHANGED))
  }

  return {
    kind: 'tauri',

    async call<T>(command: string, args?: Record<string, unknown>): Promise<T> {
      if (command in DESKTOP_ONLY) {
        return nativeInvoke<T>(command, args)
      }

      try {
        return await http.call<T>(command, args)
      } catch (error) {
        if (!isConnectionFailure(error)) throw error
        // The server may have crashed and come back on a new port; one retry
        // covers that, and a second failure is a real failure.
        await reconnect()
        return http.call<T>(command, args)
      }
    },

    async listen<T>(event: string, handler: (payload: T) => void) {
      const handlers = subscriptions.get(event) ?? new Set()
      handlers.add(handler as (payload: never) => void)
      subscriptions.set(event, handlers)

      const detach = await http.listen<T>(event, handler)
      return () => {
        handlers.delete(handler as (payload: never) => void)
        detach()
      }
    },

    mediaUrl(mediaId: number, kind: MediaKind, at?: number) {
      // Read at call time: after a restart this is a different port.
      return http.mediaUrl(mediaId, kind, at)
    },

    setMediaBase() {
      // The base is the loopback origin, which only this transport knows.
    },

    pickFolder,
  }
}
