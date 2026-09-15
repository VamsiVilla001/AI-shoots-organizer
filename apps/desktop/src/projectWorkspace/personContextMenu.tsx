import { useEffect, useState } from 'react'
import { useMutation, useQueryClient } from '@tanstack/react-query'
import type { PersonSummary } from '@skwad/shared-types'
import * as api from '../api'
import { useUi } from '../store'
import { WorkspaceDialog } from './WorkspaceDialog'

/**
 * Right-click actions for a person row — Tag media and Pre-Process both list
 * people the same way but, unlike the media-collection cards elsewhere in
 * Media Processing, had no quick-action menu. Mirrors `SourceContextMenu` in
 * processing.tsx: a plain "Open" plus the same rename/merge/clear/delete
 * actions `PlayersScreen`'s "Manage" modal already offers, just reachable in
 * one right-click instead of a detour through Manage people.
 */
export function PersonContextMenu({
  person,
  all,
  x,
  y,
  onClose,
  onOpen,
}: {
  person: PersonSummary
  all: PersonSummary[]
  x: number
  y: number
  onClose: () => void
  onOpen: () => void
}) {
  const queryClient = useQueryClient()
  const pushNotice = useUi(s => s.pushNotice)
  const [renaming, setRenaming] = useState(false)
  const [merging, setMerging] = useState(false)

  useEffect(() => {
    const close = (event: KeyboardEvent) => { if (event.key === 'Escape') onClose() }
    document.addEventListener('keydown', close)
    return () => document.removeEventListener('keydown', close)
  }, [onClose])

  const refresh = () => void queryClient.invalidateQueries({ queryKey: ['people'] })
  const run = (action: () => void) => { onClose(); action() }

  const clearRecognition = useMutation({
    mutationFn: () => api.clearPersonRecognition(person.id),
    onSuccess: () => {
      pushNotice({ level: 'success', message: `Recognition data for ${person.name} deleted.` })
      refresh()
    },
    onError: (e: unknown) => pushNotice({ level: 'error', message: String(e) }),
  })

  const remove = useMutation({
    mutationFn: () => api.deletePerson(person.id),
    onSuccess: () => {
      pushNotice({ level: 'success', message: `${person.name} deleted.` })
      refresh()
    },
    onError: (e: unknown) => pushNotice({ level: 'error', message: String(e) }),
  })

  return <>
    <div className="pw-context-layer" onClick={onClose} onContextMenu={event => { event.preventDefault(); onClose() }}>
      <div
        className="pw-context-menu"
        role="menu"
        aria-label={`${person.name} actions`}
        style={{ left: Math.min(x, window.innerWidth - 230), top: Math.min(y, window.innerHeight - 280) }}
        onClick={event => event.stopPropagation()}
      >
        <strong>Person actions</strong>
        <button autoFocus role="menuitem" onClick={() => run(onOpen)}>Open</button>
        <button role="menuitem" onClick={() => run(() => setRenaming(true))}>Rename…</button>
        <button role="menuitem" disabled={all.length < 2} onClick={() => run(() => setMerging(true))}>Merge into…</button>
        <button
          role="menuitem"
          disabled={clearRecognition.isPending}
          onClick={() => run(() => {
            if (window.confirm(`Delete recognition data for "${person.name}"?\nTheir faces return to the unknown pool; the profile is kept.`)) {
              clearRecognition.mutate()
            }
          })}
        >
          Clear Recognition Data
        </button>
        <button
          role="menuitem"
          className="danger"
          disabled={remove.isPending}
          onClick={() => run(() => {
            if (window.confirm(`Delete the player "${person.name}" entirely?`)) remove.mutate()
          })}
        >
          Delete
        </button>
      </div>
    </div>
    {renaming && <RenamePersonDialog person={person} onClose={() => setRenaming(false)} onRenamed={refresh} />}
    {merging && <MergePersonDialog person={person} all={all} onClose={() => setMerging(false)} onMerged={refresh} />}
  </>
}

function RenamePersonDialog({ person, onClose, onRenamed }: { person: PersonSummary; onClose: () => void; onRenamed: () => void }) {
  const [name, setName] = useState(person.name)
  const [busy, setBusy] = useState(false)
  const [error, setError] = useState('')
  const pushNotice = useUi(s => s.pushNotice)

  return <WorkspaceDialog title="Rename person" onClose={() => { if (!busy) onClose() }}>
    <form onSubmit={async event => {
      event.preventDefault(); setBusy(true); setError('')
      try {
        await api.renamePerson(person.id, name.trim())
        onRenamed()
        pushNotice({ level: 'success', message: `Renamed to ${name.trim()}.` })
        onClose()
      } catch (e) {
        setError(String(e))
      } finally {
        setBusy(false)
      }
    }}>
      <label className="field">Name<input autoFocus required maxLength={120} disabled={busy} value={name} onChange={e => setName(e.target.value)} /></label>
      {error && <p className="pw-error" role="alert">{error}</p>}
      <div className="pw-dialog-actions"><button type="button" disabled={busy} onClick={onClose}>Cancel</button><button className="primary" disabled={busy || !name.trim()}>{busy ? 'Saving…' : 'Save name'}</button></div>
    </form>
  </WorkspaceDialog>
}

function MergePersonDialog({ person, all, onClose, onMerged }: { person: PersonSummary; all: PersonSummary[]; onClose: () => void; onMerged: () => void }) {
  const [targetId, setTargetId] = useState('')
  const [busy, setBusy] = useState(false)
  const [error, setError] = useState('')
  const pushNotice = useUi(s => s.pushNotice)
  const options = all.filter(p => p.id !== person.id)

  return <WorkspaceDialog title={`Merge "${person.name}" into…`} onClose={() => { if (!busy) onClose() }}>
    <p className="pw-help">Every face of "{person.name}" moves onto the selected person, and this profile is deleted.</p>
    <label className="field">Target person
      <select required disabled={busy} value={targetId} onChange={e => setTargetId(e.target.value)}>
        <option value="">Choose target…</option>
        {options.map(p => <option key={p.id} value={p.id}>{p.name}</option>)}
      </select>
    </label>
    {error && <p className="pw-error" role="alert">{error}</p>}
    <div className="pw-dialog-actions">
      <button type="button" disabled={busy} onClick={onClose}>Cancel</button>
      <button className="primary" disabled={busy || !targetId} onClick={async () => {
        setBusy(true); setError('')
        try {
          const moved = await api.mergePeople(Number(targetId), person.id)
          onMerged()
          pushNotice({ level: 'success', message: `Merged — ${moved} faces moved.` })
          onClose()
        } catch (e) {
          setError(String(e))
        } finally {
          setBusy(false)
        }
      }}>{busy ? 'Merging…' : 'Merge'}</button>
    </div>
  </WorkspaceDialog>
}
