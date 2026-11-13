import { Link } from '@tanstack/react-router'
import { ArrowRight } from 'lucide-react'

/** The "page not found" message, shown for any address that isn't a page (routes/$.tsx). */
export function NotFound() {
  return (
    <section className="mx-auto max-w-2xl px-4 py-24 text-center sm:px-6 sm:py-36">
      <p className="font-mono text-xs font-medium tracking-[0.08em] text-accent-fg uppercase">404</p>
      <h1 className="mt-3 text-4xl font-semibold tracking-[-0.03em] text-balance sm:text-5xl">This page doesn't exist.</h1>
      <p className="mx-auto mt-5 max-w-md text-lg leading-relaxed text-pretty text-ink-600 dark:text-ink-300">
        The link may be out of date, or the address may have a typo.
      </p>
      <Link
        to="/"
        className="group mt-8 inline-flex min-h-11 items-center gap-2 rounded-full bg-ink-900 px-5 text-sm font-medium text-white transition-colors hover:bg-ink-700 dark:bg-white dark:text-ink-900 dark:hover:bg-ink-100"
      >
        Go to the homepage
        <ArrowRight aria-hidden="true" className="size-4 transition-transform group-hover:translate-x-0.5" />
      </Link>
    </section>
  )
}
