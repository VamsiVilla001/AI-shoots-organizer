import { useRef, useState } from 'react'
import { useQueryClient } from '@tanstack/react-query'
import type { Media } from '@skwad/shared-types'
import * as api from '../api'
import { createProjectDraft, createTemplateCollections, PROJECT_TYPES, type CollectionSource, type Project, type ProjectVisibility } from './model'
import { ProjectTemplatePreview } from './ProjectTemplatePreview'
import { WorkspaceDialog } from './WorkspaceDialog'

export function PublishCollection({ media, projects, save, onClose, onPublished }: { media: Media[]; projects: Project[]; save: (projects: Project[]) => void; onClose: () => void; onPublished: (id: string) => void }) {
  const [name, setName] = useState('')
  const editableProjects = projects.filter(project => project.status === 'active' && project.accessRole !== 'viewer')
  const [projectId, setProjectId] = useState(editableProjects[0]?.id ?? 'new')
  const [parentId, setParentId] = useState('root')
  const [projectName, setProjectName] = useState('')
  const [kind, setKind] = useState('Esports tournament')
  const [visibility, setVisibility] = useState<ProjectVisibility>('private')
  const [busy, setBusy] = useState(false)
  const [error, setError] = useState('')
  // Reuse created groups on retry if a later source or local save fails.
  const sources = useRef<CollectionSource[]>([])
  const client = useQueryClient()
  const publish = async () => {
    setBusy(true); setError('')
    try {
      // create_group is get-or-create in the existing engine. Never silently
      // append a new collection to a same-named Classic group.
      const pendingIds = [...new Set(media.map(m => m.shootId))].filter(id => !sources.current.some(s => s.shootId === id))
      const existing = await Promise.all(pendingIds.map(id => api.listGroups(id)))
      if (existing.some(groups => groups.some(g => g.name.trim().toLocaleLowerCase() === name.trim().toLocaleLowerCase()))) {
        throw new Error('A group with this name already exists in one of the media sources. Choose a different collection name, or add the existing collection from your project.')
      }
      for (const shootId of new Set(media.map(m => m.shootId))) {
        let source = sources.current.find(s => s.shootId === shootId)
        if (!source) {
          const group = await api.createGroup(shootId, name.trim())
          source = { shootId, groupId: group.id }; sources.current.push(source)
        }
        await api.addMediaToGroup({ ...source, mediaIds: media.filter(m => m.shootId === shootId).map(m => m.id), moveFiles: false })
      }
      const id = projectId === 'new' ? crypto.randomUUID() : projectId
      const stamp = new Date().toISOString()
      const collection = { id: crypto.randomUUID(), projectId: id, name: name.trim(), parentId: projectId === 'new' || parentId === 'root' ? null : parentId, notes: null, sortOrder: 0, sources: [...sources.current], createdAt: stamp, updatedAt: stamp }
      if (projectId === 'new') {
        const project = createProjectDraft(projectName.trim(), kind, visibility)
        collection.projectId = project.id
        const templateCollections = createTemplateCollections(kind, project.id)
        const matchingRoot = templateCollections.find(item => item.parentId === null && item.name.toLocaleLowerCase() === collection.name.toLocaleLowerCase())
        const collections = matchingRoot
          ? templateCollections.map(item => item.id === matchingRoot.id ? { ...item, sources: collection.sources } : item)
          : [...templateCollections, collection]
        save([...projects, { ...project, collections }])
        onPublished(project.id)
      } else {
        save(projects.map(p => p.id === id ? { ...p, collections: [...p.collections, collection] } : p))
        onPublished(id)
      }
      await Promise.all(['groups', 'groupStats', 'groupLinks', 'media'].map(key => client.invalidateQueries({ queryKey: [key] })))
    } catch (e) {
      setError(`${String(e)}${sources.current.length ? ' Any created groups are preserved in Classic. Retry to finish adding this collection.' : ''}`)
    } finally { setBusy(false) }
  }
  const started = sources.current.length > 0
  return <WorkspaceDialog title="Create collection" onClose={() => { if (!busy) onClose() }}><form onSubmit={e => { e.preventDefault(); void publish() }}>
    <p>{media.length} selected files. Your media stays in the library and can belong to other collections.</p>
    <label className="field">Collection name<input autoFocus required maxLength={120} value={name} disabled={busy || started} onChange={e => setName(e.target.value)} placeholder="e.g. Finals highlights" /></label>
    <label className="field">Project<select value={projectId} disabled={busy} onChange={e => { setProjectId(e.target.value); setParentId('root') }}>{editableProjects.map(p => <option key={p.id} value={p.id}>{p.name}</option>)}<option value="new">Create a new project</option></select></label>
    {projectId !== 'new' && <label className="field">Location<select value={parentId} disabled={busy} onChange={e => setParentId(e.target.value)}><option value="root">Project root</option>{collectionOptions(projects.find(p => p.id === projectId)?.collections ?? []).map(({ collection, depth }) => <option key={collection.id} value={collection.id}>{`${'— '.repeat(depth + 1)}${collection.name}`}</option>)}</select></label>}
    {projectId === 'new' && <><label className="field">New project name<input required value={projectName} disabled={busy} onChange={e => setProjectName(e.target.value)} placeholder="e.g. BGIS 2026" /></label><label className="field">Project type<select value={kind} disabled={busy} onChange={e => setKind(e.target.value)}>{PROJECT_TYPES.map(k => <option key={k}>{k}</option>)}</select></label><label className="field">Access<select value={visibility} onChange={event => setVisibility(event.target.value as ProjectVisibility)}><option value="private">Private · only you</option><option value="invited">Invited people</option><option value="organisation">Everyone in your organisation</option></select></label><ProjectTemplatePreview kind={kind} /></>}
    <div className="pw-note">Media stays in the reusable library. This collection follows the selected project's access.</div>
    {error && <p role="alert" className="pw-error">{error}</p>}<div className="pw-dialog-actions"><button type="button" disabled={busy} onClick={onClose}>Cancel</button><button className="primary" disabled={busy || !name.trim() || (projectId === 'new' && !projectName.trim())}>{busy ? 'Creating collection…' : 'Create collection'}</button></div>
  </form></WorkspaceDialog>
}

function collectionOptions(collections: Project['collections'], parentId: string | null = null, depth = 0): Array<{ collection: Project['collections'][number]; depth: number }> {
  return collections
    .filter((collection) => collection.parentId === parentId)
    .flatMap((collection) => [{ collection, depth }, ...collectionOptions(collections, collection.id, depth + 1)])
}
