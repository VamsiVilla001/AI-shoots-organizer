import type { Media } from '@teo/shared-types'
import { formatTime, thumbUrl } from '../media'
import { useUi } from '../store'

/**
 * The thumbnail grid used by the Albums screen and media browsing.
 * Selection is optional; screens that only browse pass no handlers.
 */
export function MediaGrid(props: {
  media: Media[]
  selected?: Set<number>
  onToggleSelect?: (mediaId: number, additive: boolean) => void
  cornerLabels?: ReadonlyMap<number, string>
}) {
  const openViewer = useUi((s) => s.openViewer)

  if (props.media.length === 0) {
    return (
      <div className="empty-state">
        <div className="big">No media here yet</div>
        <div>Files will appear as the scanner indexes them.</div>
      </div>
    )
  }

  return (
    <div className="media-grid">
      {props.media.map((item) => {
        const isSelected = props.selected?.has(item.id) ?? false
        return (
          <div
            key={item.id}
            className={`media-tile${isSelected ? ' selected' : ''}`}
            onClick={(e) => {
              if (props.onToggleSelect && (e.ctrlKey || e.metaKey || e.shiftKey)) {
                props.onToggleSelect(item.id, true)
              } else {
                openViewer(item.id)
              }
            }}
            title={`${item.filename}\n${item.path}`}
          >
            {item.thumbnailPath ? (
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
              <span className="corner">▶ {item.duration ? formatTime(item.duration) : 'video'}</span>
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
            <div className="overlay">{item.filename}</div>
          </div>
        )
      })}
    </div>
  )
}
