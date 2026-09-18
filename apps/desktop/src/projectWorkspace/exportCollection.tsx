import { useEffect, useState } from 'react'
import { pickFolder } from '../pickers'
import type { ExportOptions } from '@skwad/shared-types'
import * as api from '../api'
import { formatBytes, formatCount } from '../media'
import { useUi } from '../store'
import { WorkspaceDialog } from './WorkspaceDialog'
import type { ProjectCollection } from './model'

/**
 * Exporting a collection copies its files to the destination, rather than
 * writing shortcuts back to the originals: the point of exporting a
 * collection is to hand someone a folder they can actually work from, on a
 * drive that may never see the NAS. The Classic Export screen still offers
 * shortcuts for the cases where that is the cheaper answer.
 */
export const COLLECTION_EXPORT_OPTIONS: Omit<ExportOptions, 'groupIds'> = {
  mode: 'groups',
  delivery: 'copy',
  splitPhotosVideos: true,
  includeUnidentified: true,
  personIds: null,
  preserveMetadata: true,
  existing: 'skip',
  includeMultiPlayer: false,
  includeGroupSize: false,
  writeManifest: true,
}

/** The collection's sources gathered per shoot — one export run each. */
export function runsFor(collection: ProjectCollection) {
  const byShoot = new Map<number, number[]>()
  for (const source of collection.sources) {
    byShoot.set(source.shootId, [...(byShoot.get(source.shootId) ?? []), source.groupId])
  }
  return [...byShoot].map(([shootId, groupIds]) => ({ shootId, groupIds }))
}

export function ExportCollectionDialog({ collection, onClose }: { collection: ProjectCollection; onClose: () => void }) {
  const [destination, setDestination] = useState('')
  const [preview, setPreview] = useState<{ files: number; bytes: number } | null>(null)
  const [error, setError] = useState('')
  const [starting, setStarting] = useState(false)
  const setJob = useUi(s => s.setCollectionExport)
  const running = useUi(s => s.collectionExport)
  const runs = runsFor(collection)

  // Previewing needs a destination: the backend refuses one that sits inside
  // the shoot's own source folder, and that is worth surfacing before the
  // user commits to a copy.
  useEffect(() => {
    if (!destination.trim()) { setPreview(null); setError(''); return }
    let cancelled = false
    const timer = setTimeout(async () => {
      try {
        const previews = await Promise.all(
          runs.map(run => api.previewExport(run.shootId, destination, { ...COLLECTION_EXPORT_OPTIONS, groupIds: run.groupIds })),
        )
        if (cancelled) return
        setPreview({
          files: previews.reduce((total, item) => total + item.fileCount, 0),
          bytes: previews.reduce((total, item) => total + item.totalBytes, 0),
        })
        setError('')
      } catch (e) {
        if (!cancelled) { setPreview(null); setError(String(e)) }
      }
    }, 250)
    return () => { cancelled = true; clearTimeout(timer) }
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [destination, collection.id])

  // Hands the copy to the background job in the store and gets out of the
  // way — progress carries on in the corner card, so a long copy doesn't pin
  // the user to this dialog.
  const start = async () => {
    setStarting(true); setError('')
    try {
      const run = runs[0]
      const exportId = await api.startExport(run.shootId, destination, { ...COLLECTION_EXPORT_OPTIONS, groupIds: run.groupIds })
      setJob({ collectionName: collection.name, destination, runs, index: 0, exportId, stopping: false })
      onClose()
    } catch (e) {
      setError(String(e)); setStarting(false)
    }
  }

  const pickDestination = async () => {
    try {
      const picked = await pickFolder('Choose the destination folder')
      if (picked !== null) setDestination(picked)
    } catch (e) {
      setError(String(e))
    }
  }

  return <WorkspaceDialog title={`Export "${collection.name}"`} onClose={() => { if (!starting) onClose() }}>
    <p>Copies this collection's files into a folder at the destination, so they can be used there without the original library. Originals are only read.</p>
    <label className="field">Destination
      <div className="pw-export-destination">
        <input value={destination} disabled={starting} onChange={e => setDestination(e.target.value)} placeholder="e.g. D:\Handover\Team Entry" />
        <button type="button" disabled={starting} onClick={() => void pickDestination()}>Browse…</button>
      </div>
    </label>
    {preview && !error && <p className="pw-help">{formatCount(preview.files)} file{preview.files === 1 ? '' : 's'} · {formatBytes(preview.bytes)} will be copied.</p>}
    {collection.sources.length === 0 && <p className="pw-help">This collection has no media yet.</p>}
    {running && <p role="alert" className="pw-error">"{running.collectionName}" is still copying. Wait for it to finish, or stop it from the progress card.</p>}
    {error && <p role="alert" className="pw-error">{error}</p>}
    <div className="pw-dialog-actions">
      <button type="button" disabled={starting} onClick={onClose}>Cancel</button>
      <button className="primary" disabled={starting || !!running || !destination.trim() || !preview || preview.files === 0} onClick={() => void start()}>{starting ? 'Starting…' : 'Export'}</button>
    </div>
  </WorkspaceDialog>
}
