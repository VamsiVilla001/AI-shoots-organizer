import { useState } from 'react'
import { useQuery, useQueryClient } from '@tanstack/react-query'
import { open, save } from '@tauri-apps/plugin-dialog'
import * as api from '../api'
import { useUi } from '../store'

export function CataloguesScreen() {
  const queryClient = useQueryClient()
  const pushNotice = useUi((state) => state.pushNotice)
  const activeShootId = useUi((state) => state.activeShootId)
  const [passphrase, setPassphrase] = useState('')
  const [selected, setSelected] = useState<{ packageId: string; revisionId: string } | null>(null)
  const [groupId, setGroupId] = useState<number | null>(null)

  const session = useQuery({ queryKey: ['catalogueSession'], queryFn: api.catalogueSessionStatus })
  const loaded = useQuery({ queryKey: ['loadedCatalogues'], queryFn: api.listLoadedCatalogues })
  const groups = useQuery({
    queryKey: ['catalogueGroups', selected],
    queryFn: () => api.listCatalogueGroups(selected!.packageId, selected!.revisionId),
    enabled: selected !== null,
  })
  const media = useQuery({
    queryKey: ['catalogueMedia', selected, groupId],
    queryFn: () => api.listCatalogueMedia(selected!.packageId, selected!.revisionId, groupId),
    enabled: selected !== null,
  })

  const publish = async () => {
    if (activeShootId === null) {
      pushNotice({ level: 'warn', message: 'Open a shoot before publishing.' })
      return
    }
    if (passphrase.length < 12) {
      pushNotice({ level: 'warn', message: 'Use an offline passphrase of at least 12 characters.' })
      return
    }
    const destination = await save({ title: 'Publish encrypted SKWAD catalogue', defaultPath: 'shoot.skwad', filters: [{ name: 'SKWAD catalogue', extensions: ['skwad'] }] })
    if (!destination) return
    try {
      const result = await api.publishSkwad(activeShootId, destination, passphrase)
      setPassphrase('')
      pushNotice({ level: 'success', message: `Published ${result.mediaCount} encrypted references.` })
    } catch (error) {
      pushNotice({ level: 'error', message: String(error) })
    }
  }

  const load = async () => {
    const path = await open({ multiple: false, filters: [{ name: 'SKWAD catalogue', extensions: ['skwad'] }] })
    if (typeof path !== 'string') return
    try {
      const result = await api.loadSkwad(path, passphrase || null)
      setPassphrase('')
      setSelected({ packageId: result.packageId, revisionId: result.revisionId })
      await queryClient.invalidateQueries({ queryKey: ['loadedCatalogues'] })
      pushNotice({ level: 'success', message: `Loaded ${result.shootName} without scanning or AI analysis.` })
    } catch (error) {
      pushNotice({ level: 'error', message: String(error) })
    }
  }

  const mapLibrary = async () => {
    if (!selected) return
    const root = await open({ directory: true, multiple: false, title: 'Map this catalogue to its NAS root' })
    if (typeof root !== 'string') return
    try {
      await api.approveCatalogueLibrary(selected.packageId, selected.revisionId, root)
      await queryClient.invalidateQueries({ queryKey: ['loadedCatalogues'] })
    } catch (error) {
      pushNotice({ level: 'error', message: String(error) })
    }
  }

  const active = loaded.data?.find((item) => item.packageId === selected?.packageId && item.revisionId === selected?.revisionId)

  return <>
    <div className="workspace-header">
      <div><h1>Shared Catalogues</h1><p>Encrypted metadata only. Originals, thumbnails, face crops and embeddings stay local.</p></div>
      <div className="actions"><button onClick={load}>Load .skwad</button><button className="primary" onClick={publish} disabled={activeShootId === null || !session.data?.authenticatedOnce}>Publish current shoot</button></div>
    </div>
    <div className="settings-grid">
      <section className="card">
        <h2>Account & offline device</h2>
        <p>Authenticated as <strong>{session.data?.email || session.data?.accountId}</strong></p>
        <p className="hint mono">Device key: {session.data?.deviceKeyId}</p>
        <label className="field"><span>Offline passphrase</span><input type="password" value={passphrase} onChange={(event) => setPassphrase(event.target.value)} placeholder="Used for publish or fallback load" /><span className="hint">Never stored. Transfer it through a separate secure channel.</span></label>
      </section>
      <section className="card">
        <h2>Loaded this session</h2>
        {(loaded.data?.length ?? 0) === 0 && <p className="hint">No decrypted catalogue is retained on disk.</p>}
        {loaded.data?.map((item) => <button className={selected?.revisionId === item.revisionId ? 'catalogue-row active' : 'catalogue-row'} key={item.revisionId} onClick={() => { setSelected({ packageId: item.packageId, revisionId: item.revisionId }); setGroupId(null) }}>
          <strong>{item.shootName}</strong><span>Revision {item.publishedRevision} · {item.mediaCount} items</span><span>{item.mappedRoot ?? 'NAS mapping required'}</span>
        </button>)}
      </section>
    </div>
    {selected && <div className="catalogue-browser card">
      <div className="catalogue-groups"><button className={groupId === null ? 'active' : ''} onClick={() => setGroupId(null)}>All media</button>{groups.data?.map((group) => <button className={groupId === group.id ? 'active' : ''} key={group.id} onClick={() => setGroupId(group.id)}>{group.name} <span>{group.mediaCount}</span></button>)}</div>
      <div className="catalogue-content">
        <div className="actions"><strong>{active?.shootName}</strong>{!active?.mappedRoot && <button onClick={mapLibrary}>Map NAS root</button>}</div>
        <div className="catalogue-media-grid">{media.data?.map((item) => <button key={item.id} onDoubleClick={() => api.openCatalogueMedia(selected.packageId, selected.revisionId, item.id)} title={item.relativePath}><span className="catalogue-kind">{item.mediaType === 'video' ? 'VIDEO' : 'PHOTO'}</span><strong>{item.filename}</strong><small>{item.relativePath}</small>{item.isBestShot && <span className="badge">Best</span>}</button>)}</div>
      </div>
    </div>}
  </>
}
