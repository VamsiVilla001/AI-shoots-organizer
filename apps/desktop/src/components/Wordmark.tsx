/**
 * The SKWAD lockup used in the sidebars and on the sign-in screen.
 *
 * Two rules from the brand system shape this component:
 *
 * - **Always the drawn files — never rebuild, re-space or recolour.** So the
 *   wordmark is the official SVG, and a dark ground gets `wordmark-light.svg`
 *   (Cream) rather than a CSS-tinted copy of the dark one.
 * - **Descriptive text is never attached to the mark as a unit.** "Media
 *   Organiser" is therefore set apart as mono microcopy under the wordmark's
 *   clearspace, not welded into a custom lockup.
 *
 * The descriptor is sized so its measured width matches the wordmark's: 11px
 * JetBrains Mono at 0.1em tracking across "MEDIA ORGANISER" comes to the same
 * width as a 124px wordmark, which is why those two numbers appear together in
 * the stylesheet.
 */

import { useUi } from '../store'

export function Wordmark({ label = 'Media Organiser', className }: { label?: string; className?: string }) {
  const theme = useUi((state) => state.theme)
  // Cream on dark grounds, Cocoa on light: the brand ships one drawn file for
  // each, and picking the wrong one is what made the mark disappear in dark
  // mode before.
  const file = theme === 'dark' ? 'wordmark-light.svg' : 'wordmark-dark-colour.svg'

  return (
    <div className={className ? `wordmark ${className}` : 'wordmark'}>
      <img src={`/logo/${file}`} alt="SKWAD" />
      {label && <span className="wordmark-label">{label}</span>}
    </div>
  )
}
