/**
 * Boot: connect to a server, then start the app.
 *
 * The desktop shell has already started a private one on loopback, so that path
 * asks it where and connects. A browser may need an address and a token first,
 * and a saved connection is tried silently so a returning user sees the app
 * rather than a form.
 */

import { useCallback, useEffect, useRef, useState } from 'react'
import type { QueryClient } from '@tanstack/react-query'
import App from './App'
import { startEventBridge } from './eventBridge'
import { ConnectionScreen } from './screens/ConnectionScreen'
import { ServerErrorScreen } from './screens/ServerErrorScreen'
import {
  ENDPOINT_CHANGED,
  initDesktopTransport,
  initHttpTransport,
  initialConnection,
  isTauri,
  resetTransport,
  type HttpConnection,
} from './transport'

type Phase =
  | { state: 'starting' }
  | { state: 'connect'; previous: HttpConnection | null; reason: string | null }
  | { state: 'failed'; reason: string }
  | { state: 'ready' }

export function Boot({ queryClient }: { queryClient: QueryClient }) {
  const [phase, setPhase] = useState<Phase>({ state: 'starting' })
  const [attempt, setAttempt] = useState(0)

  // The event bridge is started once, after a transport exists. A ref rather
  // than state: starting it twice would double every invalidation, and nothing
  // renders differently for it.
  const bridged = useRef(false)
  const startBridge = useCallback(() => {
    if (bridged.current) return
    bridged.current = true
    startEventBridge(queryClient).catch((e) => console.error('event bridge failed to start', e))
  }, [queryClient])

  // A restarted server means a new port and a cold cache; everything fetched
  // through the old connection has to be fetched again.
  useEffect(() => {
    const onEndpointChanged = () => {
      queryClient.invalidateQueries()
    }
    window.addEventListener(ENDPOINT_CHANGED, onEndpointChanged)
    return () => window.removeEventListener(ENDPOINT_CHANGED, onEndpointChanged)
  }, [queryClient])

  useEffect(() => {
    if (phase.state !== 'starting') return
    let cancelled = false

    if (isTauri()) {
      initDesktopTransport()
        .then(() => {
          if (cancelled) return
          startBridge()
          setPhase({ state: 'ready' })
        })
        .catch((e) => {
          if (cancelled) return
          resetTransport()
          setPhase({
            state: 'failed',
            reason: String(e instanceof Error ? e.message : e),
          })
        })
      return () => {
        cancelled = true
      }
    }

    const saved = initialConnection()
    if (!saved) {
      setPhase({ state: 'connect', previous: null, reason: null })
      return
    }

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
  }, [phase.state, attempt, startBridge])

  if (phase.state === 'failed') {
    return (
      <ServerErrorScreen
        reason={phase.reason}
        onRetry={() => {
          setAttempt((n) => n + 1)
          setPhase({ state: 'starting' })
        }}
      />
    )
  }

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
        <div className="hint">Starting…</div>
      </div>
    )
  }

  return <App />
}
