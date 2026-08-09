import type { ReactNode } from 'react'

export function Modal(props: { title: string; onClose: () => void; children: ReactNode }) {
  return (
    <div
      className="modal-backdrop"
      onClick={(e) => {
        if (e.target === e.currentTarget) props.onClose()
      }}
    >
      <div className="modal" role="dialog" aria-label={props.title}>
        <h3>{props.title}</h3>
        {props.children}
      </div>
    </div>
  )
}
