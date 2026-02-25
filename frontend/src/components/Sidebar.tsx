import { NavLink, useNavigate } from 'react-router-dom'
import { MessageSquare, Bot, Plus } from 'lucide-react'
import { cn } from '@/lib/utils'
import { Button } from '@/components/ui/button'

export default function Sidebar() {
  const navigate = useNavigate()

  const navItems = [
    { to: '/chats', icon: MessageSquare, label: 'Chats' },
    { to: '/agents', icon: Bot, label: 'Agents' },
  ]

  return (
    <aside className="flex h-full w-60 flex-col bg-gray-900 text-gray-100 border-r border-gray-800">
      {/* Logo */}
      <div className="flex items-center gap-2 px-4 py-4 border-b border-gray-800">
        <div className="flex h-7 w-7 items-center justify-center rounded-md bg-indigo-500 text-white text-xs font-bold">
          A
        </div>
        <span className="text-sm font-semibold tracking-wide">Agento</span>
      </div>

      {/* New Chat shortcut */}
      <div className="px-3 pt-3">
        <Button
          size="sm"
          variant="outline"
          className="w-full justify-start gap-2 border-gray-700 bg-gray-800 text-gray-200 hover:bg-gray-700 hover:text-white"
          onClick={() => navigate('/chats?new=1')}
        >
          <Plus className="h-3.5 w-3.5" />
          New Chat
        </Button>
      </div>

      {/* Nav */}
      <nav className="flex-1 overflow-y-auto px-2 py-3">
        <div className="space-y-0.5">
          {navItems.map(({ to, icon: Icon, label }) => (
            <NavLink
              key={to}
              to={to}
              className={({ isActive }) =>
                cn(
                  'flex items-center gap-2.5 rounded-md px-3 py-2 text-sm transition-colors',
                  isActive
                    ? 'bg-gray-700 text-white'
                    : 'text-gray-400 hover:bg-gray-800 hover:text-gray-100',
                )
              }
            >
              <Icon className="h-4 w-4 shrink-0" />
              {label}
            </NavLink>
          ))}
        </div>
      </nav>

      {/* Footer */}
      <div className="px-4 py-3 border-t border-gray-800">
        <p className="text-xs text-gray-600">Personal AI Platform</p>
      </div>
    </aside>
  )
}
