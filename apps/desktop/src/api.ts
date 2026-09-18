/**
 * Typed wrappers over every Tauri command.
 *
 * The UI never calls `invoke` directly — going through this module keeps the
 * command names and payload shapes in one place, next to the types they must
 * match in `commands.rs`.
 */

import { transport } from './transport'
import type {
  DatabaseSettings,
  StartupStatus,
  EnrolResponse,
  MachineRosterEntry,
  MachineSettings,
  WorkerStatus,
  Album,
  AppInfo,
  AppSettings,
  BoundingBox,
  ClusterSummary,
  EmbeddingCohorts,
  EnrollDirectoryResult,
  EnrollPersonResult,
  ExportOptions,
  ExportPreview,
  ExportRecord,
  Face,
  FaceQuery,
  FaceWithContext,
  Group,
  GroupStats,
  Job,
  LogEntry,
  ManualFaceResult,
  MatchPersonReport,
  Media,
  MediaGroupLink,
  MediaPickState,
  MediaQuery,
  ModelStatus,
  NameFaceResult,
  Person,
  PersonSummary,
  ProcessingProgress,
  SeedResult,
  Shoot,
  ShootSummary,
  ShootTelemetry,
  VideoTimeline,
  CatalogueSessionStatus,
  LoadedCatalogueInfo,
  CatalogueGroup,
  CatalogueMedia,
  PublishSkwadResult,
  ProfileUpdate,
  UserProfile,
  LocalUser,
  NewLocalUser,
  LocalUserUpdate,
  LibraryLocation,
  RosterEntry,
  RosterPreview,
  RosterSummary,
  Project,
  ProjectMember,
} from '@skwad/shared-types'

/**
 * Runs a backend command over whichever transport was chosen at boot — Tauri
 * IPC inside the desktop window, HTTP against `skwad-server` otherwise. Errors
 * arrive as a plain message either way.
 */
function call<T>(command: string, args?: Record<string, unknown>): Promise<T> {
  return transport().call<T>(command, args)
}

// --- application -----------------------------------------------------------

export const appInfo = () => call<AppInfo>('app_info')
export const getShootStorage = (shootId: number) =>
  call<{ recordBytes: number; previewBytes: number }>('get_shoot_storage', { shootId })
export const getSettings = () => call<AppSettings>('get_settings')
export const updateSettings = (settings: AppSettings) =>
  call<AppSettings>('update_settings', { settings })
export const modelStatus = () => call<ModelStatus>('model_status')
export const embeddingCohorts = (shootId?: number) =>
  call<EmbeddingCohorts>('embedding_cohorts', { shootId: shootId ?? null })
export const reembedStaleFaces = (shootId?: number) =>
  call<number>('reembed_stale_faces', { shootId: shootId ?? null })
export const catalogueSessionStatus = () => call<CatalogueSessionStatus>('catalogue_session_status')
export const signInSkwad = (email: string, password: string) =>
  call<CatalogueSessionStatus>('sign_in_skwad', { email, password })
export const changeInitialPassword = (email: string, currentPassword: string, newPassword: string) =>
  call<CatalogueSessionStatus>('change_initial_password', { email, currentPassword, newPassword })
export const signOutSkwad = () => call<void>('sign_out_skwad')
export const listLocalUsers = () => call<LocalUser[]>('list_local_users')
export const createLocalUser = (user: NewLocalUser) => call<LocalUser[]>('create_local_user', { user })
export const updateLocalUser = (user: LocalUserUpdate) => call<LocalUser[]>('update_local_user', { user })
export const resetLocalUserPassword = (email: string, password: string) =>
  call<LocalUser[]>('reset_local_user_password', { email, password })
export const deleteLocalUser = (email: string) => call<LocalUser[]>('delete_local_user', { email })
export const getLibraryLocation = () => call<LibraryLocation>('get_library_location')
export const setLibraryLocation = (root: string | null, cacheRoot: string | null, networkShare: boolean | null) =>
  call<LibraryLocation>('set_library_location', { root, cacheRoot, networkShare })
export const restartForLibraryChange = () => call<void>('restart_for_library_change')

// --- team rosters ----------------------------------------------------------

export const previewRosterFile = (path: string) => call<RosterPreview>('preview_roster_file', { path })
export const previewRosterText = (source: string, text: string) =>
  call<RosterPreview>('preview_roster_text', { source, text })
export const importRoster = (source: string, entries: RosterEntry[]) =>
  call<RosterSummary>('import_roster', { source, entries })
export const rosterSummary = () => call<RosterSummary>('roster_summary')
export const listRoster = () => call<RosterEntry[]>('list_roster')
export const searchRoster = (query: string, limit?: number) =>
  call<RosterEntry[]>('search_roster', { query, limit: limit ?? null })
export const resolveRosterName = (name: string) => call<RosterEntry | null>('resolve_roster_name', { name })
export const clearRoster = (source?: string | null) => call<RosterSummary>('clear_roster', { source: source ?? null })
export const getUserProfile = () => call<UserProfile>('get_user_profile')
export const updateUserProfile = (update: ProfileUpdate) =>
  call<UserProfile>('update_user_profile', { update })
export const clearAuthenticatedSession = () => call<void>('clear_authenticated_session')
export const publishSkwad = (shootId: number, destination: string, passphrase: string) =>
  call<PublishSkwadResult>('publish_skwad', { shootId, destination, passphrase })
export const loadSkwad = (path: string, passphrase?: string | null) =>
  call<LoadedCatalogueInfo>('load_skwad', { path, passphrase: passphrase ?? null })
export const approveCatalogueLibrary = (packageId: string, revisionId: string, root: string) =>
  call<LoadedCatalogueInfo>('approve_catalogue_library', { packageId, revisionId, root })
export const listLoadedCatalogues = () => call<LoadedCatalogueInfo[]>('list_loaded_catalogues')
export const listCatalogueGroups = (packageId: string, revisionId: string) =>
  call<CatalogueGroup[]>('list_catalogue_groups', { packageId, revisionId })
export const listCatalogueMedia = (packageId: string, revisionId: string, groupId?: number | null) =>
  call<CatalogueMedia[]>('list_catalogue_media', { packageId, revisionId, groupId: groupId ?? null })
export const openCatalogueMedia = (packageId: string, revisionId: string, mediaId: number) =>
  call<void>('open_catalogue_media', { packageId, revisionId, mediaId })

// --- projects --------------------------------------------------------------

export const listProjects = () => call<Project[]>('list_projects')
export const saveProject = (project: Project) => call<Project>('save_project', { project })
export const deleteProject = (projectId: string) => call<void>('delete_project', { projectId })
export const replaceProjectMembers = (projectId: string, members: ProjectMember[]) =>
  call<Project>('replace_project_members', { projectId, members })

// --- shoots ----------------------------------------------------------------

export const listShoots = () => call<ShootSummary[]>('list_shoots')
export const getShoot = (shootId: number) => call<ShootSummary | null>('get_shoot', { shootId })
export const createShoot = (name: string, sourcePath: string) =>
  call<Shoot>('create_shoot', { name, sourcePath })
export const renameShoot = (shootId: number, name: string) =>
  call<void>('rename_shoot', { shootId, name })
export const deleteShootIndex = (shootId: number) => call<void>('delete_shoot_index', { shootId })
export const clearSelectedScannedData = (shootIds: number[]) =>
  call<number>('clear_selected_scanned_data', { shootIds })
export const clearScannedData = () => call<number>('clear_scanned_data')
export const resumeProcessing = (shootId: number) => call<number>('resume_processing', { shootId })
export const pauseProcessing = (shootId: number, paused: boolean) =>
  call<boolean>('pause_processing', { shootId, paused })
export const cancelProcessing = (shootId: number) => call<number>('cancel_processing', { shootId })
export const reanalyseShoot = (shootId: number) => call<number>('reanalyse_shoot', { shootId })
export const getProgress = (shootId: number) => call<ProcessingProgress>('get_progress', { shootId })
export const getShootTelemetry = (shootId: number) =>
  call<ShootTelemetry>('get_shoot_telemetry', { shootId })
export const listFailedJobs = (shootId: number) => call<Job[]>('list_failed_jobs', { shootId })

// --- media -----------------------------------------------------------------

export const listMedia = (query: MediaQuery) => call<Media[]>('list_media', { query })
export const getMedia = (mediaId: number) => call<Media | null>('get_media', { mediaId })
export const mediaFaces = (mediaId: number) => call<Face[]>('media_faces', { mediaId })
export const setMediaEditorial = (args: {
  mediaIds: number[]
  rating?: number | null
  pickState?: MediaPickState | null
}) =>
  call<number>('set_media_editorial', {
    mediaIds: args.mediaIds,
    rating: args.rating ?? null,
    pickState: args.pickState ?? null,
  })
export const revealInFolder = (path: string) => call<void>('reveal_in_folder', { path })
export const openPath = (path: string) => call<void>('open_path', { path })

// --- players ---------------------------------------------------------------

export const listPeople = (shootId?: number | null) =>
  call<PersonSummary[]>('list_people', { shootId: shootId ?? null })
/** Only people enrolled by name + reference photo/video in Pre-Process — not everyone in the library. */
export const listEnrolledPeople = () => call<PersonSummary[]>('list_enrolled_people')
/** The hidden shoot enrollment reference photos/video live in, or null if nobody has enrolled yet. */
export const referenceLibraryShootId = () => call<number | null>('reference_library_shoot_id')
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
/** Pre-registers a person from 3+ reference photos or one reference video, taken outside any shoot. */
export const enrollPerson = (args: {
  name: string
  team?: string | null
  photoPaths?: string[]
  videoPath?: string | null
}) =>
  call<EnrollPersonResult>('enroll_person', {
    name: args.name,
    team: args.team ?? null,
    photoPaths: args.photoPaths ?? [],
    videoPath: args.videoPath ?? null,
  })
/**
 * Bulk-enrols a roster from one folder holding `front/`, `left/` and `right/`
 * subfolders, where the same filename in each is the same person. Like
 * `enrollPerson`, this skips the scan/analyse pipeline entirely.
 */
export const enrollPeopleFromDirectory = (root: string, team?: string | null) =>
  call<EnrollDirectoryResult>('enroll_people_from_directory', { root, team: team ?? null })
/** Retroactively matches a pre-registered person against already-processed media (one shoot, or every shoot). */
export const findPersonMedia = (personId: number, shootId?: number | null) =>
  call<MatchPersonReport>('find_person_media', { personId, shootId: shootId ?? null })

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

// --- groups (the editor's own sorting) -------------------------------------

export const listGroups = (shootId: number) => call<Group[]>('list_groups', { shootId })
export const groupStats = (shootId: number) => call<GroupStats>('group_stats', { shootId })
export const groupLinks = (shootId: number) => call<MediaGroupLink[]>('group_links', { shootId })
export const createGroup = (shootId: number, name: string) =>
  call<Group>('create_group', { shootId, name })
export const renameGroup = (groupId: number, name: string) =>
  call<Group>('rename_group', { groupId, name })
export const updateGroup = (groupId: number, folderName: string | null, notes: string | null) =>
  call<Group>('update_group', { groupId, folderName, notes })
export const deleteGroup = (groupId: number) => call<void>('delete_group', { groupId })
/** `moveFiles` pulls the files out of every other group first. */
export const addMediaToGroup = (args: {
  shootId: number
  groupId?: number | null
  groupName?: string | null
  mediaIds: number[]
  moveFiles?: boolean
}) =>
  call<number>('add_media_to_group', {
    shootId: args.shootId,
    groupId: args.groupId ?? null,
    groupName: args.groupName ?? null,
    mediaIds: args.mediaIds,
    moveFiles: args.moveFiles ?? false,
  })
export const removeMediaFromGroup = (groupId: number, mediaIds: number[]) =>
  call<number>('remove_media_from_group', { groupId, mediaIds })
export const clearGroup = (groupId: number) => call<number>('clear_group', { groupId })
export const groupsFromAiAlbums = (shootId: number) =>
  call<SeedResult>('groups_from_ai_albums', { shootId })
export const groupFromAlbum = (albumId: number, name?: string | null) =>
  call<Group>('group_from_album', { albumId, name: name ?? null })

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
/** Embeds a reviewer-drawn face box and compares it with confirmed named faces. */
export const addManualFace = (mediaId: number, bbox: BoundingBox, frameTime?: number | null) =>
  call<ManualFaceResult>('add_manual_face', { mediaId, bbox, frameTime: frameTime ?? null })
/** Names a face's person and gathers all currently known appearances into their group. */
export const nameFace = (faceId: number, name: string, team?: string | null) =>
  call<NameFaceResult>('name_face', { faceId, name, team: team ?? null })

// --- video -----------------------------------------------------------------

export const videoTimelines = (mediaId: number) => call<VideoTimeline[]>('video_timelines', { mediaId })
export const videoSampleFrames = (mediaId: number) => call<number[]>('video_sample_frames', { mediaId })

// --- export ----------------------------------------------------------------

export const previewExport = (shootId: number, destination: string, options: ExportOptions) =>
  call<ExportPreview>('preview_export', { shootId, destination, options })
export const startExport = (shootId: number, destination: string, options: ExportOptions) =>
  call<number>('start_export', { shootId, destination, options })
export const cancelExport = (shootId: number) => call<void>('cancel_export', { shootId })
export const listExports = (shootId: number) => call<ExportRecord[]>('list_exports', { shootId })

// --- premiere ----------------------------------------------------------------
// Queues a job for the Premiere Pro UXP panel's next poll (apps/premiere-panel) —
// the app has no way to reach into Premiere directly, only to ask its panel to.

export const sendMediaToPremiere = (mediaIds: number[], label?: string) =>
  call<void>('send_media_to_premiere', { mediaIds, label })
export const sendCollectionToPremiere = (collectionId: string) =>
  call<void>('send_collection_to_premiere', { collectionId })

/**
 * Whether the Premiere panel is installed on this machine. The app installs it
 * on launch (src-tauri/src/premiere_plugin.rs); this is how Settings reports
 * that, and offers a retry when it could not.
 */
export interface PremierePanelStatus {
  /** The panel version this build ships, or null if it was not packaged. */
  bundledVersion: string | null
  /** The version installed for this user, or null if the panel is not installed. */
  installedVersion: string | null
  /** Whether Creative Cloud's plugin installer could be found. */
  installerAvailable: boolean
}

export const premierePanelStatus = () => call<PremierePanelStatus>('premiere_panel_status')
export const installPremierePanel = () => call<PremierePanelStatus>('install_premiere_panel')

// --- logs and privacy ------------------------------------------------------

export const recentLogs = (shootId: number | null, limit = 200) =>
  call<LogEntry[]>('recent_logs', { shootId, limit })
export const clearAllEmbeddings = () => call<number>('clear_all_embeddings')
export const clearAllRecognitionData = () => call<void>('clear_all_recognition_data')
export const clearThumbnailCache = () => call<number>('clear_thumbnail_cache')
export const clearLog = () => call<void>('clear_log')

// --- library database ------------------------------------------------------
//
// These four are the only commands that work when the database could not be
// opened: there is no application state in that case, so everything else
// fails. `startupStatus` is what the UI checks before calling anything above.

export const startupStatus = () => call<StartupStatus>('startup_status')
export const databaseSettings = () => call<DatabaseSettings>('database_settings')

/** Opens and closes a connection, returning what happened in words. */
export const testDatabaseConnection = (settings: DatabaseSettings, password: string | null) =>
  call<string>('test_database_connection', { settings, password })

/** Saves the connection — but only if it opens. A blank password keeps the saved one. */
export const saveDatabaseConnection = (settings: DatabaseSettings, password: string | null) =>
  call<string>('save_database_connection', { settings, password })

export const restartForDatabaseChange = () => call<void>('restart_for_database_change')

// --- client mode and worker machines ---------------------------------------
//
// The first three run against whichever library the transport reaches (the
// server, in client mode). The rest are answered by the desktop itself and
// describe this installation.

export const listMachines = () => call<MachineRosterEntry[]>('list_machines')
export const enrolMachine = (name: string, machineId: string) =>
  call<EnrolResponse>('enrol_machine', { name, machineId })
export const revokeMachine = (machineId: string) => call<boolean>('revoke_machine', { machineId })

export const clientStatus = () => call<WorkerStatus>('client_status')
/** Points this installation at a server; null returns it to a library of its own. Needs a restart. */
export const setServerUrl = (url: string | null) => call<boolean>('set_server_url', { url })
export const restartForClientChange = () => call<void>('restart_for_client_change')
export const storeMachineEnrolment = (token: string, name: string) =>
  call<WorkerStatus>('store_machine_enrolment', { token, name })
export const forgetMachineEnrolment = () => call<WorkerStatus>('forget_machine_enrolment')
export const setWorkerEnabled = (enabled: boolean) => call<WorkerStatus>('set_worker_enabled', { enabled })
export const updateWorkerSettings = (settings: MachineSettings) =>
  call<WorkerStatus>('update_worker_settings', { settings })

// --- this machine's view of a file -----------------------------------------

/**
 * Where a media file is *from here*. On the machine that owns the library it
 * is the indexed path. On a client it is the shoot's share mapping joined
 * with the file's relative path — when the administrator has set one — and
 * otherwise the server's own path, which Explorer will not find.
 */
export async function localPathFor(item: Pick<Media, 'path' | 'shootId' | 'normalizedRelativePath'>): Promise<string> {
  if (transport().kind === 'tauri' || !item.normalizedRelativePath) return item.path
  const shoot = await getShoot(item.shootId).catch(() => null)
  const share = shoot?.sharePath?.replace(/[\\/]+$/, '')
  if (!share) return item.path
  const separator = share.includes('\\') ? '\\' : '/'
  return `${share}${separator}${item.normalizedRelativePath.split('/').join(separator)}`
}

/** "Show in folder" for a media row, translated for this machine first. */
export const revealMedia = async (item: Pick<Media, 'path' | 'shootId' | 'normalizedRelativePath'>) =>
  revealInFolder(await localPathFor(item))
