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
import { transport, UnsupportedByTransport } from './transport'

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

/** A folder on whichever machine the backend scans, or `null` if cancelled. */
export async function pickFolder(title: string): Promise<string | null> {
  const active = transport()
  if (active.kind === 'tauri') {
    const picked = await open({ directory: true, multiple: false, title })
    return typeof picked === 'string' ? picked : null
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
