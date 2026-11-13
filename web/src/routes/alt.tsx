import { createFileRoute } from '@tanstack/react-router'
import { useEffect, useState } from 'react'
import { allHomes } from 'content-collections'
import { ArrowRight, Check, X } from 'lucide-react'
import { Install, detectOs } from '#/components/install'
import { Screenshot } from '#/components/screenshot'
import { REPO_URL } from '#/components/site-chrome'

// An alternate homepage design, for comparison with `/`. Same content (content/home.md),
// different layout: one centered column, a serif display face, and ruled lists instead of cards.
// Not linked from anywhere and kept out of search results and the sitemap.

const [home] = allHomes
if (!home) throw new Error('Missing content/home.md')

export const Route = createFileRoute('/alt')({
  head: () => ({
    meta: [{ title: `${home.title} (alternate design)` }, { name: 'robots', content: 'noindex' }],
  }),
  component: Alt,
})

function Alt() {
  return (
    <>
      <Hero />
      <Features />
      <Privacy />
      <InstallSection />
    </>
  )
}

const rule = 'border-ink-900/10 dark:border-white/10'
const muted = 'text-ink-600 dark:text-ink-300'
const display = 'font-serif leading-[1.02] tracking-[-0.015em] text-balance'
const textLink =
  'font-medium underline decoration-ink-900/25 underline-offset-4 hover:decoration-ink-900 dark:decoration-white/30 dark:hover:decoration-white'

/** "Install on macOS" once the visitor's OS is known; the plain label on the server and as a fallback. */
function InstallLabel() {
  const [os, setOs] = useState<string>()
  useEffect(() => setOs(home.install.find((o) => o.os === detectOs())?.label), [])
  return os ? <>Install on {os}</> : <>Install Postden</>
}

function Hero() {
  return (
    <section aria-labelledby="hero-title" className="relative overflow-hidden">
      <div className="mx-auto max-w-5xl px-4 pt-10 text-center sm:px-6 sm:pt-24">
        <h1 id="hero-title" className={`${display} text-[clamp(2.25rem,12.5vw,3.25rem)] sm:text-7xl lg:text-[5.5rem]`}>
          {/* Kept together, so a narrow screen breaks after "Your email," and not mid-phrase. */}
          {home.headline} <span className="inline-block">{home.headlineSecond}</span>
          <br />
          <em className="text-accent-fg">{home.headlineAccent}</em>
        </h1>
        <p className={`mx-auto mt-5 max-w-xl text-lg leading-relaxed text-pretty sm:mt-7 ${muted}`}>{home.intro}</p>

        <div className="mt-7 flex flex-wrap items-center justify-center gap-x-3 gap-y-1 sm:mt-9">
          {/* Wide enough for the longest label, so the row doesn't shift when the OS is filled in. Phones have
              no build of their own and keep the plain label, so they don't need the room. */}
          <a
            href="#install"
            className="group inline-flex min-h-12 items-center sm:min-w-56 justify-center gap-2 rounded-full bg-ink-900 px-6 font-medium text-white transition-colors hover:bg-ink-700 dark:bg-white dark:text-ink-900 dark:hover:bg-ink-100"
          >
            <InstallLabel />
            <ArrowRight aria-hidden="true" className="size-4 transition-transform group-hover:translate-x-0.5" />
          </a>
          <a href={REPO_URL} className={`inline-flex min-h-12 items-center px-2 text-ink-700 sm:px-4 dark:text-ink-100 ${textLink}`}>
            View source
          </a>
        </div>

        <ul className={`mt-5 flex flex-wrap justify-center gap-x-5 gap-y-1.5 text-sm ${muted}`}>
          {home.facts.map((fact) => (
            <li key={fact} className="flex items-center gap-1.5">
              <Check aria-hidden="true" className="size-4 text-emerald-600 dark:text-emerald-400" strokeWidth={2.25} />
              {fact}
            </li>
          ))}
        </ul>
      </div>

      {/* The screenshot sits on the next section's rule, so the page reads as one sheet. */}
      <div className="relative mx-auto mt-10 max-w-[calc(847px+2rem)] px-4 sm:mt-20">
        <div
          aria-hidden="true"
          className="pointer-events-none absolute inset-x-[-20%] top-[10%] bottom-0 bg-[radial-gradient(50%_60%_at_50%_45%,rgba(91,123,213,0.2),transparent_70%)] dark:bg-[radial-gradient(50%_60%_at_50%_45%,rgba(91,123,213,0.24),transparent_70%)]"
        />
        <figure className="relative overflow-hidden rounded-t-xl shadow-[0_-10px_80px_-20px_rgba(15,21,34,0.3)] ring-1 ring-ink-900/10 dark:shadow-[0_-10px_80px_-20px_rgba(0,0,0,0.8)] dark:ring-white/15">
          {/* On phones the whole window would be too small to read, so show the message list at a larger size. */}
          {/* 145% puts the crop on the message list's left edge; from 600px the whole window fits. */}
          <div className="flex justify-end min-[600px]:block">
            <div className="w-[145%] shrink-0 min-[600px]:w-full">
              <Screenshot {...home.heroScreenshot} priority />
            </div>
          </div>
          {/* The window's last row is cut off by its own edge; fade it into the section below. */}
          <div
            aria-hidden="true"
            className="pointer-events-none absolute inset-x-0 bottom-0 h-10 bg-linear-to-b from-transparent to-white sm:h-16 dark:to-ink-900"
          />
        </figure>
      </div>
    </section>
  )
}

function Features() {
  return (
    <section aria-labelledby="features-title" className={`relative border-t bg-white dark:bg-ink-900 ${rule}`}>
      <div className="mx-auto max-w-[calc(847px+3rem)] px-4 py-20 sm:px-6 sm:py-28">
        <h2 id="features-title" className={`${display} mx-auto max-w-3xl text-center text-4xl sm:text-6xl`}>
          {home.featuresTitle}
        </h2>
        <ol className="mt-12 grid sm:mt-16 sm:grid-cols-2 sm:gap-x-14">
          {home.features.map((feature, i) => (
            <li
              key={feature.title}
              className={`grid grid-cols-[2.75rem_minmax(0,1fr)] border-t py-7 last:border-b sm:[&:nth-last-child(-n+2)]:border-b ${rule}`}
            >
              <span aria-hidden="true" className="pt-1 font-mono text-xs text-ink-500 dark:text-ink-400">
                {String(i + 1).padStart(2, '0')}
              </span>
              <div>
                <h3 className="text-lg font-semibold tracking-tight">{feature.title}</h3>
                <p className={`mt-1.5 leading-relaxed text-pretty ${muted}`}>{feature.body}</p>
              </div>
            </li>
          ))}
        </ol>
      </div>
    </section>
  )
}

function Privacy() {
  const { title, intro, never, website, rules } = home.privacy
  return (
    <section id="privacy" aria-labelledby="privacy-title" className={`scroll-mt-16 border-t ${rule}`}>
      <div className="mx-auto max-w-3xl px-4 py-20 text-center sm:px-6 sm:py-28">
        <h2 id="privacy-title" className={`${display} text-4xl sm:text-6xl`}>
          {title}
        </h2>
        <p className={`mx-auto mt-5 max-w-xl text-lg leading-relaxed text-balance ${muted}`}>{intro}</p>

        <div className="mx-auto mt-12 max-w-2xl overflow-hidden rounded-2xl bg-ink-900 text-left font-mono text-[13px] text-white shadow-xl shadow-ink-900/15 ring-1 ring-white/10 dark:bg-ink-850 dark:shadow-black/40 dark:ring-white/15">
          <p className="flex items-center justify-between border-b border-white/10 px-5 py-3 text-xs text-ink-300">
            <span>Outgoing connections</span>
            <span>postden</span>
          </p>
          <ul>
            {rules.map((r) => {
              const deny = r.action === 'deny'
              return (
                <li
                  key={r.host}
                  className={`grid grid-cols-[3.5rem_minmax(0,1fr)] gap-x-3 px-5 py-3.5 ${deny ? 'border-t border-white/10 bg-emerald-400/[0.07]' : ''}`}
                >
                  <span className={deny ? 'font-semibold text-emerald-300' : 'text-ink-300'}>{r.action}</span>
                  <div className="min-w-0">
                    <p className={`break-words ${deny ? 'font-semibold' : 'text-ink-100'}`}>{r.host}</p>
                    <p className="mt-1 font-sans text-sm text-pretty text-ink-300 dark:text-ink-200">
                      {r.purpose}{' '}
                      <span aria-hidden="true" className="hidden text-ink-500 sm:inline">
                        {'· '}
                      </span>
                      <span className="sr-only">; </span>
                      {/* On phones it gets its own line, set like the rule's other fields. */}
                      <span
                        className={`mt-1 block font-mono text-xs sm:mt-0 sm:inline sm:font-sans sm:text-sm ${deny ? 'text-emerald-300' : 'text-ink-400 dark:text-ink-300'}`}
                      >
                        {r.when}
                      </span>
                    </p>
                  </div>
                </li>
              )
            })}
          </ul>
        </div>

        <p id="never-collected" className={`mt-10 text-sm ${muted}`}>
          Never collected
        </p>
        <ul aria-labelledby="never-collected" className="mt-3 flex flex-wrap justify-center gap-x-6 gap-y-2 text-lg font-medium">
          {never.map((item) => (
            <li key={item} className="flex items-center gap-2">
              <span aria-hidden="true" className="grid size-5 place-items-center rounded-full bg-ink-900/[0.07] dark:bg-white/10">
                <X className="size-3" strokeWidth={2.5} />
              </span>
              {item}
            </li>
          ))}
        </ul>

        <p className={`mx-auto mt-8 max-w-md text-sm leading-relaxed text-balance ${muted}`}>{website}</p>
        <a href="/privacy" className={`mt-2 inline-flex min-h-11 items-center gap-1.5 text-sm ${textLink}`}>
          Read the privacy policy <ArrowRight aria-hidden="true" className="size-4" />
        </a>
      </div>
    </section>
  )
}

function InstallSection() {
  return (
    <section id="install" aria-labelledby="install-title" className={`scroll-mt-16 border-t bg-white dark:bg-ink-900 ${rule}`}>
      <div className="mx-auto max-w-3xl px-4 py-20 sm:px-6 sm:py-28">
        <div className="text-center">
          <h2 id="install-title" className={`${display} text-4xl sm:text-6xl`}>
            {home.installTitle}
          </h2>
          <p className={`mx-auto mt-5 max-w-lg leading-relaxed text-balance ${muted}`}>{home.installIntro}</p>
        </div>
        {/* The same width as the connections panel above. */}
        <div className="mx-auto mt-10 max-w-2xl">
          <Install options={home.install} footer={home.html} />
        </div>
      </div>
    </section>
  )
}
