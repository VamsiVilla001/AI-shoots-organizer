/**
 * Renders one face as a square crop, without a server round-trip.
 *
 * The trick: bounding boxes are stored normalised against the frame, so the
 * thumbnail can be absolutely positioned and scaled inside a square viewport
 * such that only the face region shows. No crop files to generate or cache.
 *
 * Two things this has to get right, both of which it previously got wrong:
 *
 * 1. **The crop square is square in pixels, not in normalised units.** A box is
 *    stored as a fraction of each axis, so 0.2 across is a different number of
 *    pixels from 0.2 down on anything but a square frame. Sizing the window in
 *    normalised units stretched the face, and — once the over-wide window ran
 *    past a frame edge and got clamped — pushed it off centre. The aspect ratio
 *    comes from the loaded image itself, so no extra field has to be plumbed
 *    through every caller.
 *
 * 2. **The image positions itself.** The rules that make this work lived under
 *    `.face-card .crop img` in the stylesheet, so a face drawn anywhere else —
 *    the cluster sample strip in "Who is Unknown Person 1?" — left the image
 *    statically positioned, silently ignored the offsets computed here, and
 *    showed the top-left corner of a hugely magnified frame. Owning the
 *    positioning here means a new container only has to give this a size.
 */

import { useState } from 'react'
import type { BoundingBox } from '@skwad/shared-types'
import { thumbUrl } from '../media'

/** Fraction of the face's own size added around it. */
const DEFAULT_PADDING = 0.35

export function FaceCrop(props: { mediaId: number; bbox: BoundingBox; padding?: number }) {
  const src = thumbUrl(props.mediaId)
  // Keyed by src so a recycled component never positions one face using the
  // aspect ratio of the previous one.
  const [measured, setMeasured] = useState<{ src: string; ratio: number } | null>(null)
  const ratio = measured?.src === src ? measured.ratio : null

  const pad = props.padding ?? DEFAULT_PADDING
  const { x, y, w, h } = props.bbox
  const cx = x + w / 2
  const cy = y + h / 2

  // Width and height of the crop window, each as a fraction of its own axis.
  // They describe one square of pixels: the vertical fraction is the horizontal
  // one times the frame's aspect ratio.
  const aspect = ratio ?? 1
  const across = Math.max(w, h / aspect) * (1 + pad * 2)
  const down = across * aspect

  // Deliberately not clamped to the frame. Clamping keeps the tile full but
  // moves the face off centre exactly when it is near an edge, which is the one
  // thing a face crop must not do; a sliver of background is the better trade.
  const left = cx - across / 2
  const top = cy - down / 2

  const usable = Number.isFinite(across) && Number.isFinite(down) && across > 0 && down > 0

  return (
    <div
      className="crop"
      style={{ position: 'relative', width: '100%', aspectRatio: '1', overflow: 'hidden', background: 'var(--bg-hover)' }}
    >
      <img
        src={src}
        alt=""
        loading="lazy"
        onLoad={(event) => {
          const img = event.currentTarget
          if (img.naturalWidth > 0 && img.naturalHeight > 0) {
            setMeasured({ src, ratio: img.naturalWidth / img.naturalHeight })
          }
        }}
        ref={(img) => {
          // A cached thumbnail can finish loading before React attaches onLoad,
          // and a crop waiting for an event that already happened never appears.
          if (img?.complete && img.naturalWidth > 0 && img.naturalHeight > 0) {
            setMeasured((current) =>
              current?.src === src ? current : { src, ratio: img.naturalWidth / img.naturalHeight },
            )
          }
        }}
        style={{
          position: 'absolute',
          // Sized so the crop window exactly covers the square viewport. Both
          // axes use the true aspect ratio, so `fill` does no stretching.
          width: usable ? `${100 / across}%` : '100%',
          height: usable ? `${100 / down}%` : '100%',
          objectFit: 'fill',
          left: usable ? `${(-left * 100) / across}%` : '0%',
          top: usable ? `${(-top * 100) / down}%` : '0%',
          // Hidden until measured, so the first paint is not a visibly wrong
          // crop that jumps once the aspect ratio arrives.
          visibility: ratio === null ? 'hidden' : 'visible',
        }}
      />
    </div>
  )
}
