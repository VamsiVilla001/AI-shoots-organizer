/**
 * The parts of the desktop that only the shell can do.
 *
 * Since the desktop talks to its own loopback server for everything else, Tauri
 * IPC is down to three things: asking where that server is, and the two file
 * manager actions no server on another machine could perform.
 */

import { invoke } from '@tauri-apps/api/core'
import { open } from '@tauri-apps/plugin-dialog'

export interface ServerEndpoint {
  baseUrl: string
  token: string
}

export interface ServerStatus {
  endpoint: ServerEndpoint | null
  running: boolean
  /** Set when the server could not be started; shown to the user verbatim. */
  error: string | null
  restarts: number
}

export function isTauri(): boolean {
  return typeof window !== 'undefined' && '__TAURI_INTERNALS__' in window
}

/** Where the private server is, according to the shell that started it. */
export async function serverStatus(): Promise<ServerStatus> {
  try {
    return await invoke<ServerStatus>('server_status')
  } catch (raw) {
    const message =
      typeof raw === 'object' && raw !== null && 'message' in raw
        ? String((raw as { message: unknown }).message)
        : String(raw)
    throw new Error(message)
  }
}

/** Invokes one of the shell's own commands. */
export function nativeInvoke<T>(command: string, args?: Record<string, unknown>): Promise<T> {
  return invoke<T>(command, args).catch((raw) => {
    const message =
      typeof raw === 'object' && raw !== null && 'message' in raw
        ? String((raw as { message: unknown }).message)
        : String(raw)
    throw new Error(message)
  })
}

export async function pickFolder(title: string): Promise<string | null> {
  const picked = await open({ directory: true, multiple: false, title })
  return typeof picked === 'string' ? picked : null
}
