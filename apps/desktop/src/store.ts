/**
 * UI state that is not server data: navigation, the active shoot, live
 * progress pushed from the backend, and transient notices. Server data itself
 * lives in TanStack Query.
 */

import { create } from 'zustand'
import type { ExportProgressEvent, Media, NoticeEvent, ProgressEvent } from '@skwad/shared-types'

export type Screen = 'shoots' | 'groups' | 'players' | 'albums' | 'review' | 'export' | 'catalogues' | 'profile' | 'admin' | 'settings'

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

/**
 * What Cut/Copy is holding, waiting for a Paste onto a collection.
 *
 * Media carries the whole rows rather than ids because a paste has to know
 * each file's shoot to find (or create) the right group inside the target
 * collection. `source` is the group the files were cut *from* — null when
 * they were copied from the library at large, where there is nothing to cut
 * them out of.
 */
export type WorkspaceClipboard =
  | { kind: 'media'; mode: 'cut' | 'copy'; media: Media[]; source: { shootId: number; groupId: number } | null }
  | { kind: 'collection'; mode: 'cut' | 'copy'; projectId: string; collectionId: string; name: string }

/**
 * A collection export running in the background. It lives here rather than in
 * the dialog that started it so the dialog can close and the copy keeps going
 * with only the corner progress card on screen.
 */
export interface CollectionExportJob {
  collectionName: string
  destination: string
  /** One export run per shoot the collection draws from. */
  runs: Array<{ shootId: number; groupIds: number[] }>
  /** Which entry of `runs` is copying now. */
  index: number
  /** The export record the backend reports progress against. */
  exportId: number
  /** The user asked to stop. A cancelled run reports `finished` with no
   *  error, so remembering the ask is the only way to tell it from success. */
  stopping: boolean
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
  /** The background collection export, if one is running. */
  collectionExport: CollectionExportJob | null
  /** What Cut/Copy is holding, if anything. */
  clipboard: WorkspaceClipboard | null
  /**
   * The tag a collection is being built from, so the dialog can name the
   * collection after it and put the tag on the collection once it exists.
   */
  pendingCollectionTag: { tag: string | null; value: string } | null
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
  setCollectionExport: (job: CollectionExportJob | null) => void
  setClipboard: (clipboard: WorkspaceClipboard | null) => void
  setPendingCollectionTag: (pending: { tag: string | null; value: string } | null) => void
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
  collectionExport: null,
  clipboard: null,
  pendingCollectionTag: null,
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
  setCollectionExport: (job) => set({ collectionExport: job }),
  setClipboard: (clipboard) => set({ clipboard }),
  setPendingCollectionTag: (pendingCollectionTag) => set({ pendingCollectionTag }),

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
      collectionExport: null,
      exportPersonIds: null,
      viewerMediaId: null,
      viewerPreferVideoFaces: false,
    }),
}))
