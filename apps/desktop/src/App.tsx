import { useEffect, useState } from 'react'
import { useQuery } from '@tanstack/react-query'
import * as api from './api'
import { setMediaBase } from './media'
import { useUi } from './store'
import { Sidebar } from './components/Sidebar'
import { Notices } from './components/Notices'
import { MediaViewer } from './components/MediaViewer'
import { ShootsScreen } from './screens/ShootsScreen'
import { GroupsScreen } from './screens/GroupsScreen'
import { PlayersScreen } from './screens/PlayersScreen'
import { AlbumsScreen } from './screens/AlbumsScreen'
import { ReviewScreen } from './screens/ReviewScreen'
import { ExportScreen } from './screens/ExportScreen'
import { SettingsScreen } from './screens/SettingsScreen'
import { DatabaseSetupScreen } from './screens/DatabaseSetupScreen'
import { CataloguesScreen } from './screens/CataloguesScreen'
import { AuthScreen } from './screens/AuthScreen'
import { ProfileScreen } from './screens/ProfileScreen'
import { AdminScreen } from './screens/AdminScreen'
import { ProjectWorkspace } from './projectWorkspace/ProjectWorkspace'
import { ExportProgressCard } from './projectWorkspace/exportProgressCard'

export default function App() {
  const [experience, setExperience] = useState<'classic' | 'projects'>(() => {
    try { return localStorage.getItem('skwad.experience') === 'classic' ? 'classic' : 'projects' } catch { return 'projects' }
  })
  const switchExperience = (next: 'classic' | 'projects') => {
    setExperience(next)
    try { localStorage.setItem('skwad.experience', next) } catch { /* Navigation still works without browser storage. */ }
  }
  const screen = useUi((s) => s.screen)
  const theme = useUi((s) => s.theme)
  const viewerMediaId = useUi((s) => s.viewerMediaId)
  const viewerPreferVideoFaces = useUi((s) => s.viewerPreferVideoFaces)

  useEffect(() => {
    document.documentElement.dataset.theme = theme
  }, [theme])

  // Asked before anything else. When the library database could not be opened
  // there is no application state, so every other command fails — including the
  // session check below, which would otherwise land on the sign-in screen and
  // give no clue that the real problem is the database.
  const startup = useQuery({
    queryKey: ['startupStatus'],
    queryFn: api.startupStatus,
    staleTime: Infinity,
    retry: false,
  })
  const ready = startup.data?.kind === 'ready'

  const session = useQuery({
    queryKey: ['catalogueSession'],
    queryFn: api.catalogueSessionStatus,
    enabled: ready,
  })
  const info = useQuery({
    queryKey: ['appInfo'],
    queryFn: api.appInfo,
    staleTime: Infinity,
    enabled: ready && session.data?.authenticatedOnce === true,
  })

  useEffect(() => {
    if (info.data) setMediaBase(info.data.mediaUrlBase)
  }, [info.data])

  if (startup.isPending) {
    return <div className="auth-shell"><div className="auth-loading">Loading SKWAD…</div></div>
  }

  if (startup.data?.kind === 'needsDatabase') {
    return (
      <>
        <DatabaseSetupScreen
          initial={startup.data.settings}
          problem={{ title: startup.data.title, detail: startup.data.detail }}
        />
        <Notices />
      </>
    )
  }

  if (session.isPending) {
    return <div className="auth-shell"><div className="auth-loading">Loading SKWAD…</div></div>
  }

  if (!session.data?.authenticatedOnce) {
    return <><AuthScreen /><Notices /></>
  }

  if (experience === 'projects') {
    const accountId = session.data.accountId ?? session.data.email ?? 'local'
    return <><ProjectWorkspace key={accountId} accountId={accountId} onClassic={() => switchExperience('classic')} />
      {viewerMediaId !== null && <MediaViewer mediaId={viewerMediaId} preferVideoFaces={viewerPreferVideoFaces} />}<ExportProgressCard /><Notices /></>
  }

  return (
    <div className="shell">
      <Sidebar />
      <main className="workspace">
        <div className="pw-classic-switch"><span>Classic workspace</span><button onClick={() => switchExperience('projects')}>Try Project workspace</button></div>
        {screen === 'shoots' && <ShootsScreen />}
        {screen === 'groups' && <GroupsScreen />}
        {screen === 'players' && <PlayersScreen />}
        {screen === 'albums' && <AlbumsScreen />}
        {screen === 'review' && <ReviewScreen />}
        {screen === 'export' && <ExportScreen />}
        {screen === 'catalogues' && <CataloguesScreen />}
        {screen === 'profile' && <ProfileScreen />}
        {screen === 'admin' && <AdminScreen />}
        {screen === 'settings' && <SettingsScreen />}
      </main>
      {viewerMediaId !== null && <MediaViewer mediaId={viewerMediaId} preferVideoFaces={viewerPreferVideoFaces} />}
      <ExportProgressCard />
      <Notices />
    </div>
  )
}
