/// <reference types="vite/client" />
/** Dev-only, separate HTML entry. Never imported by the desktop application.
 * Sample IPC lives entirely in memory and cannot read or alter real media. */
import React from 'react'
import ReactDOM from 'react-dom/client'
import { QueryClient, QueryClientProvider } from '@tanstack/react-query'
import { isTauri } from '@tauri-apps/api/core'
import { mockIPC } from '@tauri-apps/api/mocks'
import type { Group, Media, MediaQuery, PersonSummary, Project, ShootSummary } from '@skwad/shared-types'
import App from '../App'
import '../styles.css'

if (!import.meta.env.DEV || isTauri()) throw new Error('Sample workspace is only available in the development browser.')

const stamp = '2026-09-10T10:00:00Z'
const shoots: ShootSummary[] = ['BGIS 2026 · Finals', 'Wedding · Ceremony'].map((name, i) => ({ id: i + 1, name, sourcePath: `C:/Sample/${i + 1}`, status: i === 0 ? 'processing' : 'completed', notes: null, createdAt: stamp, updatedAt: stamp, photoCount: 3, videoCount: 0, faceCount: 0, personCount: 0, unknownClusterCount: 0, pendingJobs: i === 0 ? 1 : 0, failedJobs: 0, processingStartedAt: stamp, scanCompletedAt: stamp, processingCompletedAt: i === 0 ? null : stamp, processingDurationMs: i === 0 ? null : 6000 }))
const media: Media[] = Array.from({ length: 6 }, (_, i) => ({ id: i + 1, shootId: i < 3 ? 1 : 2, path: `C:/Sample/frame-${i + 1}.jpg`, filename: `frame-${i + 1}.jpg`, mediaType: 'photo', extension: 'jpg', width: 1920, height: 1080, duration: null, fileSize: 1024000, contentKey: `sample-${i}`, capturedAt: stamp, indexedAt: stamp, cameraMake: null, cameraModel: null, lens: null, iso: null, focalLength: null, aperture: null, shutter: null, orientation: 1, thumbnailPath: null, processingStatus: 'analysed', faceCount: 0, personCount: 0, qualityScore: null, sharpnessScore: null, exposureScore: null, perceptualHash: null, duplicateGroupId: null, duplicateCount: 0, isBestShot: true, rating: 0, pickState: 'none', error: null }))
const people: PersonSummary[] = [
  { id: 1, name: 'Aarav', team: 'Team Ember', notes: null, coverFaceId: null, faceSampleCount: 3, mediaCount: 3, shootCount: 2, createdAt: stamp, updatedAt: stamp },
  { id: 2, name: 'Meera', team: 'Team Nova', notes: null, coverFaceId: null, faceSampleCount: 2, mediaCount: 2, shootCount: 2, createdAt: stamp, updatedAt: stamp },
]
const taggedMedia = new Map<number, Set<number>>([[1, new Set([1, 2, 4])], [2, new Set([3, 5])]])
const group = (id: number, shootId: number, name: string): Group => ({ id, shootId, name, folderName: null, notes: null, personId: null, sortOrder: id, mediaCount: 0, photoCount: 0, videoCount: 0, coverMediaId: null, createdAt: stamp, updatedAt: stamp })
const groups = [group(1, 1, 'Finals highlights'), group(2, 2, 'Ceremony')]
const links = new Map<number, Set<number>>([[1, new Set([1, 2, 3])], [2, new Set([4, 5, 6])]])
const libraryKey = 'skwad.sample-library.v1'
try {
  const saved = JSON.parse(localStorage.getItem(libraryKey) ?? 'null') as { groups: Group[]; links: [number, number[]][] } | null
  if (saved && Array.isArray(saved.groups) && Array.isArray(saved.links)) {
    groups.splice(0, groups.length, ...saved.groups)
    links.clear(); saved.links.forEach(([id, ids]) => links.set(id, new Set(ids)))
  }
} catch { /* Sample data can always start fresh. */ }
const persistLibrary = () => localStorage.setItem(libraryKey, JSON.stringify({ groups, links: [...links].map(([id, ids]) => [id, [...ids]]) }))
const summary = (g: Group) => ({ ...g, mediaCount: links.get(g.id)?.size ?? 0, photoCount: links.get(g.id)?.size ?? 0 })
const profile = { userId: 'workspace-preview', email: 'sample@example.test', displayName: 'Sample workspace', avatarUrl: null, jobTitle: 'UI review', organisation: 'Sample data only', location: null, bio: null, createdAt: stamp, updatedAt: stamp }
const projectKey = 'skwad.sample-projects.v3'
const collection = (id: string, projectId: string, name: string, parentId: string | null, sources: Project['collections'][number]['sources'] = []): Project['collections'][number] => ({ id, projectId, name, parentId, notes: null, sortOrder: 0, sources, createdAt: stamp, updatedAt: stamp })
const sampleProjects: Project[] = [
  { id: 'sample-esports', name: 'BGIS 2026', kind: 'Esports tournament', ownerAccountId: profile.userId, ownerEmail: profile.email, organisation: profile.organisation, visibility: 'private', status: 'active', coverMediaId: null, accessRole: 'owner', members: [], mediaCount: 3, createdAt: stamp, updatedAt: stamp, collections: [
    collection('sample-finals', 'sample-esports', 'Finals highlights', null, [{ shootId: 1, groupId: 1 }]),
    collection('sample-team-entry', 'sample-esports', 'Team Entry', 'sample-finals'),
    collection('sample-team-reveal', 'sample-esports', 'Team Reveal', 'sample-team-entry'),
  ] },
  { id: 'sample-wedding', name: 'Ananya & Rahul', kind: 'Wedding', ownerAccountId: 'another-account', ownerEmail: 'owner@example.test', organisation: profile.organisation, visibility: 'invited', status: 'active', coverMediaId: null, accessRole: 'editor', members: [{ email: profile.email, displayName: profile.displayName, role: 'editor', invitationState: 'accepted' }], mediaCount: 3, createdAt: stamp, updatedAt: stamp, collections: [collection('sample-ceremony', 'sample-wedding', 'Ceremony', null, [{ shootId: 2, groupId: 2 }])] },
  { id: 'sample-org', name: 'SKWAD League', kind: 'Esports tournament', ownerAccountId: 'league-owner', ownerEmail: 'league@example.test', organisation: profile.organisation, visibility: 'organisation', status: 'active', coverMediaId: null, accessRole: 'viewer', members: [], mediaCount: 0, createdAt: stamp, updatedAt: stamp, collections: [] },
]
let previewProjects: Project[] = sampleProjects
try { const saved = JSON.parse(localStorage.getItem(projectKey) ?? 'null'); if (Array.isArray(saved)) previewProjects = saved } catch { /* Start from sample projects. */ }
const persistProjects = () => localStorage.setItem(projectKey, JSON.stringify(previewProjects))

mockIPC((command, args) => {
  const a = (args ?? {}) as Record<string, unknown>
  switch (command) {
    case 'catalogue_session_status': return { authenticatedOnce: true, passwordChangeRequired: false, accountId: 'workspace-preview', email: profile.email, deviceKeyId: null }
    case 'get_user_profile': return profile
    case 'list_projects': return previewProjects
    case 'save_project': {
      const incoming = a.project as Project
      const existing = previewProjects.find(project => project.id === incoming.id)
      const saved = { ...incoming, ownerAccountId: existing?.ownerAccountId ?? profile.userId, ownerEmail: existing?.ownerEmail ?? profile.email, accessRole: existing?.accessRole ?? 'owner', updatedAt: new Date().toISOString() }
      previewProjects = [...previewProjects.filter(project => project.id !== saved.id), saved]
      persistProjects(); return saved
    }
    case 'delete_project': previewProjects = previewProjects.filter(project => project.id !== a.projectId); persistProjects(); return null
    case 'replace_project_members': {
      const target = previewProjects.find(project => project.id === a.projectId)
      if (!target) throw new Error('Project not found')
      const saved = { ...target, members: a.members as Project['members'], visibility: (a.members as Project['members']).length > 0 && target.visibility === 'private' ? 'invited' as const : target.visibility, updatedAt: new Date().toISOString() }
      previewProjects = previewProjects.map(project => project.id === saved.id ? saved : project)
      persistProjects(); return saved
    }
    case 'app_info': return { version: 'Sample', mediaUrlBase: '/sample-media', databaseBytes: 0, thumbnailBytes: 0, modelBytes: 0, models: [], ffmpegAvailable: true }
    case 'list_shoots': return shoots
    case 'get_shoot': return shoots.find(s => s.id === a.shootId) ?? null
    case 'list_groups': return groups.filter(g => g.shootId === a.shootId).map(summary)
    case 'list_media': {
      const q = a.query as MediaQuery
      return media.filter(m => (!q.shootId || m.shootId === q.shootId) && (!q.groupId || links.get(q.groupId)?.has(m.id)) && (!q.mediaType || m.mediaType === q.mediaType) && (!q.search || m.filename.includes(q.search)) && (!q.minRating || m.rating >= q.minRating) && (!q.personId || taggedMedia.get(q.personId)?.has(m.id))).slice(q.offset ?? 0, (q.offset ?? 0) + (q.limit ?? 120))
    }
    case 'get_media': return media.find(m => m.id === a.mediaId) ?? null
    case 'create_group': {
      const existing = groups.find(g => g.shootId === a.shootId && g.name.toLowerCase() === String(a.name).toLowerCase())
      if (existing) return summary(existing)
      const result = group(Math.max(0, ...groups.map(g => g.id)) + 1, Number(a.shootId), String(a.name)); groups.push(result); links.set(result.id, new Set()); persistLibrary(); return result
    }
    case 'add_media_to_group': { const ids = links.get(Number(a.groupId))!; const before = ids.size; (a.mediaIds as number[]).forEach(id => ids.add(id)); persistLibrary(); return ids.size - before }
    case 'set_media_editorial': media.filter(m => (a.mediaIds as number[]).includes(m.id)).forEach(m => { if (a.rating != null) m.rating = Number(a.rating); if (a.pickState != null) m.pickState = a.pickState as Media['pickState'] }); return (a.mediaIds as number[]).length
    case 'list_people': return people
    case 'list_albums': case 'list_clusters': case 'list_faces': case 'list_exports': case 'list_loaded_catalogues': case 'media_faces': return []
    case 'group_stats': return { groups: 1, grouped: 3, ungrouped: 0, total: 3 }
    case 'group_links': return [...links].flatMap(([groupId, ids]) => [...ids].map(mediaId => ({ groupId, mediaId })))
    case 'get_shoot_storage': return { recordBytes: 0, previewBytes: 0 }
    case 'get_progress': return Number(a.shootId) === 1
      ? { shootId: 1, mediaTotal: 3, mediaScanned: 3, mediaAnalysed: 2, mediaFailed: 0, facesDetected: 4, facesRecognised: 2, facesUnknown: 2, photosTotal: 3, videosTotal: 0, jobsQueued: 1, jobsRunning: 1, jobsFailed: 0, jobsDone: 2, percent: 67, stage: 'analysing', stages: [], active: [], blockedReason: null, blockedKind: null }
      : { shootId: a.shootId, mediaTotal: 3, mediaScanned: 3, mediaAnalysed: 3, mediaFailed: 0, facesDetected: 0, facesRecognised: 0, facesUnknown: 0, photosTotal: 3, videosTotal: 0, jobsQueued: 0, jobsRunning: 0, jobsFailed: 0, jobsDone: 3, percent: 100, stage: 'complete', stages: [], active: [], blockedReason: null, blockedKind: null }
    case 'get_shoot_telemetry': return { shootId: a.shootId, run: null, samples: [], stages: [] }
    case 'preview_export': return { fileCount: 3, totalBytes: 3072000, folders: ['Sample collection'] }
    default: throw new Error('This action needs the desktop app. The browser preview uses sample data only.')
  }
}, { shouldMockEvents: true })

const client = new QueryClient({ defaultOptions: { queries: { retry: false, staleTime: 15000 } } })
ReactDOM.createRoot(document.getElementById('root')!).render(<React.StrictMode><QueryClientProvider client={client}><App /></QueryClientProvider></React.StrictMode>)
