/**
 * Full-screen viewer: photos with face bounding boxes drawn over them (§5's
 * bounding-box preview), videos with per-player timestamp chips that seek the
 * player on click (§9).
 */

import { useEffect, useRef, useState } from 'react'
import { useQuery } from '@tanstack/react-query'
import * as api from '../api'
import { formatConfidence, formatTime, fullUrl, videoUrl } from '../media'
import { useUi } from '../store'

export function MediaViewer(props: { mediaId: number }) {
  const closeViewer = useUi((s) => s.closeViewer)
  const [showBoxes, setShowBoxes] = useState(true)
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

  useEffect(() => {
    const onKey = (e: KeyboardEvent) => {
      if (e.key === 'Escape') closeViewer()
      if (e.key === 'b') setShowBoxes((v) => !v)
    }
    window.addEventListener('keydown', onKey)
    return () => window.removeEventListener('keydown', onKey)
  }, [closeViewer])

  const item = media.data
  if (!item) return null

  const personName = (personId: number | null) =>
    people.data?.find((p) => p.id === personId)?.name ?? null

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
            <button className="small" onClick={() => setShowBoxes((v) => !v)}>
              {showBoxes ? 'Hide faces (b)' : 'Show faces (b)'}
            </button>
          )}
          <button className="small" onClick={() => api.revealInFolder(item.path)}>
            Show in folder
          </button>
          <button className="small" onClick={closeViewer}>
            Close (Esc)
          </button>
        </div>
      </div>

      <div className="viewer-stage" onClick={(e) => e.target === e.currentTarget && closeViewer()}>
        <div className="frame">
          {item.mediaType === 'photo' ? (
            <>
              <img src={fullUrl(item.id)} alt={item.filename} />
              {showBoxes &&
                faces.data
                  ?.filter((f) => f.assignment !== 'ignored')
                  .map((face) => (
                    <div
                      key={face.id}
                      className="face-box"
                      style={{
                        left: `${face.bbox.x * 100}%`,
                        top: `${face.bbox.y * 100}%`,
                        width: `${face.bbox.w * 100}%`,
                        height: `${face.bbox.h * 100}%`,
                      }}
                    >
                      <span>
                        {personName(face.personId) ?? 'Unknown'}
                        {face.recognitionConfidence != null &&
                          ` ${formatConfidence(face.recognitionConfidence)}`}
                      </span>
                    </div>
                  ))}
            </>
          ) : (
            <video ref={videoRef} src={videoUrl(item.id)} controls autoPlay />
          )}
        </div>
      </div>

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
