import { useUi } from '../store'
import { Icon } from './Icon'

/** A small button that flips <html data-theme>, shared by the Classic sidebar and the Project workspace sidebar. */
export function ThemeToggle({ className }: { className?: string }) {
  const theme = useUi((s) => s.theme)
  const toggleTheme = useUi((s) => s.toggleTheme)
  const switchingTo = theme === 'dark' ? 'light' : 'dark'

  return (
    <button
      type="button"
      className={className}
      onClick={toggleTheme}
      title={`Switch to ${switchingTo} theme`}
      aria-label={`Switch to ${switchingTo} theme`}
    >
      <Icon name={theme === 'dark' ? 'sun' : 'moon'} />
      <span>{switchingTo === 'dark' ? 'Dark' : 'Light'} mode</span>
    </button>
  )
}
