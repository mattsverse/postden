import { createFileRoute } from '@tanstack/react-router'
import { useEffect, useState } from 'react'
import { allHomes } from 'content-collections'
import { ArrowRight, Check, Code2, Inbox, Lock, PenLine, ShieldCheck, Sparkles, X } from 'lucide-react'
import { CopyCommand } from '#/components/copy-command'
import { Install, detectOs } from '#/components/install'
import { Screenshot } from '#/components/screenshot'
import { RELEASES_URL, REPO_URL } from '#/components/site-chrome'
import { SITE_URL, seo } from '#/site'

const [home] = allHomes
if (!home) throw new Error('Missing content/home.md')

const icons = { inbox: Inbox, lock: Lock, shield: ShieldCheck, compose: PenLine, sparkles: Sparkles, code: Code2 }

/** Tells search engines this is a free desktop app, and names the site in results. */
const structuredData = {
  '@context': 'https://schema.org',
  '@graph': [
    { '@type': 'WebSite', name: 'Postden', url: `${SITE_URL}/` },
    {
      '@type': 'SoftwareApplication',
      name: 'Postden',
      description: home.description,
      url: `${SITE_URL}/`,
      applicationCategory: 'CommunicationApplication',
      operatingSystem: 'macOS 13 or later, Windows, Linux',
      offers: { '@type': 'Offer', price: '0', priceCurrency: 'USD' },
      license: 'https://opensource.org/licenses/MIT',
      downloadUrl: RELEASES_URL,
      screenshot: `${SITE_URL}/screenshots/inbox-light.webp`,
      sameAs: REPO_URL,
      author: { '@type': 'Person', name: 'Matteo Gassend' },
    },
  ],
}

export const Route = createFileRoute('/')({
  head: () => ({
    ...seo({ path: '/', title: home.title, description: home.description }),
    scripts: [{ type: 'application/ld+json', children: JSON.stringify(structuredData) }],
  }),
  component: Home,
})

function Home() {
  return (
    <>
      <Hero />
      <Features />
      <Privacy />
      <InstallSection />
    </>
  )
}

function Hero() {
  return (
    <section className="relative overflow-hidden">
      <div
        aria-hidden="true"
        className="pointer-events-none absolute inset-x-0 top-0 h-[640px] bg-[radial-gradient(60%_50%_at_75%_25%,rgba(91,123,213,0.13),transparent_70%)] dark:bg-[radial-gradient(60%_50%_at_75%_25%,rgba(91,123,213,0.2),transparent_70%)]"
      />
      <div className="relative mx-auto grid max-w-6xl items-center gap-12 px-4 pt-14 pb-20 sm:px-6 sm:pt-20 lg:grid-cols-[minmax(0,1fr)_minmax(0,1.1fr)] lg:gap-14 lg:pt-24 lg:pb-28">
        <div>
          <h1 className="text-[2.6rem] leading-[1.04] font-semibold tracking-[-0.035em] text-balance sm:text-6xl lg:text-[3rem] xl:text-[3.4rem]">
            {home.headline}
            <br />
            {home.headlineSecond} <span className="text-accent-fg sm:whitespace-nowrap">{home.headlineAccent}</span>
          </h1>
          <p className="mt-6 max-w-lg text-lg leading-relaxed text-pretty text-ink-600 dark:text-ink-300">{home.intro}</p>
          <div className="mt-8 flex flex-wrap items-center gap-3">
            <a
              href="#install"
              className="group inline-flex min-h-11 items-center gap-2 rounded-full bg-ink-900 px-5 text-sm font-medium text-white transition-colors hover:bg-ink-700 dark:bg-white dark:text-ink-900 dark:hover:bg-ink-100"
            >
              <DownloadLabel />
              <ArrowRight className="size-4 transition-transform group-hover:translate-x-0.5" />
            </a>
            <a
              href={REPO_URL}
              className="inline-flex min-h-11 items-center rounded-full px-5 text-sm font-medium text-ink-700 ring-1 ring-ink-900/15 transition-colors ring-inset hover:bg-ink-900/[0.04] dark:text-ink-100 dark:ring-white/15 dark:hover:bg-white/5"
            >
              View source
            </a>
          </div>
          <div className="mt-6 max-w-md">
            <CopyCommand command={home.heroCommand} tone="light" />
            <p className="mt-2 text-xs text-ink-500 dark:text-ink-400">{home.heroCommandNote}</p>
          </div>
          <ul className="mt-6 flex flex-wrap gap-x-5 gap-y-2 text-sm text-ink-600 dark:text-ink-300">
            {home.facts.map((fact) => (
              <li key={fact} className="flex items-center gap-1.5">
                <Check className="size-4 text-emerald-600 dark:text-emerald-400" strokeWidth={2.25} />
                {fact}
              </li>
            ))}
          </ul>
        </div>
        <figure className="overflow-hidden rounded-xl shadow-[0_30px_80px_-20px_rgba(15,21,34,0.35)] ring-1 ring-ink-900/10 dark:shadow-[0_30px_80px_-20px_rgba(0,0,0,0.8)] dark:ring-white/10">
          <Screenshot {...home.heroScreenshot} priority />
        </figure>
      </div>
    </section>
  )
}

function SectionHeading({ eyebrow, title, intro, invert }: { eyebrow: string; title: string; intro?: string; invert?: boolean }) {
  return (
    <div className="max-w-2xl">
      <p className={`font-mono text-xs font-medium tracking-[0.08em] uppercase ${invert ? 'text-accent-soft' : 'text-accent-fg'}`}>{eyebrow}</p>
      <h2 className="mt-3 text-3xl font-semibold tracking-[-0.025em] text-balance sm:text-4xl">{title}</h2>
      {intro ? (
        <p className={`mt-4 text-lg leading-relaxed text-pretty ${invert ? 'text-ink-300' : 'text-ink-600 dark:text-ink-300'}`}>{intro}</p>
      ) : null}
    </div>
  )
}

/** "Download for macOS" once the visitor's OS is known; the plain label on the server and as a fallback. */
function DownloadLabel() {
  const [os, setOs] = useState<string>()
  useEffect(() => setOs(home.install.find((o) => o.os === detectOs())?.label), [])
  return os ? <>Download for {os}</> : <>Download Postden</>
}

const card = 'rounded-2xl border border-ink-900/[0.08] bg-white dark:border-white/[0.09] dark:bg-ink-900'

function Features() {
  return (
    <section className="border-t border-ink-900/[0.06] dark:border-white/[0.06]">
      <div className="mx-auto max-w-6xl px-4 py-16 sm:px-6 sm:py-24">
        <SectionHeading eyebrow="Features" title={home.featuresTitle} />

        <ul className="mt-10 grid gap-4 sm:mt-12 sm:grid-cols-2 lg:grid-cols-4">
          {home.features.map((feature) => {
            const Icon = icons[feature.icon]
            return (
              <li key={feature.title} className={`${card} flex gap-4 p-5 sm:flex-col sm:gap-0 sm:p-6`}>
                <Icon className="mt-0.5 size-5 shrink-0 text-ink-700 sm:mt-0 dark:text-ink-200" strokeWidth={1.75} />
                <div>
                  <h3 className="font-semibold tracking-tight sm:mt-4">{feature.title}</h3>
                  <p className="mt-1.5 text-[15px] leading-relaxed text-ink-600 dark:text-ink-300">{feature.body}</p>
                </div>
              </li>
            )
          })}
        </ul>
      </div>
    </section>
  )
}

function Privacy() {
  const { title, intro, never, website, rules } = home.privacy
  return (
    <section id="privacy" className="scroll-mt-16 bg-ink-900 text-white dark:border-y dark:border-white/[0.08] dark:bg-ink-850">
      <div className="mx-auto grid max-w-6xl gap-12 px-4 py-16 sm:px-6 sm:py-24 lg:grid-cols-[1fr_1.15fr] lg:gap-20">
        <div>
          <SectionHeading eyebrow="Privacy" title={title} intro={intro} invert />
          <p className="mt-8 text-xs font-medium tracking-wide text-ink-400 uppercase">Never collected</p>
          <ul className="mt-3 space-y-2.5">
            {never.map((item) => (
              <li key={item} className="flex items-center gap-3">
                <span className="grid size-6 place-items-center rounded-full bg-white/[0.08]">
                  <X className="size-3.5 text-ink-200" strokeWidth={2.5} />
                </span>
                {item}
              </li>
            ))}
          </ul>
          <p className="mt-8 max-w-sm text-sm leading-relaxed text-ink-300">{website}</p>
          <a href="/privacy" className="mt-4 inline-flex min-h-11 items-center gap-1.5 text-sm font-medium text-white hover:underline">
            Read the privacy policy <ArrowRight className="size-4" />
          </a>
        </div>

        <div className="self-center">
          <div className="overflow-hidden rounded-2xl border border-white/10 bg-ink-950/50 font-mono text-[13px]">
            <p className="flex items-center justify-between border-b border-white/10 px-4 py-3 text-xs text-ink-400 sm:px-5">
              <span>Outgoing connections</span>
              <span>postden</span>
            </p>
            <ul>
              {rules.map((rule) => {
                const deny = rule.action === 'deny'
                return (
                  <li
                    key={rule.host}
                    className={`grid grid-cols-[3.5rem_minmax(0,1fr)] gap-x-3 px-4 py-3.5 sm:px-5 ${deny ? 'border-t border-white/10 bg-emerald-400/[0.06]' : ''}`}
                  >
                    <span className={deny ? 'font-semibold text-emerald-300' : 'text-ink-400'}>{rule.action}</span>
                    <div className="min-w-0">
                      <p className={`break-words ${deny ? 'font-semibold text-white' : 'text-ink-100'}`}>{rule.host}</p>
                      <p className="mt-1 font-sans text-sm text-ink-300">
                        {rule.purpose}
                        <span className="text-ink-500"> · </span>
                        <span className={deny ? 'text-emerald-300' : 'text-ink-400'}>{rule.when}</span>
                      </p>
                    </div>
                  </li>
                )
              })}
            </ul>
          </div>
        </div>
      </div>
    </section>
  )
}

function InstallSection() {
  return (
    <section id="install" className="scroll-mt-16">
      <div className="mx-auto grid max-w-6xl gap-10 px-4 py-16 sm:px-6 sm:py-24 lg:grid-cols-[1fr_1.25fr] lg:gap-20">
        <SectionHeading
          eyebrow="Download"
          title={home.installTitle}
          intro={home.installIntro}
        />
        <Install options={home.install} footer={home.html} />
      </div>
    </section>
  )
}
