/**
 * The live progress readout from §18.
 *
 * Two layers. The headline answers "how far along is this shoot?"; the step
 * list underneath answers the three questions the headline cannot — what has
 * finished, what is running right now, and what is still waiting — because a
 * scan that says 1,557 / 1,557 while the bar sits at 15% is otherwise unreadable.
 */

import { useEffect, useRef, useState } from 'react'
import { useMutation, useQuery, useQueryClient } from '@tanstack/react-query'
import type { ProgressEvent, StageProgress } from '@skwad/shared-types'
import * as api from '../api'
import { formatBytes, formatCount } from '../media'
import { useUi } from '../store'

/** How each queue kind is named and described in the step list. */
const STEPS: Record<string, { label: string; doing: string }> = {
  scan: { label: 'Scan folder', doing: 'Walking the folder and indexing files' },
  thumbnail: { label: 'Thumbnails', doing: 'Decoding and caching grid previews' },
  analysePhoto: { label: 'Photo analysis', doing: 'Detecting faces and embedding them' },
  analyseVideo: { label: 'Video analysis', doing: 'Sampling frames and detecting faces' },
  recognise: { label: 'Recognise players', doing: 'Matching faces against the player library' },
  cluster: { label: 'Group unknown faces', doing: 'Clustering whatever was not recognised' },
  albums: { label: 'Build albums', doing: 'Rebuilding player, team and group-size albums' },
  proxy: { label: 'Video previews', doing: 'Building full-duration 512px proxies' },
}

type StepState = 'done' | 'running' | 'blocked' | 'waiting' | 'failed'

function stepState(stage: StageProgress, blockedKind: string | null): StepState {
  if (stage.running > 0) return 'running'
  if (stage.queued > 0) return blockedKind === stage.kind ? 'blocked' : 'waiting'
  if (stage.failed > 0) return 'failed'
  return 'done'
}

const STEP_MARK: Record<StepState, string> = {
  done: '✓',
  running: '◐',
  blocked: '!',
  waiting: '○',
  failed: '✕',
}

/** Seconds → "45s", "6m 20s", "1h 04m" — short enough to sit inside a row. */
function formatDuration(seconds: number): string {
  if (!Number.isFinite(seconds) || seconds < 0) return '—'
  if (seconds < 60) return `${Math.round(seconds)}s`
  const m = Math.floor(seconds / 60)
  if (m < 60) return `${m}m ${String(Math.round(seconds % 60)).padStart(2, '0')}s`
  return `${Math.floor(m / 60)}h ${String(m % 60).padStart(2, '0')}m`
}

function secondsSince(iso: string | null): number | null {
  if (!iso) return null
  const started = new Date(iso).getTime()
  if (Number.isNaN(started)) return null
  return Math.max(0, (Date.now() - started) / 1000)
}

/**
 * Estimates the time left from how fast files have actually been finishing.
 *
 * The rate is measured over a trailing window rather than the whole run: the
 * first minutes of a shoot include model loading and thumbnailing, which would
 * make a lifetime average permanently pessimistic.
 */
const RATE_WINDOW_MS = 60_000

function useThroughput(shootId: number, finished: number) {
  const samples = useRef<{ at: number; finished: number }[]>([])
  const lastShoot = useRef(shootId)

  if (lastShoot.current !== shootId) {
    samples.current = []
    lastShoot.current = shootId
  }

  const now = Date.now()
  const previous = samples.current[samples.current.length - 1]
  if (!previous || previous.finished !== finished) {
    samples.current.push({ at: now, finished })
    samples.current = samples.current.filter((s) => now - s.at <= RATE_WINDOW_MS)
    // Keep one sample older than the window so a slow stage still has a baseline.
    if (samples.current.length < 2 && previous) samples.current.unshift(previous)
  }

  const oldest = samples.current[0]
  if (!oldest || samples.current.length < 2) return null
  const elapsed = (now - oldest.at) / 1000
  const done = finished - oldest.finished
  if (elapsed < 2 || done <= 0) return null
  return done / elapsed
}

export function ProgressPanel(props: { shootId: number }) {
  const progress = useUi((s) => s.progress[props.shootId])
  const queryClient = useQueryClient()
  const pushNotice = useUi((s) => s.pushNotice)
  const [showSteps, setShowSteps] = useState(true)
  const storage = useQuery({
    queryKey: ['shoot-storage', props.shootId],
    queryFn: () => api.getShootStorage(props.shootId),
    refetchInterval: 30_000,
  })

  const pause = useMutation({
    mutationFn: ({ shootId, paused }: { shootId: number; paused: boolean }) => api.pauseProcessing(shootId, paused),
    onSuccess: (paused, { shootId }) => {
      const current = useUi.getState().progress[shootId]
      if (current) useUi.getState().setProgress({ ...current, paused })
    },
    onError: (e) => pushNotice({ level: 'error', message: String(e) }),
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

  const finished = progress ? progress.mediaAnalysed + progress.mediaFailed : 0
  const rate = useThroughput(props.shootId, finished)

  if (!progress) return null
  const active = progress.jobsQueued + progress.jobsRunning > 0
  const remaining = Math.max(0, progress.mediaTotal - finished)
  const eta = active && rate && remaining > 0 ? remaining / rate : null

  return (
    <div className="card progress-panel section">
      <div style={{ display: 'flex', justifyContent: 'space-between', alignItems: 'center' }}>
        <strong>
          {active
            ? progress.paused
              ? progress.jobsRunning > 0 ? 'Pausing — finishing active files' : 'Paused'
              : `Processing — ${progress.stage}`
            : 'Processing complete'}
        </strong>
        <div style={{ display: 'flex', gap: 8 }}>
          <button className="small ghost" onClick={() => setShowSteps((shown) => !shown)}>
            {showSteps ? 'Hide detail' : 'Show detail'}
          </button>
          {active && (
            <>
              <button className="small" disabled={pause.isPending} title="Pause this shoot after its active files finish. Other shoots continue." onClick={() => pause.mutate({ shootId: props.shootId, paused: !progress.paused })}>
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

      {/* The bar tracks analysis, so say so — and say what is left. */}
      <div className="progress-headline">
        <span>
          <strong>{progress.percent.toFixed(1)}%</strong> analysed —{' '}
          {formatCount(finished)} of {formatCount(progress.mediaTotal)} files
        </span>
        {active && (
          <span className="hint">
            {formatCount(remaining)} to go
            {eta !== null && ` · about ${formatDuration(eta)} left`}
            {rate !== null && ` · ${rate.toFixed(rate < 10 ? 1 : 0)} files/s`}
          </span>
        )}
      </div>

      {progress.blockedReason && (
        <div className="progress-blocked">
          <strong>Waiting on a missing dependency.</strong> {progress.blockedReason}. The queue
          retries by itself — nothing is lost, and work resumes as soon as it is available.
        </div>
      )}

      <div className="progress-stats">
        <div>
          Media scanned
          <strong>
            {formatCount(progress.mediaScanned)} / {formatCount(progress.mediaTotal)}
          </strong>
        </div>
        <div>
          Analysed
          <strong>
            {formatCount(progress.mediaAnalysed)} / {formatCount(progress.mediaTotal)}
          </strong>
        </div>
        <div>
          <details className="storage-details">
            <summary>
              Space used (est.)
              <strong>{storage.data
                ? `≈ ${formatBytes(storage.data.recordBytes + storage.data.previewBytes)}`
                : storage.isError ? 'Unavailable' : 'Calculating…'}</strong>
            </summary>
            {storage.data && <div>
              Analysis records: {formatBytes(storage.data.recordBytes)}.<br />
              Previews: {formatBytes(storage.data.previewBytes)}.<br />
              Estimated record payload plus saved thumbnails, face crops and video previews.
              Excludes originals, shared player profiles, AI models, database indexes, free space and temporary write logs.
              Shared previews count once within this shoot. Updates every 30 seconds.
              Scan and recognition records remain until you choose to delete them.
            </div>}
            {storage.isError && <button className="small" onClick={() => storage.refetch()}>Retry storage calculation</button>}
          </details>
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
        {progress.mediaFailed > 0 && (
          <div>
            Files failed
            <strong style={{ color: 'var(--error)' }}>{formatCount(progress.mediaFailed)}</strong>
          </div>
        )}
        {progress.jobsFailed > 0 && (
          <div>
            Failed jobs
            <strong style={{ color: 'var(--error)' }}>{formatCount(progress.jobsFailed)}</strong>
          </div>
        )}
      </div>

      {showSteps && <PipelineSteps progress={progress} />}
    </div>
  )
}

/** The per-step breakdown: completed, in flight, and still queued. */
function PipelineSteps(props: { progress: ProgressEvent }) {
  const { progress } = props
  // The elapsed times below tick on their own; progress events only arrive
  // twice a second and stop entirely while a single long file is being read.
  const [, setTick] = useState(0)
  useEffect(() => {
    const timer = setInterval(() => setTick((t) => t + 1), 1000)
    return () => clearInterval(timer)
  }, [])

  if (progress.stages.length === 0) {
    return <p className="hint">No queued work for this shoot yet.</p>
  }

  return (
    <div className="pipeline">
      <div className="pipeline-head">
        Pipeline · {formatCount(progress.jobsDone)} done · {formatCount(progress.jobsRunning)}{' '}
        running · {formatCount(progress.jobsQueued)} waiting
        {progress.photosTotal + progress.videosTotal > 0 && (
          <span className="hint">
            {formatCount(progress.photosTotal)} photos · {formatCount(progress.videosTotal)} videos
          </span>
        )}
      </div>

      {progress.stages.map((stage) => {
        const state = stepState(stage, progress.blockedKind)
        const total = stage.queued + stage.running + stage.done + stage.failed
        const step = STEPS[stage.kind] ?? { label: stage.kind, doing: 'Working' }
        const settled = stage.done + stage.failed
        return (
          <div key={stage.kind} className={`pipeline-step is-${state}`}>
            <span className="pipeline-mark" aria-hidden>
              {STEP_MARK[state]}
            </span>
            <span className="pipeline-label">{step.label}</span>
            <span className="pipeline-count">
              {total > 1 ? `${formatCount(settled)} / ${formatCount(total)}` : ''}
            </span>
            <span className="pipeline-note">
              {state === 'running' && `${step.doing}${stage.running > 1 ? ` ×${stage.running}` : ''}`}
              {state === 'waiting' &&
                (settled > 0
                  ? `${formatCount(stage.queued)} still queued`
                  : `Queued — starts after the steps above`)}
              {state === 'blocked' && `Blocked: ${progress.blockedReason ?? 'waiting to retry'}`}
              {state === 'done' && 'Complete'}
              {state === 'failed' && `${formatCount(stage.failed)} gave up after retries`}
              {state !== 'blocked' && stage.failed > 0 && state !== 'failed' && (
                <span className="pipeline-failed"> · {formatCount(stage.failed)} failed</span>
              )}
            </span>
            <span className="pipeline-track" aria-hidden>
              <span style={{ width: total > 0 ? `${(settled / total) * 100}%` : '0%' }} />
            </span>
          </div>
        )
      })}

      {progress.active.length > 0 && (
        <div className="pipeline-now">
          <span className="pipeline-now-label">Right now</span>
          <ul>
            {progress.active.map((job) => {
              const elapsed = secondsSince(job.startedAt)
              return (
                <li key={job.jobId}>
                  <em>{STEPS[job.kind]?.label ?? job.kind}</em>
                  {job.filename ? ` — ${job.filename}` : ' — whole shoot'}
                  {elapsed !== null && <span className="hint"> · {formatDuration(elapsed)}</span>}
                </li>
              )
            })}
          </ul>
        </div>
      )}
    </div>
  )
}
