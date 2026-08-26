/**
 * The Copy & Organise screen (§11): choose a destination and options, preview the file
 * count, run the copy with live progress. Originals are only ever read.
 */

import { useEffect, useState } from 'react'
import { useQuery } from '@tanstack/react-query'
import { open } from '@tauri-apps/plugin-dialog'
import type { ExportOptions, ExportPreview } from '@teo/shared-types'
import * as api from '../api'
import { formatBytes, formatCount } from '../media'
import { useUi } from '../store'

const DEFAULT_OPTIONS: ExportOptions = {
  splitPhotosVideos: true,
  includeUnidentified: true,
  personIds: null,
  preserveMetadata: true,
  existing: 'skip',
  includeMultiPlayer: false,
  includeGroupSize: false,
}

export function ExportScreen() {
  const shootId = useUi((s) => s.activeShootId)
  if (shootId === null) return <div className="empty-state">Open a shoot first.</div>
  return <ExportBody shootId={shootId} />
}

function ExportBody({ shootId }: { shootId: number }) {
  const exportPersonIds = useUi((s) => s.exportPersonIds)
  const [destination, setDestination] = useState('')
  const [options, setOptions] = useState<ExportOptions>(() => ({
    ...DEFAULT_OPTIONS,
    personIds: exportPersonIds === null ? null : [...exportPersonIds],
    // A selection made on Albums should copy exactly those named groups.
    includeUnidentified: exportPersonIds === null,
  }))
  const [preview, setPreview] = useState<ExportPreview | null>(null)
  const [error, setError] = useState<string | null>(null)
  const [running, setRunning] = useState(false)
  const exportProgress = useUi((s) => s.exportProgress)
  const pushNotice = useUi((s) => s.pushNotice)

  const shoot = useQuery({ queryKey: ['shoots', shootId], queryFn: () => api.getShoot(shootId) })
  const people = useQuery({ queryKey: ['people', shootId], queryFn: () => api.listPeople(shootId) })
  const history = useQuery({ queryKey: ['exports', shootId], queryFn: () => api.listExports(shootId) })

  // Re-preview whenever the inputs change.
  useEffect(() => {
    setPreview(null)
    setError(null)
    if (!destination.trim()) return
    const timer = setTimeout(() => {
      api
        .previewExport(shootId, destination, options)
        .then(setPreview)
        .catch((e) => setError(String(e instanceof Error ? e.message : e)))
    }, 350)
    return () => clearTimeout(timer)
  }, [shootId, destination, options])

  useEffect(() => {
    if (exportProgress?.finished) setRunning(false)
  }, [exportProgress])

  const pickDestination = async () => {
    const picked = await open({ directory: true, multiple: false, title: 'Choose the destination folder' })
    if (typeof picked === 'string') setDestination(picked)
  }

  const start = async () => {
    setRunning(true)
    setError(null)
    try {
      await api.startExport(shootId, destination, options)
    } catch (e) {
      setRunning(false)
      const message = String(e instanceof Error ? e.message : e)
      setError(message)
      pushNotice({ level: 'error', message })
    }
  }

  const playersWithMedia = people.data?.filter((p) => p.mediaCount > 0) ?? []
  const busy = running && exportProgress != null && !exportProgress.finished

  return (
    <>
      <div className="workspace-header">
        <h1>Copy &amp; Organise — {shoot.data?.name ?? ''}</h1>
      </div>

      {options.personIds !== null && (
        <div className="filter-bar export-selection-summary">
          <strong>{formatCount(options.personIds.length)} selected person group(s)</strong>
          <span className="hint">
            Choose a destination below. Each selected person will get a folder named after them,
            containing copies of their grouped media.
          </span>
        </div>
      )}

      <div className="settings-grid">
        <div className="card">
          <h2>Destination</h2>
          <div style={{ display: 'flex', gap: 8 }}>
            <input
              style={{ flex: 1 }}
              value={destination}
              onChange={(e) => setDestination(e.target.value)}
              placeholder="D:\Exports\BGMS_Finals"
            />
            <button onClick={pickDestination}>Browse…</button>
          </div>

          <h2 style={{ marginTop: 8 }}>Options</h2>
          <label className="checkbox-row">
            <input
              type="checkbox"
              checked={options.splitPhotosVideos}
              onChange={(e) => setOptions({ ...options, splitPhotosVideos: e.target.checked })}
            />
            Photos / Videos subfolders
          </label>
          <label className="checkbox-row">
            <input
              type="checkbox"
              checked={options.includeUnidentified}
              onChange={(e) => setOptions({ ...options, includeUnidentified: e.target.checked })}
            />
            Include the Unidentified folder
          </label>
          <label className="checkbox-row">
            <input
              type="checkbox"
              checked={options.includeMultiPlayer}
              onChange={(e) => setOptions({ ...options, includeMultiPlayer: e.target.checked })}
            />
            Include multi-player albums (duplicates files)
          </label>
          <label className="checkbox-row">
            <input
              type="checkbox"
              checked={options.includeGroupSize}
              onChange={(e) => setOptions({ ...options, includeGroupSize: e.target.checked })}
            />
            Include group-size folders — Single, Two persons… (duplicates files)
          </label>
          <label className="checkbox-row">
            <input
              type="checkbox"
              checked={options.preserveMetadata}
              onChange={(e) => setOptions({ ...options, preserveMetadata: e.target.checked })}
            />
            Preserve file timestamps
          </label>
          <label className="field">
            <span>If a file already exists</span>
            <select
              value={options.existing}
              onChange={(e) =>
                setOptions({ ...options, existing: e.target.value as ExportOptions['existing'] })
              }
            >
              <option value="skip">Skip it (fast re-runs)</option>
              <option value="rename">Keep both — add “(2)”</option>
              <option value="overwrite">Overwrite it</option>
            </select>
          </label>
        </div>

        <div className="card">
          <h2>Players</h2>
          <label className="checkbox-row">
            <input
              type="checkbox"
              checked={options.personIds === null}
              onChange={(e) =>
                setOptions({ ...options, personIds: e.target.checked ? null : [] })
              }
            />
            Copy every player group
          </label>
          {options.personIds !== null && (
            <div className="row-list" style={{ maxHeight: 280, overflowY: 'auto' }}>
              {playersWithMedia.map((person) => (
                <label key={person.id} className="checkbox-row">
                  <input
                    type="checkbox"
                    checked={options.personIds?.includes(person.id) ?? false}
                    onChange={(e) => {
                      const current = options.personIds ?? []
                      setOptions({
                        ...options,
                        personIds: e.target.checked
                          ? [...current, person.id]
                          : current.filter((id) => id !== person.id),
                      })
                    }}
                  />
                  {person.name} ({formatCount(person.mediaCount)})
                </label>
              ))}
            </div>
          )}

          <h2 style={{ marginTop: 8 }}>Summary</h2>
          {error && <div style={{ color: 'var(--error)', fontSize: 13 }}>{error}</div>}
          {preview && !error && (
            <div className="hint">
              {formatCount(preview.fileCount)} files · {formatBytes(preview.totalBytes)} into{' '}
              {preview.folders.length} folder(s). Originals are copied; the source folder is never
              modified.
            </div>
          )}
          {busy && exportProgress && (
            <div className="progress-panel">
              <div className="progress-bar">
                <div
                  style={{
                    width: `${
                      exportProgress.filesTotal > 0
                        ? ((exportProgress.filesDone + exportProgress.filesSkipped) /
                            exportProgress.filesTotal) *
                          100
                        : 0
                    }%`,
                  }}
                />
              </div>
              <div className="hint">
                {formatCount(exportProgress.filesDone)} copied
                {exportProgress.filesSkipped > 0 && `, ${exportProgress.filesSkipped} skipped`} ·{' '}
                {formatBytes(exportProgress.bytesDone)}
              </div>
              <button className="small danger" onClick={() => api.cancelExport(shootId)}>
                Cancel copying
              </button>
            </div>
          )}
          <div style={{ display: 'flex', gap: 8 }}>
            <button
              className="primary"
              disabled={!preview || preview.fileCount === 0 || busy || !!error}
              onClick={start}
            >
              {busy ? 'Copying…' : 'Copy into folders'}
            </button>
            {destination && !busy && (
              <button onClick={() => api.openPath(destination)}>Open folder</button>
            )}
          </div>
        </div>
      </div>

      {(history.data?.length ?? 0) > 0 && (
        <div className="section" style={{ marginTop: 26 }}>
          <h2>Previous copies</h2>
          <div className="row-list">
            {history.data?.map((record) => (
              <div className="row" key={record.id}>
                <div className="grow">
                  <div className="title">{record.destination}</div>
                  <div className="sub">
                    {formatCount(record.filesDone)} / {formatCount(record.filesTotal)} files ·{' '}
                    {formatBytes(record.bytesDone)}
                    {record.error && <span style={{ color: 'var(--error)' }}> · {record.error}</span>}
                  </div>
                </div>
                <span className={`badge ${record.status}`}>{record.status}</span>
              </div>
            ))}
          </div>
        </div>
      )}
    </>
  )
}
