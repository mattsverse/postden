import { createFileRoute } from '@tanstack/react-router'
import { useEffect, useState } from 'react'
import { allHomes } from 'content-collections'
import {
  ArrowRight,
  ArrowRightLeft,
  Check,
  FileDown,
  FolderInput,
  PenLine,
  Search,
  ShieldCheck,
} from 'lucide-react'
import { CopyCommand } from '#/components/copy-command'
import { Install, detectOs } from '#/components/install'
import { Screenshot } from '#/components/screenshot'
import { RELEASES_URL, REPO_URL } from '#/components/site-chrome'
import { SITE_URL, seo } from '#/site'

const [home] = allHomes
if (!home) throw new Error('Missing content/home.md')

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

/** The opening screen: the promise, the download, and the app itself, in the visitor's color scheme. */
function Hero() {
  return (
    <section className="relative isolate overflow-hidden">

      <div className="mx-auto max-w-6xl px-4 pt-16 pb-20 sm:px-6 sm:pt-24 sm:pb-28 lg:pt-28">
        <div className="mx-auto max-w-3xl text-center">
          <h1 className="font-serif text-[3.1rem] leading-[0.98] tracking-[-0.015em] text-balance text-ink-950 sm:text-7xl lg:text-[5.6rem] dark:text-white">
            {home.headline}
          </h1>
          <p className="mx-auto mt-7 max-w-2xl text-lg leading-relaxed text-pretty text-ink-600 sm:text-xl dark:text-ink-300">{home.intro}</p>

          <div className="mt-9 flex flex-wrap items-center justify-center gap-3">
            <a
              href="#install"
              className="group inline-flex min-h-12 items-center gap-2 rounded-full bg-accent px-6 font-medium text-on-accent transition-colors hover:bg-accent-ink"
            >
              <DownloadLabel />
              <ArrowRight className="size-4 transition-transform group-hover:translate-x-0.5" />
            </a>
            <a
              href={REPO_URL}
              className="inline-flex min-h-12 items-center rounded-full px-6 font-medium text-ink-800 ring-1 ring-ink-900/15 transition-colors ring-inset hover:bg-ink-900/[0.04] dark:text-ink-100 dark:ring-white/15 dark:hover:bg-white/5"
            >
              Read the source
            </a>
          </div>

          <div className="mx-auto mt-7 max-w-md text-left">
            <CopyCommand command={home.heroCommand} tone="light" />
            <p className="mt-2 text-center text-xs text-ink-500 dark:text-ink-400">{home.heroCommandNote}</p>
          </div>

          <ul className="mt-7 flex flex-wrap justify-center gap-x-6 gap-y-2 text-sm text-ink-600 dark:text-ink-300">
            {home.facts.map((fact) => (
              <li key={fact} className="flex items-center gap-1.5">
                <Check className="size-4 text-accent-fg" strokeWidth={2.5} />
                {fact}
              </li>
            ))}
          </ul>
        </div>

        <figure className="mx-auto mt-14 max-w-[1100px] overflow-hidden rounded-2xl shadow-[0_40px_100px_-30px_rgba(0,0,0,0.35)] ring-1 ring-ink-900/10 sm:mt-20 dark:shadow-[0_40px_100px_-30px_rgba(0,0,0,0.9)] dark:ring-white/10">
          <Screenshot {...home.heroScreenshot} priority />
        </figure>
      </div>
    </section>
  )
}

function SectionHeading({ eyebrow, title, intro, center }: { eyebrow: string; title: string; intro?: string; center?: boolean }) {
  return (
    <div className={`max-w-2xl ${center ? 'mx-auto text-center' : ''}`}>
      <p className="font-mono text-xs font-medium tracking-[0.12em] text-accent-fg uppercase">{eyebrow}</p>
      <h2 className="mt-4 font-serif text-[2.6rem] leading-[1.02] tracking-[-0.01em] text-balance sm:text-6xl">{title}</h2>
      {intro ? <p className="mt-5 text-lg leading-relaxed text-pretty text-ink-600 dark:text-ink-300">{intro}</p> : null}
    </div>
  )
}

/** "Download for macOS" once the visitor's OS is known; the plain label on the server and as a fallback. */
function DownloadLabel() {
  const [os, setOs] = useState<string>()
  useEffect(() => setOs(home.install.find((o) => o.os === detectOs())?.label), [])
  return os ? <>Download for {os}</> : <>Download Postden</>
}

const icons = { search: Search, sorting: FolderInput, compat: PenLine, safe: ShieldCheck, transfer: ArrowRightLeft, eml: FileDown }

/** Each feature beside a capture of it in the app, shown at the app's own size; sides alternate. */
function Features() {
  return (
    <section className="border-t border-ink-900/[0.06] dark:border-white/[0.06]">
      <div className="mx-auto max-w-6xl px-4 py-20 sm:px-6 sm:py-32">
        <SectionHeading eyebrow="Features" title={home.featuresTitle} intro={home.featuresIntro} />

        <ul className="mt-16 space-y-20 sm:mt-24 sm:space-y-28">
          {home.features.map((feature, i) => {
            const Icon = icons[feature.kind]
            return (
              <li
                key={feature.kind}
                className={`grid items-center gap-8 lg:gap-16 ${i % 2 ? 'lg:grid-cols-[610px_minmax(0,1fr)]' : 'lg:grid-cols-[minmax(0,1fr)_610px]'}`}
              >
                <div className={`max-w-md ${i % 2 ? 'lg:order-2' : ''}`}>
                  <Icon className="size-5 text-ink-500 dark:text-ink-400" strokeWidth={1.75} />
                  <h3 className="mt-4 text-2xl font-semibold tracking-tight">{feature.title}</h3>
                  <p className="mt-3 text-[17px] leading-relaxed text-pretty text-ink-600 dark:text-ink-300">{feature.body}</p>
                </div>
                <figure
                  className={`overflow-hidden rounded-2xl shadow-[0_24px_60px_-30px_rgba(0,0,0,0.3)] ring-1 ring-ink-900/10 dark:shadow-none dark:ring-white/10 ${i % 2 ? 'lg:order-1' : ''}`}
                >
                  <Screenshot {...feature.screenshot} />
                </figure>
              </li>
            )
          })}
        </ul>
      </div>
    </section>
  )
}

function Privacy() {
  const { title, intro, groups, website } = home.privacy
  return (
    <section id="privacy" className="scroll-mt-16 border-t border-ink-900/[0.06] dark:border-white/[0.06]">
      <div className="mx-auto max-w-6xl px-4 py-20 sm:px-6 sm:py-32">
        <SectionHeading eyebrow="Privacy" title={title} intro={intro} />

        <div className="mt-12 grid border-t border-ink-900/10 sm:mt-16 md:grid-cols-[1fr_1.4fr_1fr] dark:border-white/10">
          {groups.map((group) => (
            <div
              key={group.title}
              className="border-ink-900/10 py-8 md:px-8 md:first:pl-0 md:last:pr-0 [&+&]:border-t md:[&+&]:border-t-0 md:[&+&]:border-l dark:border-white/10"
            >
              <h3 className="font-serif text-3xl">{group.title}</h3>
              <ul className="mt-6 space-y-6">
                {group.items.map((item) => (
                  <li key={item.name}>
                    <p className="font-semibold tracking-tight">{item.name}</p>
                    <p className="mt-1 text-[15px] leading-relaxed text-pretty text-ink-600 dark:text-ink-300">{item.body}</p>
                  </li>
                ))}
              </ul>
            </div>
          ))}
        </div>

        <div className="flex flex-wrap items-center justify-between gap-x-8 gap-y-2 border-t border-ink-900/10 pt-6 text-sm text-ink-600 dark:border-white/10 dark:text-ink-300">
          <p>{website}</p>
          <a href="/privacy" className="inline-flex min-h-11 items-center gap-1.5 font-medium text-ink-900 hover:underline dark:text-white">
            Read the privacy policy <ArrowRight className="size-4" />
          </a>
        </div>
      </div>
    </section>
  )
}

function InstallSection() {
  return (
    <section id="install" className="scroll-mt-16 border-t border-ink-900/[0.06] dark:border-white/[0.06]">
      <div className="mx-auto grid max-w-6xl gap-10 px-4 py-20 sm:px-6 sm:py-32 lg:grid-cols-[1fr_1.25fr] lg:gap-20">
        <SectionHeading eyebrow="Download" title={home.installTitle} intro={home.installIntro} />
        <Install options={home.install} footer={home.html} />
      </div>
    </section>
  )
}
