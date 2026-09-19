/**
 * The one seam between the UI and whatever is behind it.
 *
 * The same React bundle runs in two places: inside the Tauri window, where
 * calls go over `invoke` and events over `listen`, and against `skwad-server`,
 * where the same calls are `POST /api/invoke/<command>` and the events are an
 * SSE stream. Everything above this file is written once.
 *
 * Command names are the contract, not URLs: `api.ts` asks for `list_shoots`
 * and the HTTP transport posts to `/api/invoke/list_shoots`. There is no route
 * table on either side to keep in step — the server dispatches by name from
 * the same registry the desktop's commands are generated from.
 */

import { invoke, isTauri as tauriDetected } from '@tauri-apps/api/core'
import { listen } from '@tauri-apps/api/event'

/** The API contract version this bundle speaks; the server checks it. */
export const API_VERSION = 1

/** One root the server lets clients browse for shoot folders. */
export interface FsRoot {
  path: string
  name: string
  available: boolean
}

export interface FsEntry {
  path: string
  name: string
  mediaCount: number
  hasSubfolders: boolean
}

export interface FsListing {
  path: string
  parent: string | null
  directories: FsEntry[]
  mediaCount: number
}

export interface Transport {
  readonly kind: 'tauri' | 'http'
  /** Invokes a backend command by name. Rejects with the backend's message. */
  call<T>(command: string, args?: Record<string, unknown>): Promise<T>
  /** Subscribes to a backend event; resolves to an unsubscribe function. */
  listen<T>(event: string, handler: (payload: T) => void): Promise<() => void>
  /**
   * Turns the media base the backend reported into an absolute one, plus the
   * query string media URLs need for this transport (a token, when a cookie
   * cannot travel).
   */
  mediaBase(reported: string): { base: string; query: string }
  /**
   * The server's jailed folder browser — the replacement for a native folder
   * dialog when the folders that matter are on the server. Absent in the
   * desktop window, which has the native dialog.
   */
  browse?: {
    roots(): Promise<FsRoot[]>
    list(path: string): Promise<FsListing>
  }
}

/** Thrown when the UI asks for something this transport cannot do. */
export class UnsupportedByTransport extends Error {
  constructor(what: string) {
    super(`${what} is only available in the desktop app`)
    this.name = 'UnsupportedByTransport'
  }
}

/** Raised when the server answered 401 — the session ended. */
export class NotAuthorised extends Error {
  constructor(message: string) {
    super(message)
    this.name = 'NotAuthorised'
  }
}

/** Raised when the server speaks a different API version. */
export class VersionMismatch extends Error {
  constructor(message: string) {
    super(message)
    this.name = 'VersionMismatch'
  }
}

export function isTauri(): boolean {
  try {
    return tauriDetected()
  } catch {
    return false
  }
}

// --- the desktop transport -----------------------------------------------------

/**
 * Commands the desktop answers itself and a server never sees. Over HTTP each
 * either has a sensible constant answer or is refused with a message that
 * says why.
 */
const HTTP_LOCAL: Record<string, (args: Record<string, unknown>) => unknown> = {
  // A server that answered at all has its database open.
  startup_status: () => ({ kind: 'ready' }),
  premiere_panel_status: () => ({ bundledVersion: null, installedVersion: null, installerAvailable: false }),
}

const DESKTOP_ONLY: Record<string, string> = {
  database_settings: 'Configuring the library database',
  test_database_connection: 'Configuring the library database',
  save_database_connection: 'Configuring the library database',
  restart_for_database_change: 'Restarting the app',
  get_library_location: 'The library location',
  set_library_location: 'The library location',
  restart_for_library_change: 'Restarting the app',
  install_premiere_panel: 'Installing the Premiere panel',
  reveal_in_folder: 'Opening a folder on this machine',
  open_path: 'Opening a file on this machine',
  open_catalogue_media: 'Opening a catalogue file on this machine',
  network_paths: "Working out a folder's network address",
  client_status: 'Worker mode',
  set_server_url: 'Choosing a server',
  restart_for_client_change: 'Restarting the app',
  store_machine_enrolment: 'Worker mode',
  forget_machine_enrolment: 'Worker mode',
  set_worker_enabled: 'Worker mode',
  update_worker_settings: 'Worker mode',
}

function normaliseError(raw: unknown): Error {
  const message =
    typeof raw === 'object' && raw !== null && 'message' in raw
      ? String((raw as { message: unknown }).message)
      : String(raw)
  return new Error(message)
}

export function createTauriTransport(): Transport {
  return {
    kind: 'tauri',
    async call<T>(command: string, args?: Record<string, unknown>): Promise<T> {
      try {
        return await invoke<T>(command, args)
      } catch (raw) {
        throw normaliseError(raw)
      }
    },
    async listen<T>(event: string, handler: (payload: T) => void) {
      return listen<T>(event, ({ payload }) => handler(payload))
    },
    mediaBase(reported: string) {
      return { base: reported, query: '' }
    },
  }
}

// --- the HTTP transport -------------------------------------------------------

export interface HttpConnection {
  /** Origin of the server, without a trailing slash. Empty means same-origin. */
  baseUrl: string
  /** The session token, once signed in. Sent as a bearer header. */
  token: string | null
  /**
   * Put the token in media and event URLs rather than relying on the session
   * cookie. Needed when the page is not served by the server itself — the
   * desktop client's webview — because `<img>` and `EventSource` cannot set
   * a header and a cross-origin cookie is never sent.
   */
  tokenInUrl: boolean
}

const STORAGE_KEY = 'skwad.server'

export function loadConnection(): HttpConnection | null {
  try {
    const raw = window.localStorage.getItem(STORAGE_KEY)
    if (!raw) return null
    const parsed = JSON.parse(raw) as Partial<HttpConnection>
    if (typeof parsed.baseUrl !== 'string') return null
    return {
      baseUrl: parsed.baseUrl.replace(/\/+$/, ''),
      token: typeof parsed.token === 'string' ? parsed.token : null,
      tokenInUrl: parsed.tokenInUrl === true,
    }
  } catch {
    return null
  }
}

export function saveConnection(connection: HttpConnection) {
  try {
    window.localStorage.setItem(STORAGE_KEY, JSON.stringify(connection))
  } catch {
    // A private window still works for this session.
  }
}

export function forgetConnection() {
  try {
    window.localStorage.removeItem(STORAGE_KEY)
  } catch {
    // Nothing to forget.
  }
}

export interface HttpTransport extends Transport {
  readonly kind: 'http'
  readonly connection: HttpConnection
  /**
   * The desktop IPC behind a client installation: desktop-only commands and
   * this machine's own events (its worker) go there. Absent in a browser.
   */
  readonly local?: Transport
  /** Puts a file on the server (a `.skwad` to load); answers with the server path. */
  upload(name: string, bytes: Blob): Promise<string>
  /** A URL the browser can download a published catalogue from. */
  downloadUrl(serverPath: string): string
  close(): void
}

export function createHttpTransport(initial: HttpConnection, local?: Transport): HttpTransport {
  const connection: HttpConnection = { ...initial, baseUrl: initial.baseUrl.replace(/\/+$/, '') }
  const url = (path: string) => `${connection.baseUrl}${path}`

  let source: EventSource | null = null
  const handlers = new Map<string, Set<(payload: unknown) => void>>()
  const attached = new Set<string>()

  // `EventSource` cannot set headers: the version (and, cross-origin, the
  // token) travel in the query string instead.
  const streamUrl = () =>
    connection.tokenInUrl && connection.token
      ? url(`/api/events?api=${API_VERSION}&token=${encodeURIComponent(connection.token)}`)
      : url(`/api/events?api=${API_VERSION}`)

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

  /** One stream, many listeners: opened on first subscription, re-opened after sign-in. */
  const ensureStream = () => {
    if (source || !connection.token) return
    source = new EventSource(streamUrl(), { withCredentials: true })
    source.onerror = () => {
      // The browser reconnects on its own; a log line is enough.
      console.warn('event stream interrupted; the browser will retry')
    }
    for (const name of handlers.keys()) attach(name)
  }

  const closeStream = () => {
    source?.close()
    source = null
    attached.clear()
  }

  const headersFor = (json: boolean) => {
    const headers: Record<string, string> = { 'X-Skwad-Api': String(API_VERSION) }
    if (json) headers['Content-Type'] = 'application/json'
    if (connection.token) headers.Authorization = `Bearer ${connection.token}`
    return headers
  }

  const failure = async (response: Response): Promise<Error> => {
    const message = await response
      .json()
      .then((body: { message?: string }) => body?.message)
      .catch(() => undefined)
    const text = message ?? `${response.status} ${response.statusText}`
    if (response.status === 401) return new NotAuthorised(text)
    if (response.status === 426) return new VersionMismatch(text)
    return new Error(text)
  }

  const getJson = async <T,>(path: string): Promise<T> => {
    const response = await fetch(url(path), { headers: headersFor(false), credentials: 'include' })
    if (!response.ok) throw await failure(response)
    return (await response.json()) as T
  }

  return {
    kind: 'http',
    connection,
    local,

    async call<T>(command: string, args?: Record<string, unknown>): Promise<T> {
      const constant = HTTP_LOCAL[command]
      if (constant) return constant(args ?? {}) as T
      const reason = DESKTOP_ONLY[command]
      if (reason) {
        if (local) return local.call<T>(command, args)
        throw new UnsupportedByTransport(reason)
      }

      const response = await fetch(url(`/api/invoke/${command}`), {
        method: 'POST',
        headers: headersFor(true),
        credentials: 'include',
        body: JSON.stringify(args ?? {}),
      })

      // Session lifecycle rides on the commands that already exist.
      const issued = response.headers.get('x-skwad-session-token')
      if (issued) {
        connection.token = issued
        saveConnection(connection)
        closeStream()
        ensureStream()
      }
      if (command === 'sign_out_skwad' || command === 'clear_authenticated_session') {
        connection.token = null
        saveConnection(connection)
        closeStream()
      }

      if (!response.ok) throw await failure(response)

      const text = await response.text()
      return (text ? JSON.parse(text) : undefined) as T
    },

    browse: {
      roots: () => getJson<FsRoot[]>('/api/fs/roots'),
      list: (path: string) => getJson<FsListing>(`/api/fs/list?path=${encodeURIComponent(path)}`),
    },

    async upload(name: string, bytes: Blob) {
      const response = await fetch(url(`/api/catalogues/upload?name=${encodeURIComponent(name)}`), {
        method: 'POST',
        headers: { ...headersFor(false), 'Content-Type': 'application/octet-stream' },
        credentials: 'include',
        body: bytes,
      })
      if (!response.ok) throw await failure(response)
      const answer = (await response.json()) as { path: string }
      return answer.path
    },

    downloadUrl(serverPath: string) {
      const query = [`path=${encodeURIComponent(serverPath)}`]
      if (connection.tokenInUrl && connection.token) query.push(`token=${encodeURIComponent(connection.token)}`)
      return url(`/api/catalogues/download?${query.join('&')}`)
    },

    async listen<T>(event: string, handler: (payload: T) => void) {
      const set = handlers.get(event) ?? new Set()
      set.add(handler as (payload: unknown) => void)
      handlers.set(event, set)
      ensureStream()
      attach(event)
      // A client installation's own worker reports through the desktop IPC;
      // everything about the library comes down the server's stream.
      const disposeLocal = local ? await local.listen<T>(event, handler) : null
      return () => {
        set.delete(handler as (payload: unknown) => void)
        if (set.size === 0) handlers.delete(event)
        disposeLocal?.()
      }
    },

    mediaBase(reported: string) {
      const query = connection.tokenInUrl && connection.token ? `token=${encodeURIComponent(connection.token)}` : ''
      return { base: url(reported), query }
    },

    close() {
      closeStream()
      handlers.clear()
    },
  }
}

// --- selection ----------------------------------------------------------------

let active: Transport | null = null

/** The transport in use. Throws before boot, which is a bug rather than a state. */
export function transport(): Transport {
  if (!active) throw new Error('the transport has not been initialised yet')
  return active
}

export function transportReady(): boolean {
  return active !== null
}

export function activeKind(): 'tauri' | 'http' | null {
  return active?.kind ?? null
}

/** Why a client installation could not reach its server at boot, if it could not. */
let clientProblem: { serverUrl: string; message: string } | null = null

export function clientConnectProblem() {
  return clientProblem
}

/**
 * Picks the transport once, at boot.
 *
 * Inside the Tauri window the IPC is there — and if the installation is a
 * client of a server, the HTTP transport is layered over it, so the library
 * comes from the server and the machine's own commands stay local. Served
 * by (or pointed at) a server in a browser, a saved connection is used.
 * Otherwise nothing is chosen yet and the UI shows the connect screen.
 */
export async function bootTransport(): Promise<Transport | null> {
  if (active) return active
  if (isTauri()) {
    const tauri = createTauriTransport()
    let startup: { kind?: string; serverUrl?: string } | null = null
    try {
      startup = await tauri.call<{ kind?: string; serverUrl?: string }>('startup_status')
    } catch {
      startup = null
    }
    if (startup?.kind === 'client' && startup.serverUrl) {
      try {
        return await connectToServer(startup.serverUrl, { local: tauri })
      } catch (e) {
        clientProblem = { serverUrl: startup.serverUrl, message: String((e as Error).message ?? e) }
      }
    }
    active = tauri
    return active
  }
  const saved = loadConnection()
  if (saved) {
    active = createHttpTransport(saved)
    return active
  }
  return null
}

/**
 * Connects to a server chosen on the connect screen — a browser build, or a
 * client installation whose desktop IPC is passed as `local`.
 */
export async function connectToServer(baseUrl: string, options: { local?: Transport } = {}): Promise<HttpTransport> {
  const trimmed = baseUrl.trim().replace(/\/+$/, '')
  const response = await fetch(`${trimmed}/health`, { credentials: 'include' }).catch(() => null)
  if (!response || !response.ok) {
    throw new Error(`nothing answered at ${trimmed || 'this origin'} — check the address and that the server is running`)
  }
  const health = (await response.json()) as { apiVersion?: number }
  if (health.apiVersion !== API_VERSION) {
    throw new VersionMismatch(
      `the server at ${trimmed || 'this origin'} speaks API version ${health.apiVersion ?? '?'}; this client speaks ${API_VERSION}`,
    )
  }
  // A session kept from an earlier run of the same server still works.
  const saved = loadConnection()
  const token = saved && saved.baseUrl === trimmed ? saved.token : null
  const connection: HttpConnection = {
    baseUrl: trimmed,
    token,
    tokenInUrl: options.local !== undefined || (trimmed !== '' && trimmed !== window.location.origin),
  }
  saveConnection(connection)
  const next = createHttpTransport(connection, options.local)
  if (active?.kind === 'http') (active as HttpTransport).close()
  active = next
  clientProblem = null
  return next
}

export function disconnect() {
  if (active?.kind === 'http') (active as HttpTransport).close()
  forgetConnection()
  active = null
}

/** Same-origin default for the connect screen's first render. */
export function defaultServerUrl(): string {
  if (typeof window === 'undefined') return ''
  return window.location.origin
}
