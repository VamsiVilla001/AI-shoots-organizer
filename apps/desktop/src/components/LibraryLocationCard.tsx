/**
 * The shared library location.
 *
 * SKWAD stays a local application, but a team can point every installation at
 * one folder on the network — the database, caches, face embeddings, profiles
 * and the sign-in accounts all live there — so it behaves like a shared
 * workspace without anything leaving the building.
 *
 * The pointer is per machine, so each workstation sets it once; the
 * administrator picks the folder and passes the path to the team.
 */

import { useEffect, useState } from 'react'
import { useMutation, useQuery, useQueryClient } from '@tanstack/react-query'
import { open } from '@tauri-apps/plugin-dialog'
import * as api from '../api'
import { Icon } from './Icon'
import { useUi } from '../store'

const SOURCE_LABEL: Record<string, string> = {
  environment: 'Set by SKWAD_LIBRARY_ROOT',
  configured: 'Shared library folder',
  appData: 'This machine only',
}

export function LibraryLocationCard({ isAdmin }: { isAdmin: boolean }) {
  const queryClient = useQueryClient()
  const pushNotice = useUi((state) => state.pushNotice)
  const location = useQuery({ queryKey: ['libraryLocation'], queryFn: api.getLibraryLocation })

  const [root, setRoot] = useState('')
  const [cacheRoot, setCacheRoot] = useState('')
  const [localCache, setLocalCache] = useState(false)
  const [share, setShare] = useState<boolean | null>(null)
  const [loaded, setLoaded] = useState(false)

  useEffect(() => {
    if (!location.data || loaded) return
    setRoot(location.data.configuredRoot ?? '')
    setCacheRoot(location.data.configuredCacheRoot ?? '')
    setLocalCache(location.data.configuredCacheRoot !== null)
    setShare(location.data.configuredRoot === null ? null : location.data.networkShare)
    setLoaded(true)
  }, [location.data, loaded])

  const apply = useMutation({
    mutationFn: () =>
      api.setLibraryLocation(
        root.trim() === '' ? null : root.trim(),
        localCache && cacheRoot.trim() !== '' ? cacheRoot.trim() : null,
        root.trim() === '' ? null : share,
      ),
    onSuccess: (saved) => {
      queryClient.setQueryData(['libraryLocation'], saved)
      pushNotice({
        level: 'success',
        message: saved.restartRequired ? 'Library location saved — restart to use it.' : 'Library location saved.',
      })
    },
    onError: (error) => pushNotice({ level: 'error', message: error instanceof Error ? error.message : String(error) }),
  })

  const restart = useMutation({
    mutationFn: api.restartForLibraryChange,
    onError: (error) => pushNotice({ level: 'error', message: error instanceof Error ? error.message : String(error) }),
  })

  const browse = async (target: 'root' | 'cache') => {
    const picked = await open({ directory: true, multiple: false, title: target === 'root' ? 'Choose the shared library folder' : 'Choose a local cache folder' })
    if (typeof picked !== 'string') return
    if (target === 'root') {
      setRoot(picked)
      if (share === null) setShare(picked.startsWith('\\\\') || picked.startsWith('//'))
    } else {
      setCacheRoot(picked)
    }
  }

  const data = location.data
  const dirty = data
    ? (root.trim() === '' ? null : root.trim()) !== data.configuredRoot
      || (localCache && cacheRoot.trim() !== '' ? cacheRoot.trim() : null) !== data.configuredCacheRoot
    : false

  return <div className="card library-card">
    <h2><Icon name="network" />Team library location</h2>
    <p className="hint">
      One folder holds the database, media cache, face embeddings, user accounts and profiles. Point every
      workstation at the same folder on your network and the team shares one library. Media files themselves stay
      wherever they already are.
    </p>

    {location.isPending && <p className="hint">Reading the current location…</p>}
    {data && <>
      <dl className="library-facts">
        <div><dt>In use now</dt><dd className="mono">{data.activeRoot}</dd></div>
        <div><dt>Database</dt><dd className="mono">{data.databaseFile}</dd></div>
        <div><dt>Media cache</dt><dd className="mono">{data.activeCacheRoot}</dd></div>
        <div><dt>Mode</dt><dd>{SOURCE_LABEL[data.source] ?? data.source}{data.networkShare ? ' · network share' : ''}</dd></div>
      </dl>

      {data.restartRequired && <div className="library-restart">
        <p>A different location is saved. SKWAD uses it after a restart.</p>
        <button className="primary" disabled={restart.isPending} onClick={() => restart.mutate()}>
          <Icon name="restart" />{restart.isPending ? 'Restarting…' : 'Restart now'}
        </button>
      </div>}

      <label className="field"><span>Shared library folder</span>
        <div className="library-row">
          <input
            className="mono"
            value={root}
            placeholder="\\\\NAS\\skwad-library — leave empty to keep this machine's own library"
            onChange={(event) => setRoot(event.target.value)}
          />
          <button type="button" onClick={() => void browse('root')}><Icon name="folder" />Browse…</button>
          {root.trim() !== '' && <button type="button" onClick={() => void navigator.clipboard.writeText(root.trim())}><Icon name="copy" />Copy</button>}
        </div>
        <span className="hint">
          {isAdmin
            ? 'Create the folder on the machine or NAS that stays on, share it with full read and write access for the team, then send everyone this exact path.'
            : 'Enter the path your administrator gave you. It is usually a \\\\server\\share address.'}
        </span>
      </label>

      {root.trim() !== '' && <>
        <label className="checkbox-row">
          <input type="checkbox" checked={share === true} onChange={(event) => setShare(event.target.checked)} />
          This folder is on the network
        </label>
        <div className="hint">
          Detected automatically for <span className="mono">\\server\share</span> paths. Tick it yourself when the
          share is reached through a mapped drive letter — it switches the database to a journal mode that several
          workstations can share safely.
        </div>

        <label className="checkbox-row">
          <input type="checkbox" checked={localCache} onChange={(event) => setLocalCache(event.target.checked)} />
          Keep thumbnails and previews on this machine
        </label>
        {localCache && <div className="library-row">
          <input className="mono" value={cacheRoot} placeholder="D:\\skwad-cache" onChange={(event) => setCacheRoot(event.target.value)} />
          <button type="button" onClick={() => void browse('cache')}><Icon name="folder" />Browse…</button>
        </div>}
        <div className="hint">
          Faster on a busy network, at the cost of each machine rebuilding its own previews. Leave it off to share
          one cache with everyone.
        </div>
      </>}

      <div className="actions">
        <button className="primary" disabled={apply.isPending || !dirty} onClick={() => apply.mutate()}>
          <Icon name="save" />{apply.isPending ? 'Checking the folder…' : 'Save location'}
        </button>
        {data.configuredRoot !== null && <button
          disabled={apply.isPending}
          onClick={() => {
            if (!window.confirm('Go back to this machine\u2019s own library? The shared library is left untouched.')) return
            setRoot('')
            setCacheRoot('')
            setLocalCache(false)
            setShare(null)
            apply.mutate()
          }}
        >Use this machine only</button>}
      </div>

      {data.existingLibrary
        ? <p className="hint">A library already exists in that folder — this machine will join it.</p>
        : root.trim() !== '' && <p className="hint">No library there yet — SKWAD creates an empty one on the next start.</p>}
      <p className="hint">
        Nothing is copied when the location changes. To move an existing library, copy the contents of{' '}
        <span className="mono">{data.appDataRoot}</span> into the shared folder while SKWAD is closed.
      </p>
    </>}
  </div>
}
