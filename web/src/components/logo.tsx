// The app icon (assets/icon/postden.svg), cropped to its rounded square.
export function Logo({ className }: { className?: string }) {
  return (
    <svg viewBox="100 100 824 824" aria-hidden="true" className={className}>
      <defs>
        <linearGradient id="logo-bg" x1="0" y1="0" x2="0" y2="1">
          <stop offset="0" stopColor="#26324A" />
          <stop offset="1" stopColor="#0F1522" />
        </linearGradient>
      </defs>
      <rect x="100" y="100" width="824" height="824" rx="185" fill="url(#logo-bg)" />
      <rect x="262" y="332" width="500" height="360" rx="56" fill="none" stroke="#fff" strokeWidth="48" />
      <path
        d="M310 390 L512 540 L714 390"
        fill="none"
        stroke="#fff"
        strokeWidth="48"
        strokeLinecap="round"
        strokeLinejoin="round"
      />
    </svg>
  )
}
