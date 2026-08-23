/**
 * One folder picker, two mechanisms.
 *
 * In the desktop window it opens the operating system's dialog. In a browser it
 * drives the server's jailed folder browser, which only ever shows what the
 * server was configured to allow. Callers pass the same props either way and do
 * not know which happened.
 */

import { useEffect, useState } from 'react'
import { useQuery } from '@tanstack/react-query'
import type { FsListing, FsRoot } from '@teo/shared-types'
import * as api from '../api'
import { formatCount } from '../media'
import { transport } from '../transport'
import { Modal } from './Modal'

export function PathPicker(props: {
  /** Current value, shown in the text field. */
  value: string
  onChange: (path: string) => void
  /** Dialog title, and the browser modal's heading. */
  title: string
  placeholder?: string
  /** Only offer folders an export may write into. */
  writableOnly?: boolean
  disabled?: boolean
}) {
  const [browsing, setBrowsing] = useState(false)
  const native = transport().pickFolder

  const browse = async () => {
    if (native) {
      const picked = await native(props.title)
      if (picked) props.onChange(picked)
      return
    }
    setBrowsing(true)
  }

  return (
    <>
      <div style={{ display: 'flex', gap: 8 }}>
        <input
          style={{ flex: 1 }}
          value={props.value}
          onChange={(e) => props.onChange(e.target.value)}
          placeholder={props.placeholder}
          disabled={props.disabled}
        />
        <button onClick={browse} disabled={props.disabled}>
          Browse…
        </button>
      </div>
      {browsing && (
        <FolderBrowser
          title={props.title}
          writableOnly={props.writableOnly}
          initialPath={props.value}
          onClose={() => setBrowsing(false)}
          onPick={(path) => {
            props.onChange(path)
            setBrowsing(false)
          }}
        />
      )}
    </>
  )
}

/**
 * The browser-side picker. Navigation is server-driven: it only ever asks for a
 * path the server just handed it, so there is nothing here that could walk out
 * of the allowed roots even if it wanted to.
 */
function FolderBrowser(props: {
  title: string
  writableOnly?: boolean
  initialPath: string
  onClose: () => void
  onPick: (path: string) => void
}) {
  const [path, setPath] = useState<string | null>(null)
  const [error, setError] = useState<string | null>(null)

  const roots = useQuery({ queryKey: ['fsRoots'], queryFn: api.fsRoots })

  // Start where the caller already pointed, else at the first usable root.
  useEffect(() => {
    if (path !== null || !roots.data) return
    const usable = roots.data.filter((r) => r.available && (!props.writableOnly || r.writable))
    setPath(props.initialPath.trim() || usable[0]?.path || '')
  }, [roots.data, path, props.initialPath, props.writableOnly])

  const listing = useQuery<FsListing>({
    queryKey: ['fsList', path],
    queryFn: () => api.fsList(path as string),
    enabled: !!path,
    retry: false,
  })

  useEffect(() => {
    setError(listing.error ? String(listing.error instanceof Error ? listing.error.message : listing.error) : null)
  }, [listing.error])

  const usableRoots = (roots.data ?? []).filter(
    (root: FsRoot) => root.available && (!props.writableOnly || root.writable),
  )

  return (
    <Modal title={props.title} onClose={props.onClose}>
      {usableRoots.length === 0 && !roots.isLoading && (
        <div className="hint">
          This server has no {props.writableOnly ? 'writable' : 'media'} folders configured. Set{' '}
          <span className="mono">{props.writableOnly ? 'TEO_OUTPUT_ROOTS' : 'TEO_MEDIA_ROOTS'}</span> and
          restart it.
        </div>
      )}

      {usableRoots.length > 0 && (
        <div className="filter-bar" style={{ marginBottom: 0 }}>
          {usableRoots.map((root) => (
            <button
              key={root.path}
              className={`small${path === root.path ? ' primary' : ''}`}
              onClick={() => setPath(root.path)}
            >
              {root.name}
            </button>
          ))}
        </div>
      )}

      {path && (
        <div className="hint mono" style={{ overflowWrap: 'anywhere' }}>
          {listing.data?.path ?? path}
        </div>
      )}

      {error && <div style={{ color: 'var(--error)', fontSize: 13 }}>{error}</div>}

      <div className="row-list" style={{ maxHeight: 320, overflowY: 'auto' }}>
        {listing.data?.parent && (
          <button className="group-row static" onClick={() => setPath(listing.data!.parent!)}>
            <span className="name">↑ up one level</span>
          </button>
        )}
        {listing.data?.directories.map((entry) => (
          <div key={entry.path} className="group-row" onClick={() => setPath(entry.path)}>
            <div className="grow">
              <div className="name">{entry.name}</div>
              <div className="sub">
                {formatCount(entry.mediaCount)} media{entry.hasSubfolders ? ' · has subfolders' : ''}
              </div>
            </div>
          </div>
        ))}
        {listing.data && listing.data.directories.length === 0 && (
          <div className="hint">No subfolders here.</div>
        )}
      </div>

      {listing.data && (
        <div className="hint">
          {formatCount(listing.data.mediaCount)} media file(s) directly in this folder.
        </div>
      )}

      <div className="buttons">
        <button onClick={props.onClose}>Cancel</button>
        <button
          className="primary"
          disabled={!listing.data}
          onClick={() => listing.data && props.onPick(listing.data.path)}
        >
          Use this folder
        </button>
      </div>
    </Modal>
  )
}
