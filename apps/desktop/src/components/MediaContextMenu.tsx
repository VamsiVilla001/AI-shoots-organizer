import { useEffect } from 'react'
import type { Media, MediaPickState } from '@skwad/shared-types'

/**
 * Right-click menu for a single media file. Shared by MediaGrid (thumbnail
 * tiles) and MediaViewer (the full-screen frame), both of which can render
 * inside or outside the project workspace shell, so this stays free of any
 * workspace-only state.
 */
export function MediaContextMenu({ media, x, y, onClose, onOpen, onShowFolder, onEditorial, onSendToPremiere }: {
  media: Media
  x: number
  y: number
  onClose: () => void
  /** Omitted when the menu opens from inside the viewer — there's nothing to open. */
  onOpen?: () => void
  onShowFolder: () => void
  /** Omitted where the caller has no rating/pick controls (e.g. read-only browsing). */
  onEditorial?: (args: { rating?: number; pickState?: MediaPickState }) => void
  /** Omitted where the caller has no Premiere bridge context (e.g. read-only browsing). */
  onSendToPremiere?: () => void
}) {
  useEffect(() => {
    const close = (event: KeyboardEvent) => { if (event.key === 'Escape') onClose() }
    document.addEventListener('keydown', close)
    return () => document.removeEventListener('keydown', close)
  }, [onClose])
  const left = Math.min(x, window.innerWidth - 230)
  const top = Math.min(y, window.innerHeight - 300)
  const run = (action: () => void) => { onClose(); action() }
  return (
    <div className="pw-context-layer" onClick={onClose} onContextMenu={event => { event.preventDefault(); onClose() }}>
      <div className="pw-context-menu" role="menu" aria-label={`${media.filename} actions`} style={{ left, top }} onClick={event => event.stopPropagation()}>
        <strong>{media.filename}</strong>
        {onOpen && <button autoFocus role="menuitem" onClick={() => run(onOpen)}>Open</button>}
        <button role="menuitem" onClick={() => run(onShowFolder)}>Show in folder</button>
        {onSendToPremiere && <button role="menuitem" onClick={() => run(onSendToPremiere)}>Send to Premiere</button>}
        {onEditorial && <>
          <div className="media-context-rating" role="group" aria-label="Rating">
            {[1, 2, 3, 4, 5].map(value => (
              <button key={value} role="menuitem" aria-pressed={media.rating === value} onClick={() => run(() => onEditorial({ rating: value }))}>{value}★</button>
            ))}
          </div>
          {media.rating > 0 && <button role="menuitem" onClick={() => run(() => onEditorial({ rating: 0 }))}>Clear rating</button>}
          <button role="menuitem" onClick={() => run(() => onEditorial({ pickState: media.pickState === 'pick' ? 'none' : 'pick' }))}>{media.pickState === 'pick' ? 'Remove pick' : 'Mark as pick'}</button>
          <button role="menuitem" className="danger" onClick={() => run(() => onEditorial({ pickState: media.pickState === 'reject' ? 'none' : 'reject' }))}>{media.pickState === 'reject' ? 'Remove reject' : 'Mark as reject'}</button>
        </>}
      </div>
    </div>
  )
}
