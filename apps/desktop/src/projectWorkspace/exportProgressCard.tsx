import { useEffect } from 'react'
import * as api from '../api'
import { formatBytes, formatCount } from '../media'
import { useUi } from '../store'
import { COLLECTION_EXPORT_OPTIONS } from './exportCollection'

/**
 * The corner card for a collection export running in the background, and the
 * only thing driving it forward: a collection drawing on more than one shoot
 * needs one export run per shoot, and this is mounted app-wide so that
 * sequencing survives the dialog closing and the user navigating away.
 */
export function ExportProgressCard() {
  const job = useUi(s => s.collectionExport)
  const progress = useUi(s => s.exportProgress)
  const setJob = useUi(s => s.setCollectionExport)
  const pushNotice = useUi(s => s.pushNotice)

  // `job` is a dependency as well as `progress` so a run that finishes before
  // its id reaches the store is still picked up on the next render, rather
  // than leaving the card stuck at "Copying…".
  useEffect(() => {
    if (!job || !progress?.finished || progress.exportId !== job.exportId) return

    if (progress.error) {
      pushNotice({ level: 'error', message: `Export of "${job.collectionName}" failed: ${progress.error}` })
      setJob(null)
      return
    }
    if (job.stopping) {
      pushNotice({ level: 'warn', message: `Export of "${job.collectionName}" stopped. Files already copied were kept.` })
      setJob(null)
      return
    }

    const next = job.index + 1
    if (next >= job.runs.length) {
      pushNotice({ level: 'success', message: `Exported "${job.collectionName}" to ${job.destination}.` })
      setJob(null)
      return
    }

    void (async () => {
      try {
        const run = job.runs[next]
        const exportId = await api.startExport(run.shootId, job.destination, { ...COLLECTION_EXPORT_OPTIONS, groupIds: run.groupIds })
        setJob({ ...job, index: next, exportId })
      } catch (e) {
        pushNotice({ level: 'error', message: `Export of "${job.collectionName}" failed: ${String(e)}` })
        setJob(null)
      }
    })()
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [progress, job])

  if (!job) return null

  // Only trust progress that belongs to this job: a Classic export running
  // alongside reports into the same store slot.
  const current = progress?.exportId === job.exportId ? progress : null
  const done = current ? current.filesDone + current.filesSkipped : 0
  const total = current?.filesTotal ?? 0
  const percent = total > 0 ? Math.min(100, (done / total) * 100) : 0

  return (
    <div className="export-card" role="status" aria-live="polite">
      <div className="export-card-title">
        <strong>Copying "{job.collectionName}"</strong>
        <button
          className="small danger"
          disabled={job.stopping}
          onClick={() => {
            setJob({ ...job, stopping: true })
            void api.cancelExport(job.runs[job.index].shootId)
          }}
        >
          {job.stopping ? 'Stopping…' : 'Stop'}
        </button>
      </div>
      <div className="progress-bar"><div style={{ width: `${percent}%` }} /></div>
      <div className="export-card-meta">
        <span>{formatCount(done)} / {formatCount(total)} files{job.runs.length > 1 ? ` · part ${job.index + 1} of ${job.runs.length}` : ''}</span>
        <span>{formatBytes(current?.bytesDone ?? 0)}</span>
      </div>
    </div>
  )
}
