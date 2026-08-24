/**
 * Command name → HTTP request.
 *
 * The whole point of keeping command names as the contract: this table is the
 * only place that knows the API is HTTP at all. It mirrors the route list in
 * `crates/server/src/lib.rs` one for one, so the two can be diffed by eye when
 * either changes.
 */

export type Method = 'GET' | 'POST' | 'PATCH' | 'PUT' | 'DELETE'

export interface HttpRequestSpec {
  method: Method
  path: string
  /** JSON body, when the method takes one. */
  body?: unknown
  /** Query parameters; `null`/`undefined` values are dropped. */
  query?: Record<string, unknown>
  /**
   * A 404 means "nothing there" rather than an error for the handful of
   * commands whose desktop signature returns `T | null`.
   */
  nullOn404?: boolean
}

type Args = Record<string, any>

/** Drops the `undefined` values so `JSON.stringify` does not emit them. */
const defined = (object: Args): Args =>
  Object.fromEntries(Object.entries(object).filter(([, value]) => value !== undefined))

const ROUTES: Record<string, (args: Args) => HttpRequestSpec> = {
  // --- application ---------------------------------------------------------
  app_info: () => ({ method: 'GET', path: '/api/system/status' }),
  model_status: () => ({ method: 'GET', path: '/api/system/models' }),
  get_settings: () => ({ method: 'GET', path: '/api/settings' }),
  update_settings: (a) => ({ method: 'PUT', path: '/api/settings', body: a.settings }),

  // --- shoots --------------------------------------------------------------
  list_shoots: () => ({ method: 'GET', path: '/api/shoots' }),
  get_shoot: (a) => ({ method: 'GET', path: `/api/shoots/${a.shootId}`, nullOn404: true }),
  create_shoot: (a) => ({
    method: 'POST',
    path: '/api/shoots',
    body: { name: a.name, sourcePath: a.sourcePath },
  }),
  rename_shoot: (a) => ({ method: 'PATCH', path: `/api/shoots/${a.shootId}`, body: { name: a.name } }),
  delete_shoot_index: (a) => ({ method: 'DELETE', path: `/api/shoots/${a.shootId}` }),
  resume_processing: (a) => ({ method: 'POST', path: `/api/shoots/${a.shootId}/resume` }),
  cancel_processing: (a) => ({ method: 'POST', path: `/api/shoots/${a.shootId}/cancel` }),
  reanalyse_shoot: (a) => ({ method: 'POST', path: `/api/shoots/${a.shootId}/reanalyse` }),
  get_progress: (a) => ({ method: 'GET', path: `/api/shoots/${a.shootId}/progress` }),
  list_failed_jobs: (a) => ({ method: 'GET', path: `/api/shoots/${a.shootId}/failed-jobs` }),
  pause_processing: (a) => ({ method: 'POST', path: '/api/processing/pause', body: { paused: a.paused } }),
  clear_scanned_data: () => ({ method: 'POST', path: '/api/maintenance/clear-scanned-data' }),
  jobs_summary: () => ({ method: 'GET', path: '/api/jobs/summary' }),

  // --- media ---------------------------------------------------------------
  list_media: (a) => {
    const query = { ...(a.query ?? {}) }
    const shootId = query.shootId
    if (shootId === null || shootId === undefined) {
      // Every caller scopes to a shoot; the server route is shoot-scoped, so a
      // missing id is a bug worth naming rather than a silent whole-library read.
      throw new Error('list_media over HTTP needs a shootId in the query')
    }
    delete query.shootId
    return { method: 'GET', path: `/api/shoots/${shootId}/media`, query }
  },
  get_media: (a) => ({ method: 'GET', path: `/api/media/${a.mediaId}`, nullOn404: true }),
  media_faces: (a) => ({ method: 'GET', path: `/api/media/${a.mediaId}/faces` }),
  video_timelines: (a) => ({ method: 'GET', path: `/api/media/${a.mediaId}/timelines` }),

  // --- people --------------------------------------------------------------
  list_people: (a) => ({ method: 'GET', path: '/api/people', query: { shootId: a.shootId } }),
  create_person: (a) => ({ method: 'POST', path: '/api/people', body: { name: a.name, team: a.team } }),
  rename_person: (a) => ({ method: 'PATCH', path: `/api/people/${a.personId}`, body: { name: a.name } }),
  update_person: (a) => ({
    method: 'PATCH',
    path: `/api/people/${a.personId}`,
    body: { team: a.team, notes: a.notes },
  }),
  merge_people: (a) => ({
    method: 'POST',
    path: `/api/people/${a.targetId}/merge`,
    body: { sourceId: a.sourceId },
  }),
  delete_person: (a) => ({ method: 'DELETE', path: `/api/people/${a.personId}` }),
  clear_person_recognition: (a) => ({
    method: 'POST',
    path: `/api/people/${a.personId}/clear-recognition`,
  }),

  // --- clusters ------------------------------------------------------------
  list_clusters: (a) => ({
    method: 'GET',
    path: '/api/clusters',
    query: { shootId: a.shootId, includeNamed: a.includeNamed },
  }),
  name_cluster: (a) => ({
    method: 'POST',
    path: `/api/clusters/${a.clusterId}/name`,
    body: { name: a.name, team: a.team },
  }),
  merge_clusters: (a) => ({
    method: 'POST',
    path: `/api/clusters/${a.targetId}/merge`,
    body: { sourceId: a.sourceId },
  }),
  split_cluster: (a) => ({
    method: 'POST',
    path: `/api/clusters/${a.clusterId}/split`,
    body: { faceIds: a.faceIds, label: a.label },
  }),
  ignore_cluster: (a) => ({ method: 'POST', path: `/api/clusters/${a.clusterId}/ignore` }),

  // --- albums --------------------------------------------------------------
  list_albums: (a) => ({ method: 'GET', path: '/api/albums', query: { shootId: a.shootId } }),
  regenerate_albums: (a) => ({
    method: 'POST',
    path: '/api/albums/regenerate',
    body: { shootId: a.shootId },
  }),

  // --- groups --------------------------------------------------------------
  list_groups: (a) => ({ method: 'GET', path: '/api/groups', query: { shootId: a.shootId } }),
  group_stats: (a) => ({ method: 'GET', path: '/api/groups/stats', query: { shootId: a.shootId } }),
  group_links: (a) => ({ method: 'GET', path: '/api/groups/links', query: { shootId: a.shootId } }),
  create_group: (a) => ({
    method: 'POST',
    path: '/api/groups',
    body: { shootId: a.shootId, name: a.name },
  }),
  rename_group: (a) => ({ method: 'PATCH', path: `/api/groups/${a.groupId}`, body: { name: a.name } }),
  update_group: (a) => ({
    method: 'PATCH',
    path: `/api/groups/${a.groupId}`,
    body: { folderName: a.folderName, notes: a.notes },
  }),
  delete_group: (a) => ({ method: 'DELETE', path: `/api/groups/${a.groupId}` }),
  add_media_to_group: (a) => ({
    method: 'POST',
    path: '/api/groups/media',
    body: defined({
      shootId: a.shootId,
      groupId: a.groupId,
      groupName: a.groupName,
      mediaIds: a.mediaIds,
      moveFiles: a.moveFiles,
    }),
  }),
  remove_media_from_group: (a) => ({
    method: 'DELETE',
    path: `/api/groups/${a.groupId}/media`,
    body: { mediaIds: a.mediaIds },
  }),
  clear_group: (a) => ({ method: 'POST', path: `/api/groups/${a.groupId}/clear` }),
  groups_from_ai_albums: (a) => ({
    method: 'POST',
    path: '/api/groups/from-ai-albums',
    body: { shootId: a.shootId },
  }),
  group_from_album: (a) => ({
    method: 'POST',
    path: '/api/groups/from-album',
    body: { albumId: a.albumId, name: a.name },
  }),

  // --- review --------------------------------------------------------------
  list_faces: (a) => ({ method: 'GET', path: '/api/faces', query: a.query ?? {} }),
  confirm_faces: (a) => ({ method: 'POST', path: '/api/faces/confirm', body: { faceIds: a.faceIds } }),
  reject_faces: (a) => ({ method: 'POST', path: '/api/faces/reject', body: { faceIds: a.faceIds } }),
  assign_faces: (a) => ({
    method: 'POST',
    path: '/api/faces/assign',
    body: { faceIds: a.faceIds, personId: a.personId, personName: a.personName },
  }),
  ignore_faces: (a) => ({ method: 'POST', path: '/api/faces/not-a-face', body: { faceIds: a.faceIds } }),
  name_face: (a) => ({
    method: 'POST',
    path: '/api/faces/name',
    body: { faceId: a.faceId, name: a.name, team: a.team },
  }),

  // --- export --------------------------------------------------------------
  preview_export: (a) => ({
    method: 'POST',
    path: '/api/exports/preview',
    body: { shootId: a.shootId, destination: a.destination, options: a.options },
  }),
  start_export: (a) => ({
    method: 'POST',
    path: '/api/exports',
    body: { shootId: a.shootId, destination: a.destination, options: a.options },
  }),
  cancel_export: (a) => ({ method: 'POST', path: '/api/exports/cancel', body: { shootId: a.shootId } }),
  list_exports: (a) => ({ method: 'GET', path: '/api/exports', query: { shootId: a.shootId } }),

  // --- logs and privacy ----------------------------------------------------
  recent_logs: (a) => ({
    method: 'GET',
    path: '/api/logs',
    query: { shootId: a.shootId, limit: a.limit },
  }),
  clear_all_embeddings: () => ({ method: 'POST', path: '/api/maintenance/clear-embeddings' }),
  clear_all_recognition_data: () => ({
    method: 'POST',
    path: '/api/maintenance/clear-recognition-data',
  }),
  clear_thumbnail_cache: () => ({ method: 'POST', path: '/api/maintenance/clear-thumbnails' }),
  clear_log: () => ({ method: 'POST', path: '/api/maintenance/clear-log' }),

  // --- server only: the folder browser a browser needs ---------------------
  fs_roots: () => ({ method: 'GET', path: '/api/fs/roots' }),
  fs_list: (a) => ({ method: 'GET', path: '/api/fs/list', query: { path: a.path } }),
}

/** Commands that exist only on the desktop, with the reason a browser cannot. */
export const DESKTOP_ONLY: Record<string, string> = {
  reveal_in_folder: 'Revealing a file in the file manager',
  open_path: 'Opening a folder on this machine',
}

export function specFor(command: string, args: Args): HttpRequestSpec {
  const build = ROUTES[command]
  if (!build) {
    throw new Error(`no HTTP route is mapped for the command "${command}"`)
  }
  return build(args)
}

export const MAPPED_COMMANDS = Object.keys(ROUTES)
