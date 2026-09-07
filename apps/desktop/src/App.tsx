import { useEffect } from 'react'
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
import { CataloguesScreen } from './screens/CataloguesScreen'
import { AuthScreen } from './screens/AuthScreen'
import { ProfileScreen } from './screens/ProfileScreen'

export default function App() {
  const screen = useUi((s) => s.screen)
  const viewerMediaId = useUi((s) => s.viewerMediaId)

  const session = useQuery({ queryKey: ['catalogueSession'], queryFn: api.catalogueSessionStatus })
  const info = useQuery({
    queryKey: ['appInfo'],
    queryFn: api.appInfo,
    staleTime: Infinity,
    enabled: session.data?.authenticatedOnce === true,
  })

  useEffect(() => {
    if (info.data) setMediaBase(info.data.mediaUrlBase)
  }, [info.data])

  if (session.isPending) {
    return <div className="auth-shell"><div className="auth-loading">Loading SKWAD…</div></div>
  }

  if (!session.data?.authenticatedOnce) {
    return <><AuthScreen /><Notices /></>
  }

  return (
    <div className="shell">
      <Sidebar />
      <main className="workspace">
        {screen === 'shoots' && <ShootsScreen />}
        {screen === 'groups' && <GroupsScreen />}
        {screen === 'players' && <PlayersScreen />}
        {screen === 'albums' && <AlbumsScreen />}
        {screen === 'review' && <ReviewScreen />}
        {screen === 'export' && <ExportScreen />}
        {screen === 'catalogues' && <CataloguesScreen />}
        {screen === 'profile' && <ProfileScreen />}
        {screen === 'settings' && <SettingsScreen />}
      </main>
      {viewerMediaId !== null && <MediaViewer mediaId={viewerMediaId} />}
      <Notices />
    </div>
  )
}
