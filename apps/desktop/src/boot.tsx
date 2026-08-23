/**
 * Boot: choose the transport, then start the app.
 *
 * The desktop path is immediate — Tauri is right there. The browser path may
 * need a server address and a token first, and a saved connection is tried
 * silently so a returning user sees the app rather than a form.
 */

import { useEffect, useState } from 'react'
import type { QueryClient } from '@tanstack/react-query'
import App from './App'
import { startEventBridge } from './eventBridge'
import { ConnectionScreen } from './screens/ConnectionScreen'
import {
  initHttpTransport,
  initTauriTransport,
  initialConnection,
  isTauri,
  resetTransport,
  type HttpConnection,
} from './transport'

type Phase =
  | { state: 'starting' }
  | { state: 'connect'; previous: HttpConnection | null; reason: string | null }
  | { state: 'ready' }

export function Boot({ queryClient }: { queryClient: QueryClient }) {
  const [phase, setPhase] = useState<Phase>({ state: 'starting' })

  // The event bridge is started once per transport, after it exists.
  const [bridged, setBridged] = useState(false)
  const startBridge = () => {
    if (bridged) return
    setBridged(true)
    startEventBridge(queryClient).catch((e) => console.error('event bridge failed to start', e))
  }

  useEffect(() => {
    if (phase.state !== 'starting') return

    if (isTauri()) {
      initTauriTransport()
      startBridge()
      setPhase({ state: 'ready' })
      return
    }

    const saved = initialConnection()
    if (!saved) {
      setPhase({ state: 'connect', previous: null, reason: null })
      return
    }

    let cancelled = false
    initHttpTransport(saved)
      .then(() => {
        if (cancelled) return
        startBridge()
        setPhase({ state: 'ready' })
      })
      .catch(() => {
        if (cancelled) return
        // A saved connection that no longer works lands on the form with its
        // values kept, rather than silently clearing them.
        resetTransport()
        setPhase({
          state: 'connect',
          previous: saved,
          reason: 'That saved connection could not be reached. Check the address and token.',
        })
      })

    return () => {
      cancelled = true
    }
  }, [phase.state])

  if (phase.state === 'connect') {
    return (
      <ConnectionScreen
        previous={phase.previous}
        reason={phase.reason}
        onConnected={() => {
          startBridge()
          setPhase({ state: 'ready' })
        }}
      />
    )
  }

  if (phase.state === 'starting') {
    return (
      <div className="connect-shell">
        <div className="hint">Connecting…</div>
      </div>
    )
  }

  return <App />
}
