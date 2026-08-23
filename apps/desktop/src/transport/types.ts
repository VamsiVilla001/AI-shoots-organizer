/**
 * The one seam between the UI and whatever is behind it.
 *
 * The same React bundle runs in two places: inside the Tauri window, where
 * calls go over `invoke` and events over `listen`, and in a browser talking to
 * `teo-server`, where the same calls are HTTP requests and the events are an
 * SSE stream. Everything above this file is written once.
 *
 * Command names are the contract, not URLs: `api.ts` asks for `list_shoots` and
 * the HTTP transport knows that means `GET /api/shoots`. That keeps the mapping
 * in one place and lets the two front doors stay comparable.
 */

export type MediaKind = 'thumb' | 'full' | 'video'

export interface Transport {
  readonly kind: 'tauri' | 'http'

  /** Invokes a backend command by name. Rejects with the backend's message. */
  call<T>(command: string, args?: Record<string, unknown>): Promise<T>

  /** Subscribes to a backend event; resolves to an unsubscribe function. */
  listen<T>(event: string, handler: (payload: T) => void): Promise<() => void>

  /** Where the bytes for one media id live, for this transport. */
  mediaUrl(mediaId: number, kind: MediaKind): string

  /** Called once at boot with `AppInfo.mediaUrlBase`. */
  setMediaBase(base: string): void

  /**
   * Opens the operating system's folder dialog.
   *
   * Absent in a browser, which is how `PathPicker` knows to fall back to the
   * server's own jailed folder browser instead.
   */
  pickFolder?: (title: string) => Promise<string | null>
}

/** Thrown when the UI asks for something this transport cannot do. */
export class UnsupportedByTransport extends Error {
  constructor(what: string) {
    super(`${what} is not available in the browser edition`)
    this.name = 'UnsupportedByTransport'
  }
}
