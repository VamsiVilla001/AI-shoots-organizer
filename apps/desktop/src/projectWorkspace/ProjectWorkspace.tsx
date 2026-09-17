import { useState } from 'react'
import { useQuery } from '@tanstack/react-query'
import * as api from '../api'
import { useUi } from '../store'
import { ThemeToggle } from '../components/ThemeToggle'
import { Icon } from '../components/Icon'
import { Wordmark } from '../components/Wordmark'
import { SettingsScreen } from '../screens/SettingsScreen'
import { ProfileScreen } from '../screens/ProfileScreen'
import { CataloguesScreen } from '../screens/CataloguesScreen'
import { AdminScreen } from '../screens/AdminScreen'
import { Collections } from './nestedCollections'
import { Processing } from './processing'
import { useProjects } from './model'
import './workspace.css'

export function ProjectWorkspace({ accountId, onClassic }: { accountId: string; onClassic: () => void }) {
  const [tab, setTab] = useState<'collections' | 'processing' | 'admin' | 'settings' | 'profile'>('collections')
  const [settingsTab, setSettingsTab] = useState('general')
  const [projectId, setProjectId] = useState<string | null>(null)
  const [publication, setPublication] = useState(0)
  const projectStore = useProjects(accountId)
  const progress = useUi(s => s.progress)
  const running = Object.values(progress).filter(p => p.jobsQueued + p.jobsRunning > 0).length
  const profile = useQuery({ queryKey: ['userProfile'], queryFn: api.getUserProfile })
  const session = useQuery({ queryKey: ['catalogueSession'], queryFn: api.catalogueSessionStatus })
  const isAdmin = session.data?.isAdmin === true
  const sections: Array<[string, string]> = [['general', 'General'], ['catalogues', 'Catalogue exchange']]
  const process = () => setTab('processing')

  return <div className="pw-shell">
    <aside className="pw-sidebar">
      <Wordmark className="pw-brand" />
      <nav aria-label="Main navigation">
        <button aria-current={tab === 'collections' ? 'page' : undefined} onClick={() => setTab('collections')}><span className="nav-label"><Icon name="collections" /><span>Collections</span></span></button>
        <button aria-current={tab === 'processing' ? 'page' : undefined} onClick={process}><span className="nav-label"><Icon name="processing" /><span>Media Processing</span></span>{running > 0 && <span className="badge">{running}</span>}</button>
        {/* Administrators manage the team's accounts; members never see this. */}
        {isAdmin && <button aria-current={tab === 'admin' ? 'page' : undefined} onClick={() => setTab('admin')}><span className="nav-label"><Icon name="admin" /><span>Admin</span></span></button>}
        <button aria-current={tab === 'settings' ? 'page' : undefined} onClick={() => setTab('settings')}><span className="nav-label"><Icon name="settings" /><span>Settings</span></span></button>
      </nav>
      <div className="pw-sidebar-bottom">
        <div className="pw-mode"><span>Project workspace</span><button onClick={onClassic}>Switch to Classic</button></div>
        <div className="pw-mode"><span>Appearance</span><ThemeToggle /></div>
        <button className="pw-profile" aria-current={tab === 'profile' ? 'page' : undefined} onClick={() => setTab('profile')}><Icon name="profile" /><span className="pw-profile-text"><strong>{profile.data?.displayName || 'Your workspace'}</strong><span>{profile.data?.organisation || 'Local library'}</span></span></button>
      </div>
    </aside>
    <main className="pw-main">
      {projectStore.error && <p role="alert" className="pw-error">{projectStore.error}</p>}
      <div hidden={tab !== 'collections'}><Collections key={publication} projects={projectStore.projects} save={projectStore.save} replaceMembers={projectStore.replaceMembers} loading={projectStore.loading} saving={projectStore.saving} projectId={projectId} setProjectId={setProjectId} onProcess={process} /></div>
      <div hidden={tab !== 'processing'}><Processing projects={projectStore.projects} save={projectStore.save} onPublished={id => { setProjectId(id); setPublication(v => v + 1); setTab('collections') }} /></div>
      {tab === 'admin' && isAdmin && <div className="pw-existing"><AdminScreen /></div>}
      {tab === 'settings' && <><header className="pw-heading"><div><span className="pw-eyebrow">Workspace</span><h1>Settings</h1><p>Make SKWAD work for you.</p></div></header>
        <div className="pw-tabs" aria-label="Settings sections">{sections.map(([id, label]) => <button key={id} aria-pressed={settingsTab === id} onClick={() => setSettingsTab(id)}>{label}</button>)}</div>
        <div className="pw-existing">{settingsTab === 'catalogues' ? <CataloguesScreen /> : <SettingsScreen />}</div>
      </>}
      {tab === 'profile' && <div className="pw-existing pw-profile-screen"><ProfileScreen /></div>}
    </main>
  </div>
}
