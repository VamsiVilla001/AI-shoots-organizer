import { useEffect, useState } from 'react'
import { useMutation, useQuery, useQueryClient } from '@tanstack/react-query'
import type { Media } from '@skwad/shared-types'
import * as api from '../api'
import { MediaGrid } from '../components/MediaGrid'
import { useUi } from '../store'

const PAGE_SIZE = 120

export function MediaBrowser({ shootId, groupId, personId, onCollect }: { shootId?: number; groupId?: number; personId?: number; onCollect?: (media: Media[]) => void }) {
  const [search, setSearch] = useState('')
  const [query, setQuery] = useState('')
  const [type, setType] = useState('all')
  const [person, setPerson] = useState('all')
  const [offset, setOffset] = useState(0)
  const [selected, setSelected] = useState<Media[]>([])
  const [selecting, setSelecting] = useState(false)
  const [advanced, setAdvanced] = useState(false)
  const [quality, setQuality] = useState('all')
  const [rating, setRating] = useState(0)
  const [pickState, setPickState] = useState<'all' | 'none' | 'pick' | 'reject'>('all')
  const [sort, setSort] = useState<'capturedAt' | 'filename' | 'quality' | 'rating'>('capturedAt')
  const client = useQueryClient()
  const notice = useUi(s => s.pushNotice)
  useEffect(() => { const timer = setTimeout(() => { setQuery(search.trim()); setOffset(0) }, 250); return () => clearTimeout(timer) }, [search])
  const people = useQuery({ queryKey: ['people', shootId ?? null], queryFn: () => api.listPeople(shootId) })
  const media = useQuery({ queryKey: ['media', shootId ?? null, 'workspace', groupId, query, type, person, offset, quality, rating, pickState, sort], queryFn: () => api.listMedia({
    shootId, groupId, search: query || null, mediaType: type === 'all' ? null : type as 'photo' | 'video',
    personId: personId ?? (person === 'all' || person === 'unknown' ? null : Number(person)), onlyUnidentified: personId === undefined && person === 'unknown',
    onlyBestShots: quality === 'best', onlyDuplicates: quality === 'duplicates', minRating: rating || null, pickState: pickState === 'all' ? null : pickState, sort,
    offset, limit: PAGE_SIZE,
  }) })
  const editorial = useMutation({ mutationFn: api.setMediaEditorial, onSuccess: () => client.invalidateQueries({ queryKey: ['media'] }), onError: e => notice({ level: 'error', message: String(e) }) })
  const toggle = (id: number) => {
    const item = media.data?.find(m => m.id === id)
    if (item) setSelected(current => current.some(m => m.id === id) ? current.filter(m => m.id !== id) : [...current, item])
  }
  const filter = (update: () => void) => { update(); setOffset(0); setSelected([]) }
  return <section aria-label={groupId ? 'Collection media' : 'Media library'}>
    <div className="pw-toolbar pw-media-filters">
      <label className="pw-search"><span className="sr-only">Search files</span><input type="search" placeholder="Search files…" value={search} onChange={e => { setSearch(e.target.value); setSelected([]) }} /></label>
      <label><span className="sr-only">Media type</span><select value={type} onChange={e => filter(() => setType(e.target.value))}><option value="all">Photos & videos</option><option value="photo">Photos</option><option value="video">Videos</option></select></label>
      {personId === undefined && <label><span className="sr-only">Find a person</span><select value={person} onChange={e => filter(() => setPerson(e.target.value))}><option value="all">Find a person</option><option value="unknown">Unidentified people</option>{people.data?.map(p => <option key={p.id} value={p.id}>{p.name}</option>)}</select></label>}
      <button aria-expanded={advanced} onClick={() => setAdvanced(!advanced)}>More filters</button>
      {onCollect && <button aria-pressed={selecting} onClick={() => { setSelecting(!selecting); setSelected([]) }}>{selecting ? 'Done selecting' : 'Select media'}</button>}
    </div>
    {advanced && <div className="pw-toolbar pw-filter-details"><label>Quality <select value={quality} onChange={e => filter(() => setQuality(e.target.value))}><option value="all">All media</option><option value="best">Best shots</option><option value="duplicates">Duplicates</option></select></label><label>Rating <select value={rating} onChange={e => filter(() => setRating(Number(e.target.value)))}><option value={0}>Any rating</option>{[1, 2, 3, 4, 5].map(n => <option key={n} value={n}>{n}+ stars</option>)}</select></label><label>Flag <select value={pickState} onChange={e => filter(() => setPickState(e.target.value as typeof pickState))}><option value="all">Any flag</option><option value="pick">Picks</option><option value="reject">Rejects</option><option value="none">Unflagged</option></select></label><label>Sort <select value={sort} onChange={e => filter(() => setSort(e.target.value as typeof sort))}><option value="capturedAt">Date captured</option><option value="filename">Filename</option><option value="quality">Quality</option><option value="rating">Rating</option></select></label></div>}
    {selecting && <div className="pw-selection"><strong>{selected.length} selected</strong><button onClick={() => setSelected(current => [...new Map([...current, ...(media.data ?? [])].map(m => [m.id, m])).values()])}>Select this page</button><button disabled={!selected.length} onClick={() => setSelected([])}>Clear</button><button className="primary" disabled={!selected.length} onClick={() => onCollect?.(selected)}>Create collection</button></div>}
    {people.isError && <p role="alert" className="pw-error">People could not be loaded. <button onClick={() => void people.refetch()}>Retry people</button></p>}
    {media.isPending ? <p role="status" className="pw-loading">Loading media…</p> : media.isError ? <div className="pw-empty"><h2>Couldn’t load this media</h2><p>{media.error.message}</p><button onClick={() => void media.refetch()}>Try again</button></div> : <MediaGrid media={media.data ?? []} selected={new Set(selected.map(m => m.id))} selectMode={selecting} onToggleSelect={selecting ? toggle : undefined} onEditorial={args => editorial.mutate(args)} editorialBusy={editorial.isPending} emptyTitle={query || person !== 'all' || type !== 'all' ? 'No matching media' : 'No media yet'} emptyHint={query || person !== 'all' || type !== 'all' ? 'Try another name or change your filters.' : 'Add a media folder to start. Processed files stay available here.'} />}
    <div className="pw-pagination"><span>{media.data?.length ? `${offset + 1}–${offset + media.data.length}` : '0'} files</span><button disabled={offset === 0 || media.isFetching} onClick={() => setOffset(Math.max(0, offset - PAGE_SIZE))}>Previous</button><button disabled={(media.data?.length ?? 0) < PAGE_SIZE || media.isFetching} onClick={() => setOffset(offset + PAGE_SIZE)}>Next</button></div>
  </section>
}
