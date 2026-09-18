/**
 * Wires backend events into the Zustand store and TanStack Query.
 *
 * The backend pushes; this module decides which cached queries each push
 * invalidates, so screens refresh without polling (§18).
 */

import type { QueryClient } from '@tanstack/react-query'
import { transport } from './transport'
import type {
  ExportProgressEvent,
  JobFailedEvent,
  NoticeEvent,
  ProgressEvent,
  ShootChangedEvent,
} from '@skwad/shared-types'
import { useUi } from './store'
import { LIBRARY_CHANGE_KEYS, SHOOT_COMPLETE_KEYS, invalidateKeys } from './queryKeys'

/// The bridge in use, so a reconnect can replace it rather than stack a second one.
let current: (() => void) | null = null

/** Stops the running bridge, if any, and starts one on the active transport. */
export async function restartEventBridge(queryClient: QueryClient): Promise<void> {
  current?.()
  current = null
  current = await startEventBridge(queryClient)
}

export async function startEventBridge(queryClient: QueryClient): Promise<() => void> {
  // Same handler shape as Tauri's `listen`, over whichever transport is active.
  const listen = <T,>(event: string, handler: (event: { payload: T }) => void) =>
    transport().listen<T>(event, (payload) => handler({ payload }))
  const disposers = await Promise.all([
    listen<ProgressEvent>('skwad://progress', ({ payload }) => {
      useUi.getState().setProgress(payload)
      // When a shoot finishes, its lists are stale in one go.
      if (payload.stage === 'complete') {
        void invalidateKeys(queryClient, SHOOT_COMPLETE_KEYS)
      } else {
        // During processing only the cheap headline numbers refresh.
        queryClient.invalidateQueries({ queryKey: ['shoots'] })
      }
    }),

    listen<ShootChangedEvent>('skwad://shoot-changed', ({ payload }) => {
      queryClient.invalidateQueries({ queryKey: ['shoots'] })
      queryClient.invalidateQueries({ queryKey: ['media', payload.shootId] })
      queryClient.invalidateQueries({ queryKey: ['media', null, 'workspace'] })
      queryClient.invalidateQueries({ queryKey: ['albums', payload.shootId] })
      queryClient.invalidateQueries({ queryKey: ['clusters', payload.shootId] })
      queryClient.invalidateQueries({ queryKey: ['groups', payload.shootId] })
      queryClient.invalidateQueries({ queryKey: ['groupStats', payload.shootId] })
      queryClient.invalidateQueries({ queryKey: ['groupLinks', payload.shootId] })
      queryClient.invalidateQueries({ queryKey: ['faces'] })
    }),

    listen('skwad://library-changed', () => {
      void invalidateKeys(queryClient, LIBRARY_CHANGE_KEYS)
    }),

    listen<JobFailedEvent>('skwad://job-failed', ({ payload }) => {
      useUi.getState().pushNotice({
        level: 'error',
        message: payload.file
          ? `${payload.kind} failed on ${payload.file}: ${payload.error}`
          : `${payload.kind} failed: ${payload.error}`,
      })
    }),

    listen<ExportProgressEvent>('skwad://export-progress', ({ payload }) => {
      useUi.getState().setExportProgress(payload)
      if (payload.finished) {
        queryClient.invalidateQueries({ queryKey: ['exports', payload.shootId] })
      }
    }),

    listen<NoticeEvent>('skwad://notice', ({ payload }) => {
      // Scan counters arrive as info notices; surface only the meaningful ones.
      if (payload.level !== 'info') useUi.getState().pushNotice(payload)
    }),
  ])

  return () => disposers.forEach((dispose) => dispose())
}
