/**
 * Where a browser build (or a client installation not yet reaching its
 * server) says which SKWAD server it belongs to. Replaces the database setup
 * screen for clients: they hold a server address, never database credentials.
 */

import { useState } from 'react'
import * as api from '../api'
import { connectToServer, createTauriTransport, defaultServerUrl } from '../transport'

type Props = {
  onConnected: () => void
  /**
   * Set for a client installation of the desktop app: the address it was
   * configured with, the reason it could not be reached, and the fact that
   * a changed address is kept by the desktop for the next launch.
   */
  desktop?: { serverUrl: string; problem: string | null }
}

export function ServerConnectScreen({ onConnected, desktop }: Props) {
  const [url, setUrl] = useState(desktop?.serverUrl ?? defaultServerUrl())
  const [busy, setBusy] = useState(false)
  const [error, setError] = useState<string | null>(desktop?.problem ?? null)

  const connect = async () => {
    setBusy(true)
    setError(null)
    try {
      if (desktop) {
        // The library comes from the server; this machine's own commands
        // (its worker, its settings) stay on the desktop IPC underneath.
        await connectToServer(url, { local: createTauriTransport() })
        await api.setServerUrl(url)
      } else {
        await connectToServer(url)
      }
      onConnected()
    } catch (e) {
      setError(String((e as Error).message ?? e))
    } finally {
      setBusy(false)
    }
  }

  return (
    <div className="auth-shell">
      <form
        className="auth-card"
        onSubmit={(e) => {
          e.preventDefault()
          void connect()
        }}
      >
        <h1>{desktop ? 'Cannot reach the SKWAD server' : 'Connect to a SKWAD server'}</h1>
        <p className="hint">
          {desktop
            ? 'This machine is set up as a client of a server. Check the address, and that the server is running.'
            : 'The server owns the library. This machine only needs its address — sign in with your usual account once connected.'}
        </p>
        <label className="field">
          <span>Server address</span>
          <input
            type="url"
            value={url}
            placeholder="https://studio-pc:8420"
            onChange={(e) => setUrl(e.target.value)}
            autoFocus
          />
        </label>
        {error && <div className="auth-error">{error}</div>}
        <button className="primary" type="submit" disabled={busy || !url.trim()}>
          {busy ? 'Checking…' : desktop ? 'Retry' : 'Connect'}
        </button>
        {desktop && (
          <button
            type="button"
            className="ghost"
            disabled={busy}
            onClick={() => {
              void api
                .setServerUrl(null)
                .then(() => api.restartForClientChange())
                .catch((e) => setError(String((e as Error).message ?? e)))
            }}
          >
            Use a library on this machine instead
          </button>
        )}
      </form>
    </div>
  )
}
