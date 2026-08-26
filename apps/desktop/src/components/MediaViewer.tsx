/**
 * Full-screen viewer: photos with face bounding boxes drawn over them (§5's
 * bounding-box preview), videos with per-player timestamp chips that seek the
 * player on click (§9).
 *
 * The boxes are the answer to "which of these people do you mean?" — click one
 * and name that face, rather than naming the file and hoping. A photo with
 * several people says so, and says how many.
 */

import { useEffect, useRef, useState } from 'react'
import type { PointerEvent as ReactPointerEvent } from 'react'
import { useMutation, useQuery, useQueryClient } from '@tanstack/react-query'
import type { BoundingBox } from '@teo/shared-types'
import * as api from '../api'
import { FaceTagger } from './FaceTagger'
import { formatConfidence, formatCount, formatTime, fullUrl, videoUrl } from '../media'
import { useUi } from '../store'

export function MediaViewer(props: { mediaId: number }) {
  const closeViewer = useUi((s) => s.closeViewer)
  const pushNotice = useUi((s) => s.pushNotice)
  const queryClient = useQueryClient()
  const [showBoxes, setShowBoxes] = useState(true)
  /** The face being named, if any. */
  const [taggingId, setTaggingId] = useState<number | null>(null)
  const [drawingFace, setDrawingFace] = useState(false)
  const [draftBox, setDraftBox] = useState<BoundingBox | null>(null)
  const drawStart = useRef<{ point: { x: number; y: number }; clientX: number; clientY: number } | null>(null)
  const frameRef = useRef<HTMLDivElement>(null)
  const videoRef = useRef<HTMLVideoElement>(null)

  const media = useQuery({
    queryKey: ['media-item', props.mediaId],
    queryFn: () => api.getMedia(props.mediaId),
  })
  const faces = useQuery({
    queryKey: ['media-faces', props.mediaId],
    queryFn: () => api.mediaFaces(props.mediaId),
  })
  const timelines = useQuery({
    queryKey: ['video-timelines', props.mediaId],
    queryFn: () => api.videoTimelines(props.mediaId),
    enabled: media.data?.mediaType === 'video',
  })
  const people = useQuery({ queryKey: ['people'], queryFn: () => api.listPeople(null) })

  const addFace = useMutation({
    mutationFn: (bbox: BoundingBox) => api.addManualFace(props.mediaId, bbox),
    onSuccess: async (result) => {
      await Promise.all([
        faces.refetch(),
        queryClient.invalidateQueries({ queryKey: ['media-item', props.mediaId] }),
        queryClient.invalidateQueries({ queryKey: ['media'] }),
        queryClient.invalidateQueries({ queryKey: ['faces'] }),
      ])
      setDraftBox(null)
      setDrawingFace(false)
      setTaggingId(result.face.id)
      pushNotice({
        level: 'success',
        message: result.suggestedPerson
          ? `Face found. Best match: ${result.suggestedPerson.name}. Please confirm or correct it.`
          : 'Face added. No safe named match was found, so choose or type the correct name.',
      })
    },
    onError: (error) => {
      setDraftBox(null)
      pushNotice({ level: 'error', message: String(error instanceof Error ? error.message : error) })
    },
  })

  useEffect(() => {
    const onKey = (e: KeyboardEvent) => {
      // Escape backs out of tagging first; a second press closes the viewer, so
      // the key never does two things at once.
      if (e.key === 'Escape') {
        if (drawingFace) {
          if (addFace.isPending) return
          setDrawingFace(false)
          setDraftBox(null)
          drawStart.current = null
          return
        }
        setTaggingId((current) => {
          if (current !== null) return null
          closeViewer()
          return null
        })
      }
      // Not while typing a name into the tagger.
      if (e.key === 'b' && !(e.target instanceof HTMLInputElement)) setShowBoxes((v) => !v)
    }
    window.addEventListener('keydown', onKey)
    return () => window.removeEventListener('keydown', onKey)
  }, [addFace.isPending, closeViewer, drawingFace])

  const item = media.data
  if (!item) return null

  const personName = (personId: number | null) =>
    people.data?.find((p) => p.id === personId)?.name ?? null

  const visibleFaces = (faces.data ?? []).filter((f) => f.assignment !== 'ignored')
  const unnamedCount = visibleFaces.filter((f) => f.personId == null).length
  const taggingFace = visibleFaces.find((face) => face.id === taggingId) ?? null

  const pointInFrame = (event: ReactPointerEvent<HTMLDivElement>) => {
    const rect = frameRef.current?.getBoundingClientRect()
    if (!rect || rect.width <= 0 || rect.height <= 0) return null
    return {
      x: Math.min(1, Math.max(0, (event.clientX - rect.left) / rect.width)),
      y: Math.min(1, Math.max(0, (event.clientY - rect.top) / rect.height)),
    }
  }

  const boxBetween = (start: { x: number; y: number }, end: { x: number; y: number }): BoundingBox => ({
    x: Math.min(start.x, end.x),
    y: Math.min(start.y, end.y),
    w: Math.abs(end.x - start.x),
    h: Math.abs(end.y - start.y),
  })

  const beginFaceBox = (event: ReactPointerEvent<HTMLDivElement>) => {
    if (!drawingFace || addFace.isPending) return
    const point = pointInFrame(event)
    if (!point) return
    event.preventDefault()
    event.stopPropagation()
    event.currentTarget.setPointerCapture(event.pointerId)
    drawStart.current = { point, clientX: event.clientX, clientY: event.clientY }
    setDraftBox({ x: point.x, y: point.y, w: 0, h: 0 })
  }

  const moveFaceBox = (event: ReactPointerEvent<HTMLDivElement>) => {
    if (!drawingFace || !drawStart.current || addFace.isPending) return
    const point = pointInFrame(event)
    if (!point) return
    event.preventDefault()
    setDraftBox(boxBetween(drawStart.current.point, point))
  }

  const finishFaceBox = (event: ReactPointerEvent<HTMLDivElement>) => {
    const start = drawStart.current
    if (!drawingFace || !start || addFace.isPending) return
    const point = pointInFrame(event)
    drawStart.current = null
    if (!point) {
      setDraftBox(null)
      return
    }
    event.preventDefault()
    event.stopPropagation()

    const frame = frameRef.current?.getBoundingClientRect()
    const wasClick = Math.hypot(event.clientX - start.clientX, event.clientY - start.clientY) < 8
    const box = wasClick && frame
      ? (() => {
          // A click marks the centre; use a roughly 112px square as a useful
          // starting crop. Dragging remains the precise option for large or
          // unusually framed faces.
          const w = Math.min(0.28, Math.max(0.06, 112 / frame.width))
          const h = Math.min(0.28, Math.max(0.06, 112 / frame.height))
          return {
            x: Math.min(1 - w, Math.max(0, point.x - w / 2)),
            y: Math.min(1 - h, Math.max(0, point.y - h / 2)),
            w,
            h,
          }
        })()
      : boxBetween(start.point, point)

    setDraftBox(box)
    addFace.mutate(box)
  }

  const seekTo = (seconds: number) => {
    const video = videoRef.current
    if (video) {
      video.currentTime = seconds
      video.play().catch(() => {})
    }
  }

  return (
    <div className="viewer-backdrop">
      <div className="viewer-top">
        <div>
          <strong>{item.filename}</strong>{' '}
          <span className="hint">
            {item.width && item.height ? `${item.width}×${item.height}` : ''}
            {item.cameraModel ? ` · ${item.cameraModel}` : ''}
          </span>
        </div>
        <div style={{ display: 'flex', gap: 8 }}>
          {item.mediaType === 'photo' && (
            <>
              <button
                className={`small${drawingFace ? ' primary' : ''}`}
                disabled={addFace.isPending}
                onClick={() => {
                  setDrawingFace((current) => !current)
                  setDraftBox(null)
                  setTaggingId(null)
                  setShowBoxes(true)
                }}
              >
                {drawingFace ? 'Cancel marking (Esc)' : 'Add missed face'}
              </button>
              <button className="small" onClick={() => setShowBoxes((v) => !v)}>
                {showBoxes ? 'Hide faces (b)' : 'Show faces (b)'}
              </button>
            </>
          )}
          <button className="small" onClick={() => api.revealInFolder(item.path)}>
            Show in folder
          </button>
          <button className="small" onClick={closeViewer}>
            Close (Esc)
          </button>
        </div>
      </div>

      {item.mediaType === 'photo' && drawingFace && (
        <div className="viewer-prompt manual-face-prompt">
          {addFace.isPending
            ? 'Reading the marked face and comparing it with named faces…'
            : 'Click the centre of a missed face, or drag a tight box around it.'}
        </div>
      )}

      {item.mediaType === 'photo' && !drawingFace && showBoxes && visibleFaces.length > 1 && taggingId === null && (
        <div className="viewer-prompt">
          {formatCount(visibleFaces.length)} people here
          {unnamedCount > 0 && `, ${formatCount(unnamedCount)} not named yet`} — click a face to say
          who it is.
        </div>
      )}

      <div className="viewer-stage" onClick={(e) => e.target === e.currentTarget && closeViewer()}>
        <div
          ref={frameRef}
          className={`frame${drawingFace ? ' drawing-face' : ''}`}
          onPointerDown={beginFaceBox}
          onPointerMove={moveFaceBox}
          onPointerUp={finishFaceBox}
          onPointerCancel={() => {
            drawStart.current = null
            if (!addFace.isPending) setDraftBox(null)
          }}
        >
          {item.mediaType === 'photo' ? (
            <>
              <img src={fullUrl(item.id)} alt={item.filename} />
              {showBoxes &&
                visibleFaces.map((face) => (
                  <div
                    key={face.id}
                    className={`face-box taggable${taggingId === face.id ? ' tagging' : ''}${
                      face.personId == null ? ' unnamed' : ''
                    }`}
                    style={{
                      left: `${face.bbox.x * 100}%`,
                      top: `${face.bbox.y * 100}%`,
                      width: `${face.bbox.w * 100}%`,
                      height: `${face.bbox.h * 100}%`,
                    }}
                    title={personName(face.personId) ? 'Click to change who this is' : 'Click to name this person'}
                    onClick={(e) => {
                      e.stopPropagation()
                      setDrawingFace(false)
                      setDraftBox(null)
                      setTaggingId((current) => (current === face.id ? null : face.id))
                    }}
                  >
                    <span>
                      {personName(face.personId) ?? 'Tap to name'}
                      {face.recognitionConfidence != null &&
                        ` ${formatConfidence(face.recognitionConfidence)}`}
                    </span>
                  </div>
                ))}
              {draftBox && (
                <div
                  className="manual-face-box"
                  style={{
                    left: `${draftBox.x * 100}%`,
                    top: `${draftBox.y * 100}%`,
                    width: `${draftBox.w * 100}%`,
                    height: `${draftBox.h * 100}%`,
                  }}
                />
              )}
            </>
          ) : (
            <video ref={videoRef} src={videoUrl(item.id)} controls autoPlay />
          )}
        </div>
      </div>

      {taggingFace && (
        <FaceTagger
          face={taggingFace}
          currentName={personName(taggingFace.personId)}
          mediaId={item.id}
          onClose={() => setTaggingId(null)}
        />
      )}

      {item.mediaType === 'video' && timelines.data && timelines.data.length > 0 && (
        <div className="timeline-chips">
          {timelines.data.map((timeline) =>
            timeline.appearances.map((appearance) => (
              <button
                key={appearance.id}
                className="chip"
                onClick={() => seekTo(appearance.timestamp)}
                title={`Confidence ${formatConfidence(appearance.confidence)}`}
              >
                {timeline.personName ?? 'Unknown'} · {formatTime(appearance.timestamp)}
                {appearance.endTimestamp != null && `–${formatTime(appearance.endTimestamp)}`}
              </button>
            )),
          )}
        </div>
      )}
    </div>
  )
}
