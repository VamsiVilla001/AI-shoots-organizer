/**
 * Where a browser session starts: which server, and the token for it.
 *
 * The desktop build never shows this — it has no server to find. Served from
 * `teo-server` itself, the address is already known and only the token is
 * missing, which is why the field is pre-filled with this page's own origin.
 */

import { useState } from 'react'
import {
  defaultBaseUrl,
  forgetConnection,
  initHttpTransport,
  NotAuthorised,
  saveConnection,
  type HttpConnection,
} from '../transport'

export function ConnectionScreen(props: {
  /** Set when a saved connection failed, so the message explains itself. */
  previous?: HttpConnection | null
  reason?: string | null
  onConnected: () => void
}) {
  const [baseUrl, setBaseUrl] = useState(props.previous?.baseUrl || defaultBaseUrl())
  const [token, setToken] = useState(props.previous?.token ?? '')
  const [remember, setRemember] = useState(true)
  const [busy, setBusy] = useState(false)
  const [error, setError] = useState<string | null>(props.reason ?? null)

  const connect = async () => {
    setBusy(true)
    setError(null)
    const connection: HttpConnection = {
      baseUrl: baseUrl.trim().replace(/\/+$/, ''),
      token: token.trim(),
    }

    try {
      await initHttpTransport(connection)
      if (remember) saveConnection(connection)
      else forgetConnection()
      props.onConnected()
    } catch (e) {
      setBusy(false)
      // Distinguish "wrong token" from "nothing answered", because the fix is
      // completely different and the browser's own message says neither.
      if (e instanceof NotAuthorised) {
        setError('That token was refused. Check the value in the server\'s config/token file.')
      } else {
        setError(
          `Could not reach ${connection.baseUrl || 'the server'}. Check the address, that the ` +
            'server is running, and that you are on the same network or VPN.',
        )
      }
    }
  }

  return (
    <div className="connect-shell">
      <div className="card connect-card">
        <div className="brand" style={{ padding: 0, marginBottom: 6 }}>
          Esports <em>AI</em> Media Organiser
        </div>
        <div className="hint">
          Connect to a server edition running on your NAS or edit-bay machine. Everything stays on
          that machine — this page only talks to it.
        </div>

        <label className="field">
          <span>Server address</span>
          <input
            value={baseUrl}
            onChange={(e) => setBaseUrl(e.target.value)}
            placeholder="http://nas.local:8420"
            autoFocus={!token}
            spellCheck={false}
          />
        </label>

        <label className="field">
          <span>Access token</span>
          <input
            value={token}
            onChange={(e) => setToken(e.target.value)}
            onKeyDown={(e) => {
              if (e.key === 'Enter' && token.trim()) connect()
            }}
            placeholder="from the server's config/token file"
            spellCheck={false}
            type="password"
          />
        </label>

        <label className="checkbox-row">
          <input type="checkbox" checked={remember} onChange={(e) => setRemember(e.target.checked)} />
          Remember this connection on this device
        </label>

        {error && <div style={{ color: 'var(--error)', fontSize: 13 }}>{error}</div>}

        <div className="buttons">
          <button className="primary" disabled={busy || !token.trim()} onClick={connect}>
            {busy ? 'Connecting…' : 'Connect'}
          </button>
        </div>
      </div>
    </div>
  )
}
