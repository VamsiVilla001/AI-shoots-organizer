import React from 'react'
import ReactDOM from 'react-dom/client'
import { QueryClient, QueryClientProvider } from '@tanstack/react-query'
import { Boot } from './boot'
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

ReactDOM.createRoot(document.getElementById('root')!).render(
  <React.StrictMode>
    <QueryClientProvider client={queryClient}>
      <Boot queryClient={queryClient} />
    </QueryClientProvider>
  </React.StrictMode>,
)
