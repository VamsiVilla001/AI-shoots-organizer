import { useRef, useState } from 'react'
import { useQueryClient } from '@tanstack/react-query'
import type { Media } from '@skwad/shared-types'
import * as api from '../api'
import { createProjectDraft, PROJECT_TYPES, type CollectionSource, type Project, type ProjectVisibility } from './model'
import { WorkspaceDialog } from './WorkspaceDialog'
import { addMediaToCollection } from './collectionOps'
import { GROUP_CHANGE_KEYS, invalidateKeys } from '../queryKeys'
import { useUi } from '../store'

export function PublishCollection({ media, projects, save, onClose, onPublished }: { media: Media[]; projects: Project[]; save: (projects: Project[]) => void; onClose: () => void; onPublished: (id: string) => void }) {
  // Built from a tag: the collection is offered under the value's name and
  // carries the tag once it exists.
  const pendingTag = useUi(state => state.pendingCollectionTag)
  const setPendingTag = useUi(state => state.setPendingCollectionTag)
  const [name, setName] = useState(pendingTag?.value ?? '')
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
  // Synchronous guard: `busy` state only takes effect after a re-render, so a
  // fast double Enter/click on the form could otherwise fire publish() twice
  // and create two Classic groups for one collection.
  const publishing = useRef(false)
  const client = useQueryClient()
  const publish = async () => {
    if (publishing.current) return
    publishing.current = true
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
      if (pendingTag) {
        await api.assignTag('collection', collection.id, pendingTag.tag ?? 'Tag', pendingTag.value).catch(() => {})
        setPendingTag(null)
      }
      if (projectId === 'new') {
        const project = createProjectDraft(projectName.trim(), kind, visibility)
        collection.projectId = project.id
        save([...projects, { ...project, collections: [collection] }])
        onPublished(project.id)
      } else {
        save(projects.map(p => p.id === id ? { ...p, collections: [...p.collections, collection] } : p))
        onPublished(id)
      }
      await invalidateKeys(client, GROUP_CHANGE_KEYS)
    } catch (e) {
      setError(`${String(e)}${sources.current.length ? ' Any created groups are preserved in Classic. Retry to finish adding this collection.' : ''}`)
    } finally { publishing.current = false; setBusy(false) }
  }
  const started = sources.current.length > 0
  return <WorkspaceDialog title="Create collection" onClose={() => { if (!busy) { setPendingTag(null); onClose() } }}><form onSubmit={e => { e.preventDefault(); void publish() }}>
    <p>{media.length} selected files. Your media stays in the library and can belong to other collections.</p>
    <label className="field">Collection name<input autoFocus required maxLength={120} value={name} disabled={busy || started} onChange={e => setName(e.target.value)} placeholder="e.g. Finals highlights" /></label>
    <label className="field">Project<select value={projectId} disabled={busy} onChange={e => { setProjectId(e.target.value); setParentId('root') }}>{editableProjects.map(p => <option key={p.id} value={p.id}>{p.name}</option>)}<option value="new">Create a new project</option></select></label>
    {projectId !== 'new' && <label className="field">Location<select value={parentId} disabled={busy} onChange={e => setParentId(e.target.value)}><option value="root">Project root</option>{collectionOptions(projects.find(p => p.id === projectId)?.collections ?? []).map(({ collection, depth }) => <option key={collection.id} value={collection.id}>{`${'— '.repeat(depth + 1)}${collection.name}`}</option>)}</select></label>}
    {projectId === 'new' && <><label className="field">New project name<input required value={projectName} disabled={busy} onChange={e => setProjectName(e.target.value)} placeholder="e.g. BGIS 2026" /></label><label className="field">Project type<select value={kind} disabled={busy} onChange={e => setKind(e.target.value)}>{PROJECT_TYPES.map(k => <option key={k}>{k}</option>)}</select></label><label className="field">Access<select value={visibility} onChange={event => setVisibility(event.target.value as ProjectVisibility)}><option value="private">Private · only you</option><option value="invited">Invited people</option><option value="organisation">Everyone in your organisation</option></select></label></>}
    <div className="pw-note">Media stays in the reusable library. This collection follows the selected project's access.</div>
    {error && <p role="alert" className="pw-error">{error}</p>}<div className="pw-dialog-actions"><button type="button" disabled={busy} onClick={onClose}>Cancel</button><button className="primary" disabled={busy || !name.trim() || (projectId === 'new' && !projectName.trim())}>{busy ? 'Creating collection…' : 'Create collection'}</button></div>
  </form></WorkspaceDialog>
}

/**
 * Adds the selected media to a collection that already exists, rather than
 * creating another one. A collection points at `{shootId, groupId}` sources,
 * so the media joins the source group for its own shoot — and gains a new
 * source only when the selection spans a shoot the collection has never
 * covered before.
 */
export function AddToExistingCollection({ media, projects, save, onClose, onAdded }: { media: Media[]; projects: Project[]; save: (projects: Project[]) => void; onClose: () => void; onAdded: (projectId: string, collectionId: string, added: number) => void }) {
  const pendingTag = useUi(state => state.pendingCollectionTag)
  const setPendingTag = useUi(state => state.setPendingCollectionTag)
  const editableProjects = projects.filter(project => project.status === 'active' && project.accessRole !== 'viewer')
  const [projectId, setProjectId] = useState(editableProjects.find(project => project.collections.length > 0)?.id ?? editableProjects[0]?.id ?? '')
  const [collectionId, setCollectionId] = useState('')
  const [busy, setBusy] = useState(false)
  const [error, setError] = useState('')
  // Same guard as PublishCollection: `busy` only lands on the next render, so
  // a fast double submit could otherwise add the media twice.
  const adding = useRef(false)
  const client = useQueryClient()
  const project = editableProjects.find(item => item.id === projectId)
  const options = collectionOptions(project?.collections ?? [])
  const collection = project?.collections.find(item => item.id === collectionId)

  const submit = async () => {
    if (adding.current || !project || !collection) return
    adding.current = true
    setBusy(true); setError('')
    try {
      const added = await addMediaToCollection(media, project, collection, projects, save, client)
      if (pendingTag) {
        await api.assignTag('collection', collection.id, pendingTag.tag ?? 'Tag', pendingTag.value).catch(() => {})
        setPendingTag(null)
      }
      onAdded(project.id, collection.id, added)
    } catch (e) {
      setError(String(e))
    } finally { adding.current = false; setBusy(false) }
  }

  return <WorkspaceDialog title="Add to existing collection" onClose={() => { if (!busy) { setPendingTag(null); onClose() } }}><form onSubmit={e => { e.preventDefault(); void submit() }}>
    <p>{media.length} selected file{media.length === 1 ? '' : 's'}. Your media stays in the library and can belong to more than one collection.</p>
    <label className="field">Project<select value={projectId} disabled={busy} onChange={e => { setProjectId(e.target.value); setCollectionId('') }}>{editableProjects.map(item => <option key={item.id} value={item.id}>{item.name}</option>)}</select></label>
    <label className="field">Collection<select required value={collectionId} disabled={busy || options.length === 0} onChange={e => setCollectionId(e.target.value)}><option value="">Choose a collection…</option>{options.map(({ collection: item, depth }) => <option key={item.id} value={item.id}>{`${'— '.repeat(depth)}${item.name}`}</option>)}</select></label>
    {editableProjects.length === 0 && <p className="pw-help">No editable projects yet. Create a collection instead.</p>}
    {project && options.length === 0 && <p className="pw-help">This project has no collections yet. Create one instead.</p>}
    {error && <p role="alert" className="pw-error">{error}</p>}
    <div className="pw-dialog-actions"><button type="button" disabled={busy} onClick={onClose}>Cancel</button><button className="primary" disabled={busy || !collection}>{busy ? 'Adding…' : 'Add to collection'}</button></div>
  </form></WorkspaceDialog>
}

function collectionOptions(collections: Project['collections'], parentId: string | null = null, depth = 0): Array<{ collection: Project['collections'][number]; depth: number }> {
  return collections
    .filter((collection) => collection.parentId === parentId)
    .flatMap((collection) => [{ collection, depth }, ...collectionOptions(collections, collection.id, depth + 1)])
}
