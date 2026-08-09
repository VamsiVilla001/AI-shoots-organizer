import { useQuery } from '@tanstack/react-query'
import * as api from '../api'
import { useUi, type Screen } from '../store'

const ITEMS: Array<{ id: Screen; label: string; needsShoot: boolean }> = [
  { id: 'shoots', label: 'Shoots', needsShoot: false },
  { id: 'players', label: 'Players', needsShoot: false },
  { id: 'albums', label: 'AI Albums', needsShoot: true },
  { id: 'review', label: 'Review', needsShoot: true },
  { id: 'export', label: 'Export', needsShoot: true },
  { id: 'settings', label: 'Settings', needsShoot: false },
]

export function Sidebar() {
  const screen = useUi((s) => s.screen)
  const navigate = useUi((s) => s.navigate)
  const activeShootId = useUi((s) => s.activeShootId)
  const progress = useUi((s) => s.progress)

  const shoots = useQuery({ queryKey: ['shoots'], queryFn: api.listShoots })
  const activeShoot = shoots.data?.find((s) => s.id === activeShootId)

  // A little live counter beside "Review" so pending work is visible from
  // anywhere in the app.
  const unknown = activeShootId != null ? progress[activeShootId]?.facesUnknown : undefined

  return (
    <aside className="sidebar">
      <div className="brand">
        Esports <em>AI</em> Media Organiser
      </div>
      <nav>
        {ITEMS.map((item) => (
          <button
            key={item.id}
            className={screen === item.id ? 'active' : undefined}
            disabled={item.needsShoot && activeShootId === null}
            onClick={() => navigate(item.id)}
            title={item.needsShoot && activeShootId === null ? 'Open a shoot first' : undefined}
          >
            <span>{item.label}</span>
            {item.id === 'review' && unknown !== undefined && unknown > 0 && (
              <span className="badge">{unknown}</span>
            )}
          </button>
        ))}
      </nav>
      <div className="spacer" />
      {activeShoot && (
        <div className="shoot-chip" title={activeShoot.sourcePath}>
          Working on: <strong>{activeShoot.name}</strong>
        </div>
      )}
    </aside>
  )
}
