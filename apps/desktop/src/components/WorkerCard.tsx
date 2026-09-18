/**
 * Worker mode, on a client installation.
 *
 * A client can lend its GPU to the server: an administrator enrols the
 * machine (the server hands out a token, kept by the desktop), the person
 * switches the worker on, and from then on this machine claims analysis jobs
 * over HTTP and runs them with the same engine the server would. The
 * settings here are this machine's own — worker count, accelerator, FFmpeg —
 * which the server's settings screen must not touch.
 */

import { useEffect, useState } from 'react'
import { useMutation, useQuery, useQueryClient } from '@tanstack/react-query'
import type { Accelerator, MachineSettings, WorkerStatus } from '@skwad/shared-types'
import * as api from '../api'
import { useUi } from '../store'

const ACCELERATORS: Array<{ value: Accelerator; label: string }> = [
  { value: 'auto', label: 'Automatic (GPU when available)' },
  { value: 'cpu', label: 'CPU' },
  { value: 'directMl', label: 'DirectML (Windows GPU)' },
  { value: 'coreMl', label: 'CoreML (Apple Silicon)' },
  { value: 'cuda', label: 'CUDA' },
]

export function WorkerCard({ isAdmin }: { isAdmin: boolean }) {
  const queryClient = useQueryClient()
  const pushNotice = useUi((s) => s.pushNotice)
  const status = useQuery({
    queryKey: ['clientStatus'],
    queryFn: api.clientStatus,
    // Live enough to watch jobs tick over while the worker runs.
    refetchInterval: (query) => (query.state.data?.enabled ? 3000 : false),
  })
  const [name, setName] = useState('')
  const [draft, setDraft] = useState<MachineSettings | null>(null)

  useEffect(() => {
    if (status.data && !draft) setDraft(status.data.machineSettings)
  }, [status.data, draft])

  const settle = (next: WorkerStatus) => {
    queryClient.setQueryData(['clientStatus'], next)
    queryClient.invalidateQueries({ queryKey: ['machines'] })
  }
  const fail = (e: unknown) => pushNotice({ level: 'error', message: String((e as Error).message ?? e) })

  const enrol = useMutation({
    mutationFn: async (machineName: string) => {
      const current = status.data
      if (!current) throw new Error('the worker status is not loaded yet')
      const enrolled = await api.enrolMachine(machineName, current.machineId)
      return api.storeMachineEnrolment(enrolled.token, enrolled.machine.name)
    },
    onSuccess: (next) => {
      settle(next)
      pushNotice({ level: 'success', message: `Enrolled as “${next.machineName}”. Switch the worker on to start contributing.` })
    },
    onError: fail,
  })
  const forget = useMutation({ mutationFn: api.forgetMachineEnrolment, onSuccess: settle, onError: fail })
  const toggle = useMutation({ mutationFn: api.setWorkerEnabled, onSuccess: settle, onError: fail })
  const saveSettings = useMutation({
    mutationFn: api.updateWorkerSettings,
    onSuccess: (next) => {
      settle(next)
      setDraft(next.machineSettings)
      pushNotice({ level: 'success', message: 'Worker settings saved — they apply from the next file.' })
    },
    onError: fail,
  })

  if (!status.data) return null
  const s = status.data

  const set = <K extends keyof MachineSettings>(key: K, value: MachineSettings[K]) =>
    setDraft((d) => (d ? { ...d, [key]: value } : d))

  return (
    <div className="card">
      <h2>Worker mode</h2>
      <p className="muted" style={{ marginTop: 0 }}>
        Lend this machine’s GPU to the server. Analysis jobs are fetched over the network, run here with the
        same models, and the results go back to the library.
      </p>

      {!s.enrolled ? (
        <>
          <div className="hint">
            This machine is not enrolled yet.{' '}
            {isAdmin ? 'Give it a name people will recognise on the roster.' : 'An administrator enrols it from here.'}
          </div>
          {isAdmin && (
            <form
              className="db-setup-actions"
              onSubmit={(e) => {
                e.preventDefault()
                enrol.mutate(name.trim() || 'This machine')
              }}
            >
              <input
                value={name}
                placeholder="Editing laptop"
                onChange={(e) => setName(e.target.value)}
                spellCheck={false}
              />
              <button className="primary" type="submit" disabled={enrol.isPending}>
                {enrol.isPending ? 'Enrolling…' : 'Enrol this machine'}
              </button>
            </form>
          )}
        </>
      ) : (
        <>
          <div className="hint">
            Enrolled as <strong>{s.machineName ?? s.machineId}</strong> <span className="mono">({s.machineId.slice(0, 8)})</span>
          </div>
          <div className="db-setup-actions">
            <button
              className={s.enabled ? 'small' : 'primary'}
              disabled={toggle.isPending}
              onClick={() => toggle.mutate(!s.enabled)}
            >
              {s.enabled ? 'Stop contributing' : 'Start contributing'}
            </button>
            <button
              className="small ghost"
              disabled={forget.isPending}
              onClick={() => {
                if (window.confirm('Forget this enrolment? The machine stops working for the server until an administrator enrols it again.'))
                  forget.mutate()
              }}
            >
              Forget enrolment
            </button>
          </div>
          <WorkerState status={s} />
        </>
      )}

      {draft && (
        <>
          <h2 style={{ marginTop: 8 }}>This machine</h2>
          <div className="hint">Hardware and tools on this machine only. The server keeps its own.</div>
          <label className="field">
            <span>Acceleration</span>
            <select value={draft.accelerator} onChange={(e) => set('accelerator', e.target.value as Accelerator)}>
              {ACCELERATORS.map((option) => (
                <option key={option.value} value={option.value}>
                  {option.label}
                </option>
              ))}
            </select>
          </label>
          <label className="field">
            <span>Parallel AI workers</span>
            <input
              type="number"
              min={1}
              max={10}
              value={draft.aiWorkers}
              onChange={(e) => set('aiWorkers', Number(e.target.value))}
            />
            <span className="hint">Simultaneous files analysed on this machine. Each one uses RAM and GPU memory.</span>
          </label>
          <label className="field">
            <span>Threads per inference session</span>
            <input
              type="number"
              min={1}
              max={64}
              value={draft.inferenceThreads}
              onChange={(e) => set('inferenceThreads', Number(e.target.value))}
            />
          </label>
          <label className="field">
            <span>FFmpeg folder</span>
            <input
              value={draft.ffmpegDirectory ?? ''}
              placeholder="Leave blank to use PATH"
              onChange={(e) => set('ffmpegDirectory', e.target.value.trim() || null)}
              spellCheck={false}
            />
            <span className="hint">Needed for video analysis. Without it this machine only analyses photos.</span>
          </label>
          <label className="field checkbox">
            <input
              type="checkbox"
              checked={draft.videoFramePrefetch}
              onChange={(e) => set('videoFramePrefetch', e.target.checked)}
            />
            <span>Prefetch video frames</span>
          </label>
          <div className="db-setup-actions">
            <button className="primary" disabled={saveSettings.isPending} onClick={() => saveSettings.mutate(draft)}>
              {saveSettings.isPending ? 'Saving…' : 'Save worker settings'}
            </button>
          </div>
        </>
      )}
    </div>
  )
}

function WorkerState({ status }: { status: WorkerStatus }) {
  const remote = status.remote
  if (status.starting) return <div className="hint">Starting — connecting and checking the models…</div>
  if (status.lastError) return <div className="auth-error">{status.lastError}</div>
  if (!status.enabled) return <div className="hint">Off. Switch it on to start claiming analysis jobs.</div>
  if (!remote) return <div className="hint">Starting…</div>
  return (
    <div className="hint">
      {remote.connected ? 'Connected' : 'Not connected'}
      {remote.lastError ? ` — ${remote.lastError}` : ''}
      {' · '}
      {remote.modelsReady ? 'models match the server' : 'models not ready'}
      {' · '}
      {remote.held} running · {remote.jobsCompleted} done · {remote.jobsFailed} failed
    </div>
  )
}
