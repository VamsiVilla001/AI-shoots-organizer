import { useUi } from '../store'

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
          {notice.message}
        </div>
      ))}
    </div>
  )
}
