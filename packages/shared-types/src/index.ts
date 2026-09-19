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
export type MediaPickState = 'none' | 'pick' | 'reject'

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

export type AlbumType = 'player' | 'multiPlayer' | 'unidentified' | 'team' | 'groupSize'

/** Sizes at or above this collapse into one "10+ persons" album. */
export const GROUP_SIZE_CAP = 10

export type JobState = 'queued' | 'running' | 'done' | 'failed' | 'cancelled'

export type ExportStatus = 'queued' | 'running' | 'completed' | 'failed' | 'cancelled'

export type Accelerator = 'auto' | 'cpu' | 'directMl' | 'coreMl' | 'cuda'

export interface CatalogueSessionStatus {
  authenticatedOnce: boolean
  passwordChangeRequired: boolean
  /** Administrators reach the user-management panel; members do not. */
  isAdmin: boolean
  accountId: string | null
  email: string | null
  deviceKeyId: string | null
}

/**
 * One player on an imported team roster.
 *
 * The roster is what turns naming a face into team grouping: a reviewer types
 * any part of the in-game name, player name or team tag, and SKWAD knows which
 * team that person plays for.
 */
export interface RosterEntry {
  id: number
  /** The in-game name, e.g. "iQOOS8ULNaresh". Unique across the workspace. */
  ign: string
  /** The person behind the IGN. May be empty on IGN-only rosters. */
  playerName: string
  team: string
  /** player, coach, analyst, staff, substitute… */
  role: string
  /** The file this row was imported from. */
  source: string
}

/** What a roster file turned out to hold, before anything is saved. */
export interface RosterPreview {
  source: string
  entries: RosterEntry[]
  teams: string[]
  /** Rows that could not be read, with the reason. */
  problems: string[]
}

export interface RosterSummary {
  entries: number
  teams: string[]
  sources: Array<{ source: string; entries: number }>
}

/** Where the library folder came from: an env var, the admin, or app data. */
export type LibrarySource = 'environment' | 'configured' | 'appData'

/**
 * The shared library location. One machine (or a NAS) holds the folder and
 * everyone on the network points at the same path, so the database, caches,
 * face embeddings, profiles and accounts are common to the team.
 */
export interface LibraryLocation {
  activeRoot: string
  activeCacheRoot: string
  configuredRoot: string | null
  configuredCacheRoot: string | null
  source: LibrarySource
  networkShare: boolean
  databaseFile: string
  appDataRoot: string
  restartRequired: boolean
  existingLibrary: boolean
}

export type LocalUserRole = 'admin' | 'member'

/** One account in the local credential file, as the admin panel shows it. */
export interface LocalUser {
  id: string
  email: string
  displayName: string
  role: LocalUserRole
  enabled: boolean
  mustChangePassword: boolean
}

export interface NewLocalUser {
  email: string
  displayName: string
  role: LocalUserRole
  /** Left null, the account starts on the shared testing password. */
  password: string | null
}

export interface LocalUserUpdate {
  email: string
  displayName: string
  role: LocalUserRole
  enabled: boolean
}

export interface UserProfile {
  userId: string
  email: string
  displayName: string
  avatarUrl: string | null
  jobTitle: string | null
  organisation: string | null
  location: string | null
  bio: string | null
  createdAt: string
  updatedAt: string
}

export interface ProfileUpdate {
  displayName: string
  avatarUrl: string | null
  jobTitle: string | null
  organisation: string | null
  location: string | null
  bio: string | null
}

export interface CatalogueSummary {
  libraryId: string
  shootId: string
  shootName: string
  publishedRevision: number
  mediaCount: number
  groupCount: number
}

export interface LoadedCatalogueInfo extends CatalogueSummary {
  packageId: string
  revisionId: string
  mappedRoot: string | null
}

export interface CatalogueGroup {
  id: number
  stableId: string
  name: string
  folderName: string | null
  notes: string | null
  mediaCount: number
  photoCount: number
  videoCount: number
}

export interface CatalogueMedia {
  id: number
  stableId: string
  relativePath: string
  filename: string
  mediaType: string
  width: number | null
  height: number | null
  duration: number | null
  rating: number
  pickState: string
  isBestShot: boolean
  groupIds: number[]
}

export interface PublishSkwadResult {
  packageId: string
  revisionId: string
  path: string
  mediaCount: number
}

// ---------------------------------------------------------------------------
// Records
// ---------------------------------------------------------------------------

export interface Shoot {
  id: number
  name: string
  /** Where the scanner reads the folder. Never changes once files are indexed. */
  sourcePath: string
  /**
   * Where other machines reach the same folder (usually a UNC path), or null
   * when the shoot is only reachable from the machine that scanned it.
   */
  sharePath: string | null
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
  processingStartedAt: string | null
  scanCompletedAt: string | null
  processingCompletedAt: string | null
  processingDurationMs: number | null
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
  /** Path below the shoot root, `/`-separated; joined onto `Shoot.sharePath` on other machines. */
  normalizedRelativePath: string | null
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
  /** Detected face *rows*. For a video this counts every sampled frame, so it
   *  is not the number of people — use `personCount` for that. */
  faceCount: number
  /** Distinct people in the file; what group-size albums are built from. */
  personCount: number
  /** Local editorial hints derived from the cached thumbnail. */
  qualityScore: number | null
  sharpnessScore: number | null
  exposureScore: number | null
  perceptualHash: string | null
  duplicateGroupId: number | null
  duplicateCount: number
  isBestShot: boolean
  /** Human editorial rating; zero means not rated yet. */
  rating: number
  pickState: MediaPickState
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

/** What enrolling a person from reference photos/video produced. */
export interface EnrollPersonResult {
  person: Person
  samplesAdded: number
  /** Photos with zero or more than one detected face, skipped rather than guessed. */
  rejectedCount: number
}

/** One person enrolled by the front/left/right directory import. */
export interface EnrolledFromDirectory {
  name: string
  /** Which angle folders this person was found in, e.g. `['front', 'left']`. */
  angles: string[]
  samplesAdded: number
  /** Photos with zero or more than one detected face, skipped rather than guessed. */
  rejectedCount: number
}

/** What a front/left/right reference-folder import produced. */
export interface EnrollDirectoryResult {
  enrolled: EnrolledFromDirectory[]
  /** People whose photos yielded no usable face at all, so nothing was written. */
  skipped: string[]
}

/** On-demand retroactive matching of one pre-registered person against already-processed media. */
export interface MatchPersonReport {
  shootsScanned: number
  newSuggestions: number
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

/**
 * A folder the editor named in the app and filled themselves.
 *
 * The counterpart to `Album`: an album is derived from face assignments and
 * rebuilt on demand, a group is whatever a person decided it is and survives
 * re-analysis untouched. `folderName` (when set) is what the export writes
 * instead of `name`.
 */
export interface Group {
  id: number
  shootId: number
  name: string
  folderName: string | null
  notes: string | null
  personId: number | null
  sortOrder: number
  mediaCount: number
  photoCount: number
  videoCount: number
  coverMediaId: number | null
  createdAt: string
  updatedAt: string
}

/** One membership row: which group holds which file. */
export interface MediaGroupLink {
  mediaId: number
  groupId: number
}

/** How much of a shoot has been sorted. */
export interface GroupStats {
  mediaTotal: number
  grouped: number
  ungrouped: number
}

/** Naming one face assigns its cluster and gathers that person's media. */
export interface NameFaceResult {
  person: Person
  facesNamed: number
  /** Similar unidentified faces matched immediately after this reference was named. */
  matchesFound: number
  group: Group
  filesAdded: number
}

/** A reviewer-drawn face and any safe match found in the named-face library. */
export interface ManualFaceResult {
  face: Face
  suggestedPerson: Person | null
}

/** What seeding groups from the AI albums did. */
export interface SeedResult {
  groups: number
  files: number
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

/** One step of the pipeline, counted from the job queue. */
export interface StageProgress {
  /** The `JobKind` this step is built from. */
  kind: string
  queued: number
  running: number
  done: number
  failed: number
}

/** A job the worker pool is executing right now. */
export interface ActiveJob {
  jobId: number
  kind: string
  filename: string | null
  startedAt: string | null
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
  photosTotal: number
  videosTotal: number
  jobsQueued: number
  jobsRunning: number
  jobsFailed: number
  jobsDone: number
  percent: number
  stage: string
  /** Per-step counts, in the order the queue works through them. */
  stages: StageProgress[]
  active: ActiveJob[]
  /** Set when the queue is stalled on something missing (FFmpeg, models). */
  blockedReason: string | null
  /** The `JobKind` that could not run, so the panel can mark that step. */
  blockedKind: string | null
}

export type ProjectVisibility = 'private' | 'invited' | 'organisation'
export type ProjectStatus = 'active' | 'archived'
export type ProjectRole = 'owner' | 'editor' | 'viewer'

export interface ProjectCollectionSource {
  shootId: number
  groupId: number
}

export interface ProjectCollection {
  id: string
  projectId: string
  parentId: string | null
  name: string
  notes: string | null
  sortOrder: number
  sources: ProjectCollectionSource[]
  createdAt: string
  updatedAt: string
}

export interface ProjectMember {
  email: string
  displayName: string | null
  role: Exclude<ProjectRole, 'owner'>
  invitationState: 'invited' | 'accepted'
}

export interface Project {
  id: string
  name: string
  kind: string
  ownerAccountId: string
  ownerEmail: string
  organisation: string | null
  visibility: ProjectVisibility
  status: ProjectStatus
  coverMediaId: number | null
  accessRole: ProjectRole
  collections: ProjectCollection[]
  members: ProjectMember[]
  mediaCount: number
  createdAt: string
  updatedAt: string
}

export interface ProcessingRun {
  id: number
  shootId: number
  status: 'running' | 'completed' | 'failed' | 'cancelled'
  startedAt: string
  scanCompletedAt: string | null
  completedAt: string | null
  durationMs: number
  cpuMetricScope: 'process' | 'system'
}

export interface ProcessingStageTiming {
  stage: string
  startedAt: string
  completedAt: string | null
}

export interface ProcessingResourceSample {
  recordedAt: string
  elapsedMs: number
  /** CPU used by the SKWAD process, normalised to 0–100% of the machine. */
  cpuPercent: number | null
  /** Total utilisation of the busiest NVIDIA GPU. */
  gpuPercent: number | null
  activeWorkers: number
  concurrentShoots: number
}

export interface ShootTelemetry {
  run: ProcessingRun | null
  stages: ProcessingStageTiming[]
  samples: ProcessingResourceSample[]
  sampleIntervalSeconds: number
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
  /** Leaves out one shoot's media — used to separate a pre-registered person's reference samples from real matches. */
  excludeShootId?: number | null
  personId?: number | null
  clusterId?: number | null
  albumId?: number | null
  /** Only files the editor put in this group. */
  groupId?: number | null
  mediaType?: MediaType | null
  search?: string | null
  onlyUnidentified?: boolean
  /** Only files that are not in any manual group yet — the sorting backlog. */
  ungrouped?: boolean
  /** Only files holding exactly this many people; at `GROUP_SIZE_CAP` it means
   *  "this many or more", matching how the albums bucket. */
  groupSize?: number | null
  onlyBestShots?: boolean
  onlyDuplicates?: boolean
  minRating?: number | null
  pickState?: MediaPickState | null
  /** Only media carrying this tag value; `tagName` narrows it to one tag. */
  tagValue?: string | null
  tagName?: string | null
  /** Several tag values a file must all carry — a smart collection's path. */
  tagFilters?: TagFilterPair[]
  sort?: 'capturedAt' | 'quality' | 'rating' | 'filename' | null
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
  aiWorkers: number

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
  videoFramePrefetch: boolean

  scanRecursive: boolean
  ffmpegDirectory: string | null

  detectorModel: string | null
  embedderModel: string | null
}

export interface AppPaths {
  root: string
  database: string
  thumbnails: string
  proxies: string
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
  /** BLAKE3 of the file contents — the model's identity across machines. */
  hash: string
}

export interface ModelStatus {
  modelsDirectory: string
  available: ModelInfo[]
  detector: string | null
  embedder: string | null
  detectorHash: string | null
  embedderHash: string | null
  ready: boolean
  message: string
}

/** Which embedder the library uses now, and how much of it predates that embedder. */
export interface EmbeddingCohorts {
  currentKey: string | null
  /** Faces embedded by a different or unknown model; invisible to recognition until re-embedded. */
  staleFaces: number
  staleMedia: number
}

export interface AppInfo {
  version: string
  paths: AppPaths
  mediaUrlBase: string
  ffmpegAvailable: boolean
  ffmpegVersion: string | null
  gstreamerAvailable: boolean
  gstreamerVersion: string | null
  videoTrackingBackend: string
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

/** Whether the exported folders come from the editor's groups or the AI albums. */
export type ExportMode = 'groups' | 'aiAlbums'

/**
 * What actually lands in the destination: `shortcut` writes native shortcuts
 * back to the originals (instant, near-zero disk use, only usable while the
 * originals stay put); `copy` writes real files so the folder stands alone.
 */
export type ExportDelivery = 'shortcut' | 'copy'

export interface ExportOptions {
  mode: ExportMode
  /** `groups` mode: which groups to write. `null` writes all of them. */
  groupIds: number[] | null
  /** Native shortcuts back to the originals, or real copies. */
  delivery: ExportDelivery
  splitPhotosVideos: boolean
  /** `aiAlbums` mode only. */
  includeUnidentified: boolean
  /** `aiAlbums` mode only. */
  personIds: number[] | null
  preserveMetadata: boolean
  existing: ExistingFilePolicy
  /** `aiAlbums` mode only. */
  includeMultiPlayer: boolean
  /** `aiAlbums` mode only. Write "Single", "Two persons" … folders too. Off by
   *  default: every file is in both a player album and a size album, so this
   *  doubles the output. */
  includeGroupSize: boolean
  /** Write `_sorting-report.txt` beside the folders. */
  writeManifest: boolean
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

/**
 * What startup decided, asked for before anything else.
 *
 * When the library database cannot be opened there is no application state, so
 * every other command would fail; the UI checks this first and shows the
 * database setup screen instead of the library.
 */
export type StartupStatus =
  | { kind: 'ready' }
  | { kind: 'needsDatabase'; settings: DatabaseSettings; title: string; detail: string }
  /** A client installation: the webview talks to `serverUrl`; there is no library here. */
  | { kind: 'client'; serverUrl: string; machineId: string; machineName: string | null; workerEnabled: boolean }

/** A library database connection, without the password. */
export interface DatabaseSettings {
  host: string
  port: number
  database: string
  user: string
  /**
   * Whether a password for this exact server is already in the operating
   * system's credential store, so the form can offer to keep it rather than
   * making someone retype it to change a port.
   */
  hasSavedPassword: boolean
}

// ---------------------------------------------------------------------------
// Client mode and worker machines
// ---------------------------------------------------------------------------

/** This machine's half of the settings: hardware and tools, never shared. */
export interface MachineSettings {
  accelerator: Accelerator
  inferenceThreads: number
  workerThreads: number
  aiWorkers: number
  videoFramePrefetch: boolean
  ffmpegDirectory: string | null
  detectorModel: string | null
  embedderModel: string | null
}

/** What a worker advertises when it enrols and on every claim. */
export interface MachineCapabilities {
  appVersion: string
  gpu: string | null
  detectorHash: string | null
  embedderHash: string | null
  aiWorkers: number
  onBattery: boolean
}

/** One enrolled worker machine, with what it is doing (from `list_machines`). */
export interface MachineRosterEntry {
  id: string
  name: string
  enrolledBy: string | null
  enrolledAt: string
  lastSeen: string | null
  capabilities: MachineCapabilities
  revokedAt: string | null
  running: number
  completed: number
  failed: number
}

/** The answer to `enrol_machine`. The token is shown once. */
export interface EnrolResponse {
  machine: Omit<MachineRosterEntry, 'running' | 'completed' | 'failed'>
  token: string
}

/** A client worker's connection to its server. */
export interface RemoteStatus {
  connected: boolean
  paused: boolean
  lastError: string | null
  jobsCompleted: number
  jobsFailed: number
  held: number
  libraryVersion: number
  modelsReady: boolean
}

/** The worker panel's view of this installation (desktop only). */
export interface WorkerStatus {
  serverUrl: string | null
  machineId: string
  machineName: string | null
  enrolled: boolean
  enabled: boolean
  starting: boolean
  lastError: string | null
  remote: RemoteStatus | null
  machineSettings: MachineSettings
}

// ---------------------------------------------------------------------------
// Taxonomy: tags, their values, and what they are attached to
// ---------------------------------------------------------------------------

/** What a tag value may be attached to. */
export type TagAssetKind = 'media' | 'album' | 'cluster' | 'collection'

export interface TagValue {
  id: number
  value: string
  /** How many assets carry this value. */
  uses: number
}

/** One tag with every value it has been given. */
export interface TagSummary {
  id: number
  name: string
  values: TagValue[]
  createdAt: string
  updatedAt: string
}

/** One assignment as an asset sees it. */
export interface AssetTag {
  tagId: number
  tag: string
  valueId: number
  value: string
}

/** A value offered while someone types. */
export interface TagSuggestion {
  tagId: number
  tag: string
  valueId: number
  value: string
  uses: number
}

/** One row of a taxonomy file: a tag and its values. */
export interface TaxonomyEntry {
  name: string
  values: string[]
}

export interface TaxonomyPreview {
  entries: TaxonomyEntry[]
  problems: string[]
}

export interface TaxonomyImportSummary {
  tagsCreated: number
  valuesCreated: number
  tagsSeen: number
  valuesSeen: number
}

/** One tag = value pair as a filter; `name` null matches the value under any tag. */
export interface TagFilterPair {
  name: string | null
  value: string
}

/** One node of the smart-collection tree: a tag value and its file count within the selection. */
export interface SmartNode {
  tag: string
  value: string
  mediaCount: number
}
