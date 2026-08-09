/**
 * Renders one face as a square crop, without a server round-trip.
 *
 * The trick: bounding boxes are stored normalised against the frame, so the
 * thumbnail can be absolutely positioned and scaled inside a square viewport
 * such that only the face region shows. No crop files to generate or cache.
 */

import type { BoundingBox } from '@teo/shared-types'
import { thumbUrl } from '../media'

export function FaceCrop(props: { mediaId: number; bbox: BoundingBox; padding?: number }) {
  const pad = props.padding ?? 0.35
  const { x, y, w, h } = props.bbox

  // Expand the box, clamped to the frame.
  const cx = x + w / 2
  const cy = y + h / 2
  const side = Math.min(1, Math.max(w, h) * (1 + pad * 2))
  const left = Math.min(Math.max(cx - side / 2, 0), 1 - side)
  const top = Math.min(Math.max(cy - side / 2, 0), 1 - side)

  // Scale the full image so the crop square fills the viewport.
  const scale = 1 / side
  return (
    <div className="crop">
      <img
        src={thumbUrl(props.mediaId)}
        alt=""
        loading="lazy"
        style={{
          width: `${scale * 100}%`,
          height: `${scale * 100}%`,
          objectFit: 'fill',
          left: `${-left * scale * 100}%`,
          top: `${-top * scale * 100}%`,
        }}
      />
    </div>
  )
}
