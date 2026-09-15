/**
 * Named groups of TanStack Query root keys, shared by every place that
 * invalidates caches after a backend change. Keeping this list in one spot
 * stops eventBridge.ts, processing.tsx, and publishCollection.tsx from
 * silently drifting apart on which keys a given change should refresh.
 */

import type { QueryClient } from '@tanstack/react-query'

/** A shoot finished processing: its analysis-derived lists are stale. */
export const SHOOT_COMPLETE_KEYS = ['shoots', 'media', 'albums', 'clusters', 'people', 'faces', 'groupStats'] as const

/** Named people or their face links changed. */
export const LIBRARY_CHANGE_KEYS = ['people', 'faces', 'clusters', 'albums'] as const

/** A Classic group was created, renamed, or had media added/removed. */
export const GROUP_CHANGE_KEYS = ['groups', 'groupStats', 'groupLinks', 'media'] as const

/** A manual re-process or removal ran: everything analysis-derived is stale. */
export const ANALYSIS_REFRESH_KEYS = ['shoots', 'media', 'faces', 'albums', 'clusters', 'people', 'video-timelines', 'workspace-progress', 'workspace-progress-summary'] as const

export function invalidateKeys(client: QueryClient, keys: readonly string[]) {
  return Promise.all(keys.map(key => client.invalidateQueries({ queryKey: [key] })))
}
