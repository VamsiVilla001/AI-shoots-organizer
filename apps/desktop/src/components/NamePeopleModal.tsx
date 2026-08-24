/**
 * Naming the people in one photo, in the order the work actually happens.
 *
 * A photo is chosen, the faces in it are read, and the question is *which of
 * these people are you naming* — because a group shot has several answers. Pick
 * a face, give the name, and that person's whole set of footage is gathered into
 * a group named after them. Then the next face in the same photo, and so on:
 * one group photo becomes several people's groups.
 *
 * The gathering is the point. Naming a face without it would leave the editor
 * doing the sorting by hand anyway.
 */

import { useMemo, useState } from 'react'
import { useMutation, useQuery, useQueryClient } from '@tanstack/react-query'
import type { Face, NameFaceResult } from '@teo/shared-types'
import * as api from '../api'
import { FaceCrop } from './FaceCrop'
import { Modal } from './Modal'
import { formatConfidence, formatCount } from '../media'
import { useUi } from '../store'

type Step = { kind: 'choose' } | { kind: 'name'; face: Face }

export function NamePeopleModal(props: { mediaId: number; onClose: () => void }) {
  const [step, setStep] = useState<Step>({ kind: 'choose' })
  const [done, setDone] = useState<NameFaceResult[]>([])

  const media = useQuery({
    queryKey: ['media-item', props.mediaId],
    queryFn: () => api.getMedia(props.mediaId),
  })
  const faces = useQuery({
    queryKey: ['media-faces', props.mediaId],
    queryFn: () => api.mediaFaces(props.mediaId),
  })
  const people = useQuery({ queryKey: ['people'], queryFn: () => api.listPeople(null) })

  const visible = useMemo(
    () => (faces.data ?? []).filter((face) => face.assignment !== 'ignored'),
    [faces.data],
  )
  const unnamed = visible.filter((face) => face.personId == null)

  const nameOf = (face: Face) => people.data?.find((p) => p.id === face.personId)?.name ?? null

  return (
    <Modal
      title={
        step.kind === 'choose'
          ? `Who is in ${media.data?.filename ?? 'this photo'}?`
          : 'Name this person'
      }
      onClose={props.onClose}
    >
      {step.kind === 'choose' ? (
        <>
          {faces.isLoading && <div className="hint">Reading the faces in this photo…</div>}

          {!faces.isLoading && visible.length === 0 && (
            <div className="hint">
              No faces were found in this photo. If that looks wrong, the shoot may still be
              processing, or the faces may be too small or turned too far away.
            </div>
          )}

          {visible.length > 0 && (
            <div className="hint">
              {formatCount(visible.length)} face
              {visible.length === 1 ? '' : 's'} found
              {unnamed.length > 0 && ` · ${formatCount(unnamed.length)} still to name`}. Pick the
              person you are naming.
            </div>
          )}

          <div className="face-choice-grid">
            {visible.map((face) => {
              const existing = nameOf(face)
              return (
                <button
                  key={face.id}
                  className={`face-choice${existing ? ' named' : ''}`}
                  onClick={() => setStep({ kind: 'name', face })}
                  title={existing ? `Currently ${existing} — click to change` : 'Click to name'}
                >
                  <FaceCrop mediaId={face.mediaId} bbox={face.bbox} />
                  <span className="who">{existing ?? 'Not named'}</span>
                  {face.recognitionConfidence != null && (
                    <span className="confidence">{formatConfidence(face.recognitionConfidence)}</span>
                  )}
                </button>
              )
            })}
          </div>

          {done.length > 0 && (
            <div className="row-list">
              {done.map((result) => (
                <div className="row" key={result.person.id}>
                  <div className="grow">
                    <div className="title">{result.person.name}</div>
                    <div className="sub">
                      {formatCount(result.group.mediaCount)} file(s) in their group
                      {result.filesAdded > 0 && ` · ${formatCount(result.filesAdded)} just gathered`}
                      {result.facesNamed > 1 && ` · ${formatCount(result.facesNamed)} faces matched`}
                    </div>
                  </div>
                  <span className="badge completed">grouped</span>
                </div>
              ))}
            </div>
          )}

          <div className="buttons">
            <button onClick={props.onClose}>
              {done.length > 0 ? 'Done' : 'Close'}
            </button>
          </div>
        </>
      ) : (
        <NameOneFace
          face={step.face}
          mediaId={props.mediaId}
          currentName={nameOf(step.face)}
          knownPeople={(people.data ?? []).map((p) => p.name)}
          remaining={unnamed.length}
          onCancel={() => setStep({ kind: 'choose' })}
          onNamed={(result) => {
            setDone((all) => [...all.filter((r) => r.person.id !== result.person.id), result])
            setStep({ kind: 'choose' })
          }}
        />
      )}
    </Modal>
  )
}

function NameOneFace(props: {
  face: Face
  mediaId: number
  currentName: string | null
  knownPeople: string[]
  remaining: number
  onCancel: () => void
  onNamed: (result: NameFaceResult) => void
}) {
  const [name, setName] = useState(props.currentName ?? '')
  const [team, setTeam] = useState('')
  const [error, setError] = useState<string | null>(null)
  const queryClient = useQueryClient()
  const pushNotice = useUi((s) => s.pushNotice)

  const submit = useMutation({
    mutationFn: () => api.nameFace(props.face.id, name.trim(), team.trim() || null),
    onSuccess: async (result) => {
      // Naming spreads: the person's other faces, the albums derived from them,
      // and the group just filled all changed.
      await Promise.all(
        [
          'media-faces',
          'faces',
          'people',
          'clusters',
          'albums',
          'groups',
          'groupStats',
          'groupLinks',
          'media',
          'shoots',
        ].map((key) => queryClient.invalidateQueries({ queryKey: [key] })),
      )
      pushNotice({
        level: 'success',
        message:
          result.filesAdded > 0
            ? `${result.person.name}: ${formatCount(result.filesAdded)} file(s) gathered into their group.`
            : `${result.person.name} named. Their group already had every file they appear in.`,
      })
      props.onNamed(result)
    },
    onError: (e) => setError(String(e instanceof Error ? e.message : e)),
  })

  return (
    <>
      <div className="face-choice-single">
        <FaceCrop mediaId={props.face.mediaId} bbox={props.face.bbox} padding={0.6} />
      </div>

      <label className="field">
        <span>Who is this?</span>
        <input
          autoFocus
          value={name}
          onChange={(e) => setName(e.target.value)}
          onKeyDown={(e) => {
            if (e.key === 'Enter' && name.trim()) submit.mutate()
          }}
          placeholder="Jonathan"
          list="name-people-players"
          spellCheck={false}
        />
        <datalist id="name-people-players">
          {props.knownPeople.map((person) => <option key={person} value={person} />)}
        </datalist>
      </label>

      <label className="field">
        <span>Team (optional)</span>
        <input value={team} onChange={(e) => setTeam(e.target.value)} placeholder="Gods Reign" />
      </label>

      <div className="hint">
        Naming them also finds them in the rest of this shoot and puts every file they appear in
        into a group called <strong>{name.trim() || 'their name'}</strong> — ready to export as a
        folder.
        {props.remaining > 1 && ' You can name the next person in this photo straight after.'}
      </div>

      {error && <div style={{ color: 'var(--error)', fontSize: 13 }}>{error}</div>}

      <div className="buttons">
        <button onClick={props.onCancel} disabled={submit.isPending}>
          Back
        </button>
        <button className="primary" disabled={!name.trim() || submit.isPending} onClick={() => submit.mutate()}>
          {submit.isPending ? 'Gathering their footage…' : 'Name & build their group'}
        </button>
      </div>
    </>
  )
}
