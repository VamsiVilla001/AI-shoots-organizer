/**
 * User management for administrators.
 *
 * Every account lives in the local JSON credential file next to the app, so
 * this panel is the UI over `list_local_users` and friends in `catalogue.rs`.
 * Passwords are hashed in the backend — plaintext never leaves this form.
 */

import { useState, type FormEvent } from 'react'
import { useMutation, useQuery, useQueryClient } from '@tanstack/react-query'
import type { LocalUser, LocalUserRole } from '@skwad/shared-types'
import * as api from '../api'
import { Icon } from '../components/Icon'
import { useUi } from '../store'

/** Kept in step with `SEED_PASSWORD` in `catalogue.rs`. */
const TESTING_PASSWORD = 'Tess@123'

interface Draft {
  email: string
  displayName: string
  role: LocalUserRole
}

const emptyDraft: Draft = { email: '', displayName: '', role: 'member' }

export function AdminScreen() {
  const queryClient = useQueryClient()
  const pushNotice = useUi((state) => state.pushNotice)
  const [draft, setDraft] = useState<Draft>(emptyDraft)
  const [password, setPassword] = useState('')
  const [filter, setFilter] = useState('')
  const [editing, setEditing] = useState<string | null>(null)
  const [edit, setEdit] = useState<Draft & { enabled: boolean }>({ ...emptyDraft, enabled: true })
  const [resetting, setResetting] = useState<string | null>(null)
  const [resetPassword, setResetPassword] = useState('')

  const users = useQuery({ queryKey: ['localUsers'], queryFn: api.listLocalUsers })

  /** Every command answers with the whole roster, so one handler refreshes it. */
  const applied = (message: string) => (roster: LocalUser[]) => {
    queryClient.setQueryData(['localUsers'], roster)
    void queryClient.invalidateQueries({ queryKey: ['catalogueSession'] })
    pushNotice({ level: 'success', message })
  }
  const failed = (error: unknown) =>
    pushNotice({ level: 'error', message: error instanceof Error ? error.message : String(error) })

  const create = useMutation({
    mutationFn: () =>
      api.createLocalUser({
        email: draft.email.trim(),
        displayName: draft.displayName.trim(),
        role: draft.role,
        password: password.trim() === '' ? null : password,
      }),
    onSuccess: (roster) => {
      setDraft(emptyDraft)
      setPassword('')
      applied('Account added.')(roster)
    },
    onError: failed,
  })

  const save = useMutation({
    mutationFn: (user: LocalUser) =>
      api.updateLocalUser({
        email: user.email,
        displayName: edit.displayName.trim(),
        role: edit.role,
        enabled: edit.enabled,
      }),
    onSuccess: (roster) => {
      setEditing(null)
      applied('Account updated.')(roster)
    },
    onError: failed,
  })

  const reset = useMutation({
    mutationFn: (user: LocalUser) => api.resetLocalUserPassword(user.email, resetPassword),
    onSuccess: (roster) => {
      setResetting(null)
      setResetPassword('')
      applied('Password reset.')(roster)
    },
    onError: failed,
  })

  const remove = useMutation({
    mutationFn: (user: LocalUser) => api.deleteLocalUser(user.email),
    onSuccess: applied('Account removed.'),
    onError: failed,
  })

  const busy = create.isPending || save.isPending || reset.isPending || remove.isPending

  const submitNew = (event: FormEvent) => {
    event.preventDefault()
    if (!draft.email.trim() || !draft.displayName.trim()) return
    create.mutate()
  }

  const startEdit = (user: LocalUser) => {
    setResetting(null)
    setEditing(user.email)
    setEdit({ email: user.email, displayName: user.displayName, role: user.role, enabled: user.enabled })
  }

  const startReset = (user: LocalUser) => {
    setEditing(null)
    setResetting(user.email)
    setResetPassword(TESTING_PASSWORD)
  }

  const needle = filter.trim().toLowerCase()
  const rows = (users.data ?? []).filter(
    (user) => needle === '' || user.email.includes(needle) || user.displayName.toLowerCase().includes(needle),
  )
  const admins = (users.data ?? []).filter((user) => user.enabled && user.role === 'admin').length

  return <>
    <div className="workspace-header">
      <div>
        <h1>Users</h1>
        <p>Accounts that can sign in to this SKWAD installation.</p>
      </div>
      <div className="actions">
        <span className="input-with-icon admin-filter">
          <Icon name="search" />
          <input
            type="search"
            value={filter}
            placeholder="Filter by name or email"
            onChange={(event) => setFilter(event.target.value)}
          />
        </span>
      </div>
    </div>

    <section className="card admin-card">
      <h2 className="admin-heading">Add an account</h2>
      <form className="admin-new" onSubmit={submitNew}>
        <label className="field"><span>Email</span>
          <input type="email" required value={draft.email} placeholder="person@tesseractesports.com"
            onChange={(event) => setDraft({ ...draft, email: event.target.value })} />
        </label>
        <label className="field"><span>Display name</span>
          <input required maxLength={80} value={draft.displayName}
            onChange={(event) => setDraft({ ...draft, displayName: event.target.value })} />
        </label>
        <label className="field"><span>Role</span>
          <select value={draft.role} onChange={(event) => setDraft({ ...draft, role: event.target.value as LocalUserRole })}>
            <option value="member">Member</option>
            <option value="admin">Administrator</option>
          </select>
        </label>
        <label className="field"><span>Password</span>
          <input type="text" minLength={6} value={password} placeholder={TESTING_PASSWORD}
            onChange={(event) => setPassword(event.target.value)} />
          <span className="hint">Left blank, the account starts on {TESTING_PASSWORD}.</span>
        </label>
        <button className="primary" disabled={busy}><Icon name="add" />{create.isPending ? 'Adding…' : 'Add account'}</button>
      </form>
    </section>

    <section className="card admin-card">
      <h2 className="admin-heading">{rows.length} of {users.data?.length ?? 0} accounts</h2>
      {users.isPending && <p className="hint">Loading accounts…</p>}
      {users.isError && <div className="empty">
        <p>{users.error instanceof Error ? users.error.message : 'The account list could not be loaded.'}</p>
        <button onClick={() => users.refetch()}>Try again</button>
      </div>}

      <ul className="admin-list">
        {rows.map((user) => <li key={user.email} className={user.enabled ? 'admin-row' : 'admin-row disabled'}>
          {editing === user.email ? <form className="admin-edit" onSubmit={(event) => { event.preventDefault(); save.mutate(user) }}>
            <label className="field"><span>Display name</span>
              <input required maxLength={80} value={edit.displayName}
                onChange={(event) => setEdit({ ...edit, displayName: event.target.value })} />
            </label>
            <label className="field"><span>Role</span>
              <select value={edit.role} onChange={(event) => setEdit({ ...edit, role: event.target.value as LocalUserRole })}>
                <option value="member">Member</option>
                <option value="admin">Administrator</option>
              </select>
            </label>
            <label className="admin-toggle">
              <input type="checkbox" checked={edit.enabled} onChange={(event) => setEdit({ ...edit, enabled: event.target.checked })} />
              <span>Can sign in</span>
            </label>
            <div className="actions">
              <button className="primary" disabled={busy}><Icon name="save" />{save.isPending ? 'Saving…' : 'Save'}</button>
              <button type="button" onClick={() => setEditing(null)}>Cancel</button>
            </div>
          </form> : <>
            <div className="admin-identity">
              <strong>{user.displayName}</strong>
              <span>{user.email}</span>
            </div>
            <div className="admin-tags">
              <span className={user.role === 'admin' ? 'admin-tag strong' : 'admin-tag'}>
                {user.role === 'admin' && <Icon name="admin" />}{user.role === 'admin' ? 'Administrator' : 'Member'}
              </span>
              {!user.enabled && <span className="admin-tag muted">Disabled</span>}
              {user.mustChangePassword && <span className="admin-tag muted">Must change password</span>}
            </div>
            <div className="actions">
              <button onClick={() => startEdit(user)} disabled={busy}><Icon name="edit" />Edit</button>
              <button onClick={() => startReset(user)} disabled={busy}><Icon name="password" />Reset password</button>
              <button className="danger" disabled={busy || (user.role === 'admin' && admins <= 1)}
                onClick={() => { if (confirm(`Remove ${user.email}? They will no longer be able to sign in.`)) remove.mutate(user) }}>
                <Icon name="remove" />Remove
              </button>
            </div>
          </>}

          {resetting === user.email && <form className="admin-reset" onSubmit={(event) => { event.preventDefault(); reset.mutate(user) }}>
            <label className="field"><span>New password for {user.email}</span>
              <input type="text" required minLength={6} value={resetPassword}
                onChange={(event) => setResetPassword(event.target.value)} />
            </label>
            <div className="actions">
              <button className="primary" disabled={busy}><Icon name="password" />{reset.isPending ? 'Saving…' : 'Set password'}</button>
              <button type="button" onClick={() => setResetting(null)}>Cancel</button>
            </div>
          </form>}
        </li>)}
      </ul>

      {users.data && rows.length === 0 && <p className="hint">No account matches that filter.</p>}
      <p className="hint admin-note">
        Testing build: everyone signs in with {TESTING_PASSWORD} and is never asked to change it on first sign-in.
      </p>
    </section>
  </>
}
