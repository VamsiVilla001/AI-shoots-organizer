/**
 * The application icon set.
 *
 * Drawn inline rather than pulled from an icon package: the app ships offline,
 * and a handful of 24px strokes is smaller than a dependency. Every icon uses
 * `currentColor`, so it takes the colour of the button or label it sits in and
 * follows the light and dark themes for free.
 *
 * Icons are decorative — each one sits beside a text label, so they are
 * `aria-hidden` and never the only way to tell two controls apart.
 */

import type { ReactNode } from 'react'

const PATHS: Record<string, ReactNode> = {
  // --- navigation ---------------------------------------------------------
  shoots: <>
    <path d="M14.5 4h-5L7 7H4a2 2 0 0 0-2 2v9a2 2 0 0 0 2 2h16a2 2 0 0 0 2-2V9a2 2 0 0 0-2-2h-3l-2.5-3Z" />
    <circle cx="12" cy="13" r="3.5" />
  </>,
  groups: <>
    <rect x="3" y="3" width="7" height="7" rx="1.5" />
    <rect x="14" y="3" width="7" height="7" rx="1.5" />
    <rect x="3" y="14" width="7" height="7" rx="1.5" />
    <rect x="14" y="14" width="7" height="7" rx="1.5" />
  </>,
  players: <>
    <circle cx="9" cy="8" r="3.5" />
    <path d="M2.5 20a6.5 6.5 0 0 1 13 0" />
    <path d="M16 5.2a3.5 3.5 0 0 1 0 6.6" />
    <path d="M18 14.4a6.5 6.5 0 0 1 3.5 5.6" />
  </>,
  albums: <>
    <rect x="3" y="3" width="18" height="18" rx="2" />
    <circle cx="8.5" cy="8.5" r="1.5" />
    <path d="m21 15-5-5L5 21" />
  </>,
  review: <>
    <circle cx="9" cy="8" r="3.5" />
    <path d="M2.5 20a6.5 6.5 0 0 1 12 0" />
    <path d="m16 13 2 2 4-4" />
  </>,
  export: <>
    <path d="M12 3v12" />
    <path d="m7 11 5 5 5-5" />
    <path d="M4 20h16" />
  </>,
  catalogues: <>
    <path d="m12 2 9 5v10l-9 5-9-5V7l9-5Z" />
    <path d="m3 7 9 5 9-5" />
    <path d="M12 12v10" />
  </>,
  collections: <>
    <path d="m12 2 9 5-9 5-9-5 9-5Z" />
    <path d="m3 12 9 5 9-5" />
    <path d="m3 17 9 5 9-5" />
  </>,
  processing: <>
    <rect x="6" y="6" width="12" height="12" rx="2" />
    <rect x="9.5" y="9.5" width="5" height="5" rx="1" />
    <path d="M9 2v3M15 2v3M9 19v3M15 19v3M2 9h3M2 15h3M19 9h3M19 15h3" />
  </>,
  admin: <>
    <path d="M12 2.5 20 6v6c0 4.5-3.2 8.4-8 9.5-4.8-1.1-8-5-8-9.5V6l8-3.5Z" />
    <path d="m9 12 2 2 4-4" />
  </>,
  profile: <>
    <circle cx="12" cy="12" r="9" />
    <circle cx="12" cy="10" r="3" />
    <path d="M6.2 18.6a7 7 0 0 1 11.6 0" />
  </>,
  settings: <>
    <path d="M4 7h9M19 7h1M4 17h3M13 17h7" />
    <circle cx="16" cy="7" r="2.5" />
    <circle cx="10" cy="17" r="2.5" />
  </>,

  // --- actions ------------------------------------------------------------
  add: <path d="M12 5v14M5 12h14" />,
  edit: <>
    <path d="M4 20h4L20 8l-4-4L4 16v4Z" />
    <path d="m14 6 4 4" />
  </>,
  password: <>
    <circle cx="7.5" cy="15.5" r="4" />
    <path d="m10.4 12.6 9-9" />
    <path d="m16.5 6.5 2.5 2.5" />
    <path d="m13.5 9.5 2.5 2.5" />
  </>,
  remove: <>
    <path d="M4 7h16" />
    <path d="M9 7V5a1 1 0 0 1 1-1h4a1 1 0 0 1 1 1v2" />
    <path d="m6 7 1 13a1 1 0 0 0 1 1h8a1 1 0 0 0 1-1l1-13" />
  </>,
  search: <>
    <circle cx="10.5" cy="10.5" r="6.5" />
    <path d="m15.5 15.5 5 5" />
  </>,
  folder: <path d="M3 7a2 2 0 0 1 2-2h4l2 2.5h8a2 2 0 0 1 2 2V18a2 2 0 0 1-2 2H5a2 2 0 0 1-2-2V7Z" />,
  copy: <>
    <rect x="9" y="9" width="12" height="12" rx="2" />
    <path d="M5 15H4a1 1 0 0 1-1-1V4a1 1 0 0 1 1-1h10a1 1 0 0 1 1 1v1" />
  </>,
  restart: <>
    <path d="M20.5 12a8.5 8.5 0 1 1-2.6-6.1" />
    <path d="M20.5 3v5.5H15" />
  </>,
  save: <>
    <path d="M5 4h11l3 3v13H5V4Z" />
    <path d="M9 4v5h6V4" />
    <path d="M8 20v-6h8v6" />
  </>,
  close: <path d="m6 6 12 12M18 6 6 18" />,

  // --- status -------------------------------------------------------------
  success: <>
    <circle cx="12" cy="12" r="9" />
    <path d="m8 12 2.5 2.5L16 9" />
  </>,
  info: <>
    <circle cx="12" cy="12" r="9" />
    <path d="M12 11.5v5" />
    <path d="M12 7.6h.01" />
  </>,
  error: <>
    <path d="M12 4 2.8 20h18.4L12 4Z" />
    <path d="M12 10.5v4" />
    <path d="M12 17.6h.01" />
  </>,
  network: <>
    <rect x="3" y="4" width="18" height="6" rx="2" />
    <rect x="3" y="14" width="18" height="6" rx="2" />
    <path d="M7 7h.01M7 17h.01" />
  </>,
  sun: <>
    <circle cx="12" cy="12" r="4" />
    <path d="M12 2v2.5M12 19.5V22M4.2 4.2l1.8 1.8M18 18l1.8 1.8M2 12h2.5M19.5 12H22M4.2 19.8 6 18M18 6l1.8-1.8" />
  </>,
  moon: <path d="M20 14.5A8.5 8.5 0 0 1 9.5 4 8.5 8.5 0 1 0 20 14.5Z" />,
}

export type IconName = keyof typeof PATHS

export function Icon({ name, className }: { name: IconName; className?: string }) {
  return (
    <svg
      className={className ? `icon ${className}` : 'icon'}
      viewBox="0 0 24 24"
      fill="none"
      stroke="currentColor"
      strokeWidth={1.75}
      strokeLinecap="round"
      strokeLinejoin="round"
      aria-hidden="true"
      focusable="false"
    >
      {PATHS[name]}
    </svg>
  )
}
