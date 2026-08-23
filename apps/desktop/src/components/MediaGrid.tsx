import type { Media } from '@teo/shared-types'
import { formatTime, thumbUrl } from '../media'
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
  emptyTitle?: string
  emptyHint?: string
}) {
  const openViewer = useUi((s) => s.openViewer)

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
            title={`${item.filename}\n${item.path}${
              chips.length > 0 ? `\nIn: ${chips.join(', ')}` : ''
            }`}
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
