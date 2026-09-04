/**
 * The Players screen (§22): the reusable face library. Each profile shows its
 * sample and media counts and offers rename / merge / delete-recognition-data.
 */

import { useState } from 'react'
import { useMutation, useQuery, useQueryClient } from '@tanstack/react-query'
import type { PersonSummary } from '@skwad/shared-types'
import * as api from '../api'
import { formatCount } from '../media'
import { Modal } from '../components/Modal'
import { useUi } from '../store'

export function PlayersScreen() {
  const people = useQuery({ queryKey: ['people'], queryFn: () => api.listPeople(null) })
  const [editing, setEditing] = useState<PersonSummary | null>(null)

  return (
    <>
      <div className="workspace-header">
        <h1>Players</h1>
        <span className="hint">
          {people.data ? `${people.data.length} in the library` : ''}
        </span>
      </div>

      {people.data?.length === 0 && (
        <div className="empty-state">
          <div className="big">No players yet</div>
          <div>
            Import a shoot and name the unknown-person groups it finds — each named group becomes a
            player here, remembered for every future shoot.
          </div>
        </div>
      )}

      <div className="row-list">
        {people.data?.map((person) => (
          <div className="row" key={person.id}>
            <div className="avatar">{person.name.slice(0, 2).toUpperCase()}</div>
            <div className="grow">
              <div className="title">
                {person.name}
                {person.team && <span className="badge" style={{ marginLeft: 8 }}>{person.team}</span>}
              </div>
              <div className="sub">
                {formatCount(person.mediaCount)} media · {formatCount(person.faceSampleCount)} face
                samples · {formatCount(person.shootCount)} shoot{person.shootCount === 1 ? '' : 's'}
              </div>
            </div>
            <button className="small" onClick={() => setEditing(person)}>
              Manage
            </button>
          </div>
        ))}
      </div>

      {editing && (
        <ManagePlayerModal
          person={editing}
          all={people.data ?? []}
          onClose={() => setEditing(null)}
        />
      )}
    </>
  )
}

function ManagePlayerModal(props: {
  person: PersonSummary
  all: PersonSummary[]
  onClose: () => void
}) {
  const { person, onClose } = props
  const [name, setName] = useState(person.name)
  const [team, setTeam] = useState(person.team ?? '')
  const [mergeTarget, setMergeTarget] = useState('')
  const [error, setError] = useState<string | null>(null)
  const queryClient = useQueryClient()
  const pushNotice = useUi((s) => s.pushNotice)

  const refresh = () => queryClient.invalidateQueries({ queryKey: ['people'] })

  const save = useMutation({
    mutationFn: async () => {
      if (name.trim() !== person.name) await api.renamePerson(person.id, name.trim())
      await api.updatePerson(person.id, team.trim() || null, person.notes)
    },
    onSuccess: () => {
      refresh()
      onClose()
    },
    onError: (e) => setError(String(e)),
  })

  const merge = useMutation({
    mutationFn: () => api.mergePeople(Number(mergeTarget), person.id),
    onSuccess: (moved) => {
      pushNotice({ level: 'success', message: `Merged — ${moved} faces now on the target player.` })
      refresh()
      onClose()
    },
    onError: (e) => setError(String(e)),
  })

  const clearRecognition = useMutation({
    mutationFn: () => api.clearPersonRecognition(person.id),
    onSuccess: () => {
      pushNotice({ level: 'success', message: `Recognition data for ${person.name} deleted.` })
      refresh()
      onClose()
    },
    onError: (e) => setError(String(e)),
  })

  const remove = useMutation({
    mutationFn: () => api.deletePerson(person.id),
    onSuccess: () => {
      refresh()
      onClose()
    },
    onError: (e) => setError(String(e)),
  })

  return (
    <Modal title={person.name} onClose={onClose}>
      <label className="field">
        <span>Name</span>
        <input value={name} onChange={(e) => setName(e.target.value)} />
      </label>
      <label className="field">
        <span>Team (optional)</span>
        <input value={team} onChange={(e) => setTeam(e.target.value)} placeholder="Gods Reign" />
      </label>

      <label className="field">
        <span>Merge into another player</span>
        <div style={{ display: 'flex', gap: 8 }}>
          <select style={{ flex: 1 }} value={mergeTarget} onChange={(e) => setMergeTarget(e.target.value)}>
            <option value="">Choose target…</option>
            {props.all
              .filter((p) => p.id !== person.id)
              .map((p) => (
                <option key={p.id} value={p.id}>
                  {p.name}
                </option>
              ))}
          </select>
          <button
            disabled={!mergeTarget}
            onClick={() => {
              if (window.confirm(`Move every face of "${person.name}" onto the selected player and delete this profile?`))
                merge.mutate()
            }}
          >
            Merge
          </button>
        </div>
      </label>

      <div style={{ display: 'flex', gap: 8, flexWrap: 'wrap' }}>
        <button
          className="danger small"
          onClick={() => {
            if (window.confirm(`Delete recognition data for "${person.name}"?\nTheir faces return to the unknown pool; the profile is kept.`))
              clearRecognition.mutate()
          }}
        >
          Delete Recognition Data
        </button>
        <button
          className="danger small"
          onClick={() => {
            if (window.confirm(`Delete the player "${person.name}" entirely?`)) remove.mutate()
          }}
        >
          Delete Player
        </button>
      </div>

      {error && <div style={{ color: 'var(--error)', fontSize: 13 }}>{error}</div>}
      <div className="buttons">
        <button onClick={onClose}>Cancel</button>
        <button className="primary" disabled={!name.trim()} onClick={() => save.mutate()}>
          Save
        </button>
      </div>
    </Modal>
  )
}
