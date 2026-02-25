import { Outlet } from 'react-router-dom'
import Sidebar from './Sidebar'

export default function Layout() {
  return (
    <div className="flex h-screen w-screen overflow-hidden bg-zinc-50">
      <Sidebar />
      <main className="flex flex-1 flex-col overflow-hidden bg-white">
        <Outlet />
      </main>
    </div>
  )
}
