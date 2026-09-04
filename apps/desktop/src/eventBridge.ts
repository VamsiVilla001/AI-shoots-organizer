/**
 * Wires backend events into the Zustand store and TanStack Query.
 *
 * The backend pushes; this module decides which cached queries each push
 * invalidates, so screens refresh without polling (§18).
 */

import { listen } from '@tauri-apps/api/event'
import type { QueryClient } from '@tanstack/react-query'
import type {
  ExportProgressEvent,
  JobFailedEvent,
  NoticeEvent,
  ProgressEvent,
  ShootChangedEvent,
} from '@skwad/shared-types'
import { useUi } from './store'

export async function startEventBridge(queryClient: QueryClient): Promise<() => void> {
  const disposers = await Promise.all([
    listen<ProgressEvent>('skwad://progress', ({ payload }) => {
      useUi.getState().setProgress(payload)
      // When a shoot finishes, its lists are stale in one go.
      if (payload.stage === 'complete') {
        queryClient.invalidateQueries({ queryKey: ['shoots'] })
        queryClient.invalidateQueries({ queryKey: ['media'] })
        queryClient.invalidateQueries({ queryKey: ['albums'] })
        queryClient.invalidateQueries({ queryKey: ['clusters'] })
        queryClient.invalidateQueries({ queryKey: ['people'] })
        queryClient.invalidateQueries({ queryKey: ['faces'] })
        // A finished scan changes how much is left to sort.
        queryClient.invalidateQueries({ queryKey: ['groupStats'] })
      } else {
        // During processing only the cheap headline numbers refresh.
        queryClient.invalidateQueries({ queryKey: ['shoots'] })
      }
    }),

    listen<ShootChangedEvent>('skwad://shoot-changed', ({ payload }) => {
      queryClient.invalidateQueries({ queryKey: ['shoots'] })
      queryClient.invalidateQueries({ queryKey: ['media', payload.shootId] })
      queryClient.invalidateQueries({ queryKey: ['albums', payload.shootId] })
      queryClient.invalidateQueries({ queryKey: ['clusters', payload.shootId] })
      queryClient.invalidateQueries({ queryKey: ['groups', payload.shootId] })
      queryClient.invalidateQueries({ queryKey: ['groupStats', payload.shootId] })
      queryClient.invalidateQueries({ queryKey: ['groupLinks', payload.shootId] })
      queryClient.invalidateQueries({ queryKey: ['faces'] })
    }),

    listen('skwad://library-changed', () => {
      queryClient.invalidateQueries({ queryKey: ['people'] })
      queryClient.invalidateQueries({ queryKey: ['faces'] })
      queryClient.invalidateQueries({ queryKey: ['clusters'] })
      queryClient.invalidateQueries({ queryKey: ['albums'] })
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
