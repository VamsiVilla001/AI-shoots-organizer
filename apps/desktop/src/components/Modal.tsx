import { useEffect, useRef, type ReactNode } from 'react'

/**
 * A modal over the current screen.
 *
 * Backed by a native `<dialog>` opened with `showModal()`, which puts it in
 * the browser's top layer. That is what lets one modal open from inside
 * another — the folder browser from the "Add media" form — and land on top:
 * the top layer stacks by open order, where a `z-index` cannot reach it.
 */
export function Modal(props: { title: string; onClose: () => void; children: ReactNode }) {
  const ref = useRef<HTMLDialogElement>(null)

  useEffect(() => {
    const dialog = ref.current
    if (dialog && !dialog.open) dialog.showModal()
    return () => {
      if (dialog?.open) dialog.close()
    }
  }, [])

  return (
    <dialog
      ref={ref}
      className="modal-backdrop"
      aria-label={props.title}
      onCancel={(e) => {
        // Escape: ours to handle, so the React state closes it rather than
        // the element vanishing on its own.
        e.preventDefault()
        props.onClose()
      }}
      onClick={(e) => {
        if (e.target === e.currentTarget) props.onClose()
      }}
    >
      <div className="modal">
        <h3>{props.title}</h3>
        {props.children}
      </div>
    </dialog>
  )
}
