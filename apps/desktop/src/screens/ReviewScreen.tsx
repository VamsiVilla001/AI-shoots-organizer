/**
 * The Review workspace (§10): confidence-sorted face suggestions with accept /
 * reject / reassign / ignore, multi-select and bulk operations.
 */

import { useMemo, useState } from 'react'
import { useMutation, useQuery, useQueryClient } from '@tanstack/react-query'
import type { FaceAssignment, FaceWithContext } from '@teo/shared-types'
import * as api from '../api'
import { formatConfidence, formatCount } from '../media'
import { FaceCrop } from '../components/FaceCrop'
import { Modal } from '../components/Modal'
import { useUi } from '../store'

type Filter = 'suggested' | 'unassigned' | 'confirmed' | 'all'

export function ReviewScreen() {
  const shootId = useUi((s) => s.activeShootId)
  if (shootId === null) return <div className="empty-state">Open a shoot first.</div>
  return <ReviewBody shootId={shootId} />
}

function ReviewBody({ shootId }: { shootId: number }) {
  const [filter, setFilter] = useState<Filter>('suggested')
  const [selected, setSelected] = useState<Set<number>>(new Set())
  const [assigning, setAssigning] = useState(false)
  const queryClient = useQueryClient()
  const pushNotice = useUi((s) => s.pushNotice)
  const openViewer = useUi((s) => s.openViewer)

  const faces = useQuery({
    queryKey: ['faces', shootId, filter],
    queryFn: () =>
      api.listFaces({
        shootId,
        assignment: filter === 'all' ? null : (filter as FaceAssignment),
        limit: 400,
      }),
  })

  const refresh = () => {
    setSelected(new Set())
    queryClient.invalidateQueries({ queryKey: ['faces'] })
    queryClient.invalidateQueries({ queryKey: ['people'] })
    queryClient.invalidateQueries({ queryKey: ['albums'] })
  }

  const act = (fn: (ids: number[]) => Promise<number>, doneWord: string) => {
    const ids = [...selected]
    fn(ids)
      .then((n) => {
        pushNotice({ level: 'success', message: `${n} face(s) ${doneWord}.` })
        refresh()
      })
      .catch((e) => pushNotice({ level: 'error', message: String(e) }))
  }

  const confirmAll = useMutation({
    mutationFn: () => {
      // "Confirm All" applies to what is currently visible, not the selection.
      const ids = (faces.data ?? []).filter((f) => f.assignment === 'suggested').map((f) => f.id)
      return api.confirmFaces(ids)
    },
    onSuccess: (n) => {
      pushNotice({ level: 'success', message: `${n} suggestion(s) confirmed.` })
      refresh()
    },
  })

  const toggle = (faceId: number) => {
    setSelected((current) => {
      const next = new Set(current)
      if (next.has(faceId)) next.delete(faceId)
      else next.add(faceId)
      return next
    })
  }

  const visible = faces.data ?? []
  const confidenceRange = useMemo(() => {
    const values = visible
      .map((f) => f.recognitionConfidence)
      .filter((v): v is number => v != null)
    if (values.length === 0) return null
    return [Math.min(...values), Math.max(...values)] as const
  }, [visible])

  return (
    <>
      <div className="workspace-header">
        <h1>Review</h1>
        <div className="actions">
          {filter === 'suggested' && visible.length > 0 && (
            <button className="primary" onClick={() => confirmAll.mutate()}>
              Confirm All ({visible.filter((f) => f.assignment === 'suggested').length})
            </button>
          )}
        </div>
      </div>

      <div className="filter-bar">
        {(
          [
            ['suggested', 'Suggestions'],
            ['unassigned', 'Unknown'],
            ['confirmed', 'Confirmed'],
            ['all', 'Everything'],
          ] as Array<[Filter, string]>
        ).map(([value, label]) => (
          <button
            key={value}
            className={`small${filter === value ? ' primary' : ''}`}
            onClick={() => {
              setFilter(value)
              setSelected(new Set())
            }}
          >
            {label}
          </button>
        ))}
        {confidenceRange && (
          <span className="hint">
            Confidence range {formatConfidence(confidenceRange[0])}–
            {formatConfidence(confidenceRange[1])} · lowest first
          </span>
        )}
      </div>

      {visible.length === 0 && (
        <div className="empty-state">
          <div className="big">Nothing to review here</div>
          <div>Suggestions appear as recognition runs; unknown faces are grouped on the Albums screen.</div>
        </div>
      )}

      <div className="face-grid">
        {visible.map((face) => (
          <FaceCard
            key={face.id}
            face={face}
            selected={selected.has(face.id)}
            onToggle={() => toggle(face.id)}
            onOpen={() => openViewer(face.mediaId)}
          />
        ))}
      </div>

      {selected.size > 0 && (
        <div className="selection-bar">
          <strong>{formatCount(selected.size)} selected</strong>
          <button className="small" onClick={() => act(api.confirmFaces, 'confirmed')}>
            Accept
          </button>
          <button className="small" onClick={() => act(api.rejectFaces, 'sent back to unknown')}>
            Wrong person
          </button>
          <button className="small" onClick={() => setAssigning(true)}>
            Assign to player…
          </button>
          <button className="small danger" onClick={() => act(api.ignoreFaces, 'ignored')}>
            Not a face
          </button>
          <button className="small" onClick={() => setSelected(new Set())}>
            Clear
          </button>
        </div>
      )}

      {assigning && (
        <AssignModal
          faceIds={[...selected]}
          onDone={() => {
            setAssigning(false)
            refresh()
          }}
          onClose={() => setAssigning(false)}
        />
      )}
    </>
  )
}

function FaceCard(props: {
  face: FaceWithContext
  selected: boolean
  onToggle: () => void
  onOpen: () => void
}) {
  const { face } = props
  return (
    <div
      className={`face-card${props.selected ? ' selected' : ''}`}
      onClick={props.onToggle}
      onDoubleClick={props.onOpen}
      title={`${face.mediaFilename}\nClick to select · double-click to open`}
    >
      <FaceCrop mediaId={face.mediaId} bbox={face.bbox} />
      <div className="meta">
        <span className="who">
          {face.personName ?? face.clusterLabel ?? 'Unknown'}
        </span>
        <span>{formatConfidence(face.recognitionConfidence)}</span>
      </div>
    </div>
  )
}

/** Bulk "assign selected to player" (§10) — pick an existing player or type a new name. */
function AssignModal(props: { faceIds: number[]; onDone: () => void; onClose: () => void }) {
  const [name, setName] = useState('')
  const [error, setError] = useState<string | null>(null)
  const people = useQuery({ queryKey: ['people'], queryFn: () => api.listPeople(null) })
  const pushNotice = useUi((s) => s.pushNotice)

  const assign = useMutation({
    mutationFn: () => {
      const existing = people.data?.find((p) => p.name.toLowerCase() === name.trim().toLowerCase())
      return api.assignFaces(props.faceIds, existing?.id ?? null, existing ? null : name.trim())
    },
    onSuccess: (n) => {
      pushNotice({ level: 'success', message: `${n} face(s) assigned to ${name.trim()}.` })
      props.onDone()
    },
    onError: (e) => setError(String(e)),
  })

  return (
    <Modal title={`Assign ${props.faceIds.length} face(s)`} onClose={props.onClose}>
      <label className="field">
        <span>Player</span>
        <input
          autoFocus
          value={name}
          onChange={(e) => setName(e.target.value)}
          placeholder="Type a name — existing or new"
          list="assign-player-list"
        />
        <datalist id="assign-player-list">
          {people.data?.map((p) => <option key={p.id} value={p.name} />)}
        </datalist>
      </label>
      <div className="hint">
        Assignments count as confirmed and become library samples, improving future recognition.
      </div>
      {error && <div style={{ color: 'var(--error)', fontSize: 13 }}>{error}</div>}
      <div className="buttons">
        <button onClick={props.onClose}>Cancel</button>
        <button className="primary" disabled={!name.trim() || assign.isPending} onClick={() => assign.mutate()}>
          Assign
        </button>
      </div>
    </Modal>
  )
}
