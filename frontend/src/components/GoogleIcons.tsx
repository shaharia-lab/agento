// Google brand icon SVGs as React components.
// Paths are the canonical Google brand vectors used across Google's own products.

interface IconProps {
  className?: string
  size?: number
}

export function GoogleIcon({ className, size = 16 }: IconProps) {
  return (
    <svg
      viewBox="0 0 24 24"
      width={size}
      height={size}
      className={className}
      xmlns="http://www.w3.org/2000/svg"
    >
      <path
        d="M22.56 12.25c0-.78-.07-1.53-.2-2.25H12v4.26h5.92c-.26 1.37-1.04 2.53-2.21 3.31v2.77h3.57c2.08-1.92 3.28-4.74 3.28-8.09z"
        fill="#4285F4"
      />
      <path
        d="M12 23c2.97 0 5.46-.98 7.28-2.66l-3.57-2.77c-.98.66-2.23 1.06-3.71 1.06-2.86 0-5.29-1.93-6.16-4.53H2.18v2.84C3.99 20.53 7.7 23 12 23z"
        fill="#34A853"
      />
      <path
        d="M5.84 14.09c-.22-.66-.35-1.36-.35-2.09s.13-1.43.35-2.09V7.07H2.18C1.43 8.55 1 10.22 1 12s.43 3.45 1.18 4.93l3.66-2.84z"
        fill="#FBBC05"
      />
      <path
        d="M12 5.38c1.62 0 3.06.56 4.21 1.64l3.15-3.15C17.45 2.09 14.97 1 12 1 7.7 1 3.99 3.47 2.18 7.07l3.66 2.84c.87-2.6 3.3-4.53 6.16-4.53z"
        fill="#EA4335"
      />
    </svg>
  )
}

export function GmailIcon({ className, size = 16 }: IconProps) {
  return (
    <svg
      viewBox="0 0 24 24"
      width={size}
      height={size}
      className={className}
      xmlns="http://www.w3.org/2000/svg"
    >
      {/* envelope background */}
      <path d="M2 6a2 2 0 012-2h16a2 2 0 012 2v12a2 2 0 01-2 2H4a2 2 0 01-2-2V6z" fill="white" />
      {/* M-shape fold lines */}
      <path d="M2 6l10 7 10-7" stroke="#EA4335" strokeWidth="2" fill="none" />
      {/* envelope border */}
      <path
        d="M4 4h16a2 2 0 012 2v12a2 2 0 01-2 2H4a2 2 0 01-2-2V6a2 2 0 012-2z"
        fill="none"
        stroke="#EA4335"
        strokeWidth="1.5"
      />
      {/* M shape */}
      <path
        d="M2 6l10 7 10-7"
        fill="none"
        stroke="#EA4335"
        strokeWidth="2"
        strokeLinejoin="round"
      />
    </svg>
  )
}

export function GoogleCalendarIcon({ className, size = 16 }: IconProps) {
  return (
    <svg
      viewBox="0 0 24 24"
      width={size}
      height={size}
      className={className}
      xmlns="http://www.w3.org/2000/svg"
    >
      {/* main white background */}
      <rect x="3" y="4" width="18" height="17" rx="2" fill="white" />
      {/* top bar — blue */}
      <rect x="3" y="4" width="18" height="4.5" rx="2" fill="#1A73E8" />
      <rect x="3" y="6.5" width="18" height="2" fill="#1A73E8" />
      {/* border */}
      <rect
        x="3"
        y="4"
        width="18"
        height="17"
        rx="2"
        fill="none"
        stroke="#DADCE0"
        strokeWidth="1"
      />
      {/* top knobs */}
      <rect x="7.5" y="2.5" width="2" height="3" rx="1" fill="#1A73E8" />
      <rect x="14.5" y="2.5" width="2" height="3" rx="1" fill="#1A73E8" />
      {/* grid lines */}
      <line x1="3" y1="12" x2="21" y2="12" stroke="#DADCE0" strokeWidth="0.75" />
      <line x1="3" y1="16" x2="21" y2="16" stroke="#DADCE0" strokeWidth="0.75" />
      <line x1="9" y1="8.5" x2="9" y2="21" stroke="#DADCE0" strokeWidth="0.75" />
      <line x1="15" y1="8.5" x2="15" y2="21" stroke="#DADCE0" strokeWidth="0.75" />
      {/* "31" number in red */}
      <text
        x="12"
        y="19.5"
        textAnchor="middle"
        fontSize="5.5"
        fontWeight="bold"
        fill="#EA4335"
        fontFamily="sans-serif"
      >
        31
      </text>
    </svg>
  )
}

export function GoogleDriveIcon({ className, size = 16 }: IconProps) {
  return (
    <svg
      viewBox="0 0 24 24"
      width={size}
      height={size}
      className={className}
      xmlns="http://www.w3.org/2000/svg"
    >
      {/* left triangle — green */}
      <path d="M2 19.5L6 12l4 7.5H2z" fill="#34A853" />
      {/* right triangle — blue */}
      <path d="M22 19.5h-8l-4-7.5L14 5l8 14.5z" fill="#4285F4" />
      {/* top triangle — yellow */}
      <path d="M6 12L10 4.5h4L18 12H6z" fill="#FBBC05" />
      {/* thin outlines for polish */}
      <path d="M6 12L10 4.5h4L18 12H6z" fill="none" stroke="white" strokeWidth="0.5" />
      <path d="M2 19.5L6 12l4 7.5H2z" fill="none" stroke="white" strokeWidth="0.5" />
      <path d="M22 19.5h-8l-4-7.5L14 5l8 14.5z" fill="none" stroke="white" strokeWidth="0.5" />
    </svg>
  )
}
