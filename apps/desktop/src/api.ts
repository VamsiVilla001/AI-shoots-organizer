/**
 * Typed wrappers over every Tauri command.
 *
 * The UI never calls `invoke` directly — going through this module keeps the
 * command names and payload shapes in one place, next to the types they must
 * match in `commands.rs`.
 */

import { invoke } from '@tauri-apps/api/core'
import type {
  Album,
  AppInfo,
  AppSettings,
  ClusterSummary,
  ExportOptions,
  ExportPreview,
  ExportRecord,
  Face,
  FaceQuery,
  FaceWithContext,
  Job,
  LogEntry,
  Media,
  MediaQuery,
  ModelStatus,
  Person,
  PersonSummary,
  ProcessingProgress,
  Shoot,
  ShootSummary,
  VideoTimeline,
} from '@teo/shared-types'

/** Backend errors arrive as `{ message }`; normalise to a throwable Error. */
async function call<T>(command: string, args?: Record<string, unknown>): Promise<T> {
  try {
    return await invoke<T>(command, args)
  } catch (raw) {
    const message =
      typeof raw === 'object' && raw !== null && 'message' in raw
        ? String((raw as { message: unknown }).message)
        : String(raw)
    throw new Error(message)
  }
}

// --- application -----------------------------------------------------------

export const appInfo = () => call<AppInfo>('app_info')
export const getSettings = () => call<AppSettings>('get_settings')
export const updateSettings = (settings: AppSettings) =>
  call<AppSettings>('update_settings', { settings })
export const modelStatus = () => call<ModelStatus>('model_status')

// --- shoots ----------------------------------------------------------------

export const listShoots = () => call<ShootSummary[]>('list_shoots')
export const getShoot = (shootId: number) => call<ShootSummary | null>('get_shoot', { shootId })
export const createShoot = (name: string, sourcePath: string) =>
  call<Shoot>('create_shoot', { name, sourcePath })
export const renameShoot = (shootId: number, name: string) =>
  call<void>('rename_shoot', { shootId, name })
export const deleteShootIndex = (shootId: number) => call<void>('delete_shoot_index', { shootId })
export const resumeProcessing = (shootId: number) => call<number>('resume_processing', { shootId })
export const pauseProcessing = (paused: boolean) => call<boolean>('pause_processing', { paused })
export const cancelProcessing = (shootId: number) => call<number>('cancel_processing', { shootId })
export const reanalyseShoot = (shootId: number) => call<number>('reanalyse_shoot', { shootId })
export const getProgress = (shootId: number) => call<ProcessingProgress>('get_progress', { shootId })
export const listFailedJobs = (shootId: number) => call<Job[]>('list_failed_jobs', { shootId })

// --- media -----------------------------------------------------------------

export const listMedia = (query: MediaQuery) => call<Media[]>('list_media', { query })
export const getMedia = (mediaId: number) => call<Media | null>('get_media', { mediaId })
export const mediaFaces = (mediaId: number) => call<Face[]>('media_faces', { mediaId })
export const revealInFolder = (path: string) => call<void>('reveal_in_folder', { path })
export const openPath = (path: string) => call<void>('open_path', { path })

// --- players ---------------------------------------------------------------

export const listPeople = (shootId?: number | null) =>
  call<PersonSummary[]>('list_people', { shootId: shootId ?? null })
export const createPerson = (name: string, team?: string | null) =>
  call<Person>('create_person', { name, team: team ?? null })
export const renamePerson = (personId: number, name: string) =>
  call<void>('rename_person', { personId, name })
export const updatePerson = (personId: number, team: string | null, notes: string | null) =>
  call<void>('update_person', { personId, team, notes })
export const mergePeople = (targetId: number, sourceId: number) =>
  call<number>('merge_people', { targetId, sourceId })
export const deletePerson = (personId: number) => call<void>('delete_person', { personId })
export const clearPersonRecognition = (personId: number) =>
  call<void>('clear_person_recognition', { personId })

// --- clusters --------------------------------------------------------------

export const listClusters = (shootId: number, includeNamed = false) =>
  call<ClusterSummary[]>('list_clusters', { shootId, includeNamed })
export const nameCluster = (clusterId: number, name: string, team?: string | null) =>
  call<Person>('name_cluster', { clusterId, name, team: team ?? null })
export const mergeClusters = (targetId: number, sourceId: number) =>
  call<void>('merge_clusters', { targetId, sourceId })
export const splitCluster = (clusterId: number, faceIds: number[], label?: string | null) =>
  call<number>('split_cluster', { clusterId, faceIds, label: label ?? null })
export const ignoreCluster = (clusterId: number) => call<void>('ignore_cluster', { clusterId })

// --- albums ----------------------------------------------------------------

export const listAlbums = (shootId: number) => call<Album[]>('list_albums', { shootId })
export const regenerateAlbums = (shootId: number) => call<number>('regenerate_albums', { shootId })

// --- review ----------------------------------------------------------------

export const listFaces = (query: FaceQuery) => call<FaceWithContext[]>('list_faces', { query })
export const confirmFaces = (faceIds: number[]) => call<number>('confirm_faces', { faceIds })
export const rejectFaces = (faceIds: number[]) => call<number>('reject_faces', { faceIds })
export const assignFaces = (
  faceIds: number[],
  personId: number | null,
  personName: string | null,
) => call<number>('assign_faces', { faceIds, personId, personName })
export const ignoreFaces = (faceIds: number[]) => call<number>('ignore_faces', { faceIds })

// --- video -----------------------------------------------------------------

export const videoTimelines = (mediaId: number) => call<VideoTimeline[]>('video_timelines', { mediaId })

// --- export ----------------------------------------------------------------

export const previewExport = (shootId: number, destination: string, options: ExportOptions) =>
  call<ExportPreview>('preview_export', { shootId, destination, options })
export const startExport = (shootId: number, destination: string, options: ExportOptions) =>
  call<number>('start_export', { shootId, destination, options })
export const cancelExport = (shootId: number) => call<void>('cancel_export', { shootId })
export const listExports = (shootId: number) => call<ExportRecord[]>('list_exports', { shootId })

// --- logs and privacy ------------------------------------------------------

export const recentLogs = (shootId: number | null, limit = 200) =>
  call<LogEntry[]>('recent_logs', { shootId, limit })
export const clearAllEmbeddings = () => call<number>('clear_all_embeddings')
export const clearAllRecognitionData = () => call<void>('clear_all_recognition_data')
export const clearThumbnailCache = () => call<number>('clear_thumbnail_cache')
export const clearLog = () => call<void>('clear_log')
