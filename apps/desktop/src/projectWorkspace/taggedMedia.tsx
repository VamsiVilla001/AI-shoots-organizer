import { useState } from 'react'
import { useQuery } from '@tanstack/react-query'
import type { Media } from '@skwad/shared-types'
import * as api from '../api'
import { MediaBrowser } from './mediaBrowser'

export function TaggedMedia({ onCollect, onManagePeople }: { onCollect: (media: Media[]) => void; onManagePeople: () => void }) {
  const [search, setSearch] = useState('')
  const [personId, setPersonId] = useState<number | null>(null)
  const [addingId, setAddingId] = useState<number | null>(null)
  const [error, setError] = useState('')
  const people = useQuery({ queryKey: ['people', null], queryFn: () => api.listPeople(null) })
  const person = people.data?.find(item => item.id === personId)
  const visible = (people.data ?? []).filter(item => {
    const query = search.trim().toLocaleLowerCase()
    return !query || item.name.toLocaleLowerCase().includes(query) || item.team?.toLocaleLowerCase().includes(query)
  })

  if (person) return <>
    <nav className="pw-breadcrumb" aria-label="Breadcrumb"><button onClick={() => setPersonId(null)}>Tag media</button><span>/</span><span aria-current="page">{person.name}</span></nav>
    <div className="pw-tag-heading"><div><span className="pw-eyebrow">Tagged person</span><h2>{person.name}</h2><p>{person.team || 'No team'} · {person.mediaCount} tagged file{person.mediaCount === 1 ? '' : 's'} across {person.shootCount} media collection{person.shootCount === 1 ? '' : 's'}</p></div><button onClick={() => setPersonId(null)}>All tags</button></div>
    <p className="pw-help">Select any tagged photos or videos, then create a collection in an existing or new project.</p>
    <MediaBrowser key={person.id} personId={person.id} onCollect={onCollect} />
  </>

  return <>
    <div className="pw-toolbar"><label className="pw-search"><span className="sr-only">Search tags</span><input type="search" placeholder="Search people or teams…" value={search} onChange={event => setSearch(event.target.value)} /></label><span>{people.data?.length ?? 0} tags</span><button onClick={onManagePeople}>Manage people</button></div>
    <p className="pw-help">People you name while reviewing media appear here automatically. Open a tag to find every recognised appearance and add selected media to collections.</p>
    {error && <p role="alert" className="pw-error">{error}</p>}
    {people.isPending ? <p role="status" className="pw-loading">Loading tagged media…</p> : people.isError ? <div className="pw-empty"><h2>Couldn’t load tags</h2><button onClick={() => void people.refetch()}>Try again</button></div> : visible.length > 0 ? <div className="pw-tag-list">{visible.map(item => <div className="pw-tag-row" key={item.id}><button className="pw-tag-open" onClick={() => setPersonId(item.id)}><span><strong>{item.name}</strong><small>{item.team || 'No team'}</small></span><span>{item.mediaCount} files</span><span>{item.shootCount} collection{item.shootCount === 1 ? '' : 's'}</span></button><button disabled={addingId !== null || item.mediaCount === 0} onClick={async () => {
      setAddingId(item.id); setError('')
      try {
        const tagged = await loadTaggedMedia(item.id)
        if (tagged.length === 0) throw new Error('No tagged media is available for this person yet.')
        onCollect(tagged)
      } catch (caught) { setError(String(caught)) } finally { setAddingId(null) }
    }}>{addingId === item.id ? 'Loading…' : 'Add to collection'}</button></div>)}</div> : <div className="pw-empty"><h2>{people.data?.length ? 'No matching tags' : 'No tagged media yet'}</h2><p>{people.data?.length ? 'Try another person or team name.' : 'Name a person from Identify & organise. Their recognised photos and videos will appear here.'}</p>{people.data?.length ? <button onClick={() => setSearch('')}>Clear search</button> : null}</div>}
  </>
}

async function loadTaggedMedia(personId: number) {
  const result: Media[] = []
  const pageSize = 500
  for (let offset = 0; offset < 10_000; offset += pageSize) {
    const page = await api.listMedia({ personId, offset, limit: pageSize })
    result.push(...page)
    if (page.length < pageSize) break
  }
  return result
}
