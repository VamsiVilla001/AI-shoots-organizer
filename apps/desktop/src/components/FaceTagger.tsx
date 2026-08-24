/**
 * Naming one face by clicking it — the quick path through the same operation the
 * guided flow uses.
 *
 * A photo with five people in it has five answers to "who is this?", so the
 * question is asked about a *face*. Answering it does the whole job: name_face
 * assigns that face's cluster to the person, catches the albums up, and gathers
 * every file they appear in into a group named after them. Both paths call it,
 * so clicking a box and walking the guided flow cannot drift apart.
 */

import { useEffect, useRef, useState } from 'react'
import { useMutation, useQuery, useQueryClient } from '@tanstack/react-query'
import type { Face } from '@teo/shared-types'
import * as api from '../api'
import { useUi } from '../store'

export function FaceTagger(props: {
  face: Face
  /** Current name, when the face already has one. */
  currentName: string | null
  mediaId: number
  onClose: () => void
}) {
  const [name, setName] = useState(props.currentName ?? '')
  const [error, setError] = useState<string | null>(null)
  const inputRef = useRef<HTMLInputElement>(null)
  const queryClient = useQueryClient()
  const pushNotice = useUi((s) => s.pushNotice)

  const people = useQuery({ queryKey: ['people'], queryFn: () => api.listPeople(null) })

  useEffect(() => {
    inputRef.current?.focus()
    inputRef.current?.select()
  }, [])

  const refresh = async () => {
    // The face changed, so every derived view of it is stale: albums are
    // regenerated from assignments, and the group chips read membership.
    await Promise.all([
      queryClient.invalidateQueries({ queryKey: ['media-faces', props.mediaId] }),
      queryClient.invalidateQueries({ queryKey: ['faces'] }),
      queryClient.invalidateQueries({ queryKey: ['people'] }),
      queryClient.invalidateQueries({ queryKey: ['clusters'] }),
      queryClient.invalidateQueries({ queryKey: ['albums'] }),
      queryClient.invalidateQueries({ queryKey: ['groups'] }),
      queryClient.invalidateQueries({ queryKey: ['groupStats'] }),
      queryClient.invalidateQueries({ queryKey: ['groupLinks'] }),
      queryClient.invalidateQueries({ queryKey: ['media'] }),
    ])
  }

  /** Names the person, and gathers their footage into a group. */
  const tag = useMutation({
    mutationFn: () => {
      const trimmed = name.trim()
      if (!trimmed) throw new Error('type or choose a name first')
      return api.nameFace(props.face.id, trimmed)
    },
    onSuccess: async (result) => {
      await refresh()
      pushNotice({
        level: 'success',
        message:
          result.filesAdded > 0
            ? `${result.person.name}: ${result.filesAdded} file(s) gathered into their group.`
            : `${result.person.name} named. Their group already had every file they appear in.`,
      })
      props.onClose()
    },
    onError: (e) => setError(String(e instanceof Error ? e.message : e)),
  })

  /** "Wrong person" — back to the unknown pool, keeping the detection. */
  const clear = useMutation({
    mutationFn: () => api.rejectFaces([props.face.id]),
    onSuccess: async () => {
      await refresh()
      pushNotice({ level: 'success', message: 'Tag removed.' })
      props.onClose()
    },
    onError: (e) => setError(String(e instanceof Error ? e.message : e)),
  })

  /** Not a face at all: a poster, a reflection, a blur. */
  const notAFace = useMutation({
    mutationFn: () => api.ignoreFaces([props.face.id]),
    onSuccess: async () => {
      await refresh()
      pushNotice({ level: 'success', message: 'Marked as not a face.' })
      props.onClose()
    },
    onError: (e) => setError(String(e instanceof Error ? e.message : e)),
  })

  const busy = tag.isPending || clear.isPending || notAFace.isPending

  return (
    <div
      className="face-tagger"
      // The click that opened this must not also count as a click outside it.
      onClick={(e) => e.stopPropagation()}
    >
      <div className="hint">Who is this?</div>
      <input
        ref={inputRef}
        value={name}
        onChange={(e) => setName(e.target.value)}
        onKeyDown={(e) => {
          if (e.key === 'Enter' && name.trim()) tag.mutate()
          if (e.key === 'Escape') props.onClose()
        }}
        placeholder="Player name"
        list="tagger-players"
        spellCheck={false}
      />
      <datalist id="tagger-players">
        {people.data?.map((person) => <option key={person.id} value={person.name} />)}
      </datalist>

      {error && <div style={{ color: 'var(--error)', fontSize: 12 }}>{error}</div>}

      <div className="face-tagger-actions">
        <button
          className="small primary"
          disabled={busy || !name.trim()}
          title="Name this person and gather every file they appear in into their group"
          onClick={() => tag.mutate()}
        >
          {tag.isPending ? 'Gathering…' : 'Name & group'}
        </button>
      </div>
      <div className="face-tagger-actions">
        {props.face.personId != null && (
          <button className="small" disabled={busy} onClick={() => clear.mutate()}>
            Wrong person
          </button>
        )}
        <button className="small danger" disabled={busy} onClick={() => notAFace.mutate()}>
          Not a face
        </button>
        <button className="small" disabled={busy} onClick={props.onClose}>
          Cancel
        </button>
      </div>
    </div>
  )
}
