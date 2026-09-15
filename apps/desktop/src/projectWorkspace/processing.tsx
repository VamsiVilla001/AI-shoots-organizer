import { useEffect, useState, type MouseEvent } from 'react'
import { useQuery, useQueryClient } from '@tanstack/react-query'
import { open } from '@tauri-apps/plugin-dialog'
import type { Album, Media, ShootSummary } from '@skwad/shared-types'
import * as api from '../api'
import { thumbUrl } from '../media'
import { useUi } from '../store'
import { ProgressPanel } from '../components/ProgressPanel'
import { AlbumsScreen } from '../screens/AlbumsScreen'
import { ReviewScreen } from '../screens/ReviewScreen'
import { PlayersScreen } from '../screens/PlayersScreen'
import { GroupsScreen } from '../screens/GroupsScreen'
import { ExportScreen } from '../screens/ExportScreen'
import { MediaBrowser } from './mediaBrowser'
import { PublishCollection } from './publishCollection'
import { WorkspaceDialog } from './WorkspaceDialog'
import type { Project } from './model'
import { TaggedMedia } from './taggedMedia'
import { PreProcess } from './preProcess'
import { ANALYSIS_REFRESH_KEYS, invalidateKeys } from '../queryKeys'

export function Processing({ projects, save, onPublished }: { projects: Project[]; save: (p: Project[]) => void; onPublished: (id: string) => void }) {
  const [tab, setTab] = useState('library')
  const [source, setSource] = useState<number | null>(null)
  const [showAllMedia, setShowAllMedia] = useState(false)
  const [collectionSearch, setCollectionSearch] = useState('')
  const [importing, setImporting] = useState(false)
  const [selected, setSelected] = useState<Media[] | null>(null)
  const [autoTagSource, setAutoTagSource] = useState<number | null>(null)
  const [reviewingAutoTags, setReviewingAutoTags] = useState(false)
  const [managingPeople, setManagingPeople] = useState(false)
  const [sourceMenu, setSourceMenu] = useState<{ shootId: number; x: number; y: number } | null>(null)
  const [renamingSource, setRenamingSource] = useState<ShootSummary | null>(null)
  const [busySourceId, setBusySourceId] = useState<number | null>(null)
  const [tool, setTool] = useState<'albums' | 'review' | 'players' | 'groups' | null>(null)
  const queryClient = useQueryClient()
  const pushNotice = useUi(state => state.pushNotice)
  const shoots = useQuery({ queryKey: ['shoots'], queryFn: api.listShoots })
  const activeJobs = (shoots.data ?? []).filter(shoot => shoot.status !== 'completed')
  const processedJobs = (shoots.data ?? []).filter(shoot => shoot.status === 'completed')
  const screen = useUi(s => s.screen)
  const activeTool = screen === 'albums' || screen === 'review' || screen === 'players' || screen === 'groups' ? screen : tool
  const goTool = (next: typeof tool) => { if (source !== null) useUi.getState().openShoot(source, next ?? 'albums'); setTool(next) }
  const menuSource = shoots.data?.find(shoot => shoot.id === sourceMenu?.shootId)
  const openSourceMenu = (shoot: ShootSummary, event: MouseEvent<HTMLElement>) => {
    event.preventDefault(); event.stopPropagation()
    const rect = event.currentTarget.getBoundingClientRect()
    const fromPointer = event.type === 'contextmenu'
    setSourceMenu({ shootId: shoot.id, x: fromPointer ? event.clientX : rect.right, y: fromPointer ? event.clientY : rect.bottom + 4 })
  }
  const refreshSources = async () => { await queryClient.invalidateQueries({ queryKey: ['shoots'] }) }
  const refreshAnalysis = async () => {
    await invalidateKeys(queryClient, ANALYSIS_REFRESH_KEYS)
  }
  const reprocessSource = async (shoot: ShootSummary) => {
    if (!window.confirm(`Re-process “${shoot.name}”?\n\nSKWAD will replace detected faces, face matches, unknown groups, and AI albums. Named people, manual collections, project folders, and original media files remain.`)) return
    setBusySourceId(shoot.id)
    try {
      const queued = await api.reanalyseShoot(shoot.id)
      await refreshAnalysis()
      pushNotice({ level: 'success', message: queued > 0 ? `Re-processing ${queued} file${queued === 1 ? '' : 's'} in ${shoot.name}.` : `${shoot.name} has no files to re-process.` })
    } catch (error) {
      pushNotice({ level: 'error', message: String(error) })
    } finally {
      setBusySourceId(null)
    }
  }
  const removeSource = async (shoot: ShootSummary) => {
    if (!window.confirm(`Remove “${shoot.name}” from Media Processing?\n\nIts index, analysis, generated previews, and links inside project collections will be removed. Project folders and original photos and videos are not touched.`)) return
    setBusySourceId(shoot.id)
    try {
      await api.deleteShootIndex(shoot.id)
      await refreshSources()
      pushNotice({ level: 'success', message: `${shoot.name} was removed. Original files were not touched.` })
    } catch (error) {
      pushNotice({ level: 'error', message: String(error) })
    } finally {
      setBusySourceId(null)
    }
  }
  const collectAlbum = async (album: Album) => {
    try {
      const media: Media[] = []
      const pageSize = 1000
      for (let offset = 0; ; offset += pageSize) {
        const page = await api.listMedia({ shootId: album.shootId, albumId: album.id, offset, limit: pageSize })
        media.push(...page)
        if (page.length < pageSize) break
      }
      if (media.length === 0) throw new Error('This automatic tag has no media to add.')
      setSelected(media)
    } catch (error) {
      useUi.getState().pushNotice({ level: 'error', message: String(error) })
    }
  }
  return <>
    <header className="pw-heading pw-processing-heading"><div><span className="pw-eyebrow">Your reusable library</span><h1>Media Processing</h1><p>Import once. Find people. Create as many collections as you need.</p></div><button className="primary" onClick={() => setImporting(true)}>Add media</button></header>
    {activeJobs.length > 0 && <section className="pw-live-processing" aria-labelledby="live-processing-title"><div className="pw-live-title"><div><span className="pw-eyebrow">Live</span><h2 id="live-processing-title">Processing now</h2></div><span>{activeJobs.length} active</span></div><div className="pw-jobs">{activeJobs.map(shoot => <JobCard key={shoot.id} shoot={shoot} live onOpen={() => { setSource(shoot.id); setTab('library'); setTool(null) }} onActions={event => openSourceMenu(shoot, event)} />)}</div></section>}
    <div className="pw-tabs" aria-label="Media Processing views"><button aria-pressed={tab === 'library'} onClick={() => { setTab('library'); setTool(null) }}>Media library</button><button aria-pressed={tab === 'jobs'} onClick={() => { setTab('jobs'); setTool(null) }}>Processed jobs</button><button aria-pressed={tab === 'tags'} onClick={() => { setTab('tags'); setTool(null) }}>Tag media</button><button aria-pressed={tab === 'auto-tags'} onClick={() => { setTab('auto-tags'); setTool(null) }}>Auto tags</button><button aria-pressed={tab === 'pre-process'} onClick={() => { setTab('pre-process'); setTool(null) }}>Pre-Process</button></div>
    {shoots.isError && <p role="alert" className="pw-error">Media sources could not be loaded. <button onClick={() => void shoots.refetch()}>Retry</button></p>}
    {tab === 'library' && <>
      {source === null && !showAllMedia ? <>
        <div className="pw-toolbar"><label className="pw-search"><span className="sr-only">Search collections</span><input type="search" placeholder="Search collections…" value={collectionSearch} onChange={event => setCollectionSearch(event.target.value)} /></label><span>{shoots.data?.length ?? 0} collections</span><button onClick={() => setShowAllMedia(true)}>Search all media</button></div>
        <p className="pw-help">Each collection is one imported media folder. Its name is the Name you entered when adding media.</p>
        {shoots.isPending ? <p role="status" className="pw-loading">Loading collections…</p> : <div className="pw-card-grid">{(shoots.data ?? []).filter(shoot => shoot.name.toLowerCase().includes(collectionSearch.trim().toLowerCase())).map(shoot => <ImportCollectionCard key={shoot.id} shoot={shoot} onOpen={() => { setSource(shoot.id); setTool(null) }} onActions={event => openSourceMenu(shoot, event)} />)}</div>}
        {shoots.data?.length === 0 && <div className="pw-empty"><h2>No media collections yet</h2><p>Add a folder to create your first reusable media collection.</p><button className="primary" onClick={() => setImporting(true)}>Add media</button></div>}
        {(shoots.data?.length ?? 0) > 0 && !(shoots.data ?? []).some(shoot => shoot.name.toLowerCase().includes(collectionSearch.trim().toLowerCase())) && <div className="pw-empty"><h2>No matching collections</h2><button onClick={() => setCollectionSearch('')}>Clear search</button></div>}
      </> : <>
        <div className="pw-breadcrumb"><button onClick={() => { setSource(null); setShowAllMedia(false); setTool(null) }}>Media library</button><span>/</span><span>{source === null ? 'All media' : shoots.data?.find(shoot => shoot.id === source)?.name ?? 'Collection'}</span></div>
        <div className="pw-toolbar"><strong>{source === null ? 'All processed media' : shoots.data?.find(shoot => shoot.id === source)?.name}</strong><span className="pw-muted">Processed media stays available here</span>{source !== null && <button onClick={() => goTool('albums')}>Identify & organise</button>}</div>
        {source === null && <p className="pw-help">Search every imported collection together to find a person or missing media.</p>}
      {tool && source !== null ? <><div className="pw-toolbar"><button onClick={() => setTool(null)}>Back to media</button><label>Organising tools <select value={activeTool ?? tool} onChange={e => goTool(e.target.value as typeof tool)}><option value="albums">Sampled faces & AI suggestions</option><option value="review">Review face matches</option><option value="players">Manage people</option><option value="groups">Manual grouping</option></select></label></div><div className="pw-existing" key={`${source}-${activeTool}`}>
        {screen === 'export' ? <><button onClick={() => goTool(tool)}>Back to organising</button><ExportScreen /></> : activeTool === 'albums' ? <AlbumsScreen /> : activeTool === 'review' ? <ReviewScreen /> : activeTool === 'players' ? <PlayersScreen /> : <GroupsScreen />}
      </div></> : <MediaBrowser key={source ?? 'all'} shootId={source ?? undefined} onCollect={setSelected} />}
      </>}
    </>}
    {tab === 'jobs' && <>
      {shoots.isPending && <p role="status">Loading processed jobs…</p>}
      {!shoots.isPending && processedJobs.length === 0 && <div className="pw-empty"><h2>No processed jobs yet</h2><p>Completed imports will appear here automatically.</p><button className="primary" onClick={() => setImporting(true)}>Add media</button></div>}
      <div className="pw-jobs">{processedJobs.map(shoot => <JobCard key={shoot.id} shoot={shoot} onOpen={() => { setSource(shoot.id); setTab('library'); setTool(null) }} onActions={event => openSourceMenu(shoot, event)} />)}</div>
    </>}
    {tab === 'tags' && (managingPeople ? <><nav className="pw-breadcrumb" aria-label="Breadcrumb"><button onClick={() => setManagingPeople(false)}>Tag media</button><span>/</span><span aria-current="page">Manage people</span></nav><div className="pw-existing"><PlayersScreen /></div></> : <TaggedMedia onCollect={setSelected} onManagePeople={() => setManagingPeople(true)} />)}
    {tab === 'auto-tags' && <>
      {autoTagSource === null ? <><p className="pw-help">Choose a processed media collection to review people and groups found automatically by SKWAD.</p><div className="pw-card-grid">{processedJobs.map(shoot => <ImportCollectionCard key={shoot.id} shoot={shoot} onOpen={() => { useUi.getState().openShoot(shoot.id, 'albums'); setAutoTagSource(shoot.id); setReviewingAutoTags(false) }} onActions={event => openSourceMenu(shoot, event)} />)}</div>{!shoots.isPending && processedJobs.length === 0 && <div className="pw-empty"><h2>No media ready for auto tagging</h2><p>Finish processing an import first. It will appear here when analysis is complete.</p></div>}</> : <><nav className="pw-breadcrumb" aria-label="Breadcrumb"><button onClick={() => { setAutoTagSource(null); setReviewingAutoTags(false) }}>Auto tags</button><span>/</span><span aria-current="page">{shoots.data?.find(shoot => shoot.id === autoTagSource)?.name ?? 'Media collection'}</span></nav><div className="pw-toolbar"><p className="pw-help">Review recognised people, name unknown groups, and add any automatic album directly to a project collection.</p><button onClick={() => { useUi.getState().openShoot(autoTagSource, 'review'); setReviewingAutoTags(current => !current) }}>{reviewingAutoTags ? 'Back to auto tags' : 'Review face matches'}</button></div><div className="pw-existing">{reviewingAutoTags ? <ReviewScreen /> : <AlbumsScreen onAddToCollection={album => void collectAlbum(album)} />}</div></>}
    </>}
    {tab === 'pre-process' && <PreProcess onCollect={setSelected} />}
    {sourceMenu && menuSource && <SourceContextMenu shoot={menuSource} x={sourceMenu.x} y={sourceMenu.y} busy={busySourceId === menuSource.id} onClose={() => setSourceMenu(null)} onOpen={() => { setSource(menuSource.id); setTab('library'); setTool(null) }} onReprocess={() => void reprocessSource(menuSource)} onRename={() => setRenamingSource(menuSource)} onShowFolder={() => void api.openPath(menuSource.sourcePath).catch(error => pushNotice({ level: 'error', message: String(error) }))} onRemove={() => void removeSource(menuSource)} />}
    {renamingSource && <RenameSourceDialog shoot={renamingSource} onClose={() => setRenamingSource(null)} onSave={async name => { setBusySourceId(renamingSource.id); try { await api.renameShoot(renamingSource.id, name); await refreshSources(); setRenamingSource(null); pushNotice({ level: 'success', message: `Renamed to ${name}.` }) } catch (error) { pushNotice({ level: 'error', message: String(error) }) } finally { setBusySourceId(null) } }} busy={busySourceId === renamingSource.id} />}
    {importing && <ImportMedia onClose={() => setImporting(false)} onCreated={() => { setSource(null); setTab('library'); setImporting(false) }} />}
    {selected && <PublishCollection media={selected} projects={projects} save={save} onClose={() => setSelected(null)} onPublished={id => { setSelected(null); onPublished(id) }} />}
  </>
}

function JobCard({ shoot, live = false, onOpen, onActions }: { shoot: ShootSummary; live?: boolean; onOpen: () => void; onActions: (event: MouseEvent<HTMLButtonElement>) => void }) {
  return <article className={`pw-job${live ? ' is-live' : ''}`} onContextMenu={onActions}><div className="pw-job-heading"><div><h2>{shoot.name}</h2><p>{shoot.photoCount} photos · {shoot.videoCount} videos</p></div><span className={`badge ${shoot.status}`}>{shoot.status === 'completed' ? 'Ready' : shoot.status}</span><button onClick={onOpen}>Open media</button><button onClick={onActions}>Actions</button></div>{live && <LiveJobProgress shootId={shoot.id} />}<JobDetails shootId={shoot.id} paused={shoot.status === 'paused'} label={live ? 'View processing details and controls' : 'Processing summary'} /></article>
}

function LiveJobProgress({ shootId }: { shootId: number }) {
  const eventProgress = useUi(state => state.progress[shootId])
  const initial = useQuery({ queryKey: ['workspace-progress-summary', shootId], queryFn: () => api.getProgress(shootId), refetchInterval: 5000 })
  const progress = eventProgress ?? initial.data
  if (!progress) return <p className="pw-help">Loading progress…</p>
  const finished = progress.mediaAnalysed + progress.mediaFailed
  return <div className="pw-job-progress"><div className="pw-job-progress-copy"><strong>{progress.percent.toFixed(1)}%</strong><span>{progress.stage} · {finished} of {progress.mediaTotal} analysed</span></div><div className="pw-mini-progress" aria-label={`${progress.percent.toFixed(1)}% processed`}><span style={{ width: `${Math.min(100, progress.percent)}%` }} /></div></div>
}

function JobDetails({ shootId, paused, defaultExpanded = false, label }: { shootId: number; paused: boolean; defaultExpanded?: boolean; label: string }) {
  const [expanded, setExpanded] = useState(defaultExpanded)
  const initial = useQuery({ queryKey: ['workspace-progress', shootId], queryFn: () => api.getProgress(shootId), enabled: expanded })
  useEffect(() => {
    if (initial.data && !useUi.getState().progress[shootId]) useUi.getState().setProgress({ ...initial.data, paused })
  }, [initial.data, shootId, paused])
  return <details open={expanded} onToggle={e => setExpanded(e.currentTarget.open)}><summary>{label}</summary>{expanded && <>{initial.isError && <p role="alert">Could not load progress. <button onClick={() => void initial.refetch()}>Retry</button></p>}<ProgressPanel shootId={shootId} /></>}</details>
}

function ImportMedia({ onClose, onCreated }: { onClose: () => void; onCreated: (id: number) => void }) {
  const [folder, setFolder] = useState('')
  const [name, setName] = useState('')
  const [busy, setBusy] = useState(false)
  const [error, setError] = useState('')
  const client = useQueryClient()
  const browse = async () => { try { const result = await open({ directory: true, multiple: false, title: 'Choose a media folder' }); if (typeof result === 'string') { setFolder(result); if (!name) setName(result.replaceAll('\\', '/').split('/').filter(Boolean).pop() ?? '') } } catch (e) { setError(String(e)) } }
  return <WorkspaceDialog title="Add media" onClose={() => { if (!busy) onClose() }}><form onSubmit={async e => { e.preventDefault(); setBusy(true); setError(''); try { const result = await api.createShoot(name.trim(), folder.trim()); await client.invalidateQueries({ queryKey: ['shoots'] }); onCreated(result.id) } catch (e) { setError(String(e)) } finally { setBusy(false) } }}>
    <p>Choose a folder of photos and videos. Files stay in their original location while SKWAD analyses them.</p>
    <label className="field">Media folder<div className="actions"><input required autoFocus value={folder} disabled={busy} onChange={e => setFolder(e.target.value)} placeholder="Select or enter a folder path" /><button type="button" disabled={busy} onClick={() => void browse()}>Browse</button></div></label>
    <label className="field">Name<input required value={name} disabled={busy} onChange={e => setName(e.target.value)} placeholder="e.g. BGIS 2026" /></label>
    <p className="pw-help">After analysis, select people or media and create collections in any project. You can return to this import at any time.</p>
    {error && <p className="pw-error" role="alert">{error}</p>}<div className="pw-dialog-actions"><button type="button" disabled={busy} onClick={onClose}>Cancel</button><button className="primary" disabled={busy || !folder.trim() || !name.trim()}>{busy ? 'Starting…' : 'Start processing'}</button></div>
  </form></WorkspaceDialog>
}

function ImportCollectionCard({ shoot, onOpen, onActions }: { shoot: ShootSummary; onOpen: () => void; onActions: (event: MouseEvent<HTMLButtonElement>) => void }) {
  const cover = useQuery({ queryKey: ['media', shoot.id, 'collection-cover'], queryFn: () => api.listMedia({ shootId: shoot.id, limit: 1 }) })
  const mediaId = cover.data?.find(item => item.thumbnailPath)?.id
  const [failed, setFailed] = useState(false)
  const total = shoot.photoCount + shoot.videoCount
  const status = shoot.status === 'completed' ? 'Ready' : shoot.status
  return <article className="pw-cover-card" onContextMenu={onActions}>
    <button className="pw-card-open" onClick={onOpen}><div className="pw-cover">{mediaId != null && !failed ? <img src={thumbUrl(mediaId)} alt="" loading="lazy" onError={() => setFailed(true)} /> : <span>Media collection</span>}</div>
    <div className="pw-card-body"><div className="pw-card-title"><h2>{shoot.name}</h2><span className={`badge ${shoot.status}`}>{status}</span></div><p>{total} files <span>{shoot.photoCount} photos · {shoot.videoCount} videos</span></p></div></button>
    <button className="pw-card-actions" aria-label={`Actions for ${shoot.name}`} onClick={onActions}>Actions</button>
  </article>
}

function SourceContextMenu({ shoot, x, y, busy, onClose, onOpen, onReprocess, onRename, onShowFolder, onRemove }: { shoot: ShootSummary; x: number; y: number; busy: boolean; onClose: () => void; onOpen: () => void; onReprocess: () => void; onRename: () => void; onShowFolder: () => void; onRemove: () => void }) {
  useEffect(() => { const close = (event: KeyboardEvent) => { if (event.key === 'Escape') onClose() }; document.addEventListener('keydown', close); return () => document.removeEventListener('keydown', close) }, [onClose])
  const run = (action: () => void) => { onClose(); action() }
  return <div className="pw-context-layer" onClick={onClose} onContextMenu={event => { event.preventDefault(); onClose() }}><div className="pw-context-menu" role="menu" aria-label={`${shoot.name} media collection actions`} style={{ left: Math.min(x, window.innerWidth - 230), top: Math.min(y, window.innerHeight - 280) }} onClick={event => event.stopPropagation()}><strong>Media collection actions</strong><button autoFocus role="menuitem" onClick={() => run(onOpen)}>Open media</button><button role="menuitem" disabled={busy} onClick={() => run(onReprocess)}>Re-process data</button><button role="menuitem" disabled={busy} onClick={() => run(onRename)}>Rename</button><button role="menuitem" onClick={() => run(onShowFolder)}>Show source folder</button><button role="menuitem" className="danger" disabled={busy} onClick={() => run(onRemove)}>Remove indexed data</button></div></div>
}

function RenameSourceDialog({ shoot, busy, onClose, onSave }: { shoot: ShootSummary; busy: boolean; onClose: () => void; onSave: (name: string) => Promise<void> }) {
  const [name, setName] = useState(shoot.name)
  return <WorkspaceDialog title="Rename media collection" onClose={onClose}><form onSubmit={event => { event.preventDefault(); void onSave(name.trim()) }}><label className="field">Name<input autoFocus required maxLength={120} disabled={busy} value={name} onChange={event => setName(event.target.value)} /></label><p className="pw-help">This changes the collection name only. The source folder stays where it is.</p><div className="pw-dialog-actions"><button type="button" disabled={busy} onClick={onClose}>Cancel</button><button className="primary" disabled={busy || !name.trim()}>{busy ? 'Saving…' : 'Save name'}</button></div></form></WorkspaceDialog>
}
