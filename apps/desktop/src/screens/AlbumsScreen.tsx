/**
 * The AI Albums screen (§23): player albums, multi-player pairings, and the
 * "Needs Review" clusters, with photo/video filters on the open album.
 */

import { useEffect, useMemo, useState } from 'react'
import { useMutation, useQuery, useQueryClient } from '@tanstack/react-query'
import {
  GROUP_SIZE_CAP,
  type Album,
  type ClusterSummary,
  type MediaPickState,
  type MediaType,
} from '@skwad/shared-types'
import * as api from '../api'
import { formatConfidence, formatCount, groupSizeName, thumbUrl } from '../media'
import { FaceCrop } from '../components/FaceCrop'
import { MediaGrid } from '../components/MediaGrid'
import { ProgressPanel } from '../components/ProgressPanel'
import { Modal } from '../components/Modal'
import { TAG_KEYS, TagFilter, TagNamesDatalist, TagPicker, useTaxonomy } from '../components/TagPicker'
import { useUi } from '../store'

/**
 * `withTags` is the Auto tags view: every automatic group carries a tag
 * picker, and the groups are split into "Identified" (recognised people,
 * pairings, teams) and "Needs review" (unnamed face groups) so a tag put on
 * one collection's review pile never looks like part of another's.
 */
export function AlbumsScreen({ onAddToCollection, withTags = false }: { onAddToCollection?: (album: Album) => void; withTags?: boolean } = {}) {
  const shootId = useUi((s) => s.activeShootId)
  if (shootId === null) return <div className="empty-state">Open a shoot first.</div>
  return <AlbumsBody shootId={shootId} onAddToCollection={onAddToCollection} withTags={withTags} />
}

/**
 * The key an automatic group's tags are stored under. Albums are rebuilt
 * with new ids after every analysis, so the key names what the group *is*
 * within its collection — the person, the pairing, the team, the bucket —
 * and the tags survive a rebuild. Clusters keep their id until the shoot is
 * re-analysed, which also discards them.
 */
export function albumTagKey(album: Album): string {
  switch (album.albumType) {
    case 'player':
      return `shoot:${album.shootId}/person:${album.personIds[0] ?? album.name}`
    case 'multiPlayer':
      return `shoot:${album.shootId}/persons:${[...album.personIds].sort((a, b) => a - b).join('+')}`
    default:
      return `shoot:${album.shootId}/${album.albumType}:${album.name}`
  }
}

export function clusterTagKey(cluster: ClusterSummary): string {
  return `shoot:${cluster.shootId}/cluster:${cluster.id}`
}

function AlbumsBody({ shootId, onAddToCollection, withTags = false }: { shootId: number; onAddToCollection?: (album: Album) => void; withTags?: boolean }) {
  const [groupingChoice, setGroupingChoice] = useState<'face' | 'size'>('face')
  const [appliedGrouping, setAppliedGrouping] = useState<'face' | 'size'>('face')
  const [openAlbum, setOpenAlbum] = useState<Album | null>(null)
  const [typeFilter, setTypeFilter] = useState<MediaType | 'all'>('all')
  const [namingCluster, setNamingCluster] = useState<ClusterSummary | null>(null)
  const [selectedPersonIds, setSelectedPersonIds] = useState<number[]>([])
  const [search, setSearch] = useState('')
  // The Tag → Value filter on the Auto tags view; empty value means off.
  const [tagFilter, setTagFilter] = useState<{ tag: string; value: string }>({ tag: '', value: '' })

  const shoot = useQuery({ queryKey: ['shoots', shootId], queryFn: () => api.getShoot(shootId) })
  const albums = useQuery({ queryKey: ['albums', shootId], queryFn: () => api.listAlbums(shootId) })
  const clusters = useQuery({
    queryKey: ['clusters', shootId],
    queryFn: () => api.listClusters(shootId, false),
  })
  // Tags on every group at once, for the filter: one request rather than
  // one per card. The cards keep their own per-asset queries for editing.
  const albumKeys = useMemo(() => (albums.data ?? []).map(albumTagKey), [albums.data])
  const clusterKeys = useMemo(() => (clusters.data ?? []).map(clusterTagKey), [clusters.data])
  const albumTags = useQuery({
    queryKey: ['assetTags', 'album', 'batch', shootId, albumKeys.length],
    queryFn: () => api.assetsTags('album', albumKeys),
    enabled: withTags && albumKeys.length > 0,
  })
  const clusterTags = useQuery({
    queryKey: ['assetTags', 'cluster', 'batch', shootId, clusterKeys.length],
    queryFn: () => api.assetsTags('cluster', clusterKeys),
    enabled: withTags && clusterKeys.length > 0,
  })
  const carriesTag = (key: string, lookup: Record<string, { tag: string; value: string }[]> | undefined) => {
    if (!tagFilter.value) return true
    const wanted = tagFilter.value.trim().toLowerCase()
    return (lookup?.[key] ?? []).some(
      (item) => item.value.toLowerCase() === wanted && (!tagFilter.tag || item.tag.toLowerCase() === tagFilter.tag.toLowerCase()),
    )
  }
  // Only for the search: an album carries person ids, not the team they play
  // for, and a shoot is usually easier to remember by team than by roster.
  const people = useQuery({ queryKey: ['people'], queryFn: () => api.listPeople(null) })

  const queryClient = useQueryClient()
  const pushNotice = useUi((s) => s.pushNotice)
  const openExport = useUi((s) => s.openExport)
  const regenerate = useMutation({
    mutationFn: () => api.regenerateAlbums(shootId),
    onSuccess: (count) => {
      queryClient.invalidateQueries({ queryKey: ['albums', shootId] })
      pushNotice({ level: 'success', message: `Rebuilt ${count} album(s).` })
    },
    onError: (e) => pushNotice({ level: 'error', message: String(e) }),
  })

  const query = search.trim().toLowerCase()

  const teamOf = useMemo(
    () => new Map((people.data ?? []).map((person) => [person.id, (person.team ?? '').toLowerCase()])),
    [people.data],
  )

  const grouped = useMemo(() => {
    const all = albums.data ?? []
    // A player album is named after the person, so their name matches directly.
    // A team name has to be looked up through the people the album is made of —
    // that is what makes "Gods Reign" find every player on it, and it works on a
    // pairing album too, where either player's team should count.
    const matches = (album: Album) =>
      (query === '' ||
        album.name.toLowerCase().includes(query) ||
        album.personIds.some((id) => teamOf.get(id)?.includes(query))) &&
      carriesTag(albumTagKey(album), albumTags.data)

    const ofType = (type: Album['albumType']) => all.filter((a) => a.albumType === type)
    const allPlayers = ofType('player')
    return {
      // Unfiltered, because export selection is keyed on it: a player hidden by
      // the search must not silently drop out of a selection already made.
      allPlayers,
      players: allPlayers.filter(matches),
      multi: ofType('multiPlayer').filter(matches),
      teams: ofType('team').filter(matches),
      unidentified: ofType('unidentified').filter(matches),
      // Already ordered by size from the backend; sortOrder holds the bucket.
      groupSize: ofType('groupSize').filter(matches),
    }
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [albums.data, query, teamOf, tagFilter, albumTags.data])

  // A cluster is searchable by the label the app gave it ("Unknown Person 7")
  // and by the player it has been matched to but not yet confirmed as.
  const visibleClusters = useMemo(() => {
    const all = (clusters.data ?? []).filter((cluster) => carriesTag(clusterTagKey(cluster), clusterTags.data))
    if (query === '') return all
    return all.filter(
      (cluster) =>
        cluster.label.toLowerCase().includes(query) ||
        (cluster.personName ?? '').toLowerCase().includes(query),
    )
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [clusters.data, query, tagFilter, clusterTags.data])

  /** What the search is hiding, so a thin screen never looks like an empty one. */
  const counts = useMemo(() => {
    const all = albums.data ?? []
    if (appliedGrouping === 'size') {
      return {
        shown: grouped.groupSize.length,
        total: all.filter((a) => a.albumType === 'groupSize').length,
        noun: 'albums',
      }
    }
    return {
      shown:
        grouped.players.length +
        grouped.multi.length +
        grouped.teams.length +
        grouped.unidentified.length +
        visibleClusters.length,
      total: all.filter((a) => a.albumType !== 'groupSize').length + (clusters.data?.length ?? 0),
      noun: 'albums and review groups',
    }
  }, [albums.data, clusters.data, grouped, visibleClusters, appliedGrouping])

  const playerIds = useMemo(
    () => grouped.allPlayers.flatMap((album) => album.personIds.slice(0, 1)),
    [grouped.allPlayers],
  )
  const validSelectedPersonIds = selectedPersonIds.filter((id) => playerIds.includes(id))
  const allPlayersSelected =
    playerIds.length > 0 && playerIds.every((id) => validSelectedPersonIds.includes(id))

  useEffect(() => setSelectedPersonIds([]), [shootId])

  const togglePerson = (personId: number) => {
    setSelectedPersonIds((current) =>
      current.includes(personId)
        ? current.filter((id) => id !== personId)
        : [...current, personId],
    )
  }

  if (openAlbum) {
    return (
      <AlbumDetail
        album={openAlbum}
        onAddToCollection={onAddToCollection}
        typeFilter={typeFilter}
        setTypeFilter={setTypeFilter}
        onBack={() => setOpenAlbum(null)}
      />
    )
  }

  return (
    <div className={withTags ? 'album-sections' : undefined}>
      {withTags && <TagNamesDatalist />}
      <div className="workspace-header">
        <h1>{shoot.data?.name ?? 'AI Albums'}</h1>
        <div className="actions">
          <button onClick={() => regenerate.mutate()} disabled={regenerate.isPending}>
            {regenerate.isPending ? 'Rebuilding…' : 'Regenerate albums'}
          </button>
        </div>
      </div>

      <ProgressPanel shootId={shootId} />

      <div className="filter-bar">
        <input
          value={search}
          onChange={(event) => setSearch(event.target.value)}
          placeholder="Search a player, team or review group…"
          style={{ minWidth: 280 }}
          spellCheck={false}
        />
        {withTags && <TagFilter tag={tagFilter.tag} value={tagFilter.value} onChange={setTagFilter} compact />}
        {query !== '' ? (
          <>
            <button className="small" onClick={() => setSearch('')}>
              Clear
            </button>
            <span className="hint">
              {formatCount(counts.shown)} of {formatCount(counts.total)} {counts.noun} match “
              {search.trim()}”.
            </span>
          </>
        ) : (
          <span className="hint">
            {formatCount(counts.total)} {counts.noun} in this shoot.
          </span>
        )}
      </div>

      <div className="filter-bar grouping-bar">
        <label>
          <span className="hint">Group media by</span>
          <select
            value={groupingChoice}
            onChange={(event) => setGroupingChoice(event.target.value as 'face' | 'size')}
          >
            <option value="face">Face / person</option>
            <option value="size">Number of persons</option>
          </select>
        </label>
        <button
          className="small primary"
          disabled={groupingChoice === appliedGrouping}
          onClick={() => setAppliedGrouping(groupingChoice)}
        >
          Apply grouping
        </button>
        <span className="hint">
          {appliedGrouping === 'face'
            ? 'Showing InsightFace-recognised people and unknown face groups.'
            : 'Showing files by how many people are visible, regardless of identity.'}
        </span>
      </div>

      {query !== '' && counts.shown === 0 ? (
        <div className="empty-state">
          Nothing here matches “{search.trim()}”. Search a player's name, their team, or a review
          group like “Unknown Person 3”.
        </div>
      ) : appliedGrouping === 'face' ? (
        <>
          {/* While searching, a section with no matches is noise rather than
              information — the count above already says what was filtered out. */}
          {withTags && <h2 className="album-super">Identified</h2>}
          {(grouped.players.length > 0 || query === '') && (
          <Section title="Players">
            {grouped.players.length === 0 && (
              <div className="hint">
                Player albums appear once faces are recognised or clusters are named.
              </div>
            )}
            {grouped.players.length > 0 && (
              <div className="filter-bar album-export-bar">
                <button
                  className="small"
                  onClick={() => setSelectedPersonIds(allPlayersSelected ? [] : playerIds)}
                >
                  {allPlayersSelected ? 'Clear selection' : 'Select all players'}
                </button>
                <span className="hint">
                  {validSelectedPersonIds.length === 0
                    ? 'Select one or more named person groups to copy.'
                    : `${formatCount(validSelectedPersonIds.length)} group(s) selected`}
                </span>
                <button
                  className="small primary"
                  disabled={validSelectedPersonIds.length === 0}
                  onClick={() => openExport(validSelectedPersonIds)}
                >
                  Copy selected groups…
                </button>
              </div>
            )}
            <div className="card-grid">
              {grouped.players.map((album) => {
                const personId = album.personIds[0]
                return (
                  <AlbumCard
                    key={album.id}
                    album={album}
                    onOpen={() => setOpenAlbum(album)}
                    selected={personId !== undefined && validSelectedPersonIds.includes(personId)}
                    onToggle={personId === undefined ? undefined : () => togglePerson(personId)}
                    onAddToCollection={onAddToCollection}
                    withTags={withTags}
                  />
                )
              })}
            </div>
          </Section>
          )}

          {grouped.multi.length > 0 && (
            <Section title="Multiple Players">
              <div className="card-grid">
                {grouped.multi.map((album) => (
                  <AlbumCard key={album.id} album={album} onOpen={() => setOpenAlbum(album)} onAddToCollection={onAddToCollection} withTags={withTags} />
                ))}
              </div>
            </Section>
          )}

          {grouped.teams.length > 0 && (
            <Section title="Teams">
              <div className="card-grid">
                {grouped.teams.map((album) => (
                  <AlbumCard key={album.id} album={album} onOpen={() => setOpenAlbum(album)} onAddToCollection={onAddToCollection} withTags={withTags} />
                ))}
              </div>
            </Section>
          )}

          {withTags && <h2 className="album-super">Needs review</h2>}
          {(visibleClusters.length + grouped.unidentified.length > 0 || query === '') && (
          <Section title="Needs Review">
            {visibleClusters.length === 0 && grouped.unidentified.length === 0 && (
              <div className="hint">Nothing waiting — every detected face is identified.</div>
            )}
            <div className="card-grid">
              {visibleClusters.map((cluster) => (
                <ClusterCard
                  key={cluster.id}
                  cluster={cluster}
                  onName={() => setNamingCluster(cluster)}
                  withTags={withTags}
                />
              ))}
              {grouped.unidentified.map((album) => (
                <AlbumCard key={album.id} album={album} onOpen={() => setOpenAlbum(album)} onAddToCollection={onAddToCollection} withTags={withTags} />
              ))}
            </div>
          </Section>
          )}
        </>
      ) : (
        <Section title="By number of persons">
          <div className="hint" style={{ marginBottom: 10 }}>
            Each file appears once, based on the number of distinct people visible in it.
          </div>
          <div className="card-grid">
            {grouped.groupSize.map((album) => (
              <AlbumCard key={album.id} album={album} onOpen={() => setOpenAlbum(album)} onAddToCollection={onAddToCollection} withTags={withTags} />
            ))}
          </div>
        </Section>
      )}

      {namingCluster && (
        <NameClusterModal cluster={namingCluster} onClose={() => setNamingCluster(null)} />
      )}
    </div>
  )
}

function Section(props: { title: string; children: React.ReactNode }) {
  return (
    <div className="section">
      <h2>{props.title}</h2>
      {props.children}
    </div>
  )
}

function AlbumCard({
  album,
  onOpen,
  selected = false,
  onToggle,
  onAddToCollection,
  withTags = false,
}: {
  album: Album
  onOpen: () => void
  selected?: boolean
  onToggle?: () => void
  onAddToCollection?: (album: Album) => void
  withTags?: boolean
}) {
  const queryClient = useQueryClient()
  const pushNotice = useUi((s) => s.pushNotice)
  const navigate = useUi((s) => s.navigate)

  // An album is regenerated from faces and cannot be edited by hand; turning it
  // into a group is how its contents become the editor's to correct and export.
  const toGroup = useMutation({
    mutationFn: () => api.groupFromAlbum(album.id),
    onSuccess: async (group) => {
      await queryClient.invalidateQueries({ queryKey: ['groups', album.shootId] })
      await queryClient.invalidateQueries({ queryKey: ['groupStats', album.shootId] })
      await queryClient.invalidateQueries({ queryKey: ['groupLinks', album.shootId] })
      pushNotice({
        level: 'success',
        message: `Group “${group.name}” now holds ${formatCount(group.mediaCount)} file(s). Correct it on the Sort screen.`,
      })
      navigate('groups')
    },
    onError: (e) => pushNotice({ level: 'error', message: String(e instanceof Error ? e.message : e) }),
  })
  return (
    <div className={`card shoot-card${selected ? ' selected' : ''}`} onClick={onOpen}>
      {onToggle && (
        <label className="album-select" onClick={(event) => event.stopPropagation()}>
          <input type="checkbox" checked={selected} onChange={onToggle} />
          Select for copy
        </label>
      )}
      {album.coverMediaId != null && (
        <div className="media-tile" style={{ marginBottom: 10 }}>
          <img src={thumbUrl(album.coverMediaId)} alt="" loading="lazy" />
        </div>
      )}
      <div className="title">
        <span>{album.name}</span>
        <span className="badge">{formatCount(album.mediaCount)}</span>
      </div>
      <div className="stats">
        <span>
          {formatCount(album.photoCount)} photos · {formatCount(album.videoCount)} videos
        </span>
      </div>
      {withTags && <TagPicker kind="album" assetKey={albumTagKey(album)} groupId={album.id} compact />}
      <div style={{ marginTop: 10 }} onClick={(e) => e.stopPropagation()}>
        {onAddToCollection ? <button className="small primary" onClick={() => onAddToCollection(album)}>Add to collection</button> : <button className="small" disabled={toGroup.isPending} onClick={() => toGroup.mutate()}>{toGroup.isPending ? 'Adding…' : 'Make this a group'}</button>}
      </div>
    </div>
  )
}

function ClusterCard({ cluster, onName, withTags = false }: { cluster: ClusterSummary; onName: () => void; withTags?: boolean }) {
  return (
    <div className="card shoot-card" onClick={onName}>
      {cluster.coverMediaId != null && (
        <div className="media-tile" style={{ marginBottom: 10 }}>
          <img src={thumbUrl(cluster.coverMediaId)} alt="" loading="lazy" />
        </div>
      )}
      <div className="title">
        <span>{cluster.label}</span>
        <span className="badge processing">unnamed</span>
      </div>
      <div className="stats">
        <span>
          {formatCount(cluster.mediaCount)} media · {formatCount(cluster.faceCount)} faces
        </span>
      </div>
      {withTags && <TagPicker kind="cluster" assetKey={clusterTagKey(cluster)} groupId={cluster.id} compact />}
      <div style={{ marginTop: 10 }} onClick={(e) => e.stopPropagation()}>
        <button className="small primary" onClick={onName}>
          Name this person
        </button>
      </div>
    </div>
  )
}

/**
 * Naming a cluster is the moment the app "learns" a player (§7).
 *
 * The name is picked from what the studio already imported — the roster
 * (Auto team-up) and any person-type tag in the taxonomy (Player, Person…)
 * — or from people already named. A name that is in none of them is typed
 * in, and is then stored the same way: the person is created, the name is
 * recorded under the person tag in the taxonomy so it is in the list next
 * time (and in an export), and the group's files are tagged with it.
 */
function NameClusterModal({ cluster, onClose }: { cluster: ClusterSummary; onClose: () => void }) {
  const [choice, setChoice] = useState('')
  const [name, setName] = useState('')
  const [team, setTeam] = useState('')
  const [error, setError] = useState<string | null>(null)
  const queryClient = useQueryClient()
  const pushNotice = useUi((s) => s.pushNotice)
  const people = useQuery({ queryKey: ['people'], queryFn: () => api.listPeople(null) })
  const roster = useQuery({ queryKey: ['roster'], queryFn: api.listRoster })
  const taxonomy = useTaxonomy()

  // The tag names are people-shaped: whichever the studio already has wins,
  // and "Player" is created when none does.
  const personTag = useMemo(() => {
    const tags = taxonomy.data ?? []
    return tags.find((t) => /^(player|players|person|people|name|names|talent|athlete|member|artist)$/i.test(t.name))?.name ?? 'Player'
  }, [taxonomy.data])

  // Every name on offer, grouped by where it came from. The same name in
  // two places is listed once, under the first.
  const options = useMemo(() => {
    const seen = new Set<string>()
    const take = (label: string, entries: { name: string; team: string | null; detail?: string }[]) => {
      const rows = entries.filter((e) => {
        const key = e.name.trim().toLowerCase()
        if (!key || seen.has(key)) return false
        seen.add(key)
        return true
      })
      return rows.length ? [{ label, rows }] : []
    }
    return [
      ...take('From the roster', (roster.data ?? []).map((r) => ({ name: r.playerName || r.ign, team: r.team || null, detail: [r.ign !== (r.playerName || r.ign) ? r.ign : '', r.team].filter(Boolean).join(' · ') }))),
      ...take(`From the ${personTag} tag`, (taxonomy.data ?? []).filter((t) => t.name.toLowerCase() === personTag.toLowerCase()).flatMap((t) => t.values.map((v) => ({ name: v.value, team: null })))),
      ...take('Already named', (people.data ?? []).map((p) => ({ name: p.name, team: p.team || null, detail: p.team || '' }))),
    ]
  }, [roster.data, taxonomy.data, people.data, personTag])

  const pick = (value: string) => {
    setChoice(value)
    if (value === '__new') { setName(''); setTeam(''); return }
    const row = options.flatMap((g) => g.rows).find((r) => r.name === value)
    setName(row?.name ?? '')
    setTeam(row?.team ?? '')
  }

  // The faces themselves, not the photos they came from: a cover photo with
  // four people in it does not say which one this group is.
  const samples = useQuery({
    queryKey: ['faces', 'cluster', cluster.id],
    queryFn: () => api.listFaces({ clusterId: cluster.id, limit: 8 }),
  })

  const nameIt = useMutation({
    mutationFn: async () => {
      const finalName = name.trim()
      const person = await api.nameCluster(cluster.id, finalName, team.trim() || null)
      // Stored as taxonomy too: the name joins the person tag's values, and
      // the group's files carry it, so smart collections and the tag filters
      // see this person from now on. Never fatal — the naming itself is done.
      try {
        await api.assignGroupTag('cluster', cluster.id, clusterTagKey(cluster), personTag, finalName)
      } catch (e) {
        console.warn('could not tag the group with the person name', e)
      }
      return person
    },
    onSuccess: async (person) => {
      pushNotice({
        level: 'success',
        message: `${cluster.faceCount} faces added to ${person.name}'s library.`,
      })
      await queryClient.invalidateQueries({ queryKey: ['clusters'] })
      await queryClient.invalidateQueries({ queryKey: TAG_KEYS.tags })
      await queryClient.invalidateQueries({ queryKey: ['assetTags'] })
      await api.regenerateAlbums(cluster.shootId)
      await queryClient.invalidateQueries({ queryKey: ['albums'] })
      onClose()
    },
    onError: (e) => setError(String(e)),
  })

  const isNew = choice === '__new'
  const known = options.flatMap((g) => g.rows).some((r) => r.name.toLowerCase() === name.trim().toLowerCase())

  return (
    <Modal title={`Who is ${cluster.label}?`} onClose={onClose}>
      {(samples.data?.length ?? 0) > 0 && (
        <div className="face-sample-strip">
          {samples.data?.map((face) => (
            <div key={face.id} className="face-sample" title={face.mediaFilename} style={{ width: 112, height: 112 }}>
              <FaceCrop mediaId={face.mediaId} bbox={face.bbox} />
            </div>
          ))}
        </div>
      )}
      <div className="hint">
        {formatCount(cluster.faceCount)} faces across {formatCount(cluster.mediaCount)} files.
        Naming them adds every face to this player's library, so future shoots recognise them
        automatically.
      </div>
      <label className="field">
        <span>Player name</span>
        <select autoFocus value={choice} onChange={(e) => pick(e.target.value)}>
          <option value="">Choose a name…</option>
          {options.map((group) => (
            <optgroup key={group.label} label={group.label}>
              {group.rows.map((row) => (
                <option key={row.name} value={row.name}>
                  {row.name}{row.detail ? ` — ${row.detail}` : ''}
                </option>
              ))}
            </optgroup>
          ))}
          <option value="__new">Not in the list — type a new name…</option>
        </select>
        {options.length === 0 && !roster.isPending && !taxonomy.isPending && (
          <span className="hint">No imported names yet. Import a roster (Settings → Auto team-up) or a tag list with a {personTag} tag on Auto tags, or type a name below.</span>
        )}
      </label>
      {isNew && (
        <label className="field">
          <span>New name</span>
          <input autoFocus value={name} onChange={(e) => setName(e.target.value)} placeholder="Jonathan" spellCheck={false} />
          <span className="hint">
            {known
              ? 'This name is already in the list — pick it above instead.'
              : `Saved as a person, and added to the ${personTag} tag in the taxonomy so it is in the list next time.`}
          </span>
        </label>
      )}
      <label className="field">
        <span>Team (optional)</span>
        <input value={team} onChange={(e) => setTeam(e.target.value)} placeholder="Gods Reign" />
      </label>
      {error && <div style={{ color: 'var(--error)', fontSize: 13 }}>{error}</div>}
      <div className="buttons">
        <button onClick={onClose}>Cancel</button>
        <button className="primary" disabled={!name.trim() || (isNew && known) || nameIt.isPending} onClick={() => nameIt.mutate()}>
          {nameIt.isPending ? 'Saving…' : 'Confirm'}
        </button>
      </div>
    </Modal>
  )
}

function AlbumDetail(props: {
  album: Album
  onAddToCollection?: (album: Album) => void
  typeFilter: MediaType | 'all'
  setTypeFilter: (f: MediaType | 'all') => void
  onBack: () => void
}) {
  const { album, typeFilter } = props
  // Narrowing an existing album by group size is the useful cross-filter:
  // "Jonathan's solo shots". Redundant inside a size album, so hidden there.
  const [sizeFilter, setSizeFilter] = useState<number | null>(null)
  const [qualityFilter, setQualityFilter] = useState<'all' | 'best' | 'duplicates'>('all')
  const [editorialFilter, setEditorialFilter] = useState<'all' | MediaPickState>('all')
  const [minRating, setMinRating] = useState(0)
  const [mediaSort, setMediaSort] = useState<'capturedAt' | 'quality' | 'rating' | 'filename'>(
    'capturedAt',
  )
  const showSizeFilter = album.albumType !== 'groupSize'
  const personId = album.albumType === 'player' ? (album.personIds[0] ?? null) : null
  const openExport = useUi((s) => s.openExport)
  const pushNotice = useUi((s) => s.pushNotice)
  const queryClient = useQueryClient()

  const media = useQuery({
    queryKey: [
      'media',
      album.shootId,
      'album',
      album.id,
      typeFilter,
      sizeFilter,
      qualityFilter,
      editorialFilter,
      minRating,
      mediaSort,
    ],
    queryFn: () =>
      api.listMedia({
        shootId: album.shootId,
        albumId: album.id,
        mediaType: typeFilter === 'all' ? null : typeFilter,
        groupSize: sizeFilter,
        onlyBestShots: qualityFilter === 'best',
        onlyDuplicates: qualityFilter === 'duplicates',
        pickState: editorialFilter === 'all' ? null : editorialFilter,
        minRating: minRating || null,
        sort: mediaSort,
        limit: 2000,
      }),
  })

  const matchedFaces = useQuery({
    queryKey: ['faces', album.shootId, 'person-confidence', personId],
    queryFn: () => api.listFaces({ shootId: album.shootId, personId, limit: 5000 }),
    enabled: personId !== null,
  })

  const confidenceLabels = useMemo(() => {
    const labels = new Map<number, string>()
    if (personId === null) return labels

    const bestByMedia = new Map<number, number | null>()
    for (const face of matchedFaces.data ?? []) {
      const current = bestByMedia.get(face.mediaId)
      if (face.recognitionConfidence !== null && (current == null || face.recognitionConfidence > current)) {
        bestByMedia.set(face.mediaId, face.recognitionConfidence)
      } else if (!bestByMedia.has(face.mediaId)) {
        bestByMedia.set(face.mediaId, null)
      }
    }
    for (const [mediaId, confidence] of bestByMedia) {
      labels.set(mediaId, confidence === null ? 'Reference' : `Match ${formatConfidence(confidence)}`)
    }
    return labels
  }, [matchedFaces.data, personId])

  const setEditorial = useMutation({
    mutationFn: api.setMediaEditorial,
    onSuccess: (changed) => {
      pushNotice({ level: 'success', message: `${formatCount(changed)} file(s) updated.` })
      queryClient.invalidateQueries({ queryKey: ['media', album.shootId] })
    },
    onError: (e) => pushNotice({ level: 'error', message: String(e instanceof Error ? e.message : e) }),
  })

  return (
    <>
      <div className="workspace-header">
        <h1>{album.name}</h1>
        <div className="actions">
          {props.onAddToCollection && <button className="primary" onClick={() => props.onAddToCollection?.(album)}>Add to collection</button>}
          {personId !== null && (
            <button className={props.onAddToCollection ? '' : 'primary'} onClick={() => openExport([personId])}>
              Copy this person's group…
            </button>
          )}
          <button onClick={props.onBack}>← All albums</button>
        </div>
      </div>
      <div className="filter-bar">
        {(['all', 'photo', 'video'] as const).map((option) => (
          <button
            key={option}
            className={`small${typeFilter === option ? ' primary' : ''}`}
            onClick={() => props.setTypeFilter(option)}
          >
            {option === 'all'
              ? `All (${formatCount(album.mediaCount)})`
              : option === 'photo'
                ? `Photos (${formatCount(album.photoCount)})`
                : `Videos (${formatCount(album.videoCount)})`}
          </button>
        ))}

        {showSizeFilter && (
          <label className="checkbox-row" style={{ marginLeft: 'auto' }}>
            <span className="hint">Group size</span>
            <select
              value={sizeFilter ?? ''}
              onChange={(e) => setSizeFilter(e.target.value === '' ? null : Number(e.target.value))}
            >
              <option value="">Any</option>
              {Array.from({ length: GROUP_SIZE_CAP + 1 }, (_, size) => (
                <option key={size} value={size}>
                  {groupSizeName(size)}
                </option>
              ))}
            </select>
          </label>
        )}
        <label className="checkbox-row">
          <span className="hint">Photo picks</span>
          <select
            value={qualityFilter}
            onChange={(event) => setQualityFilter(event.target.value as typeof qualityFilter)}
          >
            <option value="all">All media</option>
            <option value="best">Best picks</option>
            <option value="duplicates">Duplicate groups</option>
          </select>
        </label>
        <label className="checkbox-row">
          <span className="hint">Sort</span>
          <select
            value={mediaSort}
            onChange={(event) => setMediaSort(event.target.value as typeof mediaSort)}
          >
            <option value="capturedAt">Capture time</option>
            <option value="quality">Best quality</option>
            <option value="rating">Rating</option>
            <option value="filename">Filename</option>
          </select>
        </label>
        <label className="checkbox-row">
          <span className="hint">Flag</span>
          <select
            value={editorialFilter}
            onChange={(event) => setEditorialFilter(event.target.value as typeof editorialFilter)}
          >
            <option value="all">All</option>
            <option value="pick">Picks</option>
            <option value="reject">Rejects</option>
            <option value="none">Unflagged</option>
          </select>
        </label>
        <label className="checkbox-row">
          <span className="hint">Stars</span>
          <select value={minRating} onChange={(event) => setMinRating(Number(event.target.value))}>
            <option value={0}>Any</option>
            {[1, 2, 3, 4, 5].map((rating) => (
              <option key={rating} value={rating}>
                {rating}+ ★
              </option>
            ))}
          </select>
        </label>
      </div>
      {sizeFilter !== null && media.data?.length === 0 && (
        <div className="hint" style={{ marginBottom: 10 }}>
          Nothing in this album has {groupSizeName(sizeFilter).toLowerCase()} in it.
        </div>
      )}
      {personId !== null && (
        <div className="hint" style={{ marginBottom: 10 }}>
          Match confidence comes from the InsightFace ArcFace similarity score. “Reference” means
          you named that face manually, so no AI confidence is invented.
        </div>
      )}
      {qualityFilter !== 'all' && (
        <div className="hint" style={{ marginBottom: 10 }}>
          {qualityFilter === 'best'
            ? 'Best picks keeps unique photos and the strongest sharpness/exposure result from each similar set.'
            : 'Duplicate groups use a local perceptual fingerprint; review before excluding any alternative.'}
        </div>
      )}
      <MediaGrid
        media={media.data ?? []}
        cornerLabels={confidenceLabels}
        onEditorial={(args) => setEditorial.mutate(args)}
        editorialBusy={setEditorial.isPending}
      />
      <div className="hint media-shortcuts-hint">
        Focus a thumbnail, then press 1–5 to rate, 0 to clear stars, P to toggle Pick, or X to
        toggle Reject.
      </div>
    </>
  )
}
