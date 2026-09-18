import { useEffect, useState, type MouseEvent } from 'react'
import { useMutation, useQuery, useQueryClient } from '@tanstack/react-query'
import type { Media, PersonSummary, TagSummary } from '@skwad/shared-types'
import * as api from '../api'
import { useUi } from '../store'
import { MediaBrowser } from './mediaBrowser'
import { PersonContextMenu } from './personContextMenu'
import { useTaxonomy } from '../components/TagPicker'
import { Icon } from '../components/Icon'

export function TaggedMedia({ onCollect, onAddToExisting, onManagePeople }: { onCollect: (media: Media[]) => void; onAddToExisting?: (media: Media[]) => void; onManagePeople: () => void }) {
  const [search, setSearch] = useState('')
  const [personId, setPersonId] = useState<number | null>(null)
  // A taxonomy value opened from the list below the people: everything
  // tagged with it, across every collection.
  const [openTag, setOpenTag] = useState<{ tag: string; value: string } | null>(null)
  const taxonomy = useTaxonomy()
  const [addingId, setAddingId] = useState<number | null>(null)
  const [error, setError] = useState('')
  const [menu, setMenu] = useState<{ person: PersonSummary; x: number; y: number } | null>(null)
  const [selectedIds, setSelectedIds] = useState<Set<number>>(new Set())
  const queryClient = useQueryClient()
  const pushNotice = useUi(s => s.pushNotice)
  const people = useQuery({ queryKey: ['people', null], queryFn: () => api.listPeople(null) })
  const openMenu = (item: PersonSummary, event: MouseEvent) => { event.preventDefault(); setMenu({ person: item, x: event.clientX, y: event.clientY }) }
  const person = people.data?.find(item => item.id === personId)
  const toggleSelected = (id: number) => setSelectedIds(current => {
    const next = new Set(current)
    if (next.has(id)) next.delete(id); else next.add(id)
    return next
  })
  const exitSelecting = () => setSelectedIds(new Set())
  const deleteSelected = useMutation({
    mutationFn: async () => { for (const id of selectedIds) await api.deletePerson(id) },
    onSuccess: () => {
      pushNotice({ level: 'success', message: `Deleted ${selectedIds.size} tag${selectedIds.size === 1 ? '' : 's'}.` })
      void queryClient.invalidateQueries({ queryKey: ['people'] })
      exitSelecting()
    },
    onError: (e: unknown) => pushNotice({ level: 'error', message: String(e) }),
  })
  // One row's own bin, beside its Add button — the same delete as the bar.
  const deleteOne = useMutation({
    mutationFn: (item: PersonSummary) => api.deletePerson(item.id),
    onSuccess: (_result, item) => {
      pushNotice({ level: 'success', message: `Deleted “${item.name}”.` })
      void queryClient.invalidateQueries({ queryKey: ['people'] })
      setSelectedIds(current => { const next = new Set(current); next.delete(item.id); return next })
    },
    onError: (e: unknown) => pushNotice({ level: 'error', message: String(e) }),
  })
  const visible = (people.data ?? []).filter(item => {
    const query = search.trim().toLocaleLowerCase()
    return !query || item.name.toLocaleLowerCase().includes(query) || item.team?.toLocaleLowerCase().includes(query)
  })
  const allVisibleSelected = visible.length > 0 && visible.every(item => selectedIds.has(item.id))
  const someVisibleSelected = visible.some(item => selectedIds.has(item.id))
  const toggleAll = () => setSelectedIds(allVisibleSelected ? new Set() : new Set(visible.map(item => item.id)))
  // Opening a tag first makes sure group tags have reached their files, so
  // the list below is never empty for a value that is on a group.
  const propagated = useQuery({ queryKey: ['propagateGroupTags', openTag], queryFn: () => api.propagateGroupTags(null), enabled: openTag !== null, staleTime: 0 })
  useEffect(() => { if (propagated.data && propagated.data > 0) void queryClient.invalidateQueries({ queryKey: ['media'] }) }, [propagated.data, queryClient])

  if (openTag) return <>
    <nav className="pw-breadcrumb" aria-label="Breadcrumb"><button onClick={() => setOpenTag(null)}>Tag media</button><span>/</span><span aria-current="page">{openTag.tag}: {openTag.value}</span></nav>
    <div className="pw-tag-heading"><div><span className="pw-eyebrow">Taxonomy tag</span><h2>{openTag.value}</h2><p>{openTag.tag} · every file carrying this value, across all media collections</p></div><button onClick={() => setOpenTag(null)}>All tags</button></div>
    <p className="pw-help">Select any tagged photos or videos, then create a collection in an existing or new project.</p>
    <MediaBrowser key={`${openTag.tag}|${openTag.value}`} fixedTag={{ tag: openTag.tag, value: openTag.value }} onCollect={onCollect} onAddToExisting={onAddToExisting} />
  </>

  if (person) return <>
    <nav className="pw-breadcrumb" aria-label="Breadcrumb"><button onClick={() => setPersonId(null)}>Tag media</button><span>/</span><span aria-current="page">{person.name}</span></nav>
    <div className="pw-tag-heading"><div><span className="pw-eyebrow">Tagged person</span><h2>{person.name}</h2><p>{person.team || 'No team'} · {person.mediaCount} tagged file{person.mediaCount === 1 ? '' : 's'} across {person.shootCount} media collection{person.shootCount === 1 ? '' : 's'}</p></div><button onClick={() => setPersonId(null)}>All tags</button></div>
    <p className="pw-help">Select any tagged photos or videos, then create a collection in an existing or new project.</p>
    <MediaBrowser key={person.id} personId={person.id} onCollect={onCollect} onAddToExisting={onAddToExisting} />
  </>

  return <>
    <div className="pw-toolbar"><label className="pw-search"><span className="sr-only">Search tags</span><input type="search" placeholder="Search people, teams or tag values…" value={search} onChange={event => setSearch(event.target.value)} /></label><span>{people.data?.length ?? 0} tags</span><button onClick={onManagePeople}>Manage people</button></div>
    {selectedIds.size > 0 && <div className="pw-selection"><strong>{selectedIds.size} selected</strong><button onClick={() => setSelectedIds(new Set(visible.map(item => item.id)))}>Select all</button><button disabled={!selectedIds.size} onClick={() => setSelectedIds(new Set())}>Clear</button><button className="danger" disabled={!selectedIds.size || deleteSelected.isPending} onClick={() => {
      if (window.confirm(`Delete ${selectedIds.size} tag${selectedIds.size === 1 ? '' : 's'}? This deletes the player profile${selectedIds.size === 1 ? '' : 's'} entirely.`)) deleteSelected.mutate()
    }}>{deleteSelected.isPending ? 'Deleting…' : 'Delete selected'}</button></div>}
    <p className="pw-help">People you name while reviewing media appear here automatically. Open a tag to find every recognised appearance and add selected media to collections.</p>
    {error && <p role="alert" className="pw-error">{error}</p>}
    {people.isPending ? <p role="status" className="pw-loading">Loading tagged media…</p> : people.isError ? <div className="pw-empty"><h2>Couldn’t load tags</h2><button onClick={() => void people.refetch()}>Try again</button></div> : visible.length > 0 ? <div className="pw-tag-list">
      <div className="pw-tag-row pw-tag-header"><input type="checkbox" aria-label={allVisibleSelected ? 'Clear selection' : 'Select all people'} checked={allVisibleSelected} ref={input => { if (input) input.indeterminate = someVisibleSelected && !allVisibleSelected }} onChange={toggleAll} /><div className="pw-tag-open pw-tag-header-cells"><span>Person</span><span>Files</span><span>Collections</span></div><span className="pw-tag-header-actions">{allVisibleSelected ? 'All selected' : someVisibleSelected ? `${selectedIds.size} selected` : 'Select all'}</span></div>
      {visible.map(item => <div className={`pw-tag-row${selectedIds.has(item.id) ? ' selected' : ''}`} key={item.id} onContextMenu={event => openMenu(item, event)}><input type="checkbox" aria-label={`Select ${item.name}`} checked={selectedIds.has(item.id)} onChange={() => toggleSelected(item.id)} /><button className="pw-tag-open" onClick={() => toggleSelected(item.id)} onDoubleClick={() => setPersonId(item.id)}><span><strong>{item.name}</strong><small>{item.team || 'No team'}</small></span><span>{item.mediaCount} files</span><span>{item.shootCount} collection{item.shootCount === 1 ? '' : 's'}</span></button><button disabled={addingId !== null || item.mediaCount === 0} onClick={async () => {
      setAddingId(item.id); setError('')
      try {
        const tagged = await loadTaggedMedia(item.id)
        if (tagged.length === 0) throw new Error('No tagged media is available for this person yet.')
        onCollect(tagged)
      } catch (caught) { setError(String(caught)) } finally { setAddingId(null) }
    }}>{addingId === item.id ? 'Loading…' : 'Add to collection'}</button><button className="pw-tag-delete" aria-label={`Delete ${item.name}`} title={`Delete “${item.name}”`} disabled={deleteOne.isPending} onClick={() => { if (window.confirm(`Delete “${item.name}”? This deletes the player profile entirely.`)) deleteOne.mutate(item) }}><Icon name="remove" /></button></div>)}</div> : <div className="pw-empty"><h2>{people.data?.length ? 'No matching tags' : 'No tagged media yet'}</h2><p>{people.data?.length ? 'Try another person or team name.' : 'Name a person from Identify & organise. Their recognised photos and videos will appear here.'}</p>{people.data?.length ? <button onClick={() => setSearch('')}>Clear search</button> : null}</div>}
    <TaxonomyTagList tags={taxonomy.data ?? []} search={search} onOpen={(tag, value) => setOpenTag({ tag, value })} onCollect={async (tag, value) => {
      setError('')
      try {
        await api.propagateGroupTags(null).catch(() => 0)
        const tagged = await api.mediaWithTag(tag, value)
        if (tagged.length === 0) throw new Error(`No files carry ${tag}: ${value} yet. Tag a group on Auto tags, or files in the media library, first.`)
        useUi.getState().setPendingCollectionTag({ tag, value })
        onCollect(tagged)
      } catch (caught) { setError(String(caught)) }
    }} />
    {menu && <PersonContextMenu person={menu.person} all={people.data ?? []} x={menu.x} y={menu.y} onClose={() => setMenu(null)} onOpen={() => setPersonId(menu.person.id)} />}
  </>
}

export async function loadTaggedMedia(personId: number) {
  const result: Media[] = []
  const pageSize = 500
  for (let offset = 0; offset < 10_000; offset += pageSize) {
    const page = await api.listMedia({ personId, offset, limit: pageSize })
    result.push(...page)
    if (page.length < pageSize) break
  }
  return result
}

/**
 * The taxonomy as tagged media: every tag value that is on at least one
 * file, with how many, beside the people. The values themselves come from
 * the imported list; what appears here is which of them have been used.
 */
function TaxonomyTagList({ tags, search, onOpen, onCollect }: { tags: TagSummary[]; search: string; onOpen: (tag: string, value: string) => void; onCollect: (tag: string, value: string) => Promise<void> }) {
  const [busy, setBusy] = useState<string | null>(null)
  const query = search.trim().toLocaleLowerCase()
  const rows = tags.flatMap(tag => tag.values.map(value => ({ tag: tag.name, value: value.value, uses: value.uses })))
    .filter(row => row.uses > 0)
    .filter(row => !query || row.tag.toLocaleLowerCase().includes(query) || row.value.toLocaleLowerCase().includes(query))
    .sort((a, b) => a.tag.localeCompare(b.tag) || a.value.localeCompare(b.value))
  const unused = tags.reduce((n, tag) => n + tag.values.filter(value => value.uses === 0).length, 0)
  if (tags.length === 0) return null
  return <section className="pw-taxonomy-tagged" aria-labelledby="taxonomy-tagged">
    <h2 id="taxonomy-tagged" className="pw-section-heading">Taxonomy tags</h2>
    <p className="pw-help">Values from your imported tag list that are on media — tagged on Auto tags groups or on files directly. Open one to see everything carrying it.{unused > 0 && ` ${unused} value${unused === 1 ? '' : 's'} in the list ${unused === 1 ? 'is' : 'are'} not on any media yet.`}</p>
    {rows.length === 0 ? <div className="pw-empty"><h2>{query ? 'No matching tag values' : 'No taxonomy tags on media yet'}</h2><p>{query ? 'Try another tag or value.' : 'Tag a group on Auto tags, or select files in the media library and use Tag selected.'}</p></div> : <div className="pw-tag-list">{rows.map(row => <div className="pw-tag-row" key={`${row.tag}\u0000${row.value}`}><button className="pw-tag-open" onClick={() => onOpen(row.tag, row.value)}><span><strong>{row.value}</strong><small>{row.tag}</small></span><span>{row.uses} file{row.uses === 1 ? '' : 's'}</span><span>Open tagged media</span></button><button disabled={busy === `${row.tag}\u0000${row.value}`} onClick={async () => { setBusy(`${row.tag}\u0000${row.value}`); try { await onCollect(row.tag, row.value) } finally { setBusy(null) } }}>{busy === `${row.tag}\u0000${row.value}` ? 'Loading…' : 'Add to collection'}</button></div>)}</div>}
  </section>
}
