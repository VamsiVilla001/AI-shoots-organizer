import { useState, type FormEvent } from 'react'
import { useMutation, useQueryClient } from '@tanstack/react-query'
import * as api from '../api'
import { useUi } from '../store'

type Mode = 'signin' | 'signup'

export function AuthScreen() {
  const queryClient = useQueryClient()
  const pushNotice = useUi((state) => state.pushNotice)
  const [mode, setMode] = useState<Mode>('signin')
  const [displayName, setDisplayName] = useState('')
  const [email, setEmail] = useState('')
  const [password, setPassword] = useState('')
  const [confirmation, setConfirmation] = useState('')

  const authenticate = useMutation({
    mutationFn: async () => {
      if (mode === 'signup') {
        if (password !== confirmation) throw new Error('Passwords do not match.')
        return api.signUpSkwad(email.trim(), password, displayName.trim())
      }
      await api.signInSkwad(email.trim(), password)
      return { signedIn: true, confirmationRequired: false }
    },
    onSuccess: async (result) => {
      setPassword('')
      setConfirmation('')
      if (result.signedIn) {
        await queryClient.invalidateQueries({ queryKey: ['catalogueSession'] })
        pushNotice({ level: 'success', message: mode === 'signup' ? 'Your SKWAD account is ready.' : 'Signed in securely.' })
      } else if (result.confirmationRequired) {
        setMode('signin')
        pushNotice({ level: 'info', message: 'Check your email to confirm the account, then sign in.' })
      }
    },
    onError: (error) => pushNotice({ level: 'error', message: error instanceof Error ? error.message : String(error) }),
  })

  const submit = (event: FormEvent) => {
    event.preventDefault()
    if (!email.trim() || !password || (mode === 'signup' && !displayName.trim())) return
    authenticate.mutate()
  }

  return <div className="auth-shell">
    <section className="auth-panel">
      <div className="auth-brand"><span>SKWAD</span> Media Organiser</div>
      <h1>{mode === 'signin' ? 'Welcome back' : 'Create your profile'}</h1>
      <p className="hint">Your account controls encrypted catalogue access across authorised devices.</p>

      <div className="auth-tabs" role="tablist">
        <button role="tab" aria-selected={mode === 'signin'} className={mode === 'signin' ? 'active' : ''} type="button" onClick={() => setMode('signin')}>Sign in</button>
        <button role="tab" aria-selected={mode === 'signup'} className={mode === 'signup' ? 'active' : ''} type="button" onClick={() => setMode('signup')}>Create account</button>
      </div>

      <form className="auth-form" onSubmit={submit}>
        {mode === 'signup' && <label className="field"><span>Display name</span><input value={displayName} maxLength={80} autoComplete="name" onChange={(event) => setDisplayName(event.target.value)} /></label>}
        <label className="field"><span>Email</span><input type="email" value={email} autoComplete="email" onChange={(event) => setEmail(event.target.value)} /></label>
        <label className="field"><span>Password</span><input type="password" value={password} minLength={mode === 'signup' ? 8 : undefined} autoComplete={mode === 'signin' ? 'current-password' : 'new-password'} onChange={(event) => setPassword(event.target.value)} /></label>
        {mode === 'signup' && <label className="field"><span>Confirm password</span><input type="password" value={confirmation} minLength={8} autoComplete="new-password" onChange={(event) => setConfirmation(event.target.value)} /></label>}
        <button className="primary auth-submit" disabled={authenticate.isPending}>
          {authenticate.isPending ? 'Please wait…' : mode === 'signin' ? 'Sign in' : 'Create account'}
        </button>
      </form>

      <p className="auth-security">Passwords are hashed by Supabase Auth. Device keys and cached sessions stay in Windows Credential Manager or macOS Keychain—not in the media database.</p>
    </section>
  </div>
}
