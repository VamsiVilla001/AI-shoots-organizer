import { useState, type MouseEvent } from 'react'
import { open } from '@tauri-apps/plugin-dialog'
import { useMutation, useQuery, useQueryClient } from '@tanstack/react-query'
import type { EnrollDirectoryResult, Media, PersonSummary } from '@skwad/shared-types'
import * as api from '../api'
import { useUi } from '../store'
import { FaceCrop } from '../components/FaceCrop'
import { MediaBrowser } from './mediaBrowser'
import { loadTaggedMedia } from './taggedMedia'
import { PersonContextMenu } from './personContextMenu'

const PHOTO_EXTENSIONS = ['jpg', 'jpeg', 'png', 'webp', 'heic', 'raf', 'cr2', 'cr3', 'nef', 'arw']
const VIDEO_EXTENSIONS = ['mp4', 'mov', 'm4v', 'avi', 'mkv']
const MIN_PHOTOS = 3

function fileName(path: string) {
  return path.split(/[\\/]/).pop() ?? path
}

/**
 * Pre-registers a person from reference photos/video taken outside of any
 * shoot, then lets the user pull that person up and ask SKWAD to search
 * already-processed media for them. Nothing here writes to a project or
 * collection automatically — matches land as suggestions the user reviews
 * and adds on their own terms, exactly like every other recognition result.
 */
export function PreProcess({ onCollect, onAddToExisting }: { onCollect: (media: Media[]) => void; onAddToExisting?: (media: Media[]) => void }) {
  const queryClient = useQueryClient()
  const pushNotice = useUi(state => state.pushNotice)

  const [name, setName] = useState('')
  const [team, setTeam] = useState('')
  const [mode, setMode] = useState<'photos' | 'video' | null>(null)
  const [photoPaths, setPhotoPaths] = useState<string[]>([])
  const [videoPath, setVideoPath] = useState<string | null>(null)
  const [pickerError, setPickerError] = useState('')

  const [folderPath, setFolderPath] = useState<string | null>(null)
  const [folderTeam, setFolderTeam] = useState('')
  const [folderError, setFolderError] = useState('')
  const [folderResult, setFolderResult] = useState<EnrollDirectoryResult | null>(null)

  const [search, setSearch] = useState('')
  const [personId, setPersonId] = useState<number | null>(null)
  const [findingId, setFindingId] = useState<number | null>(null)
  const [addingId, setAddingId] = useState<number | null>(null)
  const [rowError, setRowError] = useState('')
  const [menu, setMenu] = useState<{ person: PersonSummary; x: number; y: number } | null>(null)

  const people = useQuery({ queryKey: ['people', 'enrolled'], queryFn: () => api.listEnrolledPeople() })
  const openMenu = (item: PersonSummary, event: MouseEvent) => { event.preventDefault(); setMenu({ person: item, x: event.clientX, y: event.clientY }) }
  const person = people.data?.find(item => item.id === personId)

  // Lets the person detail view keep the photos/video the user fed at
  // enrollment visually separate from media actually found by search. Faces
  // (not media) so each sample can render as a square face-crop "stamp"
  // instead of the whole photo — a candid full-body reference shouldn't
  // dominate the tile the way a tight headshot does.
  const referenceShoot = useQuery({ queryKey: ['referenceShootId'], queryFn: () => api.referenceLibraryShootId() })
  const referenceFaces = useQuery({
    queryKey: ['faces', referenceShoot.data ?? null, 'reference', person?.id],
    queryFn: () => api.listFaces({ shootId: referenceShoot.data, personId: person?.id, limit: 50 }),
    enabled: person != null && referenceShoot.data != null,
  })
  const visible = (people.data ?? []).filter(item => {
    const query = search.trim().toLocaleLowerCase()
    return !query || item.name.toLocaleLowerCase().includes(query) || item.team?.toLocaleLowerCase().includes(query)
  })

  const resetForm = () => {
    setName(''); setTeam(''); setMode(null); setPhotoPaths([]); setVideoPath(null); setPickerError('')
  }

  const enroll = useMutation({
    mutationFn: () =>
      api.enrollPerson({
        name: name.trim(),
        team: team.trim() || null,
        photoPaths: mode === 'photos' ? photoPaths : undefined,
        videoPath: mode === 'video' ? videoPath : undefined,
      }),
    onSuccess: result => {
      const skipped = result.rejectedCount > 0 ? ` (${result.rejectedCount} skipped — no single clear face found)` : ''
      pushNotice({
        level: 'success',
        message: `${result.person.name} enrolled with ${result.samplesAdded} reference sample${result.samplesAdded === 1 ? '' : 's'}${skipped}.`,
      })
      resetForm()
      void queryClient.invalidateQueries({ queryKey: ['people'] })
    },
    onError: (e: unknown) => setPickerError(String(e)),
  })

  const enrollFolder = useMutation({
    mutationFn: () => api.enrollPeopleFromDirectory(folderPath!, folderTeam.trim() || null),
    onSuccess: result => {
      setFolderResult(result)
      const samples = result.enrolled.reduce((total, item) => total + item.samplesAdded, 0)
      pushNotice({
        level: result.enrolled.length > 0 ? 'success' : 'warn',
        message: result.enrolled.length > 0
          ? `Enrolled ${result.enrolled.length} ${result.enrolled.length === 1 ? 'person' : 'people'} from ${samples} reference photo${samples === 1 ? '' : 's'}.`
          : 'No usable faces were found in that folder.',
      })
      void queryClient.invalidateQueries({ queryKey: ['people'] })
    },
    onError: (e: unknown) => setFolderError(String(e)),
  })

  const pickFolder = async () => {
    setFolderError(''); setFolderResult(null)
    try {
      const picked = await open({ directory: true, multiple: false })
      if (typeof picked !== 'string') return
      setFolderPath(picked)
    } catch (e) {
      setFolderError(String(e))
    }
  }

  const addPhotos = async () => {
    setPickerError('')
    try {
      const picked = await open({ multiple: true, filters: [{ name: 'Images', extensions: PHOTO_EXTENSIONS }] })
      if (!picked) return
      const paths = Array.isArray(picked) ? picked : [picked]
      setMode('photos')
      setVideoPath(null)
      setPhotoPaths(prev => Array.from(new Set([...prev, ...paths])))
    } catch (e) {
      setPickerError(String(e))
    }
  }

  const pickVideo = async () => {
    setPickerError('')
    try {
      const picked = await open({ multiple: false, filters: [{ name: 'Video', extensions: VIDEO_EXTENSIONS }] })
      if (typeof picked !== 'string') return
      setMode('video')
      setPhotoPaths([])
      setVideoPath(picked)
    } catch (e) {
      setPickerError(String(e))
    }
  }

  const findMedia = async (id: number, label: string) => {
    setFindingId(id); setRowError('')
    try {
      const report = await api.findPersonMedia(id)
      pushNotice({
        level: 'success',
        message: `${label}searched ${report.shootsScanned} collection${report.shootsScanned === 1 ? '' : 's'} — ${report.newSuggestions} new match${report.newSuggestions === 1 ? '' : 'es'} found.`,
      })
      void queryClient.invalidateQueries({ queryKey: ['people'] })
      void queryClient.invalidateQueries({ queryKey: ['media'] })
      void queryClient.invalidateQueries({ queryKey: ['faces'] })
    } catch (caught) {
      setRowError(String(caught))
    } finally {
      setFindingId(null)
    }
  }

  const canSubmit =
    name.trim().length > 0 && ((mode === 'photos' && photoPaths.length >= MIN_PHOTOS) || (mode === 'video' && !!videoPath))

  if (person) return <>
    <nav className="pw-breadcrumb" aria-label="Breadcrumb"><button onClick={() => setPersonId(null)}>Pre-Process</button><span>/</span><span aria-current="page">{person.name}</span></nav>
    <div className="pw-tag-heading">
      <div><span className="pw-eyebrow">Pre-registered person</span><h2>{person.name}</h2><p>{person.team || 'No team'} · {person.faceSampleCount} reference sample{person.faceSampleCount === 1 ? '' : 's'} · {person.mediaCount} matched file{person.mediaCount === 1 ? '' : 's'}</p></div>
      <button onClick={() => setPersonId(null)}>All people</button>
    </div>
    <div className="pw-toolbar">
      <p className="pw-help" style={{ margin: 0, flex: 1 }}>Search already-processed media for this person. Matches land for review — select any and add them to a collection yourself.</p>
      <button className="primary" disabled={findingId === person.id} onClick={() => void findMedia(person.id, '')}>{findingId === person.id ? 'Searching…' : 'Find media'}</button>
    </div>
    {rowError && <p role="alert" className="pw-error">{rowError}</p>}

    <section>
      <h2 className="pw-section-heading">Reference samples fed</h2>
      <p className="pw-help">What SKWAD built this person's face library from — not media found by search.</p>
      {referenceFaces.isPending ? <p role="status" className="pw-loading">Loading reference samples…</p> : (referenceFaces.data ?? []).length === 0 ? <div className="pw-empty"><h2>No reference samples</h2><p>Enrollment photos/video for this person could not be found.</p></div> : <div className="face-grid">{(referenceFaces.data ?? []).map(face => <div className="face-card" key={face.id} title={face.mediaFilename}>
        <FaceCrop mediaId={face.mediaId} bbox={face.bbox} padding={0.15} />
      </div>)}</div>}
    </section>

    <section className="pw-media-section">
      <h2 className="pw-section-heading">Media found</h2>
      <p className="pw-help">Matches from already-processed collections. Select any and add them to a collection yourself.</p>
      <MediaBrowser key={person.id} personId={person.id} excludeShootId={referenceShoot.data ?? undefined} onCollect={onCollect} onAddToExisting={onAddToExisting} />
    </section>
  </>

  return <>
    <section className="pw-enroll-card">
      <div className="pw-tag-heading">
        <div><span className="pw-eyebrow">Enroll a person</span><h2>Add by photo or video</h2><p>Provide a name and either {MIN_PHOTOS}+ reference photos or one short video. SKWAD builds their face library immediately and can search already-processed media on request.</p></div>
      </div>

      <div className="pw-enroll-grid">
        <label className="field"><span>Name</span><input value={name} onChange={e => setName(e.target.value)} placeholder="e.g. Jonathan" /></label>
        <label className="field"><span>Team (optional)</span><input value={team} onChange={e => setTeam(e.target.value)} placeholder="e.g. Gods Reign" /></label>
      </div>

      <div className="pw-enroll-sources">
        <button aria-pressed={mode === 'photos'} disabled={mode === 'video'} onClick={() => void addPhotos()}>Add photos</button>
        <span className="pw-muted">or</span>
        <button aria-pressed={mode === 'video'} disabled={mode === 'photos'} onClick={() => void pickVideo()}>Add video</button>

        {mode === 'photos' && photoPaths.length > 0 && (
          <div className="pw-chip-list">
            {photoPaths.map(path => <span key={path} className="pw-chip">{fileName(path)}<button onClick={() => setPhotoPaths(prev => prev.filter(p => p !== path))} aria-label={`Remove ${fileName(path)}`}>×</button></span>)}
          </div>
        )}
        {mode === 'photos' && photoPaths.length > 0 && photoPaths.length < MIN_PHOTOS && (
          <p className="pw-help" style={{ margin: 0, width: '100%' }}>Add at least {MIN_PHOTOS - photoPaths.length} more photo{MIN_PHOTOS - photoPaths.length === 1 ? '' : 's'}.</p>
        )}
        {mode === 'video' && videoPath && (
          <div className="pw-chip-list">
            <span className="pw-chip">{fileName(videoPath)}<button onClick={() => { setMode(null); setVideoPath(null) }} aria-label="Remove video">×</button></span>
          </div>
        )}
      </div>

      {pickerError && <p role="alert" className="pw-error">{pickerError}</p>}

      <div className="pw-enroll-actions">
        <button className="primary" disabled={!canSubmit || enroll.isPending} onClick={() => enroll.mutate()}>{enroll.isPending ? 'Enrolling…' : 'Enroll person'}</button>
      </div>
    </section>

    <section className="pw-enroll-card">
      <div className="pw-tag-heading">
        <div>
          <span className="pw-eyebrow">Enroll a roster</span>
          <h2>Add from a folder</h2>
          <p>Point at a folder holding <code>front</code>, <code>left</code> and <code>right</code> subfolders. The same filename in each is the same person — <code>front/naresh.png</code>, <code>left/naresh.png</code>, <code>right/naresh.png</code> enrolls "naresh" from all three angles. Faces are read straight into the reference library; no media is imported and no processing job runs.</p>
        </div>
      </div>

      <div className="pw-enroll-grid">
        <label className="field"><span>Team for everyone in this folder (optional)</span><input value={folderTeam} onChange={e => setFolderTeam(e.target.value)} placeholder="e.g. Gods Reign" /></label>
      </div>

      <div className="pw-enroll-sources">
        <button onClick={() => void pickFolder()}>Choose folder</button>
        {folderPath && <div className="pw-chip-list"><span className="pw-chip">{folderPath}<button onClick={() => { setFolderPath(null); setFolderResult(null) }} aria-label="Remove folder">×</button></span></div>}
      </div>

      {folderError && <p role="alert" className="pw-error">{folderError}</p>}

      {folderResult && <div className="pw-note">
        {folderResult.enrolled.length > 0 && <><strong>Enrolled {folderResult.enrolled.length}</strong>
          <div className="pw-chip-list">{folderResult.enrolled.map(item => <span key={item.name} className="pw-chip">{item.name} · {item.samplesAdded}/{item.angles.length} {item.angles.join('/')}</span>)}</div></>}
        {folderResult.skipped.length > 0 && <p className="pw-help" style={{ margin: '8px 0 0' }}>No usable face found for: {folderResult.skipped.join(', ')}. Try clearer, front-facing photos with exactly one face.</p>}
      </div>}

      <div className="pw-enroll-actions">
        <button className="primary" disabled={!folderPath || enrollFolder.isPending} onClick={() => enrollFolder.mutate()}>{enrollFolder.isPending ? 'Enrolling roster…' : 'Enroll everyone in folder'}</button>
      </div>
    </section>

    <div className="pw-toolbar"><label className="pw-search"><span className="sr-only">Search enrolled people</span><input type="search" placeholder="Search enrolled people or teams…" value={search} onChange={event => setSearch(event.target.value)} /></label><span>{people.data?.length ?? 0} enrolled</span></div>
    <p className="pw-help">People enrolled above appear here. Open one to search already-processed media, or use Find media directly from this list.</p>
    {rowError && <p role="alert" className="pw-error">{rowError}</p>}
    {people.isPending ? <p role="status" className="pw-loading">Loading enrolled people…</p> : people.isError ? <div className="pw-empty"><h2>Couldn't load enrolled people</h2><button onClick={() => void people.refetch()}>Try again</button></div> : visible.length > 0 ? <div className="pw-tag-list">{visible.map(item => <div className="pw-tag-row" key={item.id} onContextMenu={event => openMenu(item, event)}>
      <button className="pw-tag-open" onDoubleClick={() => setPersonId(item.id)}><span><strong>{item.name}</strong><small>{item.team || 'No team'}</small></span><span>{item.faceSampleCount} sample{item.faceSampleCount === 1 ? '' : 's'}</span><span>{item.mediaCount} files</span></button>
      <button disabled={findingId !== null} onClick={() => void findMedia(item.id, `${item.name}: `)}>{findingId === item.id ? 'Searching…' : 'Find media'}</button>
      <button disabled={addingId !== null || item.mediaCount === 0} onClick={async () => {
        setAddingId(item.id); setRowError('')
        try {
          const tagged = await loadTaggedMedia(item.id)
          if (tagged.length === 0) throw new Error('No matched media is available for this person yet.')
          onCollect(tagged)
        } catch (caught) { setRowError(String(caught)) } finally { setAddingId(null) }
      }}>{addingId === item.id ? 'Loading…' : 'Add to collection'}</button>
    </div>)}</div> : <div className="pw-empty"><h2>{people.data?.length ? 'No matching people' : 'Nobody enrolled yet'}</h2><p>{people.data?.length ? 'Try another name or team.' : 'Add a name and reference photos or a video above to pre-register a person.'}</p>{people.data?.length ? <button onClick={() => setSearch('')}>Clear search</button> : null}</div>}
    {menu && <PersonContextMenu person={menu.person} all={people.data ?? []} x={menu.x} y={menu.y} onClose={() => setMenu(null)} onOpen={() => setPersonId(menu.person.id)} />}
  </>
}
