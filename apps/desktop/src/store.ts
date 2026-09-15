/**
 * UI state that is not server data: navigation, the active shoot, live
 * progress pushed from the backend, and transient notices. Server data itself
 * lives in TanStack Query.
 */

import { create } from 'zustand'
import type { ExportProgressEvent, NoticeEvent, ProgressEvent } from '@skwad/shared-types'

export type Screen = 'shoots' | 'groups' | 'players' | 'albums' | 'review' | 'export' | 'catalogues' | 'profile' | 'settings'

export interface Notice extends NoticeEvent {
  id: number
}

export type Theme = 'light' | 'dark'

const THEME_STORAGE_KEY = 'skwad.theme'

/** Respects an explicit choice; otherwise follows the OS preference at startup. */
function initialTheme(): Theme {
  try {
    const stored = localStorage.getItem(THEME_STORAGE_KEY)
    if (stored === 'light' || stored === 'dark') return stored
  } catch {
    // Browser storage unavailable — fall through to the system preference.
  }
  return window.matchMedia?.('(prefers-color-scheme: dark)').matches ? 'dark' : 'light'
}

interface UiState {
  screen: Screen
  /** Applied to <html data-theme> by App; both the Classic and Project-workspace shells share it. */
  theme: Theme
  /** The shoot the workspace screens operate on. */
  activeShootId: number | null
  /** Latest progress per shoot, pushed by the backend monitor. */
  progress: Record<number, ProgressEvent>
  exportProgress: ExportProgressEvent | null
  /** Person ids handed from Albums to the Export screen; null means no filter. */
  exportPersonIds: number[] | null
  notices: Notice[]
  /** Media id open in the viewer overlay, if any. */
  viewerMediaId: number | null
  /** Open videos on analysed sample frames instead of loading the original stream. */
  viewerPreferVideoFaces: boolean

  toggleTheme: () => void
  navigate: (screen: Screen) => void
  openExport: (personIds: number[]) => void
  openShoot: (shootId: number, screen?: Screen) => void
  setProgress: (event: ProgressEvent) => void
  setExportProgress: (event: ExportProgressEvent | null) => void
  pushNotice: (notice: NoticeEvent) => void
  dismissNotice: (id: number) => void
  openViewer: (mediaId: number, preferVideoFaces?: boolean) => void
  closeViewer: () => void
  resetWorkspace: () => void
}

let noticeCounter = 0

export const useUi = create<UiState>((set) => ({
  screen: 'shoots',
  theme: initialTheme(),
  activeShootId: null,
  progress: {},
  exportProgress: null,
  exportPersonIds: null,
  notices: [],
  viewerMediaId: null,
  viewerPreferVideoFaces: false,

  toggleTheme: () =>
    set((state) => {
      const next: Theme = state.theme === 'dark' ? 'light' : 'dark'
      try {
        localStorage.setItem(THEME_STORAGE_KEY, next)
      } catch {
        // The toggle still works for this session without browser storage.
      }
      return { theme: next }
    }),

  navigate: (screen) =>
    set({
      screen,
      // Opening Export from the sidebar means a fresh, unfiltered export.
      ...(screen === 'export' ? { exportPersonIds: null } : {}),
    }),

  openExport: (personIds) => set({ screen: 'export', exportPersonIds: [...personIds] }),

  // Opening a shoot lands on sorting: that is the job the app exists for.
  openShoot: (shootId, screen = 'groups') =>
    set({ activeShootId: shootId, screen, exportPersonIds: null }),

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

  openViewer: (mediaId, preferVideoFaces = false) => set({ viewerMediaId: mediaId, viewerPreferVideoFaces: preferVideoFaces }),
  closeViewer: () => set({ viewerMediaId: null, viewerPreferVideoFaces: false }),
  resetWorkspace: () =>
    set({
      screen: 'shoots',
      activeShootId: null,
      progress: {},
      exportProgress: null,
      exportPersonIds: null,
      viewerMediaId: null,
      viewerPreferVideoFaces: false,
    }),
}))
