/** The desktop transport: Tauri IPC, Tauri events, the native folder dialog. */

import { invoke } from '@tauri-apps/api/core'
import { listen } from '@tauri-apps/api/event'
import { open } from '@tauri-apps/plugin-dialog'
import type { MediaKind, Transport } from './types'

export function createTauriTransport(): Transport {
  // Set from `AppInfo.mediaUrlBase` at boot; the fallback matches what the Rust
  // side produces on Windows, so a first paint before boot completes still
  // resolves.
  let mediaBase = 'http://teomedia.localhost'

  return {
    kind: 'tauri',

    async call<T>(command: string, args?: Record<string, unknown>): Promise<T> {
      try {
        return await invoke<T>(command, args)
      } catch (raw) {
        // Backend errors arrive as `{ message }`; normalise to a throwable.
        const message =
          typeof raw === 'object' && raw !== null && 'message' in raw
            ? String((raw as { message: unknown }).message)
            : String(raw)
        throw new Error(message)
      }
    },

    listen<T>(event: string, handler: (payload: T) => void) {
      return listen<T>(event, ({ payload }) => handler(payload))
    },

    mediaUrl(mediaId: number, kind: MediaKind) {
      return `${mediaBase}/${kind}/${mediaId}`
    },

    setMediaBase(base: string) {
      if (base) mediaBase = base
    },

    async pickFolder(title: string) {
      const picked = await open({ directory: true, multiple: false, title })
      return typeof picked === 'string' ? picked : null
    },
  }
}
