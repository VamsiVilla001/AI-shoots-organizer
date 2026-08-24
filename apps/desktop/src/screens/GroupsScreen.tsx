/**
 * The Sort screen (§34) — where an editor turns a raw footage folder into named
 * groups, and each group becomes a folder on the NAS at export time.
 *
 * The shoot's source folder is only ever read. Everything here records a
 * decision ("this clip is Jonathan's") in the database; the copy happens once,
 * on the Export screen.
 *
 * Dragging thumbnails onto a group relies on HTML5 drag events reaching the
 * page, which is why the window sets `dragDropEnabled: false` — the native
 * file-drop handler would otherwise swallow them on Windows, and the app does
 * not accept files dropped from outside.
 */

import { useEffect, useMemo, useRef, useState } from 'react'
import { useMutation, useQuery, useQueryClient } from '@tanstack/react-query'
import type { Group, MediaType } from '@teo/shared-types'
import * as api from '../api'
import { folderNameFor } from '../folders'
import { formatCount } from '../media'
import { MediaGrid } from '../components/MediaGrid'
import { Modal } from '../components/Modal'
import { NamePeopleModal } from '../components/NamePeopleModal'
import { ProgressPanel } from '../components/ProgressPanel'
import { useUi } from '../store'

/** Which slice of the shoot the grid is showing. */
type View = { kind: 'all' } | { kind: 'ungrouped' } | { kind: 'group'; id: number }

export function GroupsScreen() {
  const shootId = useUi((s) => s.activeShootId)
  if (shootId === null) return <div className="empty-state">Open a shoot first.</div>
  return <GroupsBody shootId={shootId} />
}

function GroupsBody({ shootId }: { shootId: number }) {
  const [view, setView] = useState<View>({ kind: 'ungrouped' })
  const [typeFilter, setTypeFilter] = useState<MediaType | 'all'>('all')
  const [search, setSearch] = useState('')
  const [debouncedSearch, setDebouncedSearch] = useState('')
  const [selected, setSelected] = useState<Set<number>>(new Set())
  const [creating, setCreating] = useState(false)
  const [editing, setEditing] = useState<Group | null>(null)
  const [dropTarget, setDropTarget] = useState<number | null>(null)
  /** The photo whose people are being named, if any. */
  const [namingMediaId, setNamingMediaId] = useState<number | null>(null)
  /** The files the current drag carries — set when the drag starts. */
  const dragPayload = useRef<number[]>([])
  const lastClicked = useRef<number | null>(null)

  const queryClient = useQueryClient()
  const pushNotice = useUi((s) => s.pushNotice)
  const navigate = useUi((s) => s.navigate)

  const shoot = useQuery({ queryKey: ['shoots', shootId], queryFn: () => api.getShoot(shootId) })
  const groups = useQuery({ queryKey: ['groups', shootId], queryFn: () => api.listGroups(shootId) })
  const stats = useQuery({ queryKey: ['groupStats', shootId], queryFn: () => api.groupStats(shootId) })
  const links = useQuery({ queryKey: ['groupLinks', shootId], queryFn: () => api.groupLinks(shootId) })

  useEffect(() => {
    const timer = setTimeout(() => setDebouncedSearch(search.trim()), 250)
    return () => clearTimeout(timer)
  }, [search])

  const media = useQuery({
    queryKey: ['media', shootId, 'sort', view, typeFilter, debouncedSearch],
    queryFn: () =>
      api.listMedia({
        shootId,
        groupId: view.kind === 'group' ? view.id : null,
        ungrouped: view.kind === 'ungrouped',
        mediaType: typeFilter === 'all' ? null : typeFilter,
        search: debouncedSearch || null,
        limit: 2000,
      }),
  })

  /** media id → the group names holding it, for the chips on each tile. */
  const groupNames = useMemo(() => {
    const byId = new Map((groups.data ?? []).map((g) => [g.id, g.name]))
    const out = new Map<number, string[]>()
    for (const link of links.data ?? []) {
      const name = byId.get(link.groupId)
      if (!name) continue
      const list = out.get(link.mediaId)
      if (list) list.push(name)
      else out.set(link.mediaId, [name])
    }
    return out
  }, [groups.data, links.data])

  const refresh = () => {
    queryClient.invalidateQueries({ queryKey: ['groups', shootId] })
    queryClient.invalidateQueries({ queryKey: ['groupStats', shootId] })
    queryClient.invalidateQueries({ queryKey: ['groupLinks', shootId] })
    queryClient.invalidateQueries({ queryKey: ['media', shootId] })
  }

  const visible = media.data ?? []
  const activeGroup =
    view.kind === 'group' ? (groups.data ?? []).find((g) => g.id === view.id) ?? null : null

  const toggle = (mediaId: number) => {
    setSelected((current) => {
      const next = new Set(current)
      if (next.has(mediaId)) next.delete(mediaId)
      else next.add(mediaId)
      return next
    })
    lastClicked.current = mediaId
  }

  /** Shift-click extends from the last clicked tile, as a file manager does. */
  const selectRange = (mediaId: number) => {
    const ids = visible.map((m) => m.id)
    const from = lastClicked.current === null ? mediaId : lastClicked.current
    const start = ids.indexOf(from)
    const end = ids.indexOf(mediaId)
    if (start === -1 || end === -1) return toggle(mediaId)
    const [lo, hi] = start <= end ? [start, end] : [end, start]
    setSelected((current) => {
      const next = new Set(current)
      for (const id of ids.slice(lo, hi + 1)) next.add(id)
      return next
    })
    lastClicked.current = mediaId
  }

  const sortInto = useMutation({
    mutationFn: (args: { group?: Group; name?: string; mediaIds: number[]; move: boolean }) =>
      api.addMediaToGroup({
        shootId,
        groupId: args.group?.id ?? null,
        groupName: args.name ?? null,
        mediaIds: args.mediaIds,
        moveFiles: args.move,
      }),
    onSuccess: (added, args) => {
      const label = args.group?.name ?? args.name ?? 'the group'
      pushNotice({
        level: 'success',
        message:
          added > 0
            ? `${formatCount(added)} file(s) sorted into ${label}.`
            : `Those files were already in ${label}.`,
      })
      setSelected(new Set())
      refresh()
    },
    onError: (e) => pushNotice({ level: 'error', message: String(e instanceof Error ? e.message : e) }),
  })

  const removeFromGroup = useMutation({
    mutationFn: (args: { groupId: number; mediaIds: number[] }) =>
      api.removeMediaFromGroup(args.groupId, args.mediaIds),
    onSuccess: (removed) => {
      pushNotice({ level: 'success', message: `${formatCount(removed)} file(s) taken out of the group.` })
      setSelected(new Set())
      refresh()
    },
    onError: (e) => pushNotice({ level: 'error', message: String(e instanceof Error ? e.message : e) }),
  })

  const seed = useMutation({
    mutationFn: () => api.groupsFromAiAlbums(shootId),
    onSuccess: (result) => {
      pushNotice({
        level: result.groups > 0 ? 'success' : 'warn',
        message:
          result.groups > 0
            ? `${result.groups} group(s) ready with ${formatCount(result.files)} file(s) from the AI albums. Correct anything it got wrong, then export.`
            : 'No player albums to build groups from yet — name the unknown faces on the AI Albums screen first.',
      })
      refresh()
    },
    onError: (e) => pushNotice({ level: 'error', message: String(e instanceof Error ? e.message : e) }),
  })

  const dropOn = (group: Group) => {
    const ids = dragPayload.current
    dragPayload.current = []
    setDropTarget(null)
    if (ids.length > 0) sortInto.mutate({ group, mediaIds: ids, move: false })
  }

  return (
    <>
      <div className="workspace-header">
        <div>
          <h1>Sort — {shoot.data?.name ?? ''}</h1>
          {shoot.data && (
            <div className="hint mono" title={shoot.data.sourcePath}>
              reading {shoot.data.sourcePath} · never modified
            </div>
          )}
        </div>
        <div className="actions">
          <button onClick={() => seed.mutate()} disabled={seed.isPending}>
            {seed.isPending ? 'Building…' : 'Build groups from AI players'}
          </button>
          <button className="primary" onClick={() => setCreating(true)}>
            + New group
          </button>
          <button onClick={() => navigate('export')}>Export to folders →</button>
        </div>
      </div>

      <ProgressPanel shootId={shootId} />

      {stats.data && (
        <div className="sort-stats">
          <span>
            <strong>{formatCount(stats.data.mediaTotal)}</strong> files in the shoot
          </span>
          <span>
            <strong>{formatCount(stats.data.grouped)}</strong> sorted
          </span>
          <span className={stats.data.ungrouped > 0 ? 'pending' : undefined}>
            <strong>{formatCount(stats.data.ungrouped)}</strong> left
          </span>
        </div>
      )}

      <div className="sort-layout">
        <aside className="group-panel">
          <h2>Groups → folders</h2>
          <div className="hint" style={{ marginBottom: 10 }}>
            Drag files onto a group, or select them and use the bar at the bottom. Each group name
            becomes one folder in the export destination.
          </div>

          <button
            className={`group-row static${view.kind === 'ungrouped' ? ' active' : ''}`}
            onClick={() => setView({ kind: 'ungrouped' })}
          >
            <span className="name">Not sorted yet</span>
            <span className="badge">{formatCount(stats.data?.ungrouped ?? 0)}</span>
          </button>
          <button
            className={`group-row static${view.kind === 'all' ? ' active' : ''}`}
            onClick={() => setView({ kind: 'all' })}
          >
            <span className="name">All files</span>
            <span className="badge">{formatCount(stats.data?.mediaTotal ?? 0)}</span>
          </button>

          <div className="group-list">
            {(groups.data ?? []).map((group) => (
              <div
                key={group.id}
                className={`group-row${view.kind === 'group' && view.id === group.id ? ' active' : ''}${
                  dropTarget === group.id ? ' drop-target' : ''
                }`}
                onClick={() => setView({ kind: 'group', id: group.id })}
                onDragOver={(e) => {
                  e.preventDefault()
                  e.dataTransfer.dropEffect = 'copy'
                  setDropTarget(group.id)
                }}
                onDragLeave={() => setDropTarget((current) => (current === group.id ? null : current))}
                onDrop={(e) => {
                  e.preventDefault()
                  dropOn(group)
                }}
              >
                <div className="grow">
                  <div className="name">{group.name}</div>
                  <div className="sub mono">
                    {folderNameFor(group)}/ · {formatCount(group.photoCount)} photos ·{' '}
                    {formatCount(group.videoCount)} videos
                  </div>
                </div>
                <span className="badge">{formatCount(group.mediaCount)}</span>
                <button
                  className="small"
                  title="Rename, set the folder name, or delete"
                  onClick={(e) => {
                    e.stopPropagation()
                    setEditing(group)
                  }}
                >
                  ⋯
                </button>
              </div>
            ))}
            {(groups.data?.length ?? 0) === 0 && (
              <div className="hint">
                No groups yet. Create one per person whose footage is being cut — or let the AI
                players give you a starting point.
              </div>
            )}
          </div>
        </aside>

        <section className="sort-main">
          <div className="filter-bar">
            <input
              placeholder="Search filenames…"
              value={search}
              onChange={(e) => setSearch(e.target.value)}
              style={{ minWidth: 200 }}
            />
            {(['all', 'photo', 'video'] as const).map((option) => (
              <button
                key={option}
                className={`small${typeFilter === option ? ' primary' : ''}`}
                onClick={() => setTypeFilter(option)}
              >
                {option === 'all' ? 'All' : option === 'photo' ? 'Photos' : 'Videos'}
              </button>
            ))}
            <span className="hint">
              {activeGroup
                ? `${activeGroup.name} — ${formatCount(visible.length)} shown`
                : `${formatCount(visible.length)} shown`}
            </span>
            <div style={{ flex: 1 }} />
            <button
              className="small"
              disabled={visible.length === 0}
              onClick={() => setSelected(new Set(visible.map((m) => m.id)))}
            >
              Select all
            </button>
            <button className="small" disabled={selected.size === 0} onClick={() => setSelected(new Set())}>
              Clear
            </button>
          </div>

          <MediaGrid
            media={visible}
            selected={selected}
            selectMode
            onToggleSelect={(mediaId) => toggle(mediaId)}
            onSelectRange={selectRange}
            groupsFor={(mediaId) => groupNames.get(mediaId) ?? []}
            onDragMedia={(mediaId) => {
              // Dragging a tile inside the selection drags the whole selection;
              // dragging an unselected one drags just it.
              dragPayload.current = selected.has(mediaId) ? [...selected] : [mediaId]
            }}
            emptyTitle={
              view.kind === 'ungrouped' ? 'Everything is sorted' : 'Nothing matches these filters'
            }
            emptyHint={
              view.kind === 'ungrouped'
                ? 'Every file in this shoot is in at least one group. Head to Export to write the folders.'
                : 'Try another filter, or clear the search.'
            }
          />

          {selected.size > 0 && (
            <SelectionBar
              count={selected.size}
              onNamePeople={
                selected.size === 1 ? () => setNamingMediaId([...selected][0]) : undefined
              }
              groups={groups.data ?? []}
              activeGroup={activeGroup}
              busy={sortInto.isPending || removeFromGroup.isPending}
              onSortInto={(target, move) =>
                sortInto.mutate({
                  group: typeof target === 'object' ? target : undefined,
                  name: typeof target === 'string' ? target : undefined,
                  mediaIds: [...selected],
                  move,
                })
              }
              onRemove={() =>
                activeGroup &&
                removeFromGroup.mutate({ groupId: activeGroup.id, mediaIds: [...selected] })
              }
              onClear={() => setSelected(new Set())}
            />
          )}
        </section>
      </div>

      {namingMediaId !== null && (
        <NamePeopleModal
          mediaId={namingMediaId}
          onClose={() => {
            setNamingMediaId(null)
            setSelected(new Set())
            refresh()
          }}
        />
      )}

      {creating && (
        <NewGroupModal
          shootId={shootId}
          onClose={() => setCreating(false)}
          onCreated={(group) => {
            refresh()
            setView({ kind: 'group', id: group.id })
          }}
        />
      )}
      {editing && (
        <EditGroupModal
          group={editing}
          onClose={() => setEditing(null)}
          onChanged={refresh}
          onDeleted={() => {
            setView({ kind: 'ungrouped' })
            refresh()
          }}
        />
      )}
    </>
  )
}

/**
 * The sticky bar that turns a selection into a filing decision. "Move" exists
 * because the common correction is *this went into the wrong group*, and
 * without it the file would end up in two folders.
 */
function SelectionBar(props: {
  count: number
  groups: Group[]
  activeGroup: Group | null
  busy: boolean
  /** Only offered for a single photo: naming asks who is in *this* one. */
  onNamePeople?: () => void
  onSortInto: (target: Group | string, move: boolean) => void
  onRemove: () => void
  onClear: () => void
}) {
  const [choice, setChoice] = useState('')
  const [newName, setNewName] = useState('')

  const target = props.groups.find((g) => String(g.id) === choice)
  const canSort = target !== undefined || newName.trim().length > 0

  const apply = (move: boolean) => {
    if (target) props.onSortInto(target, move)
    else if (newName.trim()) props.onSortInto(newName.trim(), move)
    setNewName('')
  }

  return (
    <div className="selection-bar">
      <strong>{formatCount(props.count)} selected</strong>
      <select value={choice} onChange={(e) => setChoice(e.target.value)}>
        <option value="">Choose a group…</option>
        {props.groups.map((group) => (
          <option key={group.id} value={String(group.id)}>
            {group.name} ({formatCount(group.mediaCount)})
          </option>
        ))}
      </select>
      <span className="hint">or</span>
      <input
        placeholder="new group name"
        value={newName}
        onChange={(e) => setNewName(e.target.value)}
        onKeyDown={(e) => {
          if (e.key === 'Enter' && newName.trim()) apply(false)
        }}
        style={{ width: 170 }}
      />
      <button className="primary" disabled={!canSort || props.busy} onClick={() => apply(false)}>
        Add to group
      </button>
      <button
        disabled={!canSort || props.busy}
        title="Put these files in this group only, taking them out of every other group"
        onClick={() => apply(true)}
      >
        Move here
      </button>
      {props.activeGroup && (
        <button className="danger" disabled={props.busy} onClick={props.onRemove}>
          Remove from {props.activeGroup.name}
        </button>
      )}
      {props.onNamePeople && (
        <button
          className="small"
          title="Read the faces in this photo and name them one at a time"
          onClick={props.onNamePeople}
        >
          Name people in it
        </button>
      )}
      <div style={{ flex: 1 }} />
      <button className="small" onClick={props.onClear}>
        Clear selection
      </button>
    </div>
  )
}

function NewGroupModal(props: {
  shootId: number
  onClose: () => void
  onCreated: (group: Group) => void
}) {
  const [name, setName] = useState('')
  const [error, setError] = useState<string | null>(null)
  const people = useQuery({ queryKey: ['people', props.shootId], queryFn: () => api.listPeople(props.shootId) })

  const create = useMutation({
    mutationFn: () => api.createGroup(props.shootId, name.trim()),
    onSuccess: (group) => {
      props.onCreated(group)
      props.onClose()
    },
    onError: (e) => setError(String(e instanceof Error ? e.message : e)),
  })

  return (
    <Modal title="New group" onClose={props.onClose}>
      <div className="hint">
        The name becomes the folder name in the export destination. Most teams use one group per
        player, but any name works — “Team B-roll”, “Day 2 Interviews”.
      </div>
      <label className="field">
        <span>Group name</span>
        <input
          autoFocus
          value={name}
          onChange={(e) => setName(e.target.value)}
          onKeyDown={(e) => {
            if (e.key === 'Enter' && name.trim()) create.mutate()
          }}
          placeholder="Jonathan"
          list="known-players-for-groups"
        />
        <datalist id="known-players-for-groups">
          {people.data?.map((p) => <option key={p.id} value={p.name} />)}
        </datalist>
      </label>
      {name.trim() && (
        <div className="hint mono">
          exports to {folderNameFor({ name: name.trim(), folderName: null })}/
        </div>
      )}
      {error && <div style={{ color: 'var(--error)', fontSize: 13 }}>{error}</div>}
      <div className="buttons">
        <button onClick={props.onClose}>Cancel</button>
        <button className="primary" disabled={!name.trim() || create.isPending} onClick={() => create.mutate()}>
          {create.isPending ? 'Creating…' : 'Create group'}
        </button>
      </div>
    </Modal>
  )
}

function EditGroupModal(props: {
  group: Group
  onClose: () => void
  onChanged: () => void
  onDeleted: () => void
}) {
  const [name, setName] = useState(props.group.name)
  const [folder, setFolder] = useState(props.group.folderName ?? '')
  const [notes, setNotes] = useState(props.group.notes ?? '')
  const [error, setError] = useState<string | null>(null)
  const pushNotice = useUi((s) => s.pushNotice)

  const save = useMutation({
    mutationFn: async () => {
      if (name.trim() && name.trim() !== props.group.name) {
        await api.renameGroup(props.group.id, name.trim())
      }
      await api.updateGroup(props.group.id, folder.trim() || null, notes.trim() || null)
    },
    onSuccess: () => {
      props.onChanged()
      props.onClose()
    },
    onError: (e) => setError(String(e instanceof Error ? e.message : e)),
  })

  const remove = useMutation({
    mutationFn: () => api.deleteGroup(props.group.id),
    onSuccess: () => {
      pushNotice({
        level: 'success',
        message: `Group “${props.group.name}” deleted. No files were touched.`,
      })
      props.onDeleted()
      props.onClose()
    },
    onError: (e) => setError(String(e instanceof Error ? e.message : e)),
  })

  const emptyIt = useMutation({
    mutationFn: () => api.clearGroup(props.group.id),
    onSuccess: (removed) => {
      pushNotice({ level: 'success', message: `${formatCount(removed)} file(s) taken out of the group.` })
      props.onChanged()
      props.onClose()
    },
    onError: (e) => setError(String(e instanceof Error ? e.message : e)),
  })

  return (
    <Modal title={`Group — ${props.group.name}`} onClose={props.onClose}>
      <label className="field">
        <span>Name</span>
        <input autoFocus value={name} onChange={(e) => setName(e.target.value)} />
      </label>
      <label className="field">
        <span>Folder name on export (optional)</span>
        <input
          value={folder}
          onChange={(e) => setFolder(e.target.value)}
          placeholder={props.group.name}
        />
      </label>
      <div className="hint mono">
        exports to {folderNameFor({ name: name.trim() || props.group.name, folderName: folder.trim() || null })}/
      </div>
      <label className="field">
        <span>Note (optional)</span>
        <input value={notes} onChange={(e) => setNotes(e.target.value)} placeholder="Sponsor cut" />
      </label>
      <div className="hint">
        Holds {formatCount(props.group.mediaCount)} file(s). Emptying or deleting a group changes
        nothing on disk — it only forgets the sorting.
      </div>
      {error && <div style={{ color: 'var(--error)', fontSize: 13 }}>{error}</div>}
      <div className="buttons" style={{ justifyContent: 'space-between' }}>
        <div style={{ display: 'flex', gap: 8 }}>
          <button
            className="danger"
            disabled={remove.isPending}
            onClick={() => {
              if (window.confirm(`Delete the group “${props.group.name}”?\nYour files are not touched.`)) {
                remove.mutate()
              }
            }}
          >
            Delete group
          </button>
          {props.group.mediaCount > 0 && (
            <button disabled={emptyIt.isPending} onClick={() => emptyIt.mutate()}>
              Empty it
            </button>
          )}
        </div>
        <div style={{ display: 'flex', gap: 8 }}>
          <button onClick={props.onClose}>Cancel</button>
          <button className="primary" disabled={save.isPending} onClick={() => save.mutate()}>
            {save.isPending ? 'Saving…' : 'Save'}
          </button>
        </div>
      </div>
    </Modal>
  )
}
