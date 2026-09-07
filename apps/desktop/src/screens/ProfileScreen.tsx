import { useEffect, useState, type FormEvent } from 'react'
import { useMutation, useQuery, useQueryClient } from '@tanstack/react-query'
import type { ProfileUpdate } from '@skwad/shared-types'
import * as api from '../api'
import { useUi } from '../store'

const emptyProfile: ProfileUpdate = {
  displayName: '',
  avatarUrl: null,
  jobTitle: null,
  organisation: null,
  location: null,
  bio: null,
}

export function ProfileScreen() {
  const queryClient = useQueryClient()
  const pushNotice = useUi((state) => state.pushNotice)
  const resetWorkspace = useUi((state) => state.resetWorkspace)
  const [form, setForm] = useState<ProfileUpdate>(emptyProfile)

  const profile = useQuery({ queryKey: ['userProfile'], queryFn: api.getUserProfile })
  useEffect(() => {
    if (!profile.data) return
    setForm({
      displayName: profile.data.displayName,
      avatarUrl: profile.data.avatarUrl,
      jobTitle: profile.data.jobTitle,
      organisation: profile.data.organisation,
      location: profile.data.location,
      bio: profile.data.bio,
    })
  }, [profile.data])

  const save = useMutation({
    mutationFn: () => api.updateUserProfile(form),
    onSuccess: (updated) => {
      queryClient.setQueryData(['userProfile'], updated)
      pushNotice({ level: 'success', message: 'Profile saved.' })
    },
    onError: (error) => pushNotice({ level: 'error', message: String(error) }),
  })

  const signOut = useMutation({
    mutationFn: api.signOutSkwad,
    onSuccess: async () => {
      resetWorkspace()
      queryClient.removeQueries({ queryKey: ['userProfile'] })
      await queryClient.invalidateQueries({ queryKey: ['catalogueSession'] })
    },
    onError: (error) => pushNotice({ level: 'error', message: String(error) }),
  })

  const update = (field: keyof ProfileUpdate, value: string) =>
    setForm((current) => ({ ...current, [field]: value.trim() === '' && field !== 'displayName' ? null : value }))

  const submit = (event: FormEvent) => {
    event.preventDefault()
    save.mutate()
  }

  const initial = (profile.data?.displayName || profile.data?.email || 'S').trim().charAt(0).toUpperCase()

  return <>
    <div className="workspace-header">
      <div><h1>Profile</h1><p>Account details stored in your protected SKWAD cloud profile.</p></div>
      <button className="danger" onClick={() => signOut.mutate()} disabled={signOut.isPending}>Sign out</button>
    </div>

    <section className="card profile-card">
      {profile.isPending && <p className="hint">Loading profile…</p>}
      {profile.isError && <div className="empty"><p>The cloud profile could not be loaded.</p><button onClick={() => profile.refetch()}>Try again</button></div>}
      {profile.data && <form onSubmit={submit}>
        <div className="profile-heading">
          <div className="profile-avatar">{initial}</div>
          <div><strong>{profile.data.displayName}</strong><span>{profile.data.email}</span><small>Account ID: {profile.data.userId}</small></div>
        </div>
        <div className="profile-grid">
          <label className="field"><span>Display name</span><input required maxLength={80} value={form.displayName} onChange={(event) => update('displayName', event.target.value)} /></label>
          <label className="field"><span>Email</span><input value={profile.data.email} disabled /><span className="hint">Managed by the authentication provider.</span></label>
          <label className="field"><span>Job title</span><input maxLength={120} value={form.jobTitle ?? ''} onChange={(event) => update('jobTitle', event.target.value)} /></label>
          <label className="field"><span>Organisation</span><input maxLength={160} value={form.organisation ?? ''} onChange={(event) => update('organisation', event.target.value)} /></label>
          <label className="field"><span>Location</span><input maxLength={120} value={form.location ?? ''} onChange={(event) => update('location', event.target.value)} /></label>
          <label className="field"><span>Avatar URL</span><input type="url" maxLength={2048} value={form.avatarUrl ?? ''} placeholder="https://…" onChange={(event) => update('avatarUrl', event.target.value)} /></label>
          <label className="field profile-bio"><span>Bio</span><textarea rows={5} maxLength={500} value={form.bio ?? ''} onChange={(event) => update('bio', event.target.value)} /><span className="hint">{form.bio?.length ?? 0}/500</span></label>
        </div>
        <div className="actions profile-actions"><button className="primary" disabled={save.isPending || !form.displayName.trim()}>{save.isPending ? 'Saving…' : 'Save profile'}</button></div>
      </form>}
    </section>
  </>
}
