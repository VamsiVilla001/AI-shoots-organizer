import { useEffect, useState, type MouseEvent } from 'react'
import { useQueries, useQuery } from '@tanstack/react-query'
import type { Group } from '@skwad/shared-types'
import * as api from '../api'
import { thumbUrl } from '../media'
import { useUi } from '../store'
import { ExportScreen } from '../screens/ExportScreen'
import { WorkspaceDialog } from './WorkspaceDialog'
import { MediaBrowser } from './mediaBrowser'
import { createTemplateCollections, PROJECT_TYPES, type Project, type ProjectCollection } from './model'
import { ProjectTemplatePreview } from './ProjectTemplatePreview'

export function Collections({ projects, save, projectId, setProjectId, onProcess }: {
  projects: Project[]
  save: (projects: Project[]) => void
  projectId: string | null
  setProjectId: (id: string | null) => void
  onProcess: () => void
}) {
  const [search, setSearch] = useState('')
  const [creatingProject, setCreatingProject] = useState(false)
  const [creatingCollection, setCreatingCollection] = useState(false)
  const [linking, setLinking] = useState(false)
  const [editingProject, setEditingProject] = useState<Project | null>(null)
  const [renamingCollection, setRenamingCollection] = useState<ProjectCollection | null>(null)
  const [folderMenu, setFolderMenu] = useState<{ collectionId: string; x: number; y: number } | null>(null)
  const [projectMenu, setProjectMenu] = useState<{ projectId: string; x: number; y: number } | null>(null)
  const [collectionId, setCollectionId] = useState<string | null>(null)
  const [exportSource, setExportSource] = useState<{ shootId: number; groupId: number } | null>(null)
  const shoots = useQuery({ queryKey: ['shoots'], queryFn: api.listShoots })
  const groups = useQueries({ queries: (shoots.data ?? []).map(shoot => ({ queryKey: ['groups', shoot.id], queryFn: () => api.listGroups(shoot.id) })) })
  const allGroups = groups.flatMap(query => query.data ?? [])
  const project = projects.find(item => item.id === projectId)
  const collection = project?.collections.find(item => item.id === collectionId)
  const menuCollection = project?.collections.find(item => item.id === folderMenu?.collectionId)
  const menuProject = projects.find(item => item.id === projectMenu?.projectId)
  const currentChildren = project ? childrenOf(project, collection?.id ?? null) : []
  const visibleChildren = currentChildren.filter(item => matches(item.name, search))
  const notice = useUi(state => state.pushNotice)

  const act = (next: Project[]) => {
    try { save(next) } catch (error) { notice({ level: 'error', message: String(error) }) }
  }
  const resolve = (item: ProjectCollection) => item.sources.flatMap(source => allGroups.filter(group => group.id === source.groupId && group.shootId === source.shootId))
  const openProject = (id: string | null) => { setProjectId(id); setCollectionId(null); setSearch(''); setExportSource(null) }
  const openCollection = (id: string | null) => { setCollectionId(id); setSearch(''); setExportSource(null) }
  const rootCount = project ? childrenOf(project, null).length : 0

  return <>
    {project && <Breadcrumb project={project} collection={collection} onProjects={() => openProject(null)} onCollection={openCollection} />}
    <header className="pw-heading">
      <div><span className="pw-eyebrow">Collections</span><h1>{collection?.name ?? project?.name ?? 'Your projects'}</h1><p>{collection ? `${currentChildren.length} collection${currentChildren.length === 1 ? '' : 's'}` : project ? `${project.kind} · ${rootCount} main collection${rootCount === 1 ? '' : 's'}` : 'Every event. Every collection. One place.'}</p></div>
      <div className="actions">
        {!project && <button className="primary" onClick={() => setCreatingProject(true)}>New project</button>}
        {project && !collection && <><button onClick={() => setEditingProject(project)}>Project settings</button><button className="primary" onClick={onProcess}>Create collection</button></>}
        {project && collection && !exportSource && <><button onClick={() => setLinking(true)}>Add existing</button><button className="primary" onClick={() => setCreatingCollection(true)}>New collection</button></>}
      </div>
    </header>

    {(shoots.isError || groups.some(query => query.isError)) && <p role="alert" className="pw-error">Some library data could not be loaded. <button onClick={() => { void shoots.refetch(); groups.forEach(query => void query.refetch()) }}>Retry</button></p>}

    {!project && <>
      <div className="pw-toolbar"><label className="pw-search"><span className="sr-only">Search projects</span><input type="search" placeholder="Search projects…" value={search} onChange={event => setSearch(event.target.value)} /></label><span>{projects.length} projects</span></div>
      <div className="pw-card-grid">{projects.filter(item => matches(item.name, search)).map(item => {
        const linked = item.collections.flatMap(resolve)
        const roots = childrenOf(item, null)
        return <CollectionCard key={item.id} name={item.name} label={item.kind} mediaId={linked.find(group => group.coverMediaId !== null)?.coverMediaId} meta={`${roots.length} main collection${roots.length === 1 ? '' : 's'}`} detail="Local project" onOpen={() => openProject(item.id)} onContextMenu={event => { event.preventDefault(); setProjectMenu({ projectId: item.id, x: event.clientX, y: event.clientY }) }} />
      })}</div>
      {projects.length === 0 && <div className="pw-empty"><span className="pw-eyebrow">A home for every event</span><h2>Start with a project</h2><p>A tournament, wedding, or anything you're working on.<br />Bring its collections together without moving your media.</p><button className="primary" onClick={() => setCreatingProject(true)}>Create your first project</button></div>}
      {projects.length > 0 && !projects.some(item => matches(item.name, search)) && <NoMatches onClear={() => setSearch('')} />}
      <div className="pw-library-callout"><div><strong>Your media is always available</strong><p>Find a person, revisit processed files, or make another collection in Media Processing.</p></div><button onClick={onProcess}>Open media library</button></div>
    </>}

    {project && !exportSource && <>
      {currentChildren.length > 0 && <div className="pw-toolbar"><label className="pw-search"><span className="sr-only">Search collections</span><input type="search" placeholder="Search collections…" value={search} onChange={event => setSearch(event.target.value)} /></label><span>{currentChildren.length} collections</span>{!collection && <button onClick={() => setLinking(true)}>Add existing collection</button>}</div>}
      {currentChildren.length > 0 && <section className="pw-folder-section" aria-labelledby="collection-folders"><h2 id="collection-folders">{collection ? 'Collections' : 'Main collections'}</h2><div className="pw-card-grid">{visibleChildren.map(item => {
        const linked = resolve(item)
        const childCount = childrenOf(project, item.id).length
        const fileCount = linked.reduce((total, group) => total + group.mediaCount, 0)
        return <CollectionCard key={item.id} name={item.name} label={childCount > 0 ? 'Collection folder' : 'Collection'} mediaId={linked.find(group => group.coverMediaId !== null)?.coverMediaId} meta={`${fileCount} files`} detail={`${childCount} collection${childCount === 1 ? '' : 's'}`} onOpen={() => openCollection(item.id)} onContextMenu={event => { event.preventDefault(); setFolderMenu({ collectionId: item.id, x: event.clientX, y: event.clientY }) }} />
      })}</div></section>}
      {currentChildren.length > 0 && visibleChildren.length === 0 && <NoMatches onClear={() => setSearch('')} />}
      {!collection && currentChildren.length === 0 && <div className="pw-empty"><h2>Ready for your first collection</h2><p>Select media or find a person in your library, then add their collection here.</p><div className="actions"><button onClick={() => setLinking(true)}>Add existing collection</button><button className="primary" onClick={onProcess}>Open media library</button></div></div>}
      {collection && collection.sources.length > 0 && <section className={currentChildren.length > 0 ? 'pw-media-section' : undefined} aria-labelledby="collection-media"><h2 id="collection-media" className="pw-section-heading">Media in this collection</h2><CollectionMedia key={collection.id} collection={collection} onExport={source => { useUi.getState().openShoot(source.shootId, 'export'); setExportSource(source) }} /></section>}
      {collection && collection.sources.length === 0 && currentChildren.length === 0 && <div className="pw-empty"><h2>This collection is empty</h2><p>Create a collection here, or add an existing collection from your processed media.</p><div className="actions"><button onClick={() => setLinking(true)}>Add existing</button><button className="primary" onClick={() => setCreatingCollection(true)}>New collection</button></div></div>}
      {collection && <CollectionOptions project={project} collection={collection} projects={projects} act={act} onRemoved={() => openCollection(collection.parentId)} />}
    </>}

    {project && collection && exportSource && <><button onClick={() => setExportSource(null)}>Back to collection</button><div className="pw-existing"><ExportScreen key={`${exportSource.shootId}-${exportSource.groupId}`} initialGroupIds={[exportSource.groupId]} /></div></>}

    {folderMenu && project && menuCollection && <FolderContextMenu x={folderMenu.x} y={folderMenu.y} name={menuCollection.name} onClose={() => setFolderMenu(null)} onOpen={() => openCollection(menuCollection.id)} onRename={() => setRenamingCollection(menuCollection)} onCreate={() => { openCollection(menuCollection.id); setCreatingCollection(true) }} onAddExisting={() => { openCollection(menuCollection.id); setLinking(true) }} onRemove={() => {
      const subtree = new Set([menuCollection.id, ...descendantsOf(project, menuCollection.id).map(item => item.id)])
      if (window.confirm(`Remove “${menuCollection.name}” and its nested collections from this project? Their media and original groups remain in the library.`)) act(projects.map(item => item.id === project.id ? { ...item, collections: item.collections.filter(child => !subtree.has(child.id)) } : item))
    }} />}
    {projectMenu && menuProject && <ProjectContextMenu x={projectMenu.x} y={projectMenu.y} name={menuProject.name} onClose={() => setProjectMenu(null)} onOpen={() => openProject(menuProject.id)} onEdit={() => setEditingProject(menuProject)} onDelete={() => {
      if (window.confirm(`Delete the project “${menuProject.name}”? Its media, imports, and original groups will remain available.`)) { act(projects.filter(item => item.id !== menuProject.id)); if (projectId === menuProject.id) openProject(null) }
    }} />}
    {(creatingProject || editingProject) && <ProjectDialog project={editingProject ?? undefined} onClose={() => { setCreatingProject(false); setEditingProject(null) }} onSave={(name, kind) => {
      const id = editingProject?.id ?? crypto.randomUUID()
      save(editingProject ? projects.map(item => item.id === id ? { ...item, name, kind } : item) : [...projects, { id, name, kind, collections: createTemplateCollections(kind) }])
      setCreatingProject(false); setEditingProject(null); openProject(id)
    }} onDelete={editingProject ? () => {
      if (window.confirm(`Delete the project “${editingProject.name}”? Its media, imports, and original groups will remain available.`)) { save(projects.filter(item => item.id !== editingProject.id)); setEditingProject(null); openProject(null) }
    } : undefined} />}
    {creatingCollection && project && collection && <NewCollectionDialog siblings={currentChildren} onClose={() => setCreatingCollection(false)} onSave={name => {
      const child: ProjectCollection = { id: crypto.randomUUID(), name, parentId: collection.id, sources: [] }
      save(projects.map(item => item.id === project.id ? { ...item, collections: [...item.collections, child] } : item)); setCreatingCollection(false); openCollection(child.id)
    }} />}
    {renamingCollection && project && <RenameCollectionDialog collection={renamingCollection} siblings={childrenOf(project, renamingCollection.parentId)} onClose={() => setRenamingCollection(null)} onSave={name => {
      save(projects.map(item => item.id === project.id ? { ...item, collections: item.collections.map(child => child.id === renamingCollection.id ? { ...child, name } : child) } : item)); setRenamingCollection(null)
    }} />}
    {linking && project && <LinkDialog groups={allGroups} project={project} parentId={collection?.id ?? null} onClose={() => setLinking(false)} onSave={selected => {
      const additions: ProjectCollection[] = selected.map(group => ({ id: crypto.randomUUID(), name: group.name, parentId: collection?.id ?? null, sources: [{ shootId: group.shootId, groupId: group.id }] }))
      save(projects.map(item => item.id === project.id ? { ...item, collections: [...item.collections, ...additions] } : item)); setLinking(false)
    }} />}
  </>
}

function Breadcrumb({ project, collection, onProjects, onCollection }: { project: Project; collection?: ProjectCollection; onProjects: () => void; onCollection: (id: string | null) => void }) {
  const trail = collection ? collectionTrail(project, collection) : []
  return <nav className="pw-breadcrumb" aria-label="Breadcrumb"><button onClick={onProjects}>Projects</button><span>/</span>{collection ? <><button onClick={() => onCollection(null)}>{project.name}</button>{trail.map((item, index) => <span className="pw-breadcrumb-part" key={item.id}><span>/</span>{index === trail.length - 1 ? <span aria-current="page">{item.name}</span> : <button onClick={() => onCollection(item.id)}>{item.name}</button>}</span>)}</> : <span aria-current="page">{project.name}</span>}</nav>
}

function CollectionCard({ name, label, mediaId, meta, detail, onOpen, onContextMenu }: { name: string; label: string; mediaId?: number | null; meta: string; detail: string; onOpen: () => void; onContextMenu?: (event: MouseEvent<HTMLButtonElement>) => void }) {
  return <button className="pw-cover-card" onClick={onOpen} onContextMenu={onContextMenu}><Cover mediaId={mediaId} label={label} /><div className="pw-card-body"><h2>{name}</h2><p>{meta}<span>{detail}</span></p></div></button>
}

function Cover({ mediaId, label }: { mediaId?: number | null; label: string }) {
  const [failed, setFailed] = useState(false)
  return <div className="pw-cover">{mediaId != null && !failed ? <img src={thumbUrl(mediaId)} alt="" loading="lazy" onError={() => setFailed(true)} /> : <span>{label}</span>}</div>
}

function CollectionMedia({ collection, onExport }: { collection: ProjectCollection; onExport: (source: ProjectCollection['sources'][number]) => void }) {
  const [index, setIndex] = useState(0)
  const source = collection.sources[index]
  const shoots = useQuery({ queryKey: ['shoots'], queryFn: api.listShoots })
  if (!source) return null
  return <><div className="pw-toolbar">{collection.sources.length > 1 && <label>Media source <select value={index} onChange={event => setIndex(Number(event.target.value))}>{collection.sources.map((item, sourceIndex) => <option key={`${item.shootId}-${item.groupId}`} value={sourceIndex}>{shoots.data?.find(shoot => shoot.id === item.shootId)?.name ?? `Source ${sourceIndex + 1}`}</option>)}</select></label>}<button className="primary" onClick={() => onExport(source)}>Export {collection.sources.length > 1 ? 'this source' : 'collection'}</button></div><MediaBrowser key={`${source.shootId}-${source.groupId}`} shootId={source.shootId} groupId={source.groupId} /></>
}

function ProjectDialog({ project, onClose, onSave, onDelete }: { project?: Project; onClose: () => void; onSave: (name: string, kind: string) => void; onDelete?: () => void }) {
  const [name, setName] = useState(project?.name ?? '')
  const [kind, setKind] = useState(project?.kind ?? 'Esports tournament')
  const [error, setError] = useState('')
  return <WorkspaceDialog title={project ? 'Project settings' : 'New project'} onClose={onClose}><form onSubmit={event => { event.preventDefault(); try { onSave(name.trim(), kind) } catch (caught) { setError(String(caught)) } }}><label className="field">Project name<input autoFocus required maxLength={120} value={name} onChange={event => setName(event.target.value)} placeholder="e.g. BGIS 2026" /></label><label className="field">Project type<select value={kind} onChange={event => setKind(event.target.value)}>{PROJECT_TYPES.map(item => <option key={item}>{item}</option>)}</select></label>{!project && <ProjectTemplatePreview kind={kind} />}<div className="pw-note"><strong>Local project</strong><p>Saved on this device. Organisation and individual sharing will be added separately; this does not change access to the source media.</p></div>{project && <p className="pw-help">Changing the project type keeps its current collections.</p>}{error && <p role="alert" className="pw-error">{error}</p>}<div className="pw-dialog-actions">{onDelete && <button type="button" className="danger pw-delete-project" onClick={onDelete}>Delete project</button>}<button type="button" onClick={onClose}>Cancel</button><button className="primary" disabled={!name.trim()}>{project ? 'Save changes' : 'Create project'}</button></div></form></WorkspaceDialog>
}

function NewCollectionDialog({ siblings, onClose, onSave }: { siblings: ProjectCollection[]; onClose: () => void; onSave: (name: string) => void }) {
  const [name, setName] = useState('')
  const [error, setError] = useState('')
  return <WorkspaceDialog title="New collection" onClose={onClose}><form onSubmit={event => { event.preventDefault(); const clean = name.trim(); if (siblings.some(item => item.name.toLocaleLowerCase() === clean.toLocaleLowerCase())) { setError('A collection with this name already exists here.'); return } try { onSave(clean) } catch (caught) { setError(String(caught)) } }}><p>Create it inside the current collection. You can add more levels later.</p><label className="field">Name<input autoFocus required maxLength={120} value={name} onChange={event => setName(event.target.value)} placeholder="e.g. Team Entry" /></label>{error && <p role="alert" className="pw-error">{error}</p>}<div className="pw-dialog-actions"><button type="button" onClick={onClose}>Cancel</button><button className="primary" disabled={!name.trim()}>Create collection</button></div></form></WorkspaceDialog>
}

function RenameCollectionDialog({ collection, siblings, onClose, onSave }: { collection: ProjectCollection; siblings: ProjectCollection[]; onClose: () => void; onSave: (name: string) => void }) {
  const [name, setName] = useState(collection.name)
  const [error, setError] = useState('')
  return <WorkspaceDialog title="Rename collection" onClose={onClose}><form onSubmit={event => { event.preventDefault(); const clean = name.trim(); if (siblings.some(item => item.id !== collection.id && item.name.toLocaleLowerCase() === clean.toLocaleLowerCase())) { setError('A collection with this name already exists here.'); return } try { onSave(clean) } catch (caught) { setError(String(caught)) } }}><label className="field">Name<input autoFocus required maxLength={120} value={name} onChange={event => setName(event.target.value)} /></label>{error && <p role="alert" className="pw-error">{error}</p>}<div className="pw-dialog-actions"><button type="button" onClick={onClose}>Cancel</button><button className="primary" disabled={!name.trim()}>Save name</button></div></form></WorkspaceDialog>
}

function FolderContextMenu({ x, y, name, onClose, onOpen, onRename, onCreate, onAddExisting, onRemove }: { x: number; y: number; name: string; onClose: () => void; onOpen: () => void; onRename: () => void; onCreate: () => void; onAddExisting: () => void; onRemove: () => void }) {
  useEffect(() => {
    const closeOnEscape = (event: KeyboardEvent) => { if (event.key === 'Escape') onClose() }
    document.addEventListener('keydown', closeOnEscape)
    return () => document.removeEventListener('keydown', closeOnEscape)
  }, [onClose])
  const run = (action: () => void) => { onClose(); action() }
  const left = Math.min(x, window.innerWidth - 230)
  const top = Math.min(y, window.innerHeight - 300)
  return <div className="pw-context-layer" onClick={onClose} onContextMenu={event => { event.preventDefault(); onClose() }}><div className="pw-context-menu" role="menu" aria-label={`${name} folder settings`} style={{ left, top }} onClick={event => event.stopPropagation()}><strong>Folder settings</strong><button autoFocus role="menuitem" onClick={() => run(onOpen)}>Open</button><button role="menuitem" onClick={() => run(onRename)}>Rename</button><button role="menuitem" onClick={() => run(onCreate)}>New collection inside</button><button role="menuitem" onClick={() => run(onAddExisting)}>Add existing inside</button><button role="menuitem" className="danger" onClick={() => run(onRemove)}>Remove from project</button></div></div>
}

function ProjectContextMenu({ x, y, name, onClose, onOpen, onEdit, onDelete }: { x: number; y: number; name: string; onClose: () => void; onOpen: () => void; onEdit: () => void; onDelete: () => void }) {
  useEffect(() => {
    const closeOnEscape = (event: KeyboardEvent) => { if (event.key === 'Escape') onClose() }
    document.addEventListener('keydown', closeOnEscape)
    return () => document.removeEventListener('keydown', closeOnEscape)
  }, [onClose])
  const run = (action: () => void) => { onClose(); action() }
  const left = Math.min(x, window.innerWidth - 230)
  const top = Math.min(y, window.innerHeight - 190)
  return <div className="pw-context-layer" onClick={onClose} onContextMenu={event => { event.preventDefault(); onClose() }}><div className="pw-context-menu" role="menu" aria-label={`${name} project settings`} style={{ left, top }} onClick={event => event.stopPropagation()}><strong>Project settings</strong><button autoFocus role="menuitem" onClick={() => run(onOpen)}>Open project</button><button role="menuitem" onClick={() => run(onEdit)}>Edit details</button><button role="menuitem" className="danger" onClick={() => run(onDelete)}>Delete project</button></div></div>
}

function LinkDialog({ groups, project, parentId, onSave, onClose }: { groups: Group[]; project: Project; parentId: string | null; onSave: (groups: Group[]) => void; onClose: () => void }) {
  const [ids, setIds] = useState<number[]>([])
  const [error, setError] = useState('')
  const available = groups.filter(group => !project.collections.some(item => item.sources.some(source => source.groupId === group.id && source.shootId === group.shootId)))
  const siblings = childrenOf(project, parentId)
  const selected = available.filter(group => ids.includes(group.id))
  const duplicate = selected.find(group => siblings.some(item => item.name.toLocaleLowerCase() === group.name.toLocaleLowerCase()))
  return <WorkspaceDialog title="Add existing collections" onClose={onClose}><p>Bring existing groups into this location. They remain available in Classic.</p><div className="pw-link-list">{available.map(group => <label key={`${group.shootId}-${group.id}`}><input type="checkbox" checked={ids.includes(group.id)} onChange={event => setIds(event.target.checked ? [...ids, group.id] : ids.filter(id => id !== group.id))} /><span>{group.name}<small>{group.mediaCount} files</small></span></label>)}</div>{available.length === 0 && <p>No unlinked groups yet. Create a collection from the media library.</p>}{duplicate && <p role="alert" className="pw-error">“{duplicate.name}” already exists in this location.</p>}{error && <p role="alert" className="pw-error">{error}</p>}<div className="pw-dialog-actions"><button onClick={onClose}>Cancel</button><button className="primary" disabled={!ids.length || Boolean(duplicate)} onClick={() => { try { onSave(selected) } catch (caught) { setError(String(caught)) } }}>Add {ids.length || ''} collections</button></div></WorkspaceDialog>
}

function CollectionOptions({ project, collection, projects, act, onRemoved }: { project: Project; collection: ProjectCollection; projects: Project[]; act: (projects: Project[]) => void; onRemoved: () => void }) {
  const subtree = new Set([collection.id, ...descendantsOf(project, collection.id).map(item => item.id)])
  return <details className="pw-collection-options"><summary>Collection options</summary><p>Removing this collection also removes {subtree.size - 1} nested collection{subtree.size - 1 === 1 ? '' : 's'} from the project. Media and original groups remain in the library.</p><button onClick={() => { if (window.confirm(`Remove “${collection.name}” and its nested collections from this project? Their media and original groups remain in the library.`)) { act(projects.map(item => item.id === project.id ? { ...item, collections: item.collections.filter(child => !subtree.has(child.id)) } : item)); onRemoved() } }}>Remove from project</button></details>
}

function NoMatches({ onClear }: { onClear: () => void }) {
  return <div className="pw-empty"><h2>No matching collections</h2><button onClick={onClear}>Clear search</button></div>
}

function childrenOf(project: Project, parentId: string | null) {
  return project.collections.filter(item => item.parentId === parentId)
}

function descendantsOf(project: Project, parentId: string): ProjectCollection[] {
  const direct = childrenOf(project, parentId)
  return direct.flatMap(item => [item, ...descendantsOf(project, item.id)])
}

function collectionTrail(project: Project, collection: ProjectCollection) {
  const trail: ProjectCollection[] = []
  const seen = new Set<string>()
  let current: ProjectCollection | undefined = collection
  while (current && !seen.has(current.id)) {
    seen.add(current.id)
    trail.unshift(current)
    current = current.parentId ? project.collections.find(item => item.id === current?.parentId) : undefined
  }
  return trail
}

function matches(value: string, query: string) {
  return value.toLocaleLowerCase().includes(query.trim().toLocaleLowerCase())
}
