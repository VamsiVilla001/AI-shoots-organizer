/**
 * Settings: AI runtime, thresholds, video sampling, models, privacy (§24) and
 * cache management. Saving pushes the new values to the workers immediately.
 */

import { useEffect, useState } from 'react'
import { useMutation, useQuery, useQueryClient } from '@tanstack/react-query'
import { open } from '@tauri-apps/plugin-dialog'
import type { AppSettings } from '@teo/shared-types'
import * as api from '../api'
import { formatBytes } from '../media'
import { useUi } from '../store'

export function SettingsScreen() {
  const queryClient = useQueryClient()
  const pushNotice = useUi((s) => s.pushNotice)
  const info = useQuery({ queryKey: ['appInfo'], queryFn: api.appInfo })
  const settingsQuery = useQuery({ queryKey: ['settings'], queryFn: api.getSettings })
  const [draft, setDraft] = useState<AppSettings | null>(null)

  useEffect(() => {
    if (settingsQuery.data && !draft) setDraft(settingsQuery.data)
  }, [settingsQuery.data, draft])

  const save = useMutation({
    mutationFn: (next: AppSettings) => api.updateSettings(next),
    onSuccess: (saved) => {
      setDraft(saved)
      queryClient.invalidateQueries({ queryKey: ['settings'] })
      queryClient.invalidateQueries({ queryKey: ['appInfo'] })
      pushNotice({ level: 'success', message: 'Settings saved — workers reload automatically.' })
    },
    onError: (e) => pushNotice({ level: 'error', message: String(e) }),
  })

  const clearThumbs = useMutation({
    mutationFn: api.clearThumbnailCache,
    onSuccess: (n) => {
      pushNotice({ level: 'success', message: `Removed ${n} cached thumbnails.` })
      queryClient.invalidateQueries({ queryKey: ['appInfo'] })
    },
  })
  const clearEmbeddings = useMutation({ mutationFn: api.clearAllEmbeddings })
  const clearEverything = useMutation({
    mutationFn: api.clearAllRecognitionData,
    onSuccess: () => queryClient.invalidateQueries(),
  })

  if (!draft) return <div className="empty-state">Loading…</div>

  const set = <K extends keyof AppSettings>(key: K, value: AppSettings[K]) =>
    setDraft({ ...draft, [key]: value })

  const number = (
    label: string,
    key: keyof AppSettings,
    step: number,
    hint?: string,
  ) => (
    <label className="field">
      <span>{label}</span>
      <input
        type="number"
        step={step}
        value={draft[key] as number}
        onChange={(e) => set(key, Number(e.target.value) as never)}
      />
      {hint && <span className="hint">{hint}</span>}
    </label>
  )

  return (
    <>
      <div className="workspace-header">
        <h1>Settings</h1>
        <div className="actions">
          <button className="primary" onClick={() => save.mutate(draft)} disabled={save.isPending}>
            Save changes
          </button>
        </div>
      </div>

      <div className="settings-grid">
        <div className="card">
          <h2>AI Runtime</h2>
          <label className="field">
            <span>Acceleration</span>
            <select
              value={draft.accelerator}
              onChange={(e) => set('accelerator', e.target.value as AppSettings['accelerator'])}
            >
              {info.data?.accelerators.map((option) => (
                <option key={option} value={option}>
                  {option === 'auto'
                    ? 'Automatic (GPU when available)'
                    : option === 'directMl'
                      ? 'DirectML (Windows GPU)'
                      : option === 'coreMl'
                        ? 'CoreML (Apple Silicon)'
                        : option.toUpperCase()}
                </option>
              ))}
            </select>
            <span className="hint">
              Falls back to CPU automatically when the GPU provider cannot start.
            </span>
          </label>
          {number('Worker threads', 'workerThreads', 1, `Background workers (maximum 2). Face AI uses one GPU worker; the second assists scanning and thumbnails. ${info.data?.cpuCores ?? '?'} cores available.`)}
          {number('Analysis image size', 'analysisMaxDim', 64, 'Longest edge before detection. Lower is faster; higher finds smaller faces.')}

          <h2 style={{ marginTop: 8 }}>Models</h2>
          <div className="hint">{info.data?.models.message}</div>
          {info.data?.models.available.map((model) => (
            <div key={model.name} className="hint mono">
              {model.name} · {formatBytes(model.sizeBytes)} · {model.role}
            </div>
          ))}
          <div className="hint">
            FFmpeg: {info.data?.ffmpegAvailable ? (info.data.ffmpegVersion ?? 'found') : 'not found — HEIC and video need it'}
          </div>
          <label className="field">
            <span>FFmpeg directory</span>
            <div style={{ display: 'flex', gap: 8 }}>
              <input
                style={{ flex: 1 }}
                value={draft.ffmpegDirectory ?? ''}
                onChange={(event) => set('ffmpegDirectory', event.target.value.trim() || null)}
                placeholder="Automatic (Homebrew, MacPorts, or PATH)"
              />
              <button
                type="button"
                onClick={async () => {
                  const selected = await open({
                    directory: true,
                    multiple: false,
                    title: 'Choose the folder containing ffmpeg and ffprobe',
                  })
                  if (typeof selected === 'string') set('ffmpegDirectory', selected)
                }}
              >
                Browse…
              </button>
            </div>
            <span className="hint">Only needed when FFmpeg is installed in a non-standard location.</span>
          </label>
        </div>

        <div className="card">
          <h2>Recognition</h2>
          {number('Recognition threshold', 'recognitionThreshold', 0.01, 'Similarity a face needs to be suggested as a known player. Lower catches more, errs more.')}
          {number('Ambiguity margin', 'recognitionMargin', 0.01, 'How far ahead of the runner-up a match must be.')}
          {number('Auto-confirm above', 'autoConfirmAbove', 0.01, '1.0 disables auto-confirmation — everything waits for review.')}
          <label className="checkbox-row">
            <input
              type="checkbox"
              checked={draft.uniquePersonPerFrame}
              onChange={(e) => set('uniquePersonPerFrame', e.target.checked)}
            />
            One player can only appear once per photo
          </label>

          <h2 style={{ marginTop: 8 }}>Clustering</h2>
          {number('Cluster similarity', 'clusterEdgeThreshold', 0.01, 'How alike two unknown faces must be to group.')}
          {number('Minimum cluster size', 'clusterMinSize', 1, 'Smaller groups stay in the unidentified pool.')}
        </div>

        <div className="card">
          <h2>Video</h2>
          <label className="checkbox-row">
            <input
              type="checkbox"
              checked={draft.videoEnabled}
              onChange={(e) => set('videoEnabled', e.target.checked)}
            />
            Analyse videos
          </label>
          {number('Sample interval (s)', 'videoSampleInterval', 0.5, 'Fallback cadence between detected scene changes.')}
          {number('Max frames per video', 'videoMaxFrames', 5)}
          {number('Scene threshold', 'videoSceneThreshold', 0.05, '0–1; lower detects more cuts.')}

          <h2 style={{ marginTop: 8 }}>Storage</h2>
          <div className="hint mono">{info.data?.paths.root}</div>
          <div className="hint">Cache size: {formatBytes(info.data?.cacheBytes ?? 0)}</div>
          <button className="small" onClick={() => clearThumbs.mutate()}>
            Clear thumbnail cache
          </button>

          <h2 style={{ marginTop: 8 }}>Privacy</h2>
          <div className="hint">
            All recognition runs locally. Nothing is uploaded, ever.
          </div>
          <button
            className="small danger"
            onClick={() => {
              if (window.confirm('Delete every stored face embedding?\nDetections and albums are kept; matching new shoots will need re-analysis.'))
                clearEmbeddings.mutate()
            }}
          >
            Delete all embeddings
          </button>
          <button
            className="small danger"
            onClick={() => {
              if (window.confirm('Delete ALL recognition data — every face, cluster, album and player profile?\nYour media files are not touched.'))
                clearEverything.mutate()
            }}
          >
            Clear all recognition data
          </button>
        </div>
      </div>
    </>
  )
}
