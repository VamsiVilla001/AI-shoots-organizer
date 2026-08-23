/**
 * The AI Albums screen (§23): player albums, multi-player pairings, and the
 * "Needs Review" clusters, with photo/video filters on the open album.
 */

import { useMemo, useState } from 'react'
import { useMutation, useQuery, useQueryClient } from '@tanstack/react-query'
import type { Album, ClusterSummary, MediaType } from '@teo/shared-types'
import * as api from '../api'
import { formatCount, thumbUrl } from '../media'
import { MediaGrid } from '../components/MediaGrid'
import { ProgressPanel } from '../components/ProgressPanel'
import { Modal } from '../components/Modal'
import { useUi } from '../store'

export function AlbumsScreen() {
  const shootId = useUi((s) => s.activeShootId)
  if (shootId === null) return <div className="empty-state">Open a shoot first.</div>
  return <AlbumsBody shootId={shootId} />
}

function AlbumsBody({ shootId }: { shootId: number }) {
  const [openAlbum, setOpenAlbum] = useState<Album | null>(null)
  const [typeFilter, setTypeFilter] = useState<MediaType | 'all'>('all')
  const [namingCluster, setNamingCluster] = useState<ClusterSummary | null>(null)

  const shoot = useQuery({ queryKey: ['shoots', shootId], queryFn: () => api.getShoot(shootId) })
  const albums = useQuery({ queryKey: ['albums', shootId], queryFn: () => api.listAlbums(shootId) })
  const clusters = useQuery({
    queryKey: ['clusters', shootId],
    queryFn: () => api.listClusters(shootId, false),
  })

  const grouped = useMemo(() => {
    const all = albums.data ?? []
    return {
      players: all.filter((a) => a.albumType === 'player'),
      multi: all.filter((a) => a.albumType === 'multiPlayer'),
      teams: all.filter((a) => a.albumType === 'team'),
      unidentified: all.filter((a) => a.albumType === 'unidentified'),
    }
  }, [albums.data])

  if (openAlbum) {
    return (
      <AlbumDetail
        album={openAlbum}
        typeFilter={typeFilter}
        setTypeFilter={setTypeFilter}
        onBack={() => setOpenAlbum(null)}
      />
    )
  }

  return (
    <>
      <div className="workspace-header">
        <h1>{shoot.data?.name ?? 'AI Albums'}</h1>
      </div>

      <ProgressPanel shootId={shootId} />

      <Section title="Players">
        {grouped.players.length === 0 && (
          <div className="hint">
            Player albums appear once faces are recognised or clusters are named.
          </div>
        )}
        <div className="card-grid">
          {grouped.players.map((album) => (
            <AlbumCard key={album.id} album={album} onOpen={() => setOpenAlbum(album)} />
          ))}
        </div>
      </Section>

      {grouped.multi.length > 0 && (
        <Section title="Multiple Players">
          <div className="card-grid">
            {grouped.multi.map((album) => (
              <AlbumCard key={album.id} album={album} onOpen={() => setOpenAlbum(album)} />
            ))}
          </div>
        </Section>
      )}

      {grouped.teams.length > 0 && (
        <Section title="Teams">
          <div className="card-grid">
            {grouped.teams.map((album) => (
              <AlbumCard key={album.id} album={album} onOpen={() => setOpenAlbum(album)} />
            ))}
          </div>
        </Section>
      )}

      <Section title="Needs Review">
        {(clusters.data?.length ?? 0) === 0 && grouped.unidentified.length === 0 && (
          <div className="hint">Nothing waiting — every detected face is identified.</div>
        )}
        <div className="card-grid">
          {clusters.data?.map((cluster) => (
            <ClusterCard
              key={cluster.id}
              cluster={cluster}
              onName={() => setNamingCluster(cluster)}
            />
          ))}
          {grouped.unidentified.map((album) => (
            <AlbumCard key={album.id} album={album} onOpen={() => setOpenAlbum(album)} />
          ))}
        </div>
      </Section>

      {namingCluster && (
        <NameClusterModal cluster={namingCluster} onClose={() => setNamingCluster(null)} />
      )}
    </>
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

function AlbumCard({ album, onOpen }: { album: Album; onOpen: () => void }) {
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
    <div className="card shoot-card" onClick={onOpen}>
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
      <div style={{ marginTop: 10 }} onClick={(e) => e.stopPropagation()}>
        <button className="small" disabled={toGroup.isPending} onClick={() => toGroup.mutate()}>
          {toGroup.isPending ? 'Adding…' : 'Make this a group'}
        </button>
      </div>
    </div>
  )
}

function ClusterCard({ cluster, onName }: { cluster: ClusterSummary; onName: () => void }) {
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
      <div style={{ marginTop: 10 }} onClick={(e) => e.stopPropagation()}>
        <button className="small primary" onClick={onName}>
          Name this person
        </button>
      </div>
    </div>
  )
}

/** Naming a cluster is the moment the app "learns" a player (§7). */
function NameClusterModal({ cluster, onClose }: { cluster: ClusterSummary; onClose: () => void }) {
  const [name, setName] = useState('')
  const [team, setTeam] = useState('')
  const [error, setError] = useState<string | null>(null)
  const queryClient = useQueryClient()
  const pushNotice = useUi((s) => s.pushNotice)
  const people = useQuery({ queryKey: ['people'], queryFn: () => api.listPeople(null) })

  const nameIt = useMutation({
    mutationFn: () => api.nameCluster(cluster.id, name.trim(), team.trim() || null),
    onSuccess: async (person) => {
      pushNotice({
        level: 'success',
        message: `${cluster.faceCount} faces added to ${person.name}'s library.`,
      })
      await queryClient.invalidateQueries({ queryKey: ['clusters'] })
      await api.regenerateAlbums(cluster.shootId)
      await queryClient.invalidateQueries({ queryKey: ['albums'] })
      onClose()
    },
    onError: (e) => setError(String(e)),
  })

  return (
    <Modal title={`Who is ${cluster.label}?`} onClose={onClose}>
      <div className="hint">
        {formatCount(cluster.faceCount)} faces across {formatCount(cluster.mediaCount)} files.
        Naming them adds every face to this player's library, so future shoots recognise them
        automatically.
      </div>
      <label className="field">
        <span>Player name</span>
        <input
          autoFocus
          value={name}
          onChange={(e) => setName(e.target.value)}
          placeholder="Jonathan"
          list="known-players"
        />
        <datalist id="known-players">
          {people.data?.map((p) => <option key={p.id} value={p.name} />)}
        </datalist>
      </label>
      <label className="field">
        <span>Team (optional)</span>
        <input value={team} onChange={(e) => setTeam(e.target.value)} placeholder="Gods Reign" />
      </label>
      {error && <div style={{ color: 'var(--error)', fontSize: 13 }}>{error}</div>}
      <div className="buttons">
        <button onClick={onClose}>Cancel</button>
        <button className="primary" disabled={!name.trim() || nameIt.isPending} onClick={() => nameIt.mutate()}>
          {nameIt.isPending ? 'Saving…' : 'Confirm'}
        </button>
      </div>
    </Modal>
  )
}

function AlbumDetail(props: {
  album: Album
  typeFilter: MediaType | 'all'
  setTypeFilter: (f: MediaType | 'all') => void
  onBack: () => void
}) {
  const { album, typeFilter } = props
  const media = useQuery({
    queryKey: ['media', album.shootId, 'album', album.id, typeFilter],
    queryFn: () =>
      api.listMedia({
        shootId: album.shootId,
        albumId: album.id,
        mediaType: typeFilter === 'all' ? null : typeFilter,
        limit: 2000,
      }),
  })

  return (
    <>
      <div className="workspace-header">
        <h1>{album.name}</h1>
        <div className="actions">
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
      </div>
      <MediaGrid media={media.data ?? []} />
    </>
  )
}
