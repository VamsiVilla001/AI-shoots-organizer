/**
 * Settings: AI runtime, thresholds, video sampling, models, privacy (§24) and
 * cache management. Saving pushes the new values to the workers immediately.
 */

import { useEffect, useState } from 'react'
import { useMutation, useQuery, useQueryClient } from '@tanstack/react-query'
import type { AppSettings } from '@skwad/shared-types'
import * as api from '../api'
import { formatBytes } from '../media'
import { LibraryLocationCard } from '../components/LibraryLocationCard'
import { RosterImport } from '../components/RosterImport'
import { WorkerCard } from '../components/WorkerCard'
import { MachinesCard } from '../components/MachinesCard'
import { transport } from '../transport'
import { DatabaseSetupScreen } from './DatabaseSetupScreen'
import { useUi } from '../store'

export function SettingsScreen() {
  const queryClient = useQueryClient()
  const pushNotice = useUi((s) => s.pushNotice)
  const info = useQuery({ queryKey: ['appInfo'], queryFn: api.appInfo })
  const session = useQuery({ queryKey: ['catalogueSession'], queryFn: api.catalogueSessionStatus })
  const settingsQuery = useQuery({ queryKey: ['settings'], queryFn: api.getSettings })
  const panel = useQuery({ queryKey: ['premierePanel'], queryFn: api.premierePanelStatus })
  // Which front door this is. A desktop with its own library shows the
  // library cards; a client installation shows worker mode instead; a
  // browser shows neither.
  const desktopLibrary = transport().kind === 'tauri'
  const client = useQuery({ queryKey: ['clientStatus'], queryFn: api.clientStatus, retry: false, enabled: !desktopLibrary })
  const isClient = client.data?.serverUrl != null
  const isAdmin = session.data?.isAdmin === true
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
  const installPanel = useMutation({
    mutationFn: api.installPremierePanel,
    onSuccess: (status) => {
      queryClient.setQueryData(['premierePanel'], status)
      pushNotice({
        level: 'success',
        message: 'Premiere panel installed — restart Premiere Pro, then open Window → Extensions → SKWAD Collections.',
      })
    },
    onError: (e) => pushNotice({ level: 'error', message: String(e) }),
  })
  const cohorts = useQuery({ queryKey: ['embeddingCohorts'], queryFn: () => api.embeddingCohorts() })
  const reembed = useMutation({
    mutationFn: (shootId?: number) => api.reembedStaleFaces(shootId),
    onSuccess: (count) => {
      queryClient.invalidateQueries({ queryKey: ['embeddingCohorts'] })
      queryClient.invalidateQueries({ queryKey: ['shoots'] })
      pushNotice({ level: 'success', message: `Queued ${count} ${count === 1 ? 'file' : 'files'} for re-embedding.` })
    },
    onError: (e) => pushNotice({ level: 'error', message: String(e) }),
  })
  const clearEmbeddings = useMutation({ mutationFn: api.clearAllEmbeddings })
  const clearEverything = useMutation({
    mutationFn: api.clearAllRecognitionData,
    onSuccess: () => queryClient.invalidateQueries(),
  })
  const clearIndexes = useMutation({
    mutationFn: api.clearScannedData,
    onSuccess: (count) => {
      queryClient.invalidateQueries()
      pushNotice({ level: 'success', message: `Removed ${count} media ${count === 1 ? 'index' : 'indexes'}. Original files were not touched.` })
    },
    onError: (e) => pushNotice({ level: 'error', message: String(e) }),
  })

  if (!draft) return <div className="empty-state">Loading…</div>

  const set = <K extends keyof AppSettings>(key: K, value: AppSettings[K]) =>
    setDraft({ ...draft, [key]: value })

  /**
   * The panel installs itself on launch, so this is only ever a report — and
   * the cases worth distinguishing are the ones where it did not: no Creative
   * Cloud to install through, or a build that never packaged the panel.
   */
  const panelHint = () => {
    if (!panel.data) return <div className="hint">Checking…</div>
    const { bundledVersion, installedVersion, installerAvailable } = panel.data
    if (installedVersion) {
      return (
        <div className="hint">
          Installed (version {installedVersion}). Open it in Premiere Pro under Window →
          {' '}Extensions → SKWAD Collections. Keep this app running — the panel reads your
          {' '}Collections from it.
        </div>
      )
    }
    if (!bundledVersion) {
      return <div className="hint">This build does not include the Premiere panel.</div>
    }
    if (!installerAvailable) {
      return (
        <div className="hint">
          Not installed — the Creative Cloud desktop app is needed to install Premiere
          {' '}plugins, and it was not found on this machine.
        </div>
      )
    }
    return (
      <div className="hint">
        Not installed yet. It installs automatically on launch; use the button if it did
        {' '}not.
      </div>
    )
  }
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
        min={key === 'aiWorkers' ? 1 : undefined}
        max={key === 'aiWorkers' ? 10 : undefined}
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

      {desktopLibrary && <LibraryLocationCard isAdmin={isAdmin} />}

      {desktopLibrary && <LibraryDatabaseCard />}

      {isClient && <WorkerCard isAdmin={isAdmin} />}

      <MachinesCard isAdmin={isAdmin} />

      <div className="card roster-card"><RosterImport /></div>

      <div className="settings-grid">
        <div className="card">
          <h2>AI Runtime</h2>
          <div className="hint">
            {isClient
              ? "The server's hardware and tools — administrators only. This machine's own are under Worker mode above."
              : 'This machine only — hardware and tools. Other machines using this library keep their own.'}
          </div>
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
          {number('Parallel AI workers', 'aiWorkers', 1, `1–10 simultaneous photo/video analyses, shared fairly across shoots. Default 2. Each worker uses additional RAM and GPU memory. A separate worker prepares thumbnails. Changes apply as current files finish. ${info.data?.cpuCores ?? '?'} CPU cores available.`)}
          {number('Analysis image size', 'analysisMaxDim', 64, 'Longest edge before detection. Lower is faster; higher finds smaller faces. Library-wide: it changes the embeddings, so every machine uses the same value.')}

          <h2 style={{ marginTop: 8 }}>Models</h2>
          <div className="hint">{info.data?.models.message}</div>
          {info.data?.models.available.map((model) => (
            <div key={model.name} className="hint mono">
              {model.name} · {formatBytes(model.sizeBytes)} · {model.role} · {model.hash.slice(0, 12)}
            </div>
          ))}
          {cohorts.data && cohorts.data.staleFaces > 0 && (
            <div className="hint" style={{ marginTop: 6 }}>
              {cohorts.data.staleFaces} {cohorts.data.staleFaces === 1 ? 'face was' : 'faces were'} embedded with an
              older model across {cohorts.data.staleMedia} {cohorts.data.staleMedia === 1 ? 'file' : 'files'}. They
              cannot be compared with current embeddings until re-embedded.{' '}
              <button
                type="button"
                className="button small"
                disabled={reembed.isPending}
                onClick={() => reembed.mutate(undefined)}
              >
                {reembed.isPending ? 'Queuing…' : 'Re-embed now'}
              </button>
            </div>
          )}
          <div className="hint">
            FFmpeg: {info.data?.ffmpegAvailable ? (info.data.ffmpegVersion ?? 'found') : 'not found — HEIC and video analysis need it'}
          </div>
          <div className="hint">
            Video tracking: {info.data?.videoTrackingBackend ?? 'checking…'}
          </div>
        </div>

        <div className="card">
          <h2>Recognition</h2>
          <div className="hint">Library-wide — applies to every machine working on this library, because it changes what is written into it.</div>
          {number('Recognition threshold', 'recognitionThreshold', 0.01, 'Similarity a face needs to be suggested as a known player. The conservative default is 0.55; lower catches more but also mixes more faces.')}
          {number('Ambiguity margin', 'recognitionMargin', 0.01, 'How far ahead of the runner-up a match must be. The default is 0.10.')}
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
          <div className="hint">Library-wide.</div>
          {number('Cluster similarity', 'clusterEdgeThreshold', 0.01, 'How alike two unknown faces must be to group.')}
          {number('Minimum cluster size', 'clusterMinSize', 1, 'Smaller groups stay in the unidentified pool.')}
        </div>

        <div className="card">
          <h2>Video</h2>
          <div className="hint">Library-wide, except frame prefetch which is this machine only.</div>
          <label className="checkbox-row">
            <input type="checkbox" checked={draft.videoFramePrefetch}
              onChange={(e) => set('videoFramePrefetch', e.target.checked)} />
            Accelerate video frame decoding
          </label>
          <div className="hint">Short videos prepare one frame ahead. Videos longer than one minute decode sampled timestamps across up to four bounded segments. Sample coverage and AI settings stay the same.</div>
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
            Clear media cache
          </button>
          <button
            className="small danger"
            disabled={clearIndexes.isPending}
            onClick={() => {
              if (window.confirm('Clear all indexed media?\n\nThis removes imports, analysis, generated previews, and their links from project collections. Project folders and original media files are not touched.')) clearIndexes.mutate()
            }}
          >
            {clearIndexes.isPending ? 'Clearing indexed media…' : 'Clear all indexed media'}
          </button>

          {desktopLibrary && (
            <>
              <h2 style={{ marginTop: 8 }}>Premiere Pro panel</h2>
              {panelHint()}
              {panel.data?.bundledVersion && !panel.data.installedVersion && panel.data.installerAvailable && (
                <button
                  className="small"
                  disabled={installPanel.isPending}
                  onClick={() => installPanel.mutate()}
                >
                  {installPanel.isPending ? 'Installing…' : 'Install panel'}
                </button>
              )}
            </>
          )}
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

/**
 * Changing which database holds the library, after the app is already running.
 *
 * The same form the setup screen shows, so there is one place where a
 * connection is described and one place where it is validated. Loaded from disk
 * rather than from the running connection: this edits what is stored, which is
 * what takes effect next launch.
 */
function LibraryDatabaseCard() {
  const settings = useQuery({ queryKey: ['databaseSettings'], queryFn: api.databaseSettings, staleTime: Infinity })

  return (
    <div className="card">
      <h2>Library database</h2>
      <p className="muted" style={{ marginTop: 0 }}>
        Where the shoot index, players and groups are kept. Changing it needs a restart, and every
        machine pointed at the same database shares one library.
      </p>
      {settings.isPending && <p className="muted">Loading…</p>}
      {settings.isError && <p className="muted">Could not read the current settings.</p>}
      {settings.data && <DatabaseSetupScreen initial={settings.data} embedded />}
    </div>
  )
}
