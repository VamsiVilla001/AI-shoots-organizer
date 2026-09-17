import { useEffect, useRef, useState } from 'react'
import type { Project, ProjectCollection, ProjectMember, ProjectVisibility } from '@skwad/shared-types'
import * as api from '../api'

export type { Project, ProjectCollection, ProjectMember, ProjectVisibility }
export type CollectionSource = ProjectCollection['sources'][number]

/** Labels a project; it does not decide what is inside one. */
export const PROJECT_TYPES = ['Esports tournament', 'Sports tournament', 'Wedding', 'Other'] as const

export function createProjectDraft(name: string, kind: string, visibility: ProjectVisibility = 'private'): Project {
  const id = crypto.randomUUID()
  const stamp = new Date().toISOString()
  return {
    id, name, kind, ownerAccountId: '', ownerEmail: '', organisation: null, visibility, status: 'active', coverMediaId: null,
    accessRole: 'owner', collections: [], members: [], mediaCount: 0,
    createdAt: stamp, updatedAt: stamp,
  }
}

interface ProjectState { projects: Project[]; error: string | null; loading: boolean; saving: boolean }

/** Loads projects from SQLite and imports the old localStorage payload once. */
export function useProjects(accountId: string) {
  const [state, setState] = useState<ProjectState>({ projects: [], error: null, loading: true, saving: false })
  const queue = useRef(Promise.resolve())
  const current = useRef<Project[]>([])
  const alive = useRef(true)

  useEffect(() => {
    alive.current = true
    void loadProjects(accountId).then(projects => {
      current.current = projects
      if (alive.current) setState({ projects, error: null, loading: false, saving: false })
    }).catch(error => {
      if (alive.current) setState({ projects: [], error: String(error), loading: false, saving: false })
    })
    return () => { alive.current = false }
  }, [accountId])

  const save = (projects: Project[]) => {
    const previous = current.current
    current.current = projects
    setState(value => ({ ...value, projects, error: null, saving: true }))
    const nextIds = new Set(projects.map(project => project.id))
    const removed = previous.filter(project => !nextIds.has(project.id))
    const changed = projects.filter(project => {
      const before = previous.find(item => item.id === project.id)
      return !before || JSON.stringify(before) !== JSON.stringify(project)
    })
    queue.current = queue.current.then(async () => {
      for (const project of removed) await api.deleteProject(project.id)
      for (const project of changed) await api.saveProject(project)
      const fresh = await api.listProjects()
      current.current = fresh
      if (alive.current) setState({ projects: fresh, error: null, loading: false, saving: false })
    }).catch(async error => {
      try {
        const fresh = await api.listProjects()
        current.current = fresh
        if (alive.current) setState({ projects: fresh, error: String(error), loading: false, saving: false })
      } catch {
        if (alive.current) setState(value => ({ ...value, error: String(error), saving: false }))
      }
    })
  }

  const replaceMembers = async (projectId: string, members: ProjectMember[]) => {
    setState(value => ({ ...value, saving: true, error: null }))
    try {
      const saved = await api.replaceProjectMembers(projectId, members)
      const projects = current.current.map(project => project.id === saved.id ? saved : project)
      current.current = projects
      if (alive.current) setState({ projects, error: null, loading: false, saving: false })
    } catch (error) {
      if (alive.current) setState(value => ({ ...value, error: String(error), saving: false }))
      throw error
    }
  }

  return { ...state, save, replaceMembers }
}

async function loadProjects(accountId: string): Promise<Project[]> {
  let projects = await api.listProjects()
  if (projects.length > 0) return projects
  const migrationKey = `skwad.project-workspace.v1.${encodeURIComponent(accountId)}`
  let raw: string | null = null
  try { raw = localStorage.getItem(migrationKey) } catch { return projects }
  if (!raw) return projects
  const parsed: unknown = JSON.parse(raw)
  if (!Array.isArray(parsed) || !parsed.every(isLegacyProject)) {
    throw new Error('Local project data could not be migrated. It has not been overwritten.')
  }
  for (const legacy of parsed) {
    const stamp = new Date().toISOString()
    await api.saveProject({
      id: legacy.id, name: legacy.name, kind: legacy.kind, ownerAccountId: '', ownerEmail: '', organisation: null,
      visibility: 'private', status: 'active', coverMediaId: null, accessRole: 'owner', members: [], mediaCount: 0,
      collections: legacy.collections.map((collection, index) => ({
        id: collection.id, projectId: legacy.id, name: collection.name, parentId: collection.parentId ?? null,
        notes: null, sortOrder: index, sources: collection.sources, createdAt: stamp, updatedAt: stamp,
      })),
      createdAt: stamp, updatedAt: stamp,
    })
  }
  projects = await api.listProjects()
  try {
    localStorage.setItem(`${migrationKey}.migrated`, new Date().toISOString())
    localStorage.removeItem(migrationKey)
  } catch { /* The durable import succeeded even when webview cleanup fails. */ }
  return projects
}

interface LegacyProject {
  id: string; name: string; kind: string
  collections: Array<{ id: string; name: string; parentId?: string | null; sources: Array<{ shootId: number; groupId: number }> }>
}

function isLegacyProject(value: unknown): value is LegacyProject {
  if (!value || typeof value !== 'object') return false
  const project = value as LegacyProject
  return typeof project.id === 'string' && typeof project.name === 'string' && typeof project.kind === 'string'
    && Array.isArray(project.collections) && project.collections.every(collection => collection
      && typeof collection.id === 'string' && typeof collection.name === 'string'
      && (collection.parentId === undefined || collection.parentId === null || typeof collection.parentId === 'string')
      && Array.isArray(collection.sources) && collection.sources.every(source => Number.isInteger(source.shootId) && Number.isInteger(source.groupId)))
}
