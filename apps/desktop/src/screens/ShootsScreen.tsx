/**
 * The Shoots screen (§21): recent shoots with their counts, plus New Shoot,
 * Resume, Export and Delete Index actions.
 */

import { useState } from 'react'
import { useMutation, useQuery, useQueryClient } from '@tanstack/react-query'
import { open } from '@tauri-apps/plugin-dialog'
import type { ShootSummary } from '@teo/shared-types'
import * as api from '../api'
import { formatCount, formatDate } from '../media'
import { Modal } from '../components/Modal'
import { useUi } from '../store'

export function ShootsScreen() {
  const [creating, setCreating] = useState(false)
  const shoots = useQuery({ queryKey: ['shoots'], queryFn: api.listShoots })
  const queryClient = useQueryClient()
  const pushNotice = useUi((state) => state.pushNotice)
  const resetWorkspace = useUi((state) => state.resetWorkspace)
  const clearScanned = useMutation({
    mutationFn: api.clearScannedData,
    onSuccess: (removed) => {
      queryClient.clear()
      resetWorkspace()
      queryClient.invalidateQueries({ queryKey: ['shoots'] })
      pushNotice({
        level: 'success',
        message: `Cleared ${removed} scanned shoot${removed === 1 ? '' : 's'} and thumbnail cache.`,
      })
    },
    onError: (error) => pushNotice({ level: 'error', message: String(error) }),
  })

  return (
    <>
      <div className="workspace-header">
        <h1>Recent Shoots</h1>
        <div className="actions">
          <button
            className="danger"
            disabled={clearScanned.isPending}
            onClick={() => {
              if (
                window.confirm(
                  'Clear all scanned data?\n\nThis removes old shoot indexes, per-shoot analysis and generated thumbnails. Your original photos/videos, settings, player profiles and AI models are not touched.',
                )
              ) {
                clearScanned.mutate()
              }
            }}
          >
            {clearScanned.isPending ? 'Clearing…' : 'Clear scanned data'}
          </button>
          <button className="primary" onClick={() => setCreating(true)}>
            + New Shoot
          </button>
        </div>
      </div>

      {shoots.data?.length === 0 && (
        <div className="empty-state">
          <div className="big">No shoots yet</div>
          <div>
            Create a shoot and point it at a folder of photos and videos. The AI will find and
            group every player automatically.
          </div>
        </div>
      )}

      <div className="card-grid">
        {shoots.data?.map((shoot) => <ShootCard key={shoot.id} shoot={shoot} />)}
      </div>

      {creating && <NewShootModal onClose={() => setCreating(false)} />}
    </>
  )
}

function ShootCard({ shoot }: { shoot: ShootSummary }) {
  const openShoot = useUi((s) => s.openShoot)
  const progress = useUi((s) => s.progress[shoot.id])
  const pushNotice = useUi((s) => s.pushNotice)
  const queryClient = useQueryClient()

  const remove = useMutation({
    mutationFn: () => api.deleteShootIndex(shoot.id),
    onSuccess: () => queryClient.invalidateQueries({ queryKey: ['shoots'] }),
  })
  const resume = useMutation({
    mutationFn: () => api.resumeProcessing(shoot.id),
    onSuccess: (queued) =>
      pushNotice({
        level: 'success',
        message: queued > 0 ? `Queued ${queued} file(s).` : 'Nothing left to process.',
      }),
    onError: (e) => pushNotice({ level: 'error', message: String(e) }),
  })

  const working = progress && progress.jobsQueued + progress.jobsRunning > 0
  const statusLabel = working ? `Processing ${progress.percent.toFixed(0)}%` : shoot.status
  const badgeClass =
    shoot.status === 'completed' ? 'completed' : shoot.status === 'failed' ? 'failed' : working ? 'processing' : ''

  return (
    <div className="card shoot-card" onClick={() => openShoot(shoot.id)}>
      <div className="title">
        <span>{shoot.name}</span>
        <span className={`badge ${badgeClass}`}>{statusLabel}</span>
      </div>
      <div className="stats">
        <span>
          {formatCount(shoot.photoCount)} photos · {formatCount(shoot.videoCount)} videos
        </span>
        <span>
          {formatCount(shoot.personCount)} players
          {shoot.unknownClusterCount > 0 && ` · ${shoot.unknownClusterCount} unknown`}
          {shoot.failedJobs > 0 && (
            <span style={{ color: 'var(--error)' }}> · {shoot.failedJobs} failed</span>
          )}
        </span>
        <span className="hint">{formatDate(shoot.createdAt)}</span>
      </div>
      <div
        style={{ display: 'flex', gap: 6, marginTop: 10 }}
        onClick={(e) => e.stopPropagation()}
      >
        <button className="small" onClick={() => openShoot(shoot.id)}>
          Open
        </button>
        {!working && shoot.status !== 'completed' && (
          <button className="small" onClick={() => resume.mutate()}>
            Resume
          </button>
        )}
        <button className="small" onClick={() => openShoot(shoot.id, 'export')}>
          Copy &amp; Organise
        </button>
        <button
          className="small danger"
          onClick={() => {
            // Deleting an index never deletes the user's files (§21); still
            // worth a beat of confirmation because re-analysis costs time.
            if (window.confirm(`Remove the index for "${shoot.name}"?\nYour original files are not touched.`)) {
              remove.mutate()
            }
          }}
        >
          Delete Index
        </button>
      </div>
    </div>
  )
}

function NewShootModal({ onClose }: { onClose: () => void }) {
  const [name, setName] = useState('')
  const [folder, setFolder] = useState('')
  const [busy, setBusy] = useState(false)
  const [error, setError] = useState<string | null>(null)
  const openShoot = useUi((s) => s.openShoot)
  const queryClient = useQueryClient()

  const pickFolder = async () => {
    const picked = await open({ directory: true, multiple: false, title: 'Choose the shoot folder' })
    if (typeof picked === 'string') {
      setFolder(picked)
      if (!name.trim()) {
        // Suggest the folder name; "BGMS_Final_Shoot" → "BGMS Final Shoot".
        const stem = picked.replaceAll('\\', '/').split('/').filter(Boolean).pop() ?? ''
        setName(stem.replaceAll(/[_-]+/g, ' ').trim())
      }
    }
  }

  const create = async () => {
    setBusy(true)
    setError(null)
    try {
      const shoot = await api.createShoot(name, folder)
      await queryClient.invalidateQueries({ queryKey: ['shoots'] })
      openShoot(shoot.id, 'albums')
      onClose()
    } catch (e) {
      setError(String(e instanceof Error ? e.message : e))
      setBusy(false)
    }
  }

  return (
    <Modal title="New Shoot" onClose={onClose}>
      <label className="field">
        <span>Source folder</span>
        <div style={{ display: 'flex', gap: 8 }}>
          <input
            style={{ flex: 1 }}
            value={folder}
            onChange={(e) => setFolder(e.target.value)}
            placeholder="D:\BGMS_Final_Shoot"
          />
          <button onClick={pickFolder}>Browse…</button>
        </div>
      </label>
      <label className="field">
        <span>Shoot name</span>
        <input
          value={name}
          onChange={(e) => setName(e.target.value)}
          placeholder="BGMS Finals Player Shoot"
        />
      </label>
      <div className="hint">
        Files are indexed in place — nothing is moved, renamed or modified. Scanning and face
        detection start immediately in the background.
      </div>
      {error && <div style={{ color: 'var(--error)', fontSize: 13 }}>{error}</div>}
      <div className="buttons">
        <button onClick={onClose}>Cancel</button>
        <button className="primary" disabled={busy || !name.trim() || !folder.trim()} onClick={create}>
          {busy ? 'Creating…' : 'Create & Scan'}
        </button>
      </div>
    </Modal>
  )
}
