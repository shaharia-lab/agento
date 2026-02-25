import { useState, useEffect } from 'react'
import { NavLink, useNavigate } from 'react-router-dom'
import { MessageSquare, Bot, Plus, PanelLeftClose, PanelLeftOpen, X } from 'lucide-react'
import { cn } from '@/lib/utils'
import { Tooltip } from '@/components/ui/tooltip'

const STORAGE_KEY = 'agento-sidebar-collapsed'

function AgentoLogo({ size = 28 }: { size?: number }) {
  return (
    <svg
      width={size}
      height={size}
      viewBox="0 0 32 32"
      xmlns="http://www.w3.org/2000/svg"
      className="shrink-0"
    >
      <rect width="32" height="32" rx="7" fill="#000" />
      <text
        x="16"
        y="23"
        fontFamily="-apple-system,BlinkMacSystemFont,'SF Pro Display',system-ui,sans-serif"
        fontSize="19"
        fontWeight="700"
        fill="#fff"
        textAnchor="middle"
      >
        A
      </text>
    </svg>
  )
}

interface SidebarProps {
  mobileOpen?: boolean
  onMobileClose?: () => void
}

export default function Sidebar({ mobileOpen = false, onMobileClose }: SidebarProps) {
  const navigate = useNavigate()
  const [collapsed, setCollapsed] = useState(() => {
    try {
      return localStorage.getItem(STORAGE_KEY) === 'true'
    } catch {
      return false
    }
  })

  useEffect(() => {
    try {
      localStorage.setItem(STORAGE_KEY, String(collapsed))
    } catch {
      // ignore
    }
  }, [collapsed])

  const navItems = [
    { to: '/chats', icon: MessageSquare, label: 'Chats' },
    { to: '/agents', icon: Bot, label: 'Agents' },
  ]

  const handleNavClick = () => {
    onMobileClose?.()
  }

  const sidebarContent = (isMobile: boolean) => (
    <>
      {/* Logo */}
      <div
        className={cn(
          'flex items-center border-b border-zinc-200 h-14 shrink-0',
          !isMobile && collapsed ? 'justify-center px-0' : 'gap-2.5 px-5',
        )}
      >
        <AgentoLogo size={28} />
        {(isMobile || !collapsed) && (
          <span className="text-sm font-semibold tracking-wide text-zinc-900">Agento</span>
        )}
        {isMobile && (
          <button
            onClick={onMobileClose}
            className="ml-auto h-8 w-8 flex items-center justify-center rounded-md text-zinc-400 hover:text-zinc-700 hover:bg-zinc-200 transition-colors cursor-pointer"
          >
            <X className="h-4 w-4" />
          </button>
        )}
      </div>

      {/* New Chat button */}
      <div className={cn('pt-3 shrink-0', !isMobile && collapsed ? 'px-2.5' : 'px-3')}>
        {!isMobile && collapsed ? (
          <Tooltip content="New Chat">
            <button
              onClick={() => navigate('/chats?new=1')}
              className="flex h-9 w-9 items-center justify-center rounded-md bg-zinc-200 text-zinc-700 hover:bg-zinc-300 hover:text-zinc-900 transition-colors mx-auto cursor-pointer"
            >
              <Plus className="h-4 w-4" />
            </button>
          </Tooltip>
        ) : (
          <button
            onClick={() => { navigate('/chats?new=1'); onMobileClose?.() }}
            className="flex w-full items-center gap-2 rounded-md border border-zinc-300 bg-white px-3 py-1.5 text-sm text-zinc-700 hover:bg-zinc-100 hover:text-zinc-900 transition-colors cursor-pointer"
          >
            <Plus className="h-3.5 w-3.5 shrink-0" />
            <span>New Chat</span>
          </button>
        )}
      </div>

      {/* Nav */}
      <nav className="flex-1 overflow-y-auto py-3 px-2">
        <div className="space-y-0.5">
          {navItems.map(({ to, icon: Icon, label }) =>
            !isMobile && collapsed ? (
              <Tooltip key={to} content={label}>
                <NavLink
                  to={to}
                  className={({ isActive }) =>
                    cn(
                      'flex h-9 w-9 items-center justify-center rounded-md transition-colors mx-auto',
                      isActive
                        ? 'bg-zinc-900 text-white'
                        : 'text-zinc-500 hover:bg-zinc-200 hover:text-zinc-900',
                    )
                  }
                >
                  <Icon className="h-4 w-4" />
                </NavLink>
              </Tooltip>
            ) : (
              <NavLink
                key={to}
                to={to}
                onClick={handleNavClick}
                className={({ isActive }) =>
                  cn(
                    'flex items-center gap-2.5 rounded-md px-3 py-2 text-sm transition-colors',
                    isActive
                      ? 'bg-zinc-900 text-white'
                      : 'text-zinc-600 hover:bg-zinc-200 hover:text-zinc-900',
                  )
                }
              >
                <Icon className="h-4 w-4 shrink-0" />
                <span>{label}</span>
              </NavLink>
            ),
          )}
        </div>
      </nav>

      {/* Collapse toggle — desktop only */}
      {!isMobile && (
        <div className={cn('border-t border-zinc-200 py-2 shrink-0', collapsed ? 'px-2.5' : 'px-2')}>
          <button
            onClick={() => setCollapsed(c => !c)}
            className={cn(
              'flex items-center rounded-md text-zinc-400 hover:text-zinc-700 hover:bg-zinc-200 transition-colors h-8 cursor-pointer',
              collapsed ? 'w-9 justify-center mx-auto' : 'w-full px-3 gap-2',
            )}
          >
            {collapsed ? (
              <PanelLeftOpen className="h-4 w-4" />
            ) : (
              <>
                <PanelLeftClose className="h-4 w-4 shrink-0" />
                <span className="text-xs">Collapse</span>
              </>
            )}
          </button>
        </div>
      )}
    </>
  )

  return (
    <>
      {/* Desktop sidebar */}
      <aside
        className={cn(
          'hidden md:flex h-full flex-col bg-zinc-50 text-zinc-900 border-r border-zinc-200 transition-[width] duration-200 ease-in-out shrink-0',
          collapsed ? 'w-[64px]' : 'w-[272px]',
        )}
      >
        {sidebarContent(false)}
      </aside>

      {/* Mobile sidebar overlay */}
      {mobileOpen && (
        <>
          {/* Backdrop */}
          <div
            className="fixed inset-0 z-40 bg-black/40 md:hidden"
            onClick={onMobileClose}
          />
          {/* Drawer */}
          <aside className="fixed inset-y-0 left-0 z-50 flex w-72 flex-col bg-zinc-50 text-zinc-900 border-r border-zinc-200 md:hidden">
            {sidebarContent(true)}
          </aside>
        </>
      )}
    </>
  )
}
