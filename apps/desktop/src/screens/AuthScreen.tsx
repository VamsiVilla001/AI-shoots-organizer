import { useState, type FormEvent } from 'react'
import { useMutation, useQueryClient } from '@tanstack/react-query'
import * as api from '../api'
import { useUi } from '../store'

type Mode = 'signin' | 'changePassword'

export function AuthScreen() {
  const queryClient = useQueryClient()
  const pushNotice = useUi((state) => state.pushNotice)
  const [mode, setMode] = useState<Mode>('signin')
  const [email, setEmail] = useState('')
  const [password, setPassword] = useState('')
  const [newPassword, setNewPassword] = useState('')
  const [confirmation, setConfirmation] = useState('')

  const authenticate = useMutation({
    mutationFn: async () => {
      if (mode === 'changePassword') {
        if (newPassword !== confirmation) throw new Error('Passwords do not match.')
        return api.changeInitialPassword(email.trim(), password, newPassword)
      }
      return api.signInSkwad(email.trim(), password)
    },
    onSuccess: async (result) => {
      setPassword('')
      if (result.passwordChangeRequired) {
        setMode('changePassword')
        pushNotice({ level: 'info', message: 'Change the temporary password before continuing.' })
        return
      }
      setNewPassword('')
      setConfirmation('')
      await queryClient.invalidateQueries({ queryKey: ['catalogueSession'] })
      pushNotice({ level: 'success', message: 'Signed in securely.' })
    },
    onError: (error) => pushNotice({ level: 'error', message: error instanceof Error ? error.message : String(error) }),
  })

  const submit = (event: FormEvent) => {
    event.preventDefault()
    if (!email.trim() || !password) return
    if (mode === 'changePassword' && (!newPassword || !confirmation)) return
    authenticate.mutate()
  }

  return <div className="auth-shell">
    <section className="auth-panel">
      <div className="auth-brand"><span>SKWAD</span> Media Organiser</div>
      <h1>{mode === 'signin' ? 'Welcome back' : 'Set your private password'}</h1>
      <p className="hint">
        {mode === 'signin'
          ? 'Sign in with an account from the local SKWAD credential file.'
          : 'Enter the temporary password once, then choose a password used only by you.'}
      </p>

      <form className="auth-form" onSubmit={submit}>
        <label className="field"><span>Email</span><input type="email" value={email} autoComplete="email" disabled={mode === 'changePassword'} onChange={(event) => setEmail(event.target.value)} /></label>
        <label className="field"><span>{mode === 'signin' ? 'Password' : 'Temporary password'}</span><input type="password" value={password} autoComplete="current-password" onChange={(event) => setPassword(event.target.value)} /></label>
        {mode === 'changePassword' && <>
          <label className="field"><span>New password</span><input type="password" value={newPassword} minLength={6} autoComplete="new-password" onChange={(event) => setNewPassword(event.target.value)} /></label>
          <label className="field"><span>Confirm new password</span><input type="password" value={confirmation} minLength={6} autoComplete="new-password" onChange={(event) => setConfirmation(event.target.value)} /></label>
        </>}
        <button className="primary auth-submit" disabled={authenticate.isPending}>
          {authenticate.isPending ? 'Please wait…' : mode === 'signin' ? 'Sign in' : 'Change password and sign in'}
        </button>
        {mode === 'changePassword' && <button type="button" onClick={() => { setMode('signin'); setPassword(''); setNewPassword(''); setConfirmation('') }}>Back to sign in</button>}
      </form>

      <p className="auth-security">Passwords are Argon2id-hashed in the local JSON credential file. Device keys and the signed-in session stay in Windows Credential Manager or macOS Keychain.</p>
    </section>
  </div>
}
