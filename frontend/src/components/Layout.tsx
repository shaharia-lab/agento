import { useState } from 'react'
import { Outlet } from 'react-router-dom'
import { Menu } from 'lucide-react'
import Sidebar from './Sidebar'

function AgentoLogo() {
  return (
    <svg width="24" height="24" viewBox="0 0 32 32" xmlns="http://www.w3.org/2000/svg">
      <rect width="32" height="32" rx="7" fill="#000" />
      <text
        x="16" y="23"
        fontFamily="-apple-system,BlinkMacSystemFont,'SF Pro Display',system-ui,sans-serif"
        fontSize="19" fontWeight="700" fill="#fff" textAnchor="middle"
      >A</text>
    </svg>
  )
}

export default function Layout() {
  const [mobileOpen, setMobileOpen] = useState(false)

  return (
    <div className="flex h-screen w-screen overflow-hidden bg-white">
      <Sidebar mobileOpen={mobileOpen} onMobileClose={() => setMobileOpen(false)} />

      <div className="flex flex-1 flex-col overflow-hidden">
        {/* Mobile top bar */}
        <header className="flex items-center gap-3 border-b border-zinc-200 px-4 h-14 shrink-0 md:hidden">
          <button
            onClick={() => setMobileOpen(true)}
            className="h-8 w-8 flex items-center justify-center rounded-md text-zinc-500 hover:bg-zinc-100 hover:text-zinc-900 transition-colors cursor-pointer"
          >
            <Menu className="h-5 w-5" />
          </button>
          <AgentoLogo />
          <span className="text-sm font-semibold text-zinc-900">Agento</span>
        </header>

        <main className="flex flex-1 flex-col overflow-hidden bg-white">
          <Outlet />
        </main>
      </div>
    </div>
  )
}
