import { allPages } from 'content-collections'
import { ChevronDown, History } from 'lucide-react'
import { useEffect, useState } from 'react'
import { seo } from '#/site'
import { REPO_URL } from './site-chrome'

type Page = (typeof allPages)[number]

/** Looks up a page from content/pages by file name, failing the build if it's missing. */
export function getPage(slug: string): Page {
  const page = allPages.find((p) => p.slug === slug)
  if (!page) throw new Error(`Missing content/pages/${slug}.md`)
  return page
}

export function pageHead(page: Page) {
  return seo({ path: `/${page.slug}`, title: `${page.title} · Postden`, description: page.description })
}

const dateFormat = new Intl.DateTimeFormat('en', { dateStyle: 'long', timeZone: 'UTC' })

/** The id of the last heading scrolled past the top of the viewport. */
function useActiveHeading(ids: string[]) {
  const [active, setActive] = useState(ids[0])
  useEffect(() => {
    const update = () => {
      let current = ids[0]
      for (const id of ids) {
        const el = document.getElementById(id)
        if (el && el.getBoundingClientRect().top < 140) current = id
      }
      setActive(current)
    }
    update()
    window.addEventListener('scroll', update, { passive: true })
    return () => window.removeEventListener('scroll', update)
  }, [ids])
  return active
}

export function ContentPage({ page }: { page: Page }) {
  const history = `${REPO_URL}/commits/main/web/content/pages/${page._meta.filePath}`
  const others = allPages.filter((p) => p.slug !== page.slug)
  const [ids] = useState(() => page.headings.map((h) => h.id))
  const active = useActiveHeading(ids)

  return (
    <div className="mx-auto max-w-6xl px-4 sm:px-6">
      <header className="border-b border-ink-900/[0.07] pt-12 pb-8 sm:pt-20 sm:pb-10 dark:border-white/[0.07]">
        <p className="font-mono text-xs font-medium tracking-[0.08em] text-accent-fg uppercase">Legal</p>
        <h1 className="mt-3 font-display text-4xl font-medium tracking-[-0.035em] sm:text-5xl">{page.title}</h1>
        <p className="mt-4 max-w-2xl text-lg text-pretty text-ink-600 dark:text-ink-300">{page.description}</p>
        <div className="mt-6 flex flex-wrap items-center gap-x-5 gap-y-2 text-sm text-ink-500 dark:text-ink-400">
          <span>
            Last updated <time dateTime={page.updated}>{dateFormat.format(new Date(page.updated))}</time>
          </span>
          <a href={history} className="inline-flex min-h-11 items-center gap-1.5 hover:text-ink-900 sm:min-h-0 dark:hover:text-white">
            <History className="size-4" /> Version history
          </a>
        </div>
      </header>

      {/* Below lg the sidebar collapses into this menu. */}
      <details className="group mt-6 rounded-xl border border-ink-900/10 bg-white lg:hidden dark:border-white/10 dark:bg-white/[0.03]">
        <summary className="flex min-h-12 cursor-pointer list-none items-center justify-between px-4 text-sm font-medium [&::-webkit-details-marker]:hidden">
          On this page
          <ChevronDown className="size-4 transition-transform group-open:rotate-180" />
        </summary>
        <ul className="border-t border-ink-900/[0.07] px-2 py-2 text-sm dark:border-white/[0.07]">
          {page.headings.map((h) => (
            <li key={h.id}>
              <a href={`#${h.id}`} className="flex min-h-11 items-center rounded-lg px-2 text-ink-600 hover:bg-ink-900/[0.04] dark:text-ink-300 dark:hover:bg-white/5">
                {h.title}
              </a>
            </li>
          ))}
          {others.map((p) => (
            <li key={p.slug} className="mt-1 border-t border-ink-900/[0.07] pt-1 dark:border-white/[0.07]">
              <a href={`/${p.slug}`} className="flex min-h-11 items-center rounded-lg px-2 font-medium hover:bg-ink-900/[0.04] dark:hover:bg-white/5">
                {p.title} →
              </a>
            </li>
          ))}
        </ul>
      </details>

      <div className="grid gap-12 py-10 sm:py-14 lg:grid-cols-[220px_minmax(0,1fr)] lg:gap-16">
        <aside className="hidden lg:block">
          <nav aria-label="On this page" className="sticky top-28 text-sm">
            <p className="font-medium">On this page</p>
            <ul className="mt-3 space-y-2 border-l border-ink-900/10 dark:border-white/10">
              {page.headings.map((h) => (
                <li key={h.id}>
                  <a
                    href={`#${h.id}`}
                    aria-current={active === h.id ? 'location' : undefined}
                    className="-ml-px block border-l border-transparent py-0.5 pl-4 text-ink-500 hover:text-ink-900 aria-[current]:border-ink-900 aria-[current]:font-medium aria-[current]:text-ink-900 dark:text-ink-400 dark:hover:text-white dark:aria-[current]:border-white dark:aria-[current]:text-white"
                  >
                    {h.title}
                  </a>
                </li>
              ))}
            </ul>
            {others.map((p) => (
              <a
                key={p.slug}
                href={`/${p.slug}`}
                className="mt-8 block text-ink-500 hover:text-ink-900 dark:text-ink-400 dark:hover:text-white"
              >
                {p.title} →
              </a>
            ))}
          </nav>
        </aside>

        {/* Our own Markdown, compiled at build time without raw HTML. */}
        <article
          className="prose max-w-[68ch] dark:prose-invert prose-headings:scroll-mt-24 prose-headings:font-semibold prose-headings:tracking-tight prose-h2:mt-12 prose-h2:text-2xl prose-h3:mt-8 prose-h3:mb-2 prose-h3:text-lg prose-a:font-medium prose-a:underline-offset-2 prose-code:rounded prose-code:bg-ink-900/[0.06] prose-code:px-1 prose-code:py-0.5 prose-code:font-normal prose-code:before:content-none prose-code:after:content-none dark:prose-code:bg-white/10 [&>p:first-child]:text-lg"
          dangerouslySetInnerHTML={{ __html: page.html }}
        />
      </div>
    </div>
  )
}
