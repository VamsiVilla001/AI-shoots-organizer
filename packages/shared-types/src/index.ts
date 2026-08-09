/**
 * The shapes that cross the Tauri IPC boundary.
 *
 * These mirror the serde definitions in `crates/database/src/models.rs` and the
 * command return types in `apps/desktop/src-tauri/src/commands.rs`. Rust
 * serialises everything as camelCase, so these are a direct transcription —
 * when a Rust struct changes, change it here in the same commit.
 */

// ---------------------------------------------------------------------------
// Enumerations (string unions, matching the Rust `string_enum!` values)
// ---------------------------------------------------------------------------

export type ShootStatus =
  | 'created'
  | 'scanning'
  | 'scanned'
  | 'processing'
  | 'paused'
  | 'completed'
  | 'failed'

export type MediaType = 'photo' | 'video'

export type ProcessingStatus =
  | 'pending'
  | 'indexed'
  | 'thumbnailed'
  | 'analysing'
  | 'analysed'
  | 'failed'
  | 'skipped'

/** How a detected face relates to a player. */
export type FaceAssignment =
  | 'unassigned'
  | 'suggested'
  | 'confirmed'
  | 'rejected'
  | 'ignored'

export type ClusterStatus = 'unnamed' | 'named' | 'ignored'

export type AlbumType = 'player' | 'multiPlayer' | 'unidentified' | 'team'

export type JobState = 'queued' | 'running' | 'done' | 'failed' | 'cancelled'

export type ExportStatus = 'queued' | 'running' | 'completed' | 'failed' | 'cancelled'

export type Accelerator = 'auto' | 'cpu' | 'directMl' | 'coreMl' | 'cuda'

// ---------------------------------------------------------------------------
// Records
// ---------------------------------------------------------------------------

export interface Shoot {
  id: number
  name: string
  sourcePath: string
  status: ShootStatus
  notes: string | null
  createdAt: string
  updatedAt: string
}

/** A shoot with its counts rolled up, as shown on the Shoots screen. */
export interface ShootSummary extends Shoot {
  photoCount: number
  videoCount: number
  faceCount: number
  personCount: number
  unknownClusterCount: number
  pendingJobs: number
  failedJobs: number
}

export interface Media {
  id: number
  shootId: number
  path: string
  filename: string
  mediaType: MediaType
  extension: string
  width: number | null
  height: number | null
  duration: number | null
  fileSize: number
  contentKey: string
  capturedAt: string | null
  indexedAt: string
  cameraMake: string | null
  cameraModel: string | null
  lens: string | null
  iso: number | null
  focalLength: number | null
  aperture: number | null
  shutter: string | null
  orientation: number
  thumbnailPath: string | null
  processingStatus: ProcessingStatus
  faceCount: number
  error: string | null
}

export interface Person {
  id: number
  name: string
  team: string | null
  notes: string | null
  coverFaceId: number | null
  createdAt: string
  updatedAt: string
}

export interface PersonSummary extends Person {
  faceSampleCount: number
  mediaCount: number
  shootCount: number
}

/** Normalised against the full frame, so it stays valid on a thumbnail. */
export interface BoundingBox {
  x: number
  y: number
  w: number
  h: number
}

export interface Face {
  id: number
  mediaId: number
  shootId: number
  personId: number | null
  clusterId: number | null
  embeddingDim: number | null
  bbox: BoundingBox
  detectionConfidence: number
  recognitionConfidence: number | null
  assignment: FaceAssignment
  quality: number | null
  frameTime: number | null
  cropPath: string | null
  createdAt: string
}

/** A face joined with what the review screen needs to draw it. */
export interface FaceWithContext extends Face {
  mediaPath: string
  mediaFilename: string
  mediaType: MediaType
  thumbnailPath: string | null
  personName: string | null
  clusterLabel: string | null
}

export interface Cluster {
  id: number
  shootId: number
  label: string
  personId: number | null
  status: ClusterStatus
  faceCount: number
  coverFaceId: number | null
  createdAt: string
}

export interface ClusterSummary extends Cluster {
  mediaCount: number
  personName: string | null
  coverMediaId: number | null
  coverThumbnailPath: string | null
}

export interface Album {
  id: number
  shootId: number
  name: string
  albumType: AlbumType
  personIds: number[]
  clusterId: number | null
  coverMediaId: number | null
  mediaCount: number
  photoCount: number
  videoCount: number
  sortOrder: number
  generatedAt: string
}

export interface VideoDetection {
  id: number
  mediaId: number
  personId: number | null
  faceId: number | null
  timestamp: number
  endTimestamp: number | null
  confidence: number
}

export interface VideoTimeline {
  mediaId: number
  personId: number | null
  personName: string | null
  appearances: VideoDetection[]
}

export interface Job {
  id: number
  shootId: number
  mediaId: number | null
  kind: string
  state: JobState
  priority: number
  attempts: number
  payload: string | null
  error: string | null
  createdAt: string
  startedAt: string | null
  finishedAt: string | null
}

export interface ProcessingProgress {
  shootId: number
  mediaTotal: number
  mediaScanned: number
  mediaAnalysed: number
  mediaFailed: number
  facesDetected: number
  facesRecognised: number
  facesUnknown: number
  jobsQueued: number
  jobsRunning: number
  jobsFailed: number
  percent: number
  stage: string
}

export interface ExportRecord {
  id: number
  shootId: number
  destination: string
  options: string
  status: ExportStatus
  filesTotal: number
  filesDone: number
  bytesDone: number
  error: string | null
  startedAt: string | null
  finishedAt: string | null
}

export interface LogEntry {
  id: number
  timestamp: string
  event: string
  shootId: number | null
  mediaId: number | null
  personId: number | null
  detail: string | null
}

// ---------------------------------------------------------------------------
// Queries
// ---------------------------------------------------------------------------

export interface MediaQuery {
  shootId?: number | null
  personId?: number | null
  clusterId?: number | null
  albumId?: number | null
  mediaType?: MediaType | null
  search?: string | null
  onlyUnidentified?: boolean
  limit?: number | null
  offset?: number | null
}

export interface FaceQuery {
  shootId?: number | null
  personId?: number | null
  clusterId?: number | null
  assignment?: FaceAssignment | null
  minConfidence?: number | null
  maxConfidence?: number | null
  limit?: number | null
  offset?: number | null
}

// ---------------------------------------------------------------------------
// Settings and application info
// ---------------------------------------------------------------------------

export interface AppSettings {
  accelerator: Accelerator
  inferenceThreads: number
  workerThreads: number

  detectionThreshold: number
  detectionNmsThreshold: number
  detectionInputSize: number
  maxFacesPerImage: number
  analysisMaxDim: number

  recognitionThreshold: number
  recognitionMargin: number
  uniquePersonPerFrame: boolean
  autoConfirmAbove: number

  clusterEdgeThreshold: number
  clusterMinSize: number
  clusterMergeThreshold: number
  clusterNeighbours: number

  videoEnabled: boolean
  videoSceneThreshold: number
  videoSampleInterval: number
  videoMaxFrames: number

  scanRecursive: boolean
  ffmpegDirectory: string | null

  detectorModel: string | null
  embedderModel: string | null
}

export interface AppPaths {
  root: string
  database: string
  thumbnails: string
  faceCache: string
  models: string
  logs: string
}

export type ModelRole = 'detector' | 'embedder' | 'unknown'

export interface ModelInfo {
  name: string
  path: string
  sizeBytes: number
  role: ModelRole
}

export interface ModelStatus {
  modelsDirectory: string
  available: ModelInfo[]
  detector: string | null
  embedder: string | null
  ready: boolean
  message: string
}

export interface AppInfo {
  version: string
  paths: AppPaths
  mediaUrlBase: string
  ffmpegAvailable: boolean
  ffmpegVersion: string | null
  models: ModelStatus
  accelerators: Accelerator[]
  cpuCores: number
  supportedExtensions: string[]
  cacheBytes: number
}

// ---------------------------------------------------------------------------
// Export
// ---------------------------------------------------------------------------

export type ExistingFilePolicy = 'skip' | 'rename' | 'overwrite'

export interface ExportOptions {
  splitPhotosVideos: boolean
  includeUnidentified: boolean
  personIds: number[] | null
  preserveMetadata: boolean
  existing: ExistingFilePolicy
  includeMultiPlayer: boolean
}

export interface ExportPreview {
  fileCount: number
  totalBytes: number
  folders: string[]
}

// ---------------------------------------------------------------------------
// Events
// ---------------------------------------------------------------------------

export interface ProgressEvent extends ProcessingProgress {
  paused: boolean
}

export interface ShootChangedEvent {
  shootId: number
  reason: string
}

export interface JobFailedEvent {
  shootId: number
  kind: string
  file: string | null
  error: string
}

export interface ExportProgressEvent {
  exportId: number
  shootId: number
  filesDone: number
  filesTotal: number
  filesSkipped: number
  bytesDone: number
  finished: boolean
  error: string | null
}

export interface NoticeEvent {
  level: 'info' | 'success' | 'warn' | 'error'
  message: string
}
