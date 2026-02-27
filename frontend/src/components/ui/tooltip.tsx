import * as React from 'react'
import { cn } from '@/lib/utils'

interface TooltipProps {
  content: string
  children: React.ReactElement
  side?: 'right' | 'left' | 'top' | 'bottom'
}

export function Tooltip({ content, children, side = 'right' }: TooltipProps) {
  const [visible, setVisible] = React.useState(false)

  const sideClasses = {
    right: 'left-full ml-2 top-1/2 -translate-y-1/2',
    left: 'right-full mr-2 top-1/2 -translate-y-1/2',
    top: 'bottom-full mb-2 left-1/2 -translate-x-1/2',
    bottom: 'top-full mt-2 left-1/2 -translate-x-1/2',
  }

  return (
    <div
      className="relative inline-flex"
      onMouseEnter={() => setVisible(true)}
      onMouseLeave={() => setVisible(false)}
    >
      {children}
      {visible && (
        <div
          className={cn(
            'absolute z-50 whitespace-nowrap rounded-md bg-zinc-900 px-2.5 py-1 text-xs text-white shadow-md pointer-events-none',
            sideClasses[side],
          )}
        >
          {content}
        </div>
      )}
    </div>
  )
}
