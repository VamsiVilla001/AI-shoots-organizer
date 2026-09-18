/**
 * Where a browser build (or a desktop client not yet pointed anywhere) says
 * which SKWAD server it belongs to. Replaces the database setup screen for
 * clients: they hold a server address, never database credentials.
 */

import { useState } from 'react'
import { connectToServer, defaultServerUrl } from '../transport'

export function ServerConnectScreen({ onConnected }: { onConnected: () => void }) {
  const [url, setUrl] = useState(defaultServerUrl())
  const [busy, setBusy] = useState(false)
  const [error, setError] = useState<string | null>(null)

  const connect = async () => {
    setBusy(true)
    setError(null)
    try {
      await connectToServer(url)
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
        <h1>Connect to a SKWAD server</h1>
        <p className="hint">
          The server owns the library. This machine only needs its address — sign in with your usual account once
          connected.
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
          {busy ? 'Checking…' : 'Connect'}
        </button>
      </form>
    </div>
  )
}
