import { useEffect, useState, type MouseEvent, type ReactNode } from 'react'
import { useMutation, useQueries, useQuery, useQueryClient } from '@tanstack/react-query'
import type { Group, Media, SmartNode } from '@skwad/shared-types'
import * as api from '../api'
import { thumbUrl } from '../media'
import { useUi } from '../store'
import { addMediaToCollection, removeMediaFromGroup } from './collectionOps'
import { ExportCollectionDialog } from './exportCollection'
import { WorkspaceDialog } from './WorkspaceDialog'
import { RosterImport } from '../components/RosterImport'
import { MediaBrowser } from './mediaBrowser'
import { TagFilter, TagNamesDatalist, TagPicker } from '../components/TagPicker'
import { SmartCollections } from './smartCollections'
import { AddToExistingCollection, PublishCollection } from './publishCollection'
import {
  createProjectDraft,
  PROJECT_TYPES,
  type Project,
  type ProjectCollection,
  type ProjectMember,
  type ProjectVisibility,
} from './model'

type ProjectView = 'personal' | 'shared' | 'organisation' | 'archived' | 'smart'

export function Collections({ projects, save, replaceMembers, loading, saving, projectId, setProjectId, onProcess }: {
  projects: Project[]
  save: (projects: Project[]) => void
  replaceMembers: (projectId: string, members: ProjectMember[]) => Promise<void>
  loading: boolean
  saving: boolean
  projectId: string | null
  setProjectId: (id: string | null) => void
  onProcess: () => void
}) {
  const [view, setView] = useState<ProjectView>('personal')
  const [search, setSearch] = useState('')
  const [creatingProject, setCreatingProject] = useState(false)
  const [creatingCollection, setCreatingCollection] = useState(false)
  const [linking, setLinking] = useState(false)
  const [editingProject, setEditingProject] = useState<Project | null>(null)
  const [sharingProject, setSharingProject] = useState<Project | null>(null)
  const [editingCollection, setEditingCollection] = useState<ProjectCollection | null>(null)
  const [folderMenu, setFolderMenu] = useState<{ collectionId: string; x: number; y: number } | null>(null)
  const [projectMenu, setProjectMenu] = useState<{ projectId: string; x: number; y: number } | null>(null)
  const [collectionId, setCollectionId] = useState<string | null>(null)
  const [exporting, setExporting] = useState<ProjectCollection | null>(null)
  // A smart collection being saved into a project, or added to one.
  const [smartSave, setSmartSave] = useState<Media[] | null>(null)
  const [smartAdd, setSmartAdd] = useState<Media[] | null>(null)
  const [selectedIds, setSelectedIds] = useState<Set<string>>(new Set())
  const clipboard = useUi(state => state.clipboard)
  const setClipboard = useUi(state => state.setClipboard)
  const client = useQueryClient()
  const [pasting, setPasting] = useState(false)
  const shoots = useQuery({ queryKey: ['shoots'], queryFn: api.listShoots })
  const groups = useQueries({ queries: (shoots.data ?? []).map(shoot => ({ queryKey: ['groups', shoot.id], queryFn: () => api.listGroups(shoot.id) })) })
  const allGroups = groups.flatMap(query => query.data ?? [])
  const project = projects.find(item => item.id === projectId)
  const canEdit = project?.accessRole === 'owner' || project?.accessRole === 'editor'
  const collection = project?.collections.find(item => item.id === collectionId)
  const menuCollection = project?.collections.find(item => item.id === folderMenu?.collectionId)
  const menuProject = projects.find(item => item.id === projectMenu?.projectId)
  const currentChildren = project ? childrenOf(project, collection?.id ?? null) : []
  const searchScope = project ? (collection ? descendantsOf(project, collection.id) : project.collections) : []
  const visibleChildren = (search.trim() ? searchScope : currentChildren).filter(item => matches(item.name, search))
  const visibleProjects = projects.filter(item => inView(item, view) && matches(item.name, search))
  const viewProjects = projects.filter(item => inView(item, view))
  const notice = useUi(state => state.pushNotice)

  const act = (next: Project[]) => {
    try { save(next) } catch (error) { notice({ level: 'error', message: String(error) }) }
  }

  // Collections only store {shootId, groupId} pointers into Classic data, and
  // nothing scrubs those pointers when a shoot or group is deleted elsewhere
  // (Media Processing's "Remove indexed data", a re-process that drops a
  // group, or Classic's own group delete). Once shoots and every group list
  // have loaded, prune any source that no longer resolves to a live group so
  // collections don't accumulate permanently empty, dead entries.
  useEffect(() => {
    if (shoots.isPending || shoots.isError || groups.some(query => query.isPending || query.isError)) return
    const liveShootIds = new Set((shoots.data ?? []).map(shoot => shoot.id))
    const liveGroupKeys = new Set(allGroups.map(group => `${group.shootId}:${group.id}`))
    let dirty = false
    const next = projects.map(item => ({
      ...item,
      collections: item.collections.map(collectionItem => {
        const sources = collectionItem.sources.filter(source => liveShootIds.has(source.shootId) && liveGroupKeys.has(`${source.shootId}:${source.groupId}`))
        if (sources.length === collectionItem.sources.length) return collectionItem
        dirty = true
        return { ...collectionItem, sources }
      }),
    }))
    if (dirty) act(next)
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [projects, shoots.data, groups])

  const resolve = (item: ProjectCollection) => item.sources.flatMap(source => allGroups.filter(group => group.id === source.groupId && group.shootId === source.shootId))
  const openProject = (id: string | null) => { setProjectId(id); setCollectionId(null); setSearch(''); setExporting(null); setSelectedIds(new Set()) }
  const openCollection = (id: string | null) => { setCollectionId(id); setSearch(''); setExporting(null); setSelectedIds(new Set()) }
  /** What Paste would do right now, or null when the clipboard is empty. */
  const pasteLabel = !clipboard
    ? null
    : clipboard.kind === 'media'
      ? `Paste ${clipboard.media.length} file${clipboard.media.length === 1 ? '' : 's'}`
      : `Paste “${clipboard.name}”`

  const setClipboardCollection = (item: ProjectCollection, mode: 'cut' | 'copy') => {
    setClipboard({ kind: 'collection', mode, projectId: item.projectId, collectionId: item.id, name: item.name })
    notice({ level: 'info', message: `“${item.name}” ready — right-click another collection and choose Paste.` })
  }

  const selectCard = (id: string, additive: boolean) => setSelectedIds(current => {
    if (!additive) return new Set([id])
    const next = new Set(current)
    if (next.has(id)) next.delete(id); else next.add(id)
    return next
  })

  /**
   * Pastes onto `target`. Media joins the collection; a collection is either
   * re-parented under it (Cut) or duplicated into it (Copy), reusing the same
   * tree helpers the Collection settings dialog uses for a move or duplicate.
   */
  const paste = async (target: ProjectCollection) => {
    if (!clipboard || !project || pasting) return
    setPasting(true)
    try {
      if (clipboard.kind === 'media') {
        const added = await addMediaToCollection(clipboard.media, project, target, projects, act, client)
        if (clipboard.mode === 'cut' && clipboard.source) {
          await removeMediaFromGroup(clipboard.source.groupId, clipboard.media, client)
        }
        notice({ level: 'success', message: added > 0 ? `${added} file${added === 1 ? '' : 's'} ${clipboard.mode === 'cut' ? 'moved' : 'added'} to “${target.name}”.` : `Those files were already in “${target.name}”.` })
        if (clipboard.mode === 'cut') setClipboard(null)
        return
      }

      const sourceProject = projects.find(item => item.id === clipboard.projectId)
      const source = sourceProject?.collections.find(item => item.id === clipboard.collectionId)
      if (!sourceProject || !source) { notice({ level: 'error', message: 'That collection no longer exists.' }); setClipboard(null); return }
      if (source.id === target.id || descendantsOf(sourceProject, source.id).some(item => item.id === target.id)) {
        notice({ level: 'error', message: 'A collection cannot be pasted into itself or its own nested collections.' })
        return
      }

      if (clipboard.mode === 'cut') {
        if (sourceProject.id === project.id) {
          // Same project: re-parenting the subtree root is enough, its
          // descendants stay attached to it.
          act(projects.map(item => item.id === project.id
            ? { ...item, collections: item.collections.map(child => child.id === source.id ? { ...child, parentId: target.id, updatedAt: new Date().toISOString() } : child) }
            : item))
        } else {
          // Across projects every descendant needs a new projectId too, so
          // clone the subtree in and drop the originals — the same move the
          // Collection settings dialog performs.
          const moved = cloneCollectionTree(sourceProject, source, project, target.id, source.name, source.notes)
          const removed = new Set([source.id, ...descendantsOf(sourceProject, source.id).map(item => item.id)])
          act(projects.map(item =>
            item.id === sourceProject.id ? { ...item, collections: item.collections.filter(child => !removed.has(child.id)) }
              : item.id === project.id ? { ...item, collections: [...item.collections, ...moved] }
                : item))
        }
        notice({ level: 'success', message: `“${source.name}” moved into “${target.name}”.` })
        setClipboard(null)
      } else {
        const copies = cloneCollectionTree(sourceProject, source, project, target.id, `${source.name} copy`, source.notes)
        act(projects.map(item => item.id === project.id ? { ...item, collections: [...item.collections, ...copies] } : item))
        notice({ level: 'success', message: `“${source.name}” copied into “${target.name}”.` })
      }
    } catch (error) {
      notice({ level: 'error', message: String(error) })
    } finally {
      setPasting(false)
    }
  }
  const rootCount = project ? childrenOf(project, null).length : 0
  const openProjectMenu = (item: Project, event: MouseEvent<HTMLElement>) => {
    event.preventDefault(); event.stopPropagation()
    const rect = event.currentTarget.getBoundingClientRect()
    setProjectMenu({ projectId: item.id, x: Math.min(rect.right, window.innerWidth - 230), y: rect.bottom + 4 })
  }
  const openFolderMenu = (item: ProjectCollection, event: MouseEvent<HTMLElement>) => {
    event.preventDefault(); event.stopPropagation()
    const rect = event.currentTarget.getBoundingClientRect()
    setFolderMenu({ collectionId: item.id, x: Math.min(rect.right, window.innerWidth - 230), y: rect.bottom + 4 })
  }

  return <>
    {project && <Breadcrumb project={project} collection={collection} onProjects={() => openProject(null)} onCollection={openCollection} />}
    <header className="pw-heading">
      <div><span className="pw-eyebrow">Collections</span><h1>{collection?.name ?? project?.name ?? 'Projects'}</h1><p>{collection ? `${currentChildren.length} collection${currentChildren.length === 1 ? '' : 's'}` : project ? `${project.kind} · ${rootCount} main collection${rootCount === 1 ? '' : 's'}` : 'Every event. Every collection. One place.'}</p></div>
      <div className="actions">
        {saving && <span className="pw-saving" role="status">Saving…</span>}
        {!project && <button className="primary" onClick={() => setCreatingProject(true)}>New project</button>}
        {project && !collection && project.accessRole === 'owner' && <button disabled={saving} onClick={() => setSharingProject(project)}>Share</button>}
        {project && !collection && canEdit && <><button onClick={() => setEditingProject(project)}>Project settings</button><button onClick={() => setCreatingCollection(true)}>New collection</button><button className="primary" onClick={onProcess}>Add media</button></>}
        {project && collection && canEdit && <><button onClick={() => setLinking(true)}>Add existing</button><button className="primary" onClick={() => setCreatingCollection(true)}>New collection</button></>}
      </div>
    </header>

    {(shoots.isError || groups.some(query => query.isError)) && <p role="alert" className="pw-error">Some library data could not be loaded. <button onClick={() => { void shoots.refetch(); groups.forEach(query => void query.refetch()) }}>Retry</button></p>}

    {!project && <>
      <div className="pw-tabs pw-project-tabs" role="tablist" aria-label="Project views">
        {([['organisation', 'Organisation'], ['shared', 'Shared'], ['personal', 'Personal'], ['smart', 'Smart'], ['archived', 'Archived']] as const).map(([id, label]) => <button key={id} role="tab" aria-selected={view === id} onClick={() => { setView(id); setSearch('') }}>{label}<span>{projects.filter(projectItem => inView(projectItem, id)).length}</span></button>)}
      </div>
      {view !== 'smart' && <div className="pw-toolbar"><label className="pw-search"><span className="sr-only">Search projects</span><input type="search" placeholder="Search projects…" value={search} onChange={event => setSearch(event.target.value)} /></label><span>{viewProjects.length} project{viewProjects.length === 1 ? '' : 's'}</span></div>}
      {view === 'smart' && <SmartCollections onCollect={setSmartSave} onAddToExisting={setSmartAdd} />}
      {view !== 'smart' && loading && <p className="pw-loading" role="status">Loading projects…</p>}
      {view !== 'smart' && !loading && <div className="pw-card-grid">{visibleProjects.map(item => {
        const linked = item.collections.flatMap(resolve)
        const roots = childrenOf(item, null)
        return <CollectionCard key={item.id} name={item.name} label={item.kind} mediaId={item.coverMediaId ?? linked.find(group => group.coverMediaId !== null)?.coverMediaId} meta={`${item.mediaCount || linked.reduce((sum, group) => sum + group.mediaCount, 0)} media · ${roots.length} main collection${roots.length === 1 ? '' : 's'}`} detail={`${accessLabel(item)} · ${formatUpdated(item.updatedAt)}`} selected={selectedIds.has(item.id)} onSelect={additive => selectCard(item.id, additive)} onOpen={() => openProject(item.id)} onActions={event => openProjectMenu(item, event)} onContextMenu={event => openProjectMenu(item, event)} />
      })}</div>}
      {view !== 'smart' && !loading && viewProjects.length === 0 && <ProjectEmpty view={view} onCreate={() => setCreatingProject(true)} />}
      {view !== 'smart' && viewProjects.length > 0 && visibleProjects.length === 0 && <NoMatches noun="projects" onClear={() => setSearch('')} />}
      <div className="pw-library-callout"><div><strong>Your media is always available</strong><p>Find a person, revisit processed files, or make another collection in Media Processing.</p></div><button onClick={onProcess}>Open media library</button></div>
    </>}

    {project && <>
      <div className="pw-project-meta"><span className={`pw-access pw-access-${project.visibility}`}>{accessLabel(project)}</span><span>Owner: {project.ownerEmail}</span><span>{project.members.length} member{project.members.length === 1 ? '' : 's'}</span><span>{project.mediaCount} media</span></div>
      {project.accessRole === 'viewer' && <div className="pw-note"><strong>View-only project</strong><p>You can browse and export this project. Ask the owner for Editor access to organise collections.</p></div>}
      {(currentChildren.length > 0 || search.trim()) && <div className="pw-toolbar"><label className="pw-search"><span className="sr-only">Search collections</span><input type="search" placeholder="Search all project collections…" value={search} onChange={event => setSearch(event.target.value)} /></label><span>{search.trim() ? `${visibleChildren.length} results` : `${currentChildren.length} collections`}</span>{!collection && canEdit && <button onClick={() => setLinking(true)}>Add existing collection</button>}</div>}
      {(currentChildren.length > 0 || visibleChildren.length > 0) && <section className="pw-folder-section" aria-labelledby="collection-folders"><h2 id="collection-folders">{search.trim() ? 'Search results' : collection ? 'Collections' : 'Main collections'}</h2><div className="pw-card-grid">{visibleChildren.map(item => {
        const linked = resolve(item)
        const childCount = childrenOf(project, item.id).length
        const fileCount = linked.reduce((total, group) => total + group.mediaCount, 0)
        return <CollectionCard key={item.id} name={item.name} label={childCount > 0 ? 'Collection folder' : 'Collection'} mediaId={linked.find(group => group.coverMediaId !== null)?.coverMediaId} meta={`${fileCount} media`} detail={`${childCount} collection${childCount === 1 ? '' : 's'}`} selected={selectedIds.has(item.id)} onSelect={additive => selectCard(item.id, additive)} onOpen={() => openCollection(item.id)} onActions={canEdit ? event => openFolderMenu(item, event) : undefined} onContextMenu={canEdit ? event => openFolderMenu(item, event) : undefined} />
      })}</div></section>}
      {search.trim() && visibleChildren.length === 0 && <NoMatches noun="collections" onClear={() => setSearch('')} />}
      {!collection && currentChildren.length === 0 && <div className="pw-empty"><h2>Ready for your first collection</h2><p>Select media or find a person in your library, then add their collection here.</p>{canEdit && <div className="actions"><button onClick={() => setLinking(true)}>Add existing collection</button><button className="primary" onClick={onProcess}>Open media library</button></div>}</div>}
      {collection && collection.notes && <p className="pw-collection-notes">{collection.notes}</p>}
      {collection && collection.sources.length > 0 && <section className={currentChildren.length > 0 ? 'pw-media-section' : undefined} aria-labelledby="collection-media"><h2 id="collection-media" className="pw-section-heading">Media in this collection</h2><CollectionMedia key={collection.id} collection={collection} onExport={() => setExporting(collection)} addByTag={canEdit && project ? (media) => addMediaToCollection(media, project, collection, projects, act, client) : undefined} /></section>}
      {collection && collection.sources.length === 0 && currentChildren.length === 0 && <div className="pw-empty"><h2>This collection is empty</h2><p>Create a collection here, add an existing collection from your processed media, or fill it from a tag.</p>{canEdit && <div className="actions"><button onClick={() => setLinking(true)}>Add existing</button><button className="primary" onClick={() => setCreatingCollection(true)}>New collection</button></div>}{canEdit && project && <AddByTag onAdd={(media) => addMediaToCollection(media, project, collection, projects, act, client)} />}</div>}
    </>}

    {smartSave && <PublishCollection media={smartSave} projects={projects} save={act} onClose={() => setSmartSave(null)} onPublished={id => { setSmartSave(null); openProject(id); notice({ level: 'success', message: 'Smart collection saved to the project.' }) }} />}
    {smartAdd && <AddToExistingCollection media={smartAdd} projects={projects} save={act} onClose={() => setSmartAdd(null)} onAdded={(_projectId, _collectionId, added) => { setSmartAdd(null); notice({ level: 'success', message: added > 0 ? `Added ${added} file${added === 1 ? '' : 's'} to the collection.` : 'Those files were already in that collection.' }) }} />}
    {exporting && <ExportCollectionDialog collection={exporting} onClose={() => setExporting(null)} />}

    {folderMenu && project && menuCollection && <FolderContextMenu x={folderMenu.x} y={folderMenu.y} name={menuCollection.name} pasteLabel={pasteLabel} onClose={() => setFolderMenu(null)} onOpen={() => openCollection(menuCollection.id)} onCut={() => setClipboardCollection(menuCollection, 'cut')} onCopy={() => setClipboardCollection(menuCollection, 'copy')} onPaste={() => void paste(menuCollection)} onEdit={() => setEditingCollection(menuCollection)} onCreate={() => { openCollection(menuCollection.id); setCreatingCollection(true) }} onAddExisting={() => { openCollection(menuCollection.id); setLinking(true) }} onSendToPremiere={() => {
      void api.sendCollectionToPremiere(menuCollection.id)
        .then(() => notice({ level: 'success', message: `Sent "${menuCollection.name}" to Premiere — open the panel there to see it land.` }))
        .catch(error => notice({ level: 'error', message: String(error) }))
    }} onRemove={() => removeCollection(project, menuCollection, projects, act, () => openCollection(menuCollection.parentId))} />}
    {projectMenu && menuProject && <ProjectContextMenu x={projectMenu.x} y={projectMenu.y} project={menuProject} onClose={() => setProjectMenu(null)} onOpen={() => openProject(menuProject.id)} onEdit={() => setEditingProject(menuProject)} onShare={() => setSharingProject(menuProject)} onArchive={() => act(projects.map(item => item.id === menuProject.id ? { ...item, status: item.status === 'archived' ? 'active' : 'archived' } : item))} onDelete={() => deleteProject(menuProject, projects, act, () => openProject(null))} />}
    {(creatingProject || editingProject) && <ProjectDialog project={editingProject ?? undefined} canManageAccess={!editingProject || editingProject.accessRole === 'owner'} onClose={() => { setCreatingProject(false); setEditingProject(null) }} onSave={(name, kind, visibility) => {
      const next = editingProject ? { ...editingProject, name, kind, visibility } : createProjectDraft(name, kind, visibility)
      act(editingProject ? projects.map(item => item.id === next.id ? next : item) : [...projects, next])
      setCreatingProject(false); setEditingProject(null); openProject(next.id)
    }} onDelete={editingProject?.accessRole === 'owner' ? () => deleteProject(editingProject, projects, act, () => { setEditingProject(null); openProject(null) }) : undefined} />}
    {sharingProject && <ShareDialog project={sharingProject} busy={saving} onClose={() => setSharingProject(null)} onSave={async members => { await replaceMembers(sharingProject.id, members); setSharingProject(null) }} />}
    {creatingCollection && project && <NewCollectionDialog siblings={currentChildren} onClose={() => setCreatingCollection(false)} onSave={name => {
      const child = newCollection(project.id, name, collection?.id ?? null, currentChildren.length)
      act(projects.map(item => item.id === project.id ? { ...item, collections: [...item.collections, child] } : item)); setCreatingCollection(false); openCollection(child.id)
    }} />}
    {editingCollection && project && <CollectionSettingsDialog project={project} projects={projects} collection={editingCollection} onClose={() => setEditingCollection(null)} onSave={(name, notes, parentId, targetProjectId) => {
      if (targetProjectId === project.id) {
        act(projects.map(item => item.id === project.id ? { ...item, collections: item.collections.map(child => child.id === editingCollection.id ? { ...child, name, notes, parentId } : child) } : item)); setEditingCollection(null); openCollection(editingCollection.id)
        return
      }
      const target = projects.find(item => item.id === targetProjectId)
      if (!target) return
      const moved = cloneCollectionTree(project, editingCollection, target, parentId, name, notes)
      const removed = new Set([editingCollection.id, ...descendantsOf(project, editingCollection.id).map(item => item.id)])
      act(projects.map(item => item.id === project.id ? { ...item, collections: item.collections.filter(child => !removed.has(child.id)) } : item.id === target.id ? { ...item, collections: [...item.collections, ...moved] } : item))
      setEditingCollection(null); setProjectId(target.id); setCollectionId(moved[0].id); setSearch('')
    }} onDuplicate={() => {
      const copies = duplicateCollectionTree(project, editingCollection)
      act(projects.map(item => item.id === project.id ? { ...item, collections: [...item.collections, ...copies] } : item)); setEditingCollection(null)
    }} onRemove={() => removeCollection(project, editingCollection, projects, act, () => { setEditingCollection(null); openCollection(editingCollection.parentId) })} />}
    {linking && project && <LinkDialog groups={allGroups} project={project} parentId={collection?.id ?? null} onClose={() => setLinking(false)} onSave={selected => {
      const siblings = childrenOf(project, collection?.id ?? null)
      const additions = selected.map((group, index) => newCollection(project.id, group.name, collection?.id ?? null, siblings.length + index, [{ shootId: group.shootId, groupId: group.id }]))
      act(projects.map(item => item.id === project.id ? { ...item, collections: [...item.collections, ...additions] } : item)); setLinking(false)
    }} />}
  </>
}

function Breadcrumb({ project, collection, onProjects, onCollection }: { project: Project; collection?: ProjectCollection; onProjects: () => void; onCollection: (id: string | null) => void }) {
  const trail = collection ? collectionTrail(project, collection) : []
  return <nav className="pw-breadcrumb" aria-label="Breadcrumb"><button onClick={onProjects}>Projects</button><span>/</span>{collection ? <><button onClick={() => onCollection(null)}>{project.name}</button>{trail.map((item, index) => <span className="pw-breadcrumb-part" key={item.id}><span>/</span>{index === trail.length - 1 ? <span aria-current="page">{item.name}</span> : <button onClick={() => onCollection(item.id)}>{item.name}</button>}</span>)}</> : <span aria-current="page">{project.name}</span>}</nav>
}

/**
 * Single click selects, double click opens — the same rule as the media
 * grid, so nothing in the workspace opens from one click.
 */
function CollectionCard(props: { name: string; label: string; mediaId?: number | null; meta: string; detail: string; selected?: boolean; onOpen: () => void; onSelect?: (additive: boolean) => void; onActions?: (event: MouseEvent<HTMLButtonElement>) => void; onContextMenu?: (event: MouseEvent<HTMLElement>) => void }) {
  return <article className={`pw-cover-card${props.selected ? ' selected' : ''}`} onContextMenu={props.onContextMenu}><button
    className="pw-card-open"
    onClick={event => props.onSelect?.(event.ctrlKey || event.metaKey)}
    onDoubleClick={props.onOpen}
    onKeyDown={event => { if (event.key === 'Enter') { event.preventDefault(); props.onOpen() } }}
  ><Cover mediaId={props.mediaId} label={props.label} /><div className="pw-card-body"><h2>{props.name}</h2><p>{props.meta}<span>{props.detail}</span></p></div></button>{props.onActions && <button className="pw-card-actions" aria-label={`Actions for ${props.name}`} onClick={props.onActions}>Actions</button>}</article>
}

function Cover({ mediaId, label }: { mediaId?: number | null; label: string }) {
  const [failed, setFailed] = useState(false)
  return <div className="pw-cover">{mediaId != null && !failed ? <img src={thumbUrl(mediaId)} alt="" loading="lazy" onError={() => setFailed(true)} /> : <span>{label}</span>}</div>
}

function CollectionMedia({ collection, onExport, addByTag }: { collection: ProjectCollection; onExport: () => void; addByTag?: (media: Media[]) => Promise<number> }) {
  const [index, setIndex] = useState(0)
  const source = collection.sources[index]
  const shoots = useQuery({ queryKey: ['shoots'], queryFn: api.listShoots })
  if (!source) return null
  // The source picker only chooses what to *browse*; exporting always takes
  // the whole collection, which is what "export this collection" means to
  // someone handing the folder on.
  return <><div className="pw-toolbar">{collection.sources.length > 1 && <label>Media source <select value={index} onChange={event => setIndex(Number(event.target.value))}>{collection.sources.map((item, sourceIndex) => <option key={`${item.shootId}-${item.groupId}`} value={sourceIndex}>{shoots.data?.find(shoot => shoot.id === item.shootId)?.name ?? `Source ${sourceIndex + 1}`}</option>)}</select></label>}<button className="primary" onClick={onExport}>Export collection</button></div><div className="card collection-tags">{addByTag && <AddByTag onAdd={addByTag} />}<TagNamesDatalist /><TagPicker kind="collection" assetKey={collection.id} compact label="Collection tags" /><TagsInCollection groupId={source.groupId} /></div><MediaBrowser key={`${source.shootId}-${source.groupId}`} shootId={source.shootId} groupId={source.groupId} /></>
}

function ProjectDialog({ project, canManageAccess = true, onClose, onSave, onDelete }: { project?: Project; canManageAccess?: boolean; onClose: () => void; onSave: (name: string, kind: string, visibility: ProjectVisibility) => void; onDelete?: () => void }) {
  const [name, setName] = useState(project?.name ?? '')
  const [kind, setKind] = useState(project?.kind ?? 'Esports tournament')
  const [visibility, setVisibility] = useState<ProjectVisibility>(project?.visibility ?? 'private')
  return <WorkspaceDialog title={project ? 'Project settings' : 'New project'} onClose={onClose}><form onSubmit={event => { event.preventDefault(); onSave(name.trim(), kind, visibility) }}><label className="field">Project name<input autoFocus required maxLength={120} value={name} onChange={event => setName(event.target.value)} placeholder="e.g. BGIS 2026" /></label><label className="field">Project type<select value={kind} onChange={event => setKind(event.target.value)}>{PROJECT_TYPES.map(item => <option key={item}>{item}</option>)}</select></label>{canManageAccess ? <label className="field">Access<select value={visibility} onChange={event => setVisibility(event.target.value as ProjectVisibility)}><option value="private">Private · only you</option><option value="invited">Invited people</option><option value="organisation">Everyone in your organisation</option></select></label> : <p className="pw-help">Only the project owner can change access or delete this project.</p>}{!project && <><p className="pw-help">The project starts empty. Add collections as you go, or publish processed media straight into it.</p><details className="pw-autoteam"><summary>Auto team-up &mdash; optional</summary><RosterImport compact /></details></>}<div className="pw-dialog-actions">{onDelete && <button type="button" className="danger pw-delete-project" onClick={onDelete}>Delete project</button>}<button type="button" onClick={onClose}>Cancel</button><button className="primary" disabled={!name.trim()}>{project ? 'Save changes' : 'Create project'}</button></div></form></WorkspaceDialog>
}

function ShareDialog({ project, busy, onClose, onSave }: { project: Project; busy: boolean; onClose: () => void; onSave: (members: ProjectMember[]) => Promise<void> }) {
  const [members, setMembers] = useState(project.members)
  const [email, setEmail] = useState('')
  const [role, setRole] = useState<'editor' | 'viewer'>('viewer')
  const [error, setError] = useState('')
  const add = () => {
    const clean = email.trim().toLowerCase()
    if (!clean.includes('@')) { setError('Enter a valid email address.'); return }
    if (clean === project.ownerEmail.toLowerCase()) { setError('The owner already has full access.'); return }
    setMembers(current => [...current.filter(member => member.email.toLowerCase() !== clean), { email: clean, displayName: null, role, invitationState: 'invited' }])
    setEmail(''); setError('')
  }
  return <WorkspaceDialog title={`Share ${project.name}`} onClose={onClose}><p>Invite people who use this SKWAD workspace. Editors can organise collections; viewers can browse and export.</p><div className="pw-share-add"><label className="field">Email<input type="email" value={email} onChange={event => setEmail(event.target.value)} placeholder="name@organisation.com" /></label><label className="field">Role<select value={role} onChange={event => setRole(event.target.value as 'editor' | 'viewer')}><option value="viewer">Viewer</option><option value="editor">Editor</option></select></label><button type="button" onClick={add}>Add</button></div><div className="pw-member-list">{members.map(member => <div key={member.email}><span><strong>{member.email}</strong><small>{member.invitationState}</small></span><select aria-label={`Role for ${member.email}`} value={member.role} onChange={event => setMembers(current => current.map(item => item.email === member.email ? { ...item, role: event.target.value as 'editor' | 'viewer' } : item))}><option value="viewer">Viewer</option><option value="editor">Editor</option></select><button type="button" onClick={() => setMembers(current => current.filter(item => item.email !== member.email))}>Remove</button></div>)}{members.length === 0 && <p className="pw-help">No invited members yet.</p>}</div>{error && <p role="alert" className="pw-error">{error}</p>}<div className="pw-dialog-actions"><button type="button" onClick={onClose}>Cancel</button><button className="primary" disabled={busy} onClick={() => void onSave(members)}>{busy ? 'Saving…' : 'Save access'}</button></div></WorkspaceDialog>
}

function NewCollectionDialog({ siblings, onClose, onSave }: { siblings: ProjectCollection[]; onClose: () => void; onSave: (name: string) => void }) {
  const [name, setName] = useState('')
  const [error, setError] = useState('')
  return <WorkspaceDialog title="New collection" onClose={onClose}><form onSubmit={event => { event.preventDefault(); const clean = name.trim(); if (siblings.some(item => item.name.toLocaleLowerCase() === clean.toLocaleLowerCase())) { setError('A collection with this name already exists here.'); return } onSave(clean) }}><p>Create it inside the current collection. You can add more levels later.</p><label className="field">Name<input autoFocus required maxLength={120} value={name} onChange={event => setName(event.target.value)} placeholder="e.g. Team Entry" /></label>{error && <p role="alert" className="pw-error">{error}</p>}<div className="pw-dialog-actions"><button type="button" onClick={onClose}>Cancel</button><button className="primary" disabled={!name.trim()}>Create collection</button></div></form></WorkspaceDialog>
}

function CollectionSettingsDialog({ project, projects, collection, onClose, onSave, onDuplicate, onRemove }: { project: Project; projects: Project[]; collection: ProjectCollection; onClose: () => void; onSave: (name: string, notes: string | null, parentId: string | null, projectId: string) => void; onDuplicate: () => void; onRemove: () => void }) {
  const [name, setName] = useState(collection.name)
  const [notes, setNotes] = useState(collection.notes ?? '')
  const [targetProjectId, setTargetProjectId] = useState(project.id)
  const [parentId, setParentId] = useState(collection.parentId ?? 'root')
  const [error, setError] = useState('')
  const destination = projects.find(item => item.id === targetProjectId) ?? project
  const blocked = new Set(targetProjectId === project.id ? [collection.id, ...descendantsOf(project, collection.id).map(item => item.id)] : [])
  const targetParent = parentId === 'root' ? null : parentId
  const siblings = childrenOf(destination, targetParent)
  const editableProjects = projects.filter(item => item.status === 'active' && item.accessRole !== 'viewer')
  return <WorkspaceDialog title="Collection settings" onClose={onClose}><form onSubmit={event => { event.preventDefault(); const clean = name.trim(); if (siblings.some(item => item.id !== collection.id && item.name.toLocaleLowerCase() === clean.toLocaleLowerCase())) { setError('A collection with this name already exists in that location.'); return } onSave(clean, notes.trim() || null, targetParent, targetProjectId) }}><label className="field">Name<input autoFocus required maxLength={120} value={name} onChange={event => setName(event.target.value)} /></label><label className="field">Project<select value={targetProjectId} onChange={event => { setTargetProjectId(event.target.value); setParentId('root'); setError('') }}>{editableProjects.map(item => <option key={item.id} value={item.id}>{item.name}</option>)}</select></label><label className="field">Location<select value={parentId} onChange={event => setParentId(event.target.value)}><option value="root">Project root</option>{destination.collections.filter(item => !blocked.has(item.id)).map(item => <option key={item.id} value={item.id}>{`${'— '.repeat(collectionTrail(destination, item).length)}${item.name}`}</option>)}</select></label>{targetProjectId !== project.id && <p className="pw-help">Saving moves this collection and everything inside it to {destination.name}.</p>}<label className="field">Notes<textarea maxLength={500} value={notes} onChange={event => setNotes(event.target.value)} placeholder="Purpose, deliverable, sponsor, or editing notes" /></label>{error && <p role="alert" className="pw-error">{error}</p>}<div className="pw-dialog-actions"><button type="button" className="danger pw-delete-project" onClick={onRemove}>Remove collection</button><button type="button" onClick={onDuplicate}>Duplicate</button><button type="button" onClick={onClose}>Cancel</button><button className="primary" disabled={!name.trim()}>Save changes</button></div></form></WorkspaceDialog>
}

function FolderContextMenu(props: { x: number; y: number; name: string; pasteLabel: string | null; onClose: () => void; onOpen: () => void; onEdit: () => void; onCreate: () => void; onAddExisting: () => void; onCut: () => void; onCopy: () => void; onPaste: () => void; onSendToPremiere: () => void; onRemove: () => void }) {
  useCloseMenu(props.onClose)
  const run = (action: () => void) => { props.onClose(); action() }
  return <MenuLayer x={props.x} y={props.y} label={`${props.name} collection actions`} onClose={props.onClose}><strong>Collection actions</strong><button autoFocus role="menuitem" onClick={() => run(props.onOpen)}>Open</button><button role="menuitem" onClick={() => run(props.onCut)}>Cut</button><button role="menuitem" onClick={() => run(props.onCopy)}>Copy</button><button role="menuitem" disabled={!props.pasteLabel} onClick={() => run(props.onPaste)}>{props.pasteLabel ?? 'Paste'}</button><button role="menuitem" onClick={() => run(props.onEdit)}>Settings</button><button role="menuitem" onClick={() => run(props.onCreate)}>New collection inside</button><button role="menuitem" onClick={() => run(props.onAddExisting)}>Add existing inside</button><button role="menuitem" onClick={() => run(props.onSendToPremiere)}>Send to Premiere</button><button role="menuitem" className="danger" onClick={() => run(props.onRemove)}>Remove from project</button></MenuLayer>
}

function ProjectContextMenu(props: { x: number; y: number; project: Project; onClose: () => void; onOpen: () => void; onEdit: () => void; onShare: () => void; onArchive: () => void; onDelete: () => void }) {
  useCloseMenu(props.onClose)
  const run = (action: () => void) => { props.onClose(); action() }
  const owner = props.project.accessRole === 'owner'
  return <MenuLayer x={props.x} y={props.y} label={`${props.project.name} project actions`} onClose={props.onClose}><strong>Project actions</strong><button autoFocus role="menuitem" onClick={() => run(props.onOpen)}>Open project</button>{props.project.accessRole !== 'viewer' && <button role="menuitem" onClick={() => run(props.onEdit)}>Edit details</button>}{owner && <button role="menuitem" onClick={() => run(props.onShare)}>Manage access</button>}{owner && <button role="menuitem" onClick={() => run(props.onArchive)}>{props.project.status === 'archived' ? 'Restore project' : 'Archive project'}</button>}{owner && <button role="menuitem" className="danger" onClick={() => run(props.onDelete)}>Delete project</button>}</MenuLayer>
}

function MenuLayer({ x, y, label, onClose, children }: { x: number; y: number; label: string; onClose: () => void; children: ReactNode }) {
  const left = Math.min(x, window.innerWidth - 230)
  const top = Math.min(y, window.innerHeight - 240)
  return <div className="pw-context-layer" onClick={onClose} onContextMenu={event => { event.preventDefault(); onClose() }}><div className="pw-context-menu" role="menu" aria-label={label} style={{ left, top }} onClick={event => event.stopPropagation()}>{children}</div></div>
}

function useCloseMenu(onClose: () => void) {
  useEffect(() => { const close = (event: KeyboardEvent) => { if (event.key === 'Escape') onClose() }; document.addEventListener('keydown', close); return () => document.removeEventListener('keydown', close) }, [onClose])
}

function LinkDialog({ groups, project, parentId, onSave, onClose }: { groups: Group[]; project: Project; parentId: string | null; onSave: (groups: Group[]) => void; onClose: () => void }) {
  const [ids, setIds] = useState<number[]>([])
  const available = groups.filter(group => !project.collections.some(item => item.sources.some(source => source.groupId === group.id && source.shootId === group.shootId)))
  const siblings = childrenOf(project, parentId)
  const selected = available.filter(group => ids.includes(group.id))
  const duplicate = selected.find(group => siblings.some(item => item.name.toLocaleLowerCase() === group.name.toLocaleLowerCase()))
  return <WorkspaceDialog title="Add existing collections" onClose={onClose}><p>Bring existing groups into this location. They remain available in Classic.</p><div className="pw-link-list">{available.map(group => <label key={`${group.shootId}-${group.id}`}><input type="checkbox" checked={ids.includes(group.id)} onChange={event => setIds(event.target.checked ? [...ids, group.id] : ids.filter(id => id !== group.id))} /><span>{group.name}<small>{group.mediaCount} media</small></span></label>)}</div>{available.length === 0 && <p>No unlinked groups yet. Create a collection from the media library.</p>}{duplicate && <p role="alert" className="pw-error">“{duplicate.name}” already exists in this location.</p>}<div className="pw-dialog-actions"><button onClick={onClose}>Cancel</button><button className="primary" disabled={!ids.length || Boolean(duplicate)} onClick={() => onSave(selected)}>Add {ids.length || ''} collections</button></div></WorkspaceDialog>
}

function ProjectEmpty({ view, onCreate }: { view: ProjectView; onCreate: () => void }) {
  if (view === 'personal') return <div className="pw-empty"><span className="pw-eyebrow">A home for every event</span><h2>Start with a project</h2><p>A tournament, wedding, or anything you are working on.</p><button className="primary" onClick={onCreate}>Create your first project</button></div>
  if (view === 'shared') return <div className="pw-empty"><h2>No projects shared with you</h2><p>Projects appear here when an owner invites your account email.</p></div>
  if (view === 'organisation') return <div className="pw-empty"><h2>No organisation projects</h2><p>Projects published to your organisation appear here.</p></div>
  return <div className="pw-empty"><h2>No archived projects</h2><p>Archived projects stay available without crowding active work.</p></div>
}

function NoMatches({ noun, onClear }: { noun: string; onClear: () => void }) { return <div className="pw-empty"><h2>No matching {noun}</h2><button onClick={onClear}>Clear search</button></div> }
function childrenOf(project: Project, parentId: string | null) { return project.collections.filter(item => item.parentId === parentId).sort((a, b) => a.sortOrder - b.sortOrder || a.name.localeCompare(b.name)) }
function descendantsOf(project: Project, parentId: string): ProjectCollection[] { const direct = childrenOf(project, parentId); return direct.flatMap(item => [item, ...descendantsOf(project, item.id)]) }
function collectionTrail(project: Project, collection: ProjectCollection) { const trail: ProjectCollection[] = []; const seen = new Set<string>(); let current: ProjectCollection | undefined = collection; while (current && !seen.has(current.id)) { seen.add(current.id); trail.unshift(current); current = current.parentId ? project.collections.find(item => item.id === current?.parentId) : undefined } return trail }
function matches(value: string, query: string) { return value.toLocaleLowerCase().includes(query.trim().toLocaleLowerCase()) }
function inView(project: Project, view: ProjectView) { if (view === 'archived') return project.status === 'archived' && project.accessRole === 'owner'; if (project.status === 'archived') return false; if (project.visibility === 'organisation') return view === 'organisation'; if (view === 'personal') return project.accessRole === 'owner'; if (view === 'shared') return project.accessRole !== 'owner'; return false }
function accessLabel(project: Project) { if (project.visibility === 'organisation') return 'Organisation'; if (project.accessRole !== 'owner') return `Shared · ${project.accessRole}`; if (project.visibility === 'invited') return 'Invited people'; return 'Private' }
function formatUpdated(value: string) { const time = new Date(value); return Number.isNaN(time.getTime()) ? 'Recently updated' : `Updated ${time.toLocaleDateString(undefined, { day: 'numeric', month: 'short' })}` }
function newCollection(projectId: string, name: string, parentId: string | null, sortOrder: number, sources: ProjectCollection['sources'] = []): ProjectCollection { const stamp = new Date().toISOString(); return { id: crypto.randomUUID(), projectId, name, parentId, notes: null, sortOrder, sources, createdAt: stamp, updatedAt: stamp } }
function duplicateCollectionTree(project: Project, collection: ProjectCollection): ProjectCollection[] { const originals = [collection, ...descendantsOf(project, collection.id)]; const ids = new Map(originals.map(item => [item.id, crypto.randomUUID()])); const siblings = childrenOf(project, collection.parentId); const base = `${collection.name} copy`; let name = base; let index = 2; while (siblings.some(item => item.name.toLocaleLowerCase() === name.toLocaleLowerCase())) name = `${base} ${index++}`; const stamp = new Date().toISOString(); return originals.map((item, itemIndex) => ({ ...item, id: ids.get(item.id)!, name: itemIndex === 0 ? name : item.name, parentId: itemIndex === 0 ? item.parentId : ids.get(item.parentId!)!, sortOrder: itemIndex === 0 ? siblings.length : item.sortOrder, createdAt: stamp, updatedAt: stamp })) }
function cloneCollectionTree(source: Project, collection: ProjectCollection, destination: Project, parentId: string | null, name: string, notes: string | null): ProjectCollection[] { const originals = [collection, ...descendantsOf(source, collection.id)]; const ids = new Map(originals.map(item => [item.id, crypto.randomUUID()])); const stamp = new Date().toISOString(); return originals.map((item, index) => ({ ...item, id: ids.get(item.id)!, projectId: destination.id, name: index === 0 ? name : item.name, notes: index === 0 ? notes : item.notes, parentId: index === 0 ? parentId : ids.get(item.parentId!)!, sortOrder: index === 0 ? childrenOf(destination, parentId).length : item.sortOrder, createdAt: stamp, updatedAt: stamp })) }
function deleteProject(project: Project, projects: Project[], act: (projects: Project[]) => void, after: () => void) { if (window.confirm(`Delete the project “${project.name}”? Its media, imports, and original groups will remain available.`)) { act(projects.filter(item => item.id !== project.id)); after() } }
function removeCollection(project: Project, collection: ProjectCollection, projects: Project[], act: (projects: Project[]) => void, after: () => void) { const subtree = new Set([collection.id, ...descendantsOf(project, collection.id).map(item => item.id)]); if (window.confirm(`Remove “${collection.name}” and its nested collections from this project? Their media and original groups remain in the library.`)) { act(projects.map(item => item.id === project.id ? { ...item, collections: item.collections.filter(child => !subtree.has(child.id)) } : item)); after() } }

/**
 * Fills a collection from a tag: choose Tag and Value, see how many files
 * carry it, add them all. The files join the collection's source group for
 * their own shoot, exactly as a selection would.
 */
function AddByTag({ onAdd }: { onAdd: (media: Media[]) => Promise<number> }) {
  const [open, setOpen] = useState(false)
  const [filter, setFilter] = useState<{ tag: string; value: string }>({ tag: '', value: '' })
  const notice = useUi(state => state.pushNotice)
  const matching = useQuery({ queryKey: ['mediaWithTag', filter], queryFn: () => api.mediaWithTag(filter.tag || null, filter.value), enabled: open && Boolean(filter.value) })
  const add = useMutation({
    mutationFn: async () => onAdd(matching.data ?? []),
    onSuccess: (added: number) => { notice({ level: 'success', message: added > 0 ? `Added ${added} file${added === 1 ? '' : 's'} tagged ${filter.tag ? `${filter.tag}: ` : ''}${filter.value}.` : 'Every file with that tag was already in this collection.' }); setOpen(false) },
    onError: (error: unknown) => notice({ level: 'error', message: String(error) }),
  })
  if (!open) return <button onClick={() => setOpen(true)}>Add media by tag…</button>
  return <div className="pw-toolbar pw-add-by-tag"><TagFilter tag={filter.tag} value={filter.value} onChange={setFilter} compact />{filter.value && <span className="hint">{matching.isPending ? 'Counting…' : `${matching.data?.length ?? 0} file${(matching.data?.length ?? 0) === 1 ? '' : 's'} carry it`}</span>}<button className="primary" disabled={!filter.value || add.isPending || (matching.data?.length ?? 0) === 0} onClick={() => add.mutate()}>{add.isPending ? 'Adding…' : 'Add them'}</button><button onClick={() => setOpen(false)}>Cancel</button></div>
}

/**
 * The tags found on a collection's files, with counts — the taxonomy as it
 * applies to what is actually in here, whether it arrived through a tagged
 * group on Auto tags or a tag put on files directly.
 */
function TagsInCollection({ groupId }: { groupId: number }) {
  const tags = useQuery({ queryKey: ['tagsInGroup', groupId], queryFn: () => api.tagsInGroup(groupId) })
  const byTag = new Map<string, SmartNode[]>()
  for (const node of tags.data ?? []) byTag.set(node.tag, [...(byTag.get(node.tag) ?? []), node])
  if (tags.isPending || byTag.size === 0) return null
  return <div className="tags-in-collection"><span className="tag-picker-label">Tags on these files</span><div className="tag-chips">{[...byTag.entries()].map(([tag, nodes]) => <span key={tag} className="tag-group"><span className="tag-group-name">{tag}</span>{nodes.map(node => <span key={node.value} className="tag-chip" title={`${node.mediaCount} file${node.mediaCount === 1 ? '' : 's'}`}>{node.value}<small>{node.mediaCount}</small></span>)}</span>)}</div></div>
}
