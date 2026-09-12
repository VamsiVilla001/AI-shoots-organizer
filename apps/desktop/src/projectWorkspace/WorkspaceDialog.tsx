import { useEffect, useRef, type ReactNode } from 'react'

export function WorkspaceDialog({ title, onClose, children }: { title: string; onClose: () => void; children: ReactNode }) {
  const ref = useRef<HTMLDialogElement>(null)
  useEffect(() => {
    const previous = document.activeElement as HTMLElement | null
    ref.current?.showModal()
    return () => { previous?.focus() }
  }, [])
  return <dialog className="pw-dialog" ref={ref} aria-label={title} onCancel={event => { event.preventDefault(); onClose() }}>
    <div className="pw-dialog-heading"><h2>{title}</h2><button onClick={onClose} aria-label="Close dialog">Close</button></div>
    {children}
  </dialog>
}
