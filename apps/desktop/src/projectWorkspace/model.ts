import { useState } from 'react'

export interface CollectionSource { shootId: number; groupId: number }
export interface ProjectCollection {
  id: string
  name: string
  /** Null means the collection sits at the project root. */
  parentId: string | null
  sources: CollectionSource[]
}
export interface Project { id: string; name: string; kind: string; collections: ProjectCollection[] }

export const PROJECT_TYPES = ['Esports tournament', 'Sports tournament', 'Wedding', 'Other'] as const

export interface CollectionTemplate {
  name: string
  children?: CollectionTemplate[]
}

export const PROJECT_TEMPLATES: Record<(typeof PROJECT_TYPES)[number], CollectionTemplate[]> = {
  'Esports tournament': [
    { name: 'Teams', children: [{ name: 'Team Entry' }, { name: 'Team Reveal' }, { name: 'WWCD Moments' }] },
    { name: 'Players', children: [{ name: 'Player Highlights' }, { name: 'Player vs Player' }] },
    { name: 'MVP Videos' },
    { name: 'Match Highlights' },
  ],
  'Sports tournament': [
    { name: 'Teams', children: [{ name: 'Team Entry' }, { name: 'Team Highlights' }] },
    { name: 'Players', children: [{ name: 'Player Highlights' }, { name: 'Player vs Player' }] },
    { name: 'Matches', children: [{ name: 'Match Highlights' }, { name: 'Winning Moments' }] },
    { name: 'Awards' },
  ],
  Wedding: [
    { name: 'Couple', children: [{ name: 'Bride' }, { name: 'Groom' }, { name: 'Couple Moments' }] },
    { name: 'Ceremony' },
    { name: 'Family' },
    { name: 'Guests' },
    { name: 'Reception' },
    { name: 'Highlights' },
  ],
  Other: [],
}

export function createTemplateCollections(kind: string): ProjectCollection[] {
  const template = PROJECT_TEMPLATES[kind as keyof typeof PROJECT_TEMPLATES] ?? []
  const build = (items: CollectionTemplate[], parentId: string | null): ProjectCollection[] => items.flatMap(item => {
    const id = crypto.randomUUID()
    const collection: ProjectCollection = { id, name: item.name, parentId, sources: [] }
    return [collection, ...build(item.children ?? [], id)]
  })
  return build(template, null)
}

/** Separate, versioned navigation metadata. Media and memberships remain in SQLite.
 * This is local organisation, not an ACL or an organisation sharing implementation. */
export function useProjects(accountId: string) {
  const key = `skwad.project-workspace.v1.${encodeURIComponent(accountId)}`
  const [state, setState] = useState<{ projects: Project[]; error: string | null }>(() => {
    try {
      const value: unknown = JSON.parse(localStorage.getItem(key) ?? '[]')
      if (!Array.isArray(value) || !value.every(isProject)) throw new Error('Invalid project data')
      const projects = value.map((project) => ({
          ...project,
          // v1 project data did not have nesting. Treat those collections as roots.
          collections: project.collections.map((collection) => ({
            ...collection,
            parentId: typeof collection.parentId === 'string' ? collection.parentId : null,
          })),
        }))
      if (!projects.every(hasValidHierarchy)) throw new Error('Invalid collection hierarchy')
      return { projects, error: null }
    } catch {
      return { projects: [], error: 'Local project data could not be read. Switch to Classic to access your media. Existing project data has not been overwritten.' }
    }
  })
  const save = (projects: Project[]) => {
    if (state.error) throw new Error(state.error)
    localStorage.setItem(key, JSON.stringify(projects))
    setState({ projects, error: null })
  }
  return { ...state, save }
}

function hasValidHierarchy(project: Project): boolean {
  const ids = new Set(project.collections.map(collection => collection.id))
  if (ids.size !== project.collections.length) return false
  for (const collection of project.collections) {
    if (collection.parentId !== null && !ids.has(collection.parentId)) return false
    const seen = new Set<string>([collection.id])
    let parentId = collection.parentId
    while (parentId !== null) {
      if (seen.has(parentId)) return false
      seen.add(parentId)
      parentId = project.collections.find(candidate => candidate.id === parentId)?.parentId ?? null
    }
  }
  return true
}

function isProject(value: unknown): value is Project {
  if (!value || typeof value !== 'object') return false
  const p = value as Project
  return typeof p.id === 'string' && typeof p.name === 'string' && typeof p.kind === 'string'
    && Array.isArray(p.collections) && p.collections.every(c => c && typeof c === 'object' && typeof c.id === 'string'
      && typeof c.name === 'string' && (c.parentId === undefined || c.parentId === null || typeof c.parentId === 'string')
      && Array.isArray(c.sources) && c.sources.every(s => s && typeof s === 'object'
        && Number.isInteger(s.shootId) && Number.isInteger(s.groupId)))
}
