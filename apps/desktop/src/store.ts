/**
 * UI state that is not server data: navigation, the active shoot, live
 * progress pushed from the backend, and transient notices. Server data itself
 * lives in TanStack Query.
 */

import { create } from 'zustand'
import type { ExportProgressEvent, NoticeEvent, ProgressEvent } from '@teo/shared-types'

export type Screen = 'shoots' | 'groups' | 'players' | 'albums' | 'review' | 'export' | 'settings'

export interface Notice extends NoticeEvent {
  id: number
}

interface UiState {
  screen: Screen
  /** The shoot the workspace screens operate on. */
  activeShootId: number | null
  /** Latest progress per shoot, pushed by the backend monitor. */
  progress: Record<number, ProgressEvent>
  exportProgress: ExportProgressEvent | null
  notices: Notice[]
  /** Media id open in the viewer overlay, if any. */
  viewerMediaId: number | null

  navigate: (screen: Screen) => void
  openShoot: (shootId: number, screen?: Screen) => void
  setProgress: (event: ProgressEvent) => void
  setExportProgress: (event: ExportProgressEvent | null) => void
  pushNotice: (notice: NoticeEvent) => void
  dismissNotice: (id: number) => void
  openViewer: (mediaId: number) => void
  closeViewer: () => void
  resetWorkspace: () => void
}

let noticeCounter = 0

export const useUi = create<UiState>((set) => ({
  screen: 'shoots',
  activeShootId: null,
  progress: {},
  exportProgress: null,
  notices: [],
  viewerMediaId: null,

  navigate: (screen) => set({ screen }),

  // Opening a shoot lands on sorting: that is the job the app exists for.
  openShoot: (shootId, screen = 'groups') => set({ activeShootId: shootId, screen }),

  setProgress: (event) =>
    set((state) => ({ progress: { ...state.progress, [event.shootId]: event } })),

  setExportProgress: (event) => set({ exportProgress: event }),

  pushNotice: (notice) =>
    set((state) => {
      const entry: Notice = { ...notice, id: ++noticeCounter }
      // Auto-dismiss everything except errors, which stay until closed.
      if (notice.level !== 'error') {
        setTimeout(() => useUi.getState().dismissNotice(entry.id), 5000)
      }
      // Keep the stack shallow; old news is not worth scrolling.
      return { notices: [...state.notices.slice(-4), entry] }
    }),

  dismissNotice: (id) => set((state) => ({ notices: state.notices.filter((n) => n.id !== id) })),

  openViewer: (mediaId) => set({ viewerMediaId: mediaId }),
  closeViewer: () => set({ viewerMediaId: null }),
  resetWorkspace: () =>
    set({
      screen: 'shoots',
      activeShootId: null,
      progress: {},
      exportProgress: null,
      viewerMediaId: null,
    }),
}))
