/**
 * Points this machine at its library database.
 *
 * Shown instead of the library when startup could not open one, and reachable
 * from Settings afterwards. Before this existed the only way to configure a
 * machine was to hand-write `database.json` and `pgpass.conf` into AppData,
 * which is not a reasonable thing to ask and went wrong on both machines it was
 * tried on.
 *
 * Testing is separate from saving on purpose: the point is to find out the
 * address is wrong while the form is still on screen, not at the next launch.
 * Saving tests first and refuses to store a connection that does not open —
 * a saved-but-broken setting is indistinguishable, next launch, from never
 * having configured anything.
 */
import { useState } from 'react'
import * as api from '../api'
import type { DatabaseSettings } from '@skwad/shared-types'

type Props = {
  /** What startup was trying to use, so the form opens on the failing values. */
  initial: DatabaseSettings
  /** Why startup failed. Omitted when opened from Settings. */
  problem?: { title: string; detail: string }
  /** Settings shows this inside the app, where a full-page shell would be wrong. */
  embedded?: boolean
}

type Outcome = { ok: boolean; message: string }

export function DatabaseSetupScreen({ initial, problem, embedded }: Props) {
  const [settings, setSettings] = useState<DatabaseSettings>(initial)
  const [password, setPassword] = useState('')
  const [busy, setBusy] = useState<'test' | 'save' | null>(null)
  const [outcome, setOutcome] = useState<Outcome | null>(null)
  const [saved, setSaved] = useState(false)

  const edit = (patch: Partial<DatabaseSettings>) => {
    setSettings((s) => ({ ...s, ...patch }))
    // Any edit invalidates the previous result; leaving a green "Connected"
    // next to changed values would be a lie.
    setOutcome(null)
    setSaved(false)
  }

  const run = async (what: 'test' | 'save') => {
    setBusy(what)
    setOutcome(null)
    try {
      const message =
        what === 'test'
          ? await api.testDatabaseConnection(settings, password || null)
          : await api.saveDatabaseConnection(settings, password || null)
      setOutcome({ ok: true, message })
      if (what === 'save') setSaved(true)
    } catch (e) {
      setOutcome({ ok: false, message: e instanceof Error ? e.message : String(e) })
    } finally {
      setBusy(null)
    }
  }

  const form = (
    <>
      <div className="db-setup-grid">
        <label>
          <span>Server address</span>
          <input
            value={settings.host}
            onChange={(e) => edit({ host: e.target.value })}
            placeholder="192.168.1.10, or localhost"
            autoFocus
            spellCheck={false}
          />
          <small>The machine running PostgreSQL. Use localhost if the library is on this one.</small>
        </label>

        <label className="db-setup-narrow">
          <span>Port</span>
          <input
            type="number"
            value={settings.port}
            onChange={(e) => edit({ port: Number(e.target.value) })}
            min={1}
            max={65535}
          />
          <small>5432 unless changed.</small>
        </label>

        <label>
          <span>Database</span>
          <input value={settings.database} onChange={(e) => edit({ database: e.target.value })} spellCheck={false} />
        </label>

        <label>
          <span>User</span>
          <input value={settings.user} onChange={(e) => edit({ user: e.target.value })} spellCheck={false} />
        </label>

        <label>
          <span>Password</span>
          <input
            type="password"
            value={password}
            onChange={(e) => {
              setPassword(e.target.value)
              setOutcome(null)
              setSaved(false)
            }}
            placeholder={settings.hasSavedPassword ? 'Leave blank to keep the saved password' : ''}
          />
          <small>
            Stored in this computer&rsquo;s credential manager, not in the library folder.
          </small>
        </label>
      </div>

      {outcome && (
        <p className={outcome.ok ? 'db-setup-result ok' : 'db-setup-result bad'}>{outcome.message}</p>
      )}

      <div className="db-setup-actions">
        <button className="ghost" onClick={() => run('test')} disabled={busy !== null}>
          {busy === 'test' ? 'Testing…' : 'Test connection'}
        </button>
        <button className="primary" onClick={() => run('save')} disabled={busy !== null}>
          {busy === 'save' ? 'Saving…' : 'Save'}
        </button>
        {saved && (
          <button className="primary" onClick={() => api.restartForDatabaseChange()}>
            Restart SKWAD
          </button>
        )}
      </div>
    </>
  )

  if (embedded) {
    return (
      <section className="db-setup embedded">
        {form}
        <ServerOption />
      </section>
    )
  }

  return (
    <div className="auth-shell">
      <section className="db-setup">
        <h1>{problem?.title ?? 'Connect to your library'}</h1>
        {problem && <pre className="db-setup-problem">{problem.detail}</pre>}
        <p className="db-setup-intro">
          SKWAD keeps its library in a PostgreSQL database. Tell it where that is, and it will
          remember.
        </p>
        {form}
        <ServerOption />
      </section>
    </div>
  )
}

/**
 * The other way to set a machine up: as a client of a SKWAD server. It then
 * holds the server's address and nothing else — no database, no library
 * folder — and everything it shows comes from the server.
 */
function ServerOption() {
  const [url, setUrl] = useState('')
  const [busy, setBusy] = useState(false)
  const [outcome, setOutcome] = useState<Outcome | null>(null)
  const [saved, setSaved] = useState(false)

  const save = async () => {
    setBusy(true)
    setOutcome(null)
    try {
      await api.setServerUrl(url)
      setSaved(true)
      setOutcome({ ok: true, message: 'Saved. SKWAD will start as a client of that server after a restart.' })
    } catch (e) {
      setOutcome({ ok: false, message: e instanceof Error ? e.message : String(e) })
    } finally {
      setBusy(false)
    }
  }

  return (
    <div className="db-setup-server">
      <h2>Or use a SKWAD server</h2>
      <p className="db-setup-intro">
        When one machine runs the SKWAD server, this one only needs its address. Nothing else is
        configured here: sign in with your usual account once connected.
      </p>
      <div className="db-setup-actions">
        <input
          value={url}
          placeholder="https://studio-pc:8420"
          onChange={(e) => {
            setUrl(e.target.value)
            setOutcome(null)
            setSaved(false)
          }}
          spellCheck={false}
        />
        <button className="ghost" onClick={() => void save()} disabled={busy || !url.trim()}>
          {busy ? 'Saving…' : 'Use this server'}
        </button>
        {saved && (
          <button className="primary" onClick={() => api.restartForClientChange()}>
            Restart SKWAD
          </button>
        )}
      </div>
      {outcome && <p className={outcome.ok ? 'db-setup-result ok' : 'db-setup-result bad'}>{outcome.message}</p>}
    </div>
  )
}
