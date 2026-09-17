import { useUi } from '../store'
import { Icon, type IconName } from './Icon'

const LEVEL_ICON: Record<string, IconName> = { success: 'success', error: 'error', info: 'info' }

export function Notices() {
  const notices = useUi((s) => s.notices)
  const dismiss = useUi((s) => s.dismissNotice)

  if (notices.length === 0) return null
  return (
    <div className="notices">
      {notices.map((notice) => (
        <div
          key={notice.id}
          className={`notice ${notice.level}`}
          onClick={() => dismiss(notice.id)}
          title="Click to dismiss"
        >
          <Icon name={LEVEL_ICON[notice.level] ?? 'info'} />
          <span>{notice.message}</span>
        </div>
      ))}
    </div>
  )
}
