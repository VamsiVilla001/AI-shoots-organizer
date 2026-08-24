/**
 * Shown when the desktop shell could not start its private server.
 *
 * The app is a client of that server, so there is nothing to browse without it.
 * A blank window would leave someone guessing; this says what failed, where to
 * look, and offers to try again — which is often all it takes when the cause was
 * a stale process holding the database.
 */

export function ServerErrorScreen(props: { reason: string; onRetry: () => void }) {
  return (
    <div className="connect-shell">
      <div className="card connect-card">
        <div className="brand" style={{ padding: 0, marginBottom: 6 }}>
          Esports <em>AI</em> Media Organiser
        </div>

        <h2 style={{ margin: 0 }}>The local server did not start</h2>

        <div style={{ color: 'var(--error)', fontSize: 13 }}>{props.reason}</div>

        <div className="hint">
          Everything in this app runs through a small server started on this
          machine, reachable only from it. Your library and original files are
          untouched by this failure.
        </div>

        <div className="hint">
          Worth checking, in order:
          <ul style={{ margin: '6px 0 0', paddingLeft: 18 }}>
            <li>another copy of the app still running and holding the database</li>
            <li>
              <span className="mono">logs/teo.log</span> in the application data
              folder, which records the server's own output
            </li>
            <li>security software blocking a local network connection</li>
          </ul>
        </div>

        <div className="buttons">
          <button className="primary" onClick={props.onRetry}>
            Try again
          </button>
        </div>
      </div>
    </div>
  )
}
