import React from 'react'
import ReactDOM from 'react-dom/client'
import { QueryClient, QueryClientProvider } from '@tanstack/react-query'
import App from './App'
import { restartEventBridge } from './eventBridge'
import { bootTransport, transportReady } from './transport'
import './styles.css'

const queryClient = new QueryClient({
  defaultOptions: {
    queries: {
      // The event bridge invalidates precisely; background refetching on focus
      // would only add noise.
      refetchOnWindowFocus: false,
      staleTime: 15_000,
      retry: 1,
    },
  },
})

// The transport is chosen before anything renders: the desktop IPC, a server
// over HTTP, or — for a client installation — the server layered over the
// IPC. A browser with no server chosen yet renders the connect screen.
void bootTransport()
  .catch((e) => console.error('transport boot failed', e))
  .then(() => {
    if (transportReady()) {
      restartEventBridge(queryClient).catch((e) => console.error('event bridge failed to start', e))
    }
    ReactDOM.createRoot(document.getElementById('root')!).render(
      <React.StrictMode>
        <QueryClientProvider client={queryClient}>
          <App />
        </QueryClientProvider>
      </React.StrictMode>,
    )
  })
