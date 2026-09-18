import { useEffect, useState } from 'react'
import { useMutation, useQuery, useQueryClient } from '@tanstack/react-query'
import type { Media } from '@skwad/shared-types'
import * as api from '../api'
import { MediaGrid } from '../components/MediaGrid'
import { TAG_KEYS, TagNamesDatalist, TagPairEditor, TagValueInput } from '../components/TagPicker'
import { useUi } from '../store'

const PAGE_SIZE = 120

export function MediaBrowser({ shootId, excludeShootId, groupId, personId, onCollect, onAddToExisting }: { shootId?: number; excludeShootId?: number; groupId?: number; personId?: number; onCollect?: (media: Media[]) => void; onAddToExisting?: (media: Media[]) => void }) {
  const [search, setSearch] = useState('')
  const [query, setQuery] = useState('')
  const [type, setType] = useState('all')
  const [person, setPerson] = useState('all')
  const [offset, setOffset] = useState(0)
  const [selected, setSelected] = useState<Media[]>([])
  const [advanced, setAdvanced] = useState(false)
  const [quality, setQuality] = useState('all')
  const [rating, setRating] = useState(0)
  const [pickState, setPickState] = useState<'all' | 'none' | 'pick' | 'reject'>('all')
  const [sort, setSort] = useState<'capturedAt' | 'filename' | 'quality' | 'rating'>('capturedAt')
  // A tag value to filter by, typed with suggestions from the taxonomy;
  // the tag name is remembered when a suggestion is picked so "Final" under
  // Stage does not also match "Final" under some other tag.
  const [tagValue, setTagValue] = useState('')
  const [tagName, setTagName] = useState<string | null>(null)
  const [appliedTag, setAppliedTag] = useState<{ value: string; name: string | null }>({ value: '', name: null })
  const [bulkTag, setBulkTag] = useState(false)
  const client = useQueryClient()
  const notice = useUi(s => s.pushNotice)
  const setClipboard = useUi(s => s.setClipboard)
  useEffect(() => { const timer = setTimeout(() => { setQuery(search.trim()); setOffset(0) }, 250); return () => clearTimeout(timer) }, [search])
  useEffect(() => { const timer = setTimeout(() => { setAppliedTag({ value: tagValue.trim(), name: tagValue.trim() ? tagName : null }); setOffset(0) }, 300); return () => clearTimeout(timer) }, [tagValue, tagName])
  const people = useQuery({ queryKey: ['people', shootId ?? null], queryFn: () => api.listPeople(shootId) })
  const media = useQuery({ queryKey: ['media', shootId ?? null, excludeShootId ?? null, personId ?? null, 'workspace', groupId, query, type, person, offset, quality, rating, pickState, sort, appliedTag], queryFn: () => api.listMedia({
    shootId, excludeShootId, groupId, search: query || null, mediaType: type === 'all' ? null : type as 'photo' | 'video',
    tagValue: appliedTag.value || null, tagName: appliedTag.name,
    personId: personId ?? (person === 'all' || person === 'unknown' ? null : Number(person)), onlyUnidentified: personId === undefined && person === 'unknown',
    onlyBestShots: quality === 'best', onlyDuplicates: quality === 'duplicates', minRating: rating || null, pickState: pickState === 'all' ? null : pickState, sort,
    offset, limit: PAGE_SIZE,
  }) })
  const editorial = useMutation({ mutationFn: api.setMediaEditorial, onSuccess: () => client.invalidateQueries({ queryKey: ['media'] }), onError: e => notice({ level: 'error', message: String(e) }) })
  const tagMany = useMutation({
    mutationFn: ({ tag, value }: { tag: string; value: string }) => api.assignTagToMany('media', selected.map(m => String(m.id)), tag, value),
    onSuccess: (count, { tag, value }) => {
      void client.invalidateQueries({ queryKey: ['assetTags'] })
      void client.invalidateQueries({ queryKey: TAG_KEYS.tags })
      void client.invalidateQueries({ queryKey: ['tagSuggest'] })
      notice({ level: 'success', message: `${tag}: ${value} added to ${count} file${count === 1 ? '' : 's'}.` })
    },
    onError: e => notice({ level: 'error', message: String(e) }),
  })
  // Plain click replaces the selection, Ctrl/Cmd-click adds or removes.
  const toggle = (id: number, additive: boolean) => {
    const item = media.data?.find(m => m.id === id)
    if (!item) return
    setSelected(current => {
      if (!additive) return [item]
      return current.some(m => m.id === id) ? current.filter(m => m.id !== id) : [...current, item]
    })
  }
  const filter = (update: () => void) => { update(); setOffset(0); setSelected([]) }

  // Cut is only offered while browsing inside a collection's group: that is
  // the only context with somewhere to remove the files *from*.
  const copySelection = (mode: 'cut' | 'copy', items: Media[] = selected) => {
    if (items.length === 0) return
    setClipboard({
      kind: 'media',
      mode,
      media: items,
      source: mode === 'cut' && groupId !== undefined && shootId !== undefined ? { shootId, groupId } : null,
    })
    notice({ level: 'info', message: `${items.length} file${items.length === 1 ? '' : 's'} ready — right-click a collection and choose Paste.` })
  }
  return <section aria-label={groupId ? 'Collection media' : 'Media library'}>
    <div className="pw-toolbar pw-media-filters">
      <label className="pw-search"><span className="sr-only">Search files</span><input type="search" placeholder="Search files…" value={search} onChange={e => { setSearch(e.target.value); setSelected([]) }} /></label>
      <div className="pw-segment" role="tablist" aria-label="Media type">
        <button type="button" role="tab" aria-pressed={type === 'all'} onClick={() => filter(() => setType('all'))}>All</button>
        <button type="button" role="tab" aria-pressed={type === 'photo'} onClick={() => filter(() => setType('photo'))}>Photos</button>
        <button type="button" role="tab" aria-pressed={type === 'video'} onClick={() => filter(() => setType('video'))}>Videos</button>
      </div>
      {personId === undefined && <label><span className="sr-only">Find a person</span><select value={person} onChange={e => filter(() => setPerson(e.target.value))}><option value="all">Find a person</option><option value="unknown">Unidentified people</option>{people.data?.map(p => <option key={p.id} value={p.id}>{p.name}</option>)}</select></label>}
      <label className="pw-tag-filter"><span className="sr-only">Filter by tag</span><TagValueInput tag={tagName} value={tagValue} placeholder="Filter by tag value…" onChange={value => { setTagValue(value); if (!value.trim()) setTagName(null); setSelected([]) }} onPick={suggestion => setTagName(suggestion.tag)} />{tagValue.trim() && <span className="hint">{tagName ? `${tagName}: ` : 'any tag: '}{tagValue.trim()}</span>}</label>
      <button aria-expanded={advanced} onClick={() => setAdvanced(!advanced)}>More filters</button>
    </div>
    {advanced && <div className="pw-toolbar pw-filter-details"><label>Quality <select value={quality} onChange={e => filter(() => setQuality(e.target.value))}><option value="all">All media</option><option value="best">Best shots</option><option value="duplicates">Duplicates</option></select></label><label>Rating <select value={rating} onChange={e => filter(() => setRating(Number(e.target.value)))}><option value={0}>Any rating</option>{[1, 2, 3, 4, 5].map(n => <option key={n} value={n}>{n}+ stars</option>)}</select></label><label>Flag <select value={pickState} onChange={e => filter(() => setPickState(e.target.value as typeof pickState))}><option value="all">Any flag</option><option value="pick">Picks</option><option value="reject">Rejects</option><option value="none">Unflagged</option></select></label><label>Sort <select value={sort} onChange={e => filter(() => setSort(e.target.value as typeof sort))}><option value="capturedAt">Date captured</option><option value="filename">Filename</option><option value="quality">Quality</option><option value="rating">Rating</option></select></label></div>}
    {selected.length > 0 && <div className="pw-selection"><strong>{selected.length} selected</strong><button onClick={() => setSelected(current => [...new Map([...current, ...(media.data ?? [])].map(m => [m.id, m])).values()])}>Select this page</button><button onClick={() => setSelected([])}>Clear</button><button onClick={() => copySelection('copy')}>Copy</button>{groupId !== undefined && shootId !== undefined && <button onClick={() => copySelection('cut')}>Cut</button>}{onAddToExisting && <button onClick={() => onAddToExisting(selected)}>Add to existing collection</button>}{onCollect && <button className="primary" onClick={() => onCollect(selected)}>Create collection</button>}<button aria-expanded={bulkTag} onClick={() => setBulkTag(current => !current)}>{bulkTag ? 'Close tagging' : 'Tag selected'}</button></div>}
    {selected.length > 0 && bulkTag && <div className="pw-toolbar pw-bulk-tag"><TagNamesDatalist /><span className="hint">Add a tag to all {selected.length} selected file{selected.length === 1 ? '' : 's'}</span><TagPairEditor busy={tagMany.isPending} addLabel="Apply to selection" onAdd={(tag, value) => tagMany.mutate({ tag, value })} /></div>}
    {people.isError && <p role="alert" className="pw-error">People could not be loaded. <button onClick={() => void people.refetch()}>Retry people</button></p>}
    {media.isPending ? <p role="status" className="pw-loading">Loading media…</p> : media.isError ? <div className="pw-empty"><h2>Couldn’t load this media</h2><p>{media.error.message}</p><button onClick={() => void media.refetch()}>Try again</button></div> : <MediaGrid media={media.data ?? []} selected={new Set(selected.map(m => m.id))} onToggleSelect={toggle} onClipboard={(mode, items) => copySelection(mode, items)} canCut={groupId !== undefined && shootId !== undefined} onEditorial={args => editorial.mutate(args)} editorialBusy={editorial.isPending} preferVideoFaces emptyTitle={query || person !== 'all' || type !== 'all' ? 'No matching media' : 'No media yet'} emptyHint={query || person !== 'all' || type !== 'all' ? 'Try another name or change your filters.' : 'Add a media folder to start. Processed files stay available here.'} />}
    <div className="pw-pagination"><span>{media.data?.length ? `${offset + 1}–${offset + media.data.length}` : '0'} files</span><button disabled={offset === 0 || media.isFetching} onClick={() => setOffset(Math.max(0, offset - PAGE_SIZE))}>Previous</button><button disabled={(media.data?.length ?? 0) < PAGE_SIZE || media.isFetching} onClick={() => setOffset(offset + PAGE_SIZE)}>Next</button></div>
  </section>
}
