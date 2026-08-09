import { useEffect } from 'react'
import { useQuery } from '@tanstack/react-query'
import * as api from './api'
import { setMediaBase } from './media'
import { useUi } from './store'
import { Sidebar } from './components/Sidebar'
import { Notices } from './components/Notices'
import { MediaViewer } from './components/MediaViewer'
import { ShootsScreen } from './screens/ShootsScreen'
import { PlayersScreen } from './screens/PlayersScreen'
import { AlbumsScreen } from './screens/AlbumsScreen'
import { ReviewScreen } from './screens/ReviewScreen'
import { ExportScreen } from './screens/ExportScreen'
import { SettingsScreen } from './screens/SettingsScreen'

export default function App() {
  const screen = useUi((s) => s.screen)
  const viewerMediaId = useUi((s) => s.viewerMediaId)

  const info = useQuery({ queryKey: ['appInfo'], queryFn: api.appInfo, staleTime: Infinity })

  useEffect(() => {
    if (info.data) setMediaBase(info.data.mediaUrlBase)
  }, [info.data])

  return (
    <div className="shell">
      <Sidebar />
      <main className="workspace">
        {screen === 'shoots' && <ShootsScreen />}
        {screen === 'players' && <PlayersScreen />}
        {screen === 'albums' && <AlbumsScreen />}
        {screen === 'review' && <ReviewScreen />}
        {screen === 'export' && <ExportScreen />}
        {screen === 'settings' && <SettingsScreen />}
      </main>
      {viewerMediaId !== null && <MediaViewer mediaId={viewerMediaId} />}
      <Notices />
    </div>
  )
}
