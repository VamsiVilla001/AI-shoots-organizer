/**
 * Choosing folders and files, whichever front door is behind the UI.
 *
 * Inside the desktop window the operating system's dialog is right: the
 * folders that matter are on this machine. Against a server they are on the
 * *server*, so the same call opens the server's jailed folder browser instead
 * — a source path has to mean something to the scanner, which runs there.
 * File pickers (reference photos, roster files) stay local either way,
 * because those are files the person has, not files on a share.
 */

import { open, save } from '@tauri-apps/plugin-dialog'
import * as api from './api'
import { isTauri, transport, UnsupportedByTransport, type HttpTransport } from './transport'

export interface FolderRequest {
  title: string
  resolve: (path: string | null) => void
}

type Listener = (request: FolderRequest | null) => void
let pending: FolderRequest | null = null
const listeners = new Set<Listener>()

/** The browser modal subscribes here; see `FolderBrowserHost`. */
export function subscribeFolderRequests(listener: Listener): () => void {
  listeners.add(listener)
  listener(pending)
  return () => {
    listeners.delete(listener)
  }
}

function setPending(next: FolderRequest | null) {
  pending = next
  for (const listener of listeners) listener(next)
}

/** Settles the active server-browser request. Called by the modal. */
export function settleFolderRequest(path: string | null) {
  const current = pending
  setPending(null)
  current?.resolve(path)
}

/**
 * A folder on whichever machine the backend scans, or `null` if cancelled.
 *
 * In a desktop window the operating system's own dialog opens, even on a
 * client of a server — that is the dialog people expect. A client's pick is
 * then translated (a mapped drive letter becomes its network path) and the
 * server is asked whether it can read the folder, because the server is
 * what scans it; a folder only this machine can see is refused with a
 * message that says what to do instead.
 */
export async function pickFolder(title: string): Promise<string | null> {
  const active = transport()
  if (isTauri()) {
    const picked = await open({ directory: true, multiple: false, title })
    if (typeof picked !== 'string') return null
    if (active.kind === 'tauri') return picked
    return shareableWithServer(picked)
  }
  if (!active.browse) throw new UnsupportedByTransport('Choosing a folder')
  if (pending) settleFolderRequest(null)
  return new Promise<string | null>((resolve) => {
    setPending({ title, resolve })
  })
}

/** Local files the person has — reference photos, a video, a roster. */
export async function pickFiles(options: {
  title: string
  multiple: boolean
  filters: { name: string; extensions: string[] }[]
}): Promise<string[] | null> {
  if (transport().kind !== 'tauri') {
    throw new UnsupportedByTransport('Choosing local files')
  }
  const picked = await open({ multiple: options.multiple, title: options.title, filters: options.filters })
  if (!picked) return null
  return Array.isArray(picked) ? picked : [picked]
}

/** Where to save a file this machine will write. */
export async function pickSavePath(options: {
  title: string
  defaultPath?: string
  filters: { name: string; extensions: string[] }[]
}): Promise<string | null> {
  if (transport().kind !== 'tauri') {
    throw new UnsupportedByTransport('Choosing where to save')
  }
  const picked = await save({ title: options.title, defaultPath: options.defaultPath, filters: options.filters })
  return typeof picked === 'string' ? picked : null
}

/** True when this front door can open local file dialogs at all. */
export function hasLocalFileDialogs(): boolean {
  return transport().kind === 'tauri'
}

/**
 * Turns a folder this machine picked into one the server can scan.
 *
 * The desktop works out every network spelling it can vouch for — a mapped
 * drive's share, a mounted volume's share, or `\this-machineshare…`
 * when the folder sits inside something this machine shares — and the
 * server is asked to list each in turn; the first it can open is the one
 * that goes into the shoot. A folder nobody shares is refused with the
 * steps that would share it, because the server reads media in place and a
 * laptop's disk is otherwise invisible to it.
 */
async function shareableWithServer(picked: string): Promise<string> {
  const active = transport()
  const browse = active.browse
  const serverUrl = active.kind === 'http' ? (active as HttpTransport).connection.baseUrl : null
  const answer = await api.networkPaths(picked, serverUrl).catch(() => null)
  const candidates = answer?.candidates ?? []
  if (!browse) return candidates[0] ?? picked

  const refusals: string[] = []
  for (const candidate of candidates) {
    try {
      await browse.list(candidate)
      return candidate
    } catch (error) {
      refusals.push(`${candidate}: ${error instanceof Error ? error.message : String(error)}`)
    }
  }

  if (candidates.length === 0) {
    throw new Error(
      `${picked} is a folder on this computer, not on the network, so the server cannot read it. ` +
        (answer?.howToShare ?? 'Share it over the network, or choose it through its network location (\\server\share\…).'),
    )
  }
  throw new Error(
    `This folder is shared from this computer, but the server could not open it as ${candidates.join(' or ')}. ` +
      'Check that file sharing is on here, that this computer is awake and on the same network, and that the ' +
      'server has a login for this share (on the server: cmdkey /add:<this computer> /user:<account> /pass:<password>). ' +
      `Details: ${refusals.join('; ')}`,
  )
}
