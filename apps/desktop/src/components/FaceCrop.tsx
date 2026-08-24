/**
 * Renders one face as a square crop, without a server round-trip.
 *
 * The trick: bounding boxes are stored normalised against the frame, so the
 * image can be absolutely positioned and scaled inside a square viewport such
 * that only the face region shows. No crop files to generate or cache.
 *
 * Two things make that less trivial than it looks:
 *
 * 1. **Normalised space is not square.** A shoot is 3:2 or 16:9, so 0.2 across
 *    is a different number of pixels from 0.2 down. Treating the box as square
 *    stretched the face, and — because the resulting window was far wider than
 *    it was tall — pushed it against the tile edge as soon as the window was
 *    clamped to the frame. The crop is therefore sized in *pixels* and needs
 *    the image's aspect ratio, which it takes from the image itself on load.
 * 2. **A video face is not on the poster frame.** Faces in a clip are stored
 *    with the timestamp they were found at, while the cached thumbnail is a
 *    frame a tenth of the way in. Pass `frameTime` and the crop asks the server
 *    to render that frame instead.
 *
 * The face is always dead centre. A face near an edge means the square runs
 * past the frame, and the tile's own background shows through — off-centre would
 * be the worse answer, since the point of the tile is "who is this".
 */

import { useCallback, useState } from 'react'
import type { BoundingBox } from '@teo/shared-types'
import { frameUrl, thumbUrl } from '../media'

/** Context around the face, as a fraction of its own size on each side. */
const DEFAULT_PADDING = 0.4

export function FaceCrop(props: {
  mediaId: number
  bbox: BoundingBox
  /** The video timestamp this face was detected at; `null` for stills. */
  frameTime?: number | null
  padding?: number
}) {
  const src = props.frameTime != null ? frameUrl(props.mediaId, props.frameTime) : thumbUrl(props.mediaId)

  // Keyed by src so a re-used tile does not position the next face with the
  // previous image's aspect ratio.
  const [measured, setMeasured] = useState<{ src: string; aspect: number } | null>(null)
  const aspect = measured?.src === src ? measured.aspect : null

  /**
   * Measured from the element rather than the `load` event alone: a cached
   * thumbnail — most of them, once a tile has been seen — can finish loading
   * before React attaches the handler, and a crop that waits for an event that
   * already happened never appears at all.
   */
  const measure = useCallback(
    (img: HTMLImageElement | null) => {
      if (!img || img.naturalWidth === 0 || img.naturalHeight === 0) return
      const found = img.naturalWidth / img.naturalHeight
      // Returning the previous object keeps this out of a render loop, since a
      // ref callback runs on every commit.
      setMeasured((prev) => (prev?.src === src && prev.aspect === found ? prev : { src, aspect: found }))
    },
    [src],
  )

  const { x, y, w, h } = props.bbox
  const pad = props.padding ?? DEFAULT_PADDING

  // A square of `side` pixels is `side/width` of the frame across and
  // `side/height` of it down — the two differ by exactly the aspect ratio.
  const ratio = aspect && aspect > 0 ? aspect : 1
  const across = Math.max(w, h / ratio) * (1 + pad * 2)
  const down = across * ratio

  const left = x + w / 2 - across / 2
  const top = y + h / 2 - down / 2

  return (
    <div className="crop">
      <img
        src={src}
        alt=""
        loading="lazy"
        ref={measure}
        onLoad={(e) => measure(e.currentTarget)}
        // Nothing to measure, so stop hiding it: an empty tile says less about
        // what went wrong than the browser's own broken-image mark.
        onError={() => setMeasured({ src, aspect: 1 })}
        style={{
          // Scale the whole image so the crop square fills the viewport, then
          // slide the square's top-left corner to the viewport's.
          width: `${(1 / across) * 100}%`,
          height: `${(1 / down) * 100}%`,
          objectFit: 'fill',
          left: `${(-left / across) * 100}%`,
          top: `${(-top / down) * 100}%`,
          // Positioned with a guessed aspect ratio until the image reports its
          // own; showing that first pass would be a visible jump.
          visibility: aspect === null ? 'hidden' : 'visible',
        }}
      />
    </div>
  )
}
