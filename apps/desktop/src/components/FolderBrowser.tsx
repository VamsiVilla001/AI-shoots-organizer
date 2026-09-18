/**
 * The server's folder browser, in a modal: the picker a client uses when
 * the folders that matter are on the server. Shows subfolders and a media
 * count per folder — enough to pick a shoot, nothing that enumerates a share.
 */

import { useEffect, useState } from 'react'
import { Modal } from './Modal'
import { settleFolderRequest, subscribeFolderRequests, type FolderRequest } from '../pickers'
import { transport, type FsListing, type FsRoot } from '../transport'

export function FolderBrowserHost() {
  const [request, setRequest] = useState<FolderRequest | null>(null)
  useEffect(() => subscribeFolderRequests(setRequest), [])
  if (!request) return null
  // Its own stacking context: the browser opens from inside other dialogs
  // (the "Add media" form) and has to sit above them.
  return (
    <div className="folder-browser-host">
      <FolderBrowser title={request.title} />
    </div>
  )
}

function FolderBrowser({ title }: { title: string }) {
  const [roots, setRoots] = useState<FsRoot[] | null>(null)
  const [listing, setListing] = useState<FsListing | null>(null)
  const [error, setError] = useState<string | null>(null)
  const [busy, setBusy] = useState(false)

  const browse = transport().browse

  useEffect(() => {
    if (!browse) return
    browse
      .roots()
      .then(setRoots)
      .catch((e) => setError(String((e as Error).message ?? e)))
  }, [browse])

  const enter = async (path: string) => {
    if (!browse) return
    setBusy(true)
    setError(null)
    try {
      setListing(await browse.list(path))
    } catch (e) {
      setError(String((e as Error).message ?? e))
    } finally {
      setBusy(false)
    }
  }

  return (
    <Modal title={title} onClose={() => settleFolderRequest(null)}>
      {error && <div className="hint" style={{ color: 'var(--danger, #e66)' }}>{error}</div>}
      {!listing && (
        <div className="folder-browser">
          {roots === null && !error && <div className="hint">Loading…</div>}
          {roots && roots.length === 0 && (
            <div className="hint">
              The server has no browsable folders configured. Set its media roots to the folders shoots live under.
            </div>
          )}
          {roots?.map((root) => (
            <button
              key={root.path}
              type="button"
              className="folder-row"
              disabled={!root.available || busy}
              onClick={() => void enter(root.path)}
            >
              <span className="mono">{root.path}</span>
              {!root.available && <span className="hint">not reachable right now</span>}
            </button>
          ))}
        </div>
      )}
      {listing && (
        <div className="folder-browser">
          <div className="hint mono">{listing.path}</div>
          <div className="folder-list">
            {listing.parent && (
              <button type="button" className="folder-row" disabled={busy} onClick={() => void enter(listing.parent!)}>
                ..
              </button>
            )}
            {!listing.parent && (
              <button type="button" className="folder-row" disabled={busy} onClick={() => setListing(null)}>
                .. (all roots)
              </button>
            )}
            {listing.directories.map((dir) => (
              <button key={dir.path} type="button" className="folder-row" disabled={busy} onClick={() => void enter(dir.path)}>
                <span>{dir.name}</span>
                <span className="hint">
                  {dir.mediaCount > 0 ? `${dir.mediaCount} media` : ''}
                  {dir.hasSubfolders ? ' ›' : ''}
                </span>
              </button>
            ))}
            {listing.directories.length === 0 && <div className="hint">No subfolders.</div>}
          </div>
          <div className="hint">
            {listing.mediaCount > 0 ? `${listing.mediaCount} media files directly in this folder.` : 'No media directly in this folder.'}
          </div>
        </div>
      )}
      <div className="buttons">
        <button type="button" onClick={() => settleFolderRequest(null)}>
          Cancel
        </button>
        <button type="button" className="primary" disabled={!listing || busy} onClick={() => settleFolderRequest(listing!.path)}>
          Choose this folder
        </button>
      </div>
    </Modal>
  )
}
