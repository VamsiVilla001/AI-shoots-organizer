/**
 * The live progress readout from §18:
 *
 *   Photos scanned  2,431 / 2,431
 *   Faces detected  1,892 …
 */

import { useMutation, useQueryClient } from '@tanstack/react-query'
import * as api from '../api'
import { formatCount } from '../media'
import { useUi } from '../store'

export function ProgressPanel(props: { shootId: number }) {
  const progress = useUi((s) => s.progress[props.shootId])
  const queryClient = useQueryClient()
  const pushNotice = useUi((s) => s.pushNotice)

  const pause = useMutation({
    mutationFn: (paused: boolean) => api.pauseProcessing(paused),
  })
  const cancel = useMutation({
    mutationFn: () => api.cancelProcessing(props.shootId),
    onSuccess: () => queryClient.invalidateQueries({ queryKey: ['shoots'] }),
  })
  const resume = useMutation({
    mutationFn: () => api.resumeProcessing(props.shootId),
    onSuccess: (queued) =>
      pushNotice({ level: 'success', message: `Queued ${queued} file(s) for processing.` }),
    onError: (e) => pushNotice({ level: 'error', message: String(e) }),
  })

  if (!progress) return null
  const active = progress.jobsQueued + progress.jobsRunning > 0

  return (
    <div className="card progress-panel section">
      <div style={{ display: 'flex', justifyContent: 'space-between', alignItems: 'center' }}>
        <strong>
          {active
            ? progress.paused
              ? 'Paused'
              : `Processing — ${progress.stage}`
            : 'Processing complete'}
        </strong>
        <div style={{ display: 'flex', gap: 8 }}>
          {active && (
            <>
              <button className="small" onClick={() => pause.mutate(!progress.paused)}>
                {progress.paused ? 'Resume' : 'Pause'}
              </button>
              <button className="small danger" onClick={() => cancel.mutate()}>
                Cancel
              </button>
            </>
          )}
          {!active && (progress.jobsFailed > 0 || progress.mediaFailed > 0) && (
            <button className="small" onClick={() => resume.mutate()}>
              Retry failed
            </button>
          )}
        </div>
      </div>

      <div className="progress-bar">
        <div style={{ width: `${Math.min(100, progress.percent).toFixed(1)}%` }} />
      </div>

      <div className="progress-stats">
        <div>
          Media scanned
          <strong>
            {formatCount(progress.mediaScanned)} / {formatCount(progress.mediaTotal)}
          </strong>
        </div>
        <div>
          Faces detected
          <strong>{formatCount(progress.facesDetected)}</strong>
        </div>
        <div>
          Players recognised
          <strong>{formatCount(progress.facesRecognised)}</strong>
        </div>
        <div>
          Unknown faces
          <strong>{formatCount(progress.facesUnknown)}</strong>
        </div>
        {progress.jobsFailed > 0 && (
          <div>
            Failed jobs
            <strong style={{ color: 'var(--error)' }}>{formatCount(progress.jobsFailed)}</strong>
          </div>
        )}
      </div>
    </div>
  )
}
