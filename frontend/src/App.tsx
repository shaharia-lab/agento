import { BrowserRouter, Routes, Route, Navigate } from 'react-router-dom'
import Layout from '@/components/Layout'
import AgentsPage from '@/pages/AgentsPage'
import AgentCreatePage from '@/pages/AgentCreatePage'
import AgentEditPage from '@/pages/AgentEditPage'
import ChatsPage from '@/pages/ChatsPage'
import ChatSessionPage from '@/pages/ChatSessionPage'

export default function App() {
  return (
    <BrowserRouter>
      <Routes>
        <Route path="/" element={<Layout />}>
          <Route index element={<Navigate to="/chats" replace />} />
          <Route path="chats" element={<ChatsPage />} />
          <Route path="chats/:id" element={<ChatSessionPage />} />
          <Route path="agents" element={<AgentsPage />} />
          <Route path="agents/new" element={<AgentCreatePage />} />
          <Route path="agents/:slug/edit" element={<AgentEditPage />} />
        </Route>
      </Routes>
    </BrowserRouter>
  )
}
