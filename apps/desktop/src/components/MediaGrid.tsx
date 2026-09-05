import { useEffect, useRef, useState } from 'react'
import type { Media, MediaPickState } from '@teo/shared-types'
import { formatTime, thumbUrl, videoPreviewUrl } from '../media'
import { useUi } from '../store'

/**
 * The thumbnail grid used for browsing and for sorting.
 *
 * Two modes, because the two jobs want opposite defaults. Browsing: a click
 * opens the viewer. Sorting (`selectMode`): a click picks the file, because an
 * editor filing a hundred clips clicks far more than they look, and the viewer
 * is a double-click away.
 */
export function MediaGrid(props: {
  media: Media[]
  selected?: Set<number>
  /** A plain click selects instead of opening the viewer. */
  selectMode?: boolean
  onToggleSelect?: (mediaId: number, additive: boolean) => void
  /** Shift-click: select everything between the last click and this tile. */
  onSelectRange?: (mediaId: number) => void
  /** Names of the groups this file already sits in, drawn as chips. */
  groupsFor?: (mediaId: number) => string[]
  /** A drag started on this tile — the screen turns it into a payload. */
  onDragMedia?: (mediaId: number) => void
  /** Extra per-tile label, e.g. the album's match confidence. */
  cornerLabels?: ReadonlyMap<number, string>
  /** Persists stars and pick/reject flags. The focused tile is used unless it
   *  belongs to the current selection, in which case the whole selection is updated. */
  onEditorial?: (args: {
    mediaIds: number[]
    rating?: number
    pickState?: MediaPickState
  }) => void
  editorialBusy?: boolean
  emptyTitle?: string
  emptyHint?: string
}) {
  const openViewer = useUi((s) => s.openViewer)
  const [previewingVideoId, setPreviewingVideoId] = useState<number | null>(null)
  const previewTimer = useRef<number | null>(null)

  const cancelPreviewTimer = () => {
    if (previewTimer.current !== null) {
      window.clearTimeout(previewTimer.current)
      previewTimer.current = null
    }
  }

  useEffect(() => cancelPreviewTimer, [])

  if (props.media.length === 0) {
    return (
      <div className="empty-state">
        <div className="big">{props.emptyTitle ?? 'No media here yet'}</div>
        <div>{props.emptyHint ?? 'Files will appear as the scanner indexes them.'}</div>
      </div>
    )
  }

  return (
    <div className="media-grid">
      {props.media.map((item) => {
        const isSelected = props.selected?.has(item.id) ?? false
        const chips = props.groupsFor?.(item.id) ?? []
        return (
          <div
            key={item.id}
            className={`media-tile${isSelected ? ' selected' : ''}`}
            tabIndex={0}
            role="button"
            onMouseEnter={() => {
              if (item.mediaType !== 'video') return
              cancelPreviewTimer()
              // Avoid starting FFmpeg work while the pointer is merely passing
              // across the grid. This still feels immediate once a tile is
              // intentionally hovered, like a video-library thumbnail.
              previewTimer.current = window.setTimeout(() => {
                setPreviewingVideoId(item.id)
                previewTimer.current = null
              }, 300)
            }}
            onMouseLeave={() => {
              if (item.mediaType !== 'video') return
              cancelPreviewTimer()
              setPreviewingVideoId((current) => (current === item.id ? null : current))
            }}
            draggable={props.onDragMedia !== undefined}
            onDragStart={(e) => {
              props.onDragMedia?.(item.id)
              // The payload itself travels in the screen's state; the data
              // transfer only has to make the drag legal and show a copy cursor.
              e.dataTransfer.setData('text/plain', String(item.id))
              e.dataTransfer.effectAllowed = 'copy'
            }}
            onClick={(e) => {
              const additive = e.ctrlKey || e.metaKey
              if (e.shiftKey && props.onSelectRange) {
                props.onSelectRange(item.id)
              } else if (props.onToggleSelect && (props.selectMode || additive)) {
                props.onToggleSelect(item.id, props.selectMode ? true : additive)
              } else {
                openViewer(item.id)
              }
            }}
            onDoubleClick={() => props.selectMode && openViewer(item.id)}
            onKeyDown={(event) => {
              if (!props.onEditorial || props.editorialBusy) return
              const mediaIds = isSelected && props.selected?.size ? [...props.selected] : [item.id]
              if (/^[0-5]$/.test(event.key)) {
                event.preventDefault()
                props.onEditorial({ mediaIds, rating: Number(event.key) })
                return
              }
              const key = event.key.toLowerCase()
              if (key === 'p') {
                event.preventDefault()
                props.onEditorial({
                  mediaIds,
                  pickState: item.pickState === 'pick' ? 'none' : 'pick',
                })
              } else if (key === 'x') {
                event.preventDefault()
                props.onEditorial({
                  mediaIds,
                  pickState: item.pickState === 'reject' ? 'none' : 'reject',
                })
              }
            }}
            title={`${item.filename}\n${item.path}${
              chips.length > 0 ? `\nIn: ${chips.join(', ')}` : ''
            }\nShortcuts: 1–5 stars · 0 clears · P pick · X reject`}
          >
            {previewingVideoId === item.id ? (
              <VideoHoverPreview media={item} />
            ) : item.thumbnailPath ? (
              <img src={thumbUrl(item.id)} alt={item.filename} loading="lazy" />
            ) : (
              <div className="placeholder">
                {item.processingStatus === 'failed' ? 'failed' : 'indexing…'}
              </div>
            )}
            {props.cornerLabels?.get(item.id) && (
              <span className="corner confidence">{props.cornerLabels.get(item.id)}</span>
            )}
            {item.mediaType === 'video' && (
              <span className="corner">{item.duration ? formatTime(item.duration) : 'video'}</span>
            )}
            {item.faceCount > 0 && item.mediaType === 'photo' && (
              <span className="corner">
                {item.faceCount} face{item.faceCount > 1 ? 's' : ''}
              </span>
            )}
            {item.mediaType === 'photo' && item.qualityScore !== null && (
              <div className="quality-badges">
                {item.duplicateCount > 1 && (
                  <span className="quality-badge duplicate">{item.duplicateCount} similar</span>
                )}
                {item.isBestShot && item.duplicateCount > 1 && (
                  <span className="quality-badge best">★ Best</span>
                )}
                <span className="quality-badge">Quality {Math.round(item.qualityScore * 100)}</span>
              </div>
            )}
            {(item.rating > 0 || item.pickState !== 'none') && (
              <div className="editorial-badges">
                {item.rating > 0 && <span>{'★'.repeat(item.rating)}</span>}
                {item.pickState === 'pick' && <span className="pick">PICK</span>}
                {item.pickState === 'reject' && <span className="reject">REJECT</span>}
              </div>
            )}
            {chips.length > 0 && (
              <div className="group-chips">
                {chips.slice(0, 2).map((name) => (
                  <span key={name} className="chip" title={name}>
                    {name}
                  </span>
                ))}
                {chips.length > 2 && <span className="chip">+{chips.length - 2}</span>}
              </div>
            )}
            <div className="overlay">{item.filename}</div>
          </div>
        )
      })}
    </div>
  )
}

/**
 * Smooth hover playback from a complete local 512px proxy. GStreamer creates
 * it once during import; subsequent hovers use WebView2's normal hardware
 * video decoder. Only one instance is mounted by MediaGrid.
 */
function VideoHoverPreview({ media }: { media: Media }) {
  const [progress, setProgress] = useState(0)
  const [attempt, setAttempt] = useState(0)
  const [failed, setFailed] = useState(false)
  const [ready, setReady] = useState(false)
  const retryTimer = useRef<number | null>(null)

  useEffect(
    () => () => {
      if (retryTimer.current !== null) window.clearTimeout(retryTimer.current)
    },
    [],
  )

  if (failed) {
    return media.thumbnailPath ? (
      <img src={thumbUrl(media.id)} alt={`${media.filename} preview`} />
    ) : (
      <div className="placeholder">preview unavailable</div>
    )
  }

  return (
    <>
      {media.thumbnailPath ? (
        <img
          className="video-hover-poster"
          src={thumbUrl(media.id)}
          alt={`${media.filename} preview`}
        />
      ) : (
        <div className="placeholder">video</div>
      )}
      <video
        key={attempt}
        className={`video-hover-player${ready ? ' ready' : ''}`}
        src={videoPreviewUrl(media.id, media.contentKey, attempt)}
        muted
        loop
        playsInline
        autoPlay
        preload="auto"
        onCanPlay={(event) => {
          setReady(true)
          event.currentTarget.play().catch(() => {})
        }}
        onTimeUpdate={(event) => {
          const video = event.currentTarget
          setProgress(video.duration > 0 ? video.currentTime / video.duration : 0)
        }}
        onError={() => {
          setReady(false)
          if (attempt >= 7) {
            setFailed(true)
            return
          }
          if (retryTimer.current !== null) window.clearTimeout(retryTimer.current)
          retryTimer.current = window.setTimeout(() => setAttempt((value) => value + 1), 900)
        }}
      />
      {ready && (
        <div className="video-hover-timeline" aria-hidden="true">
          <span style={{ width: `${Math.min(1, Math.max(0, progress)) * 100}%` }} />
        </div>
      )}
    </>
  )
}
