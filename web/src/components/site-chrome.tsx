import { Link } from '@tanstack/react-router'
import { Logo } from './logo'

export const REPO_URL = 'https://github.com/mattsverse/postden'
export const RELEASES_URL = `${REPO_URL}/releases/latest`

const navLink = 'text-ink-500 transition-colors hover:text-ink-900 dark:text-ink-300 dark:hover:text-white'

export function SiteHeader() {
  return (
    <header className="sticky top-0 z-20 w-full border-b border-ink-900/[0.06] bg-paper/80 backdrop-blur-md dark:border-white/[0.06] dark:bg-ink-950/75">
      <div className="mx-auto flex h-16 max-w-6xl items-center justify-between px-4 sm:px-6">
        <Link to="/" className="flex min-h-11 items-center gap-2.5 text-[15px] font-semibold tracking-tight">
          <Logo className="size-7" />
          Postden
        </Link>
        <nav className="flex items-center gap-1 text-sm sm:gap-6">
          <Link to="/" hash="privacy" className={`hidden sm:block ${navLink}`}>
            Privacy
          </Link>
          <a href={REPO_URL} className={`hidden sm:block ${navLink}`}>
            GitHub
          </a>
          <Link
            to="/"
            hash="install"
            className="inline-flex min-h-11 items-center rounded-full bg-ink-900 px-4 font-medium text-white sm:min-h-9 transition-colors hover:bg-ink-700 dark:bg-white dark:text-ink-900 dark:hover:bg-ink-100"
          >
            Download
          </Link>
        </nav>
      </div>
    </header>
  )
}

export function SiteFooter() {
  return (
    <footer className="w-full border-t border-ink-900/[0.06] dark:border-white/[0.06]">
      <div className="mx-auto grid max-w-6xl gap-8 px-4 py-12 text-sm sm:grid-cols-[1fr_auto] sm:px-6">
        <div>
          <Link to="/" className="inline-flex items-center gap-2 font-semibold tracking-tight">
            <Logo className="size-6" />
            Postden
          </Link>
          <p className="mt-3 max-w-xs text-ink-500 dark:text-ink-400">
            Local, privacy-focused, no-frills email.
            <br />© {new Date().getFullYear()} Matteo Gassend · MIT License
          </p>
        </div>
        <nav className="flex gap-12">
          <ul className="space-y-2.5">
            <li className="font-medium text-ink-900 dark:text-white">Project</li>
            <li>
              <a href={RELEASES_URL} className={navLink}>
                Releases
              </a>
            </li>
            <li>
              <a href={REPO_URL} className={navLink}>
                Source code
              </a>
            </li>
          </ul>
          <ul className="space-y-2.5">
            <li className="font-medium text-ink-900 dark:text-white">Legal</li>
            <li>
              <Link to="/privacy" className={navLink}>
                Privacy Policy
              </Link>
            </li>
            <li>
              <Link to="/tos" className={navLink}>
                Terms of Service
              </Link>
            </li>
          </ul>
        </nav>
      </div>
    </footer>
  )
}
