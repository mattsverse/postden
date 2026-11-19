import { ArrowUpRight } from 'lucide-react'
import { useEffect, useId, useRef, useState, type KeyboardEvent } from 'react'
import { CopyCommand } from './copy-command'

type Os = 'mac' | 'windows' | 'linux'
type Method = {
  note: string
  command?: string
  download?: { label: string; href: string }
  steps?: boolean
}
type Option = Method & { label: string; os: Os; variants?: (Method & { label: string })[] }

export function detectOs(): Os | undefined {
  const ua = navigator.userAgent
  if (/Windows/i.test(ua)) return 'windows'
  if (/Mac OS X|Macintosh/i.test(ua) && !/iPhone|iPad/i.test(ua)) return 'mac'
  if (/Linux|X11/i.test(ua) && !/Android/i.test(ua)) return 'linux'
}

/** Tabs with roving focus and arrow-key navigation, per the WAI-ARIA tabs pattern. */
function useTabs(count: number) {
  const [selected, setSelected] = useState(0)
  const refs = useRef<(HTMLButtonElement | null)[]>([])
  function onKeyDown(event: KeyboardEvent) {
    const last = count - 1
    const next =
      event.key === 'ArrowRight' ? (selected === last ? 0 : selected + 1)
      : event.key === 'ArrowLeft' ? (selected === 0 ? last : selected - 1)
      : event.key === 'Home' ? 0
      : event.key === 'End' ? last
      : undefined
    if (next === undefined) return
    event.preventDefault()
    setSelected(next)
    refs.current[next]?.focus()
  }
  return { selected, setSelected, refs, onKeyDown }
}

function DownloadLink({ download, primary }: { download: NonNullable<Method['download']>; primary: boolean }) {
  return (
    <a
      href={download.href}
      className={`inline-flex min-h-11 items-center gap-1.5 rounded-lg px-4 text-sm font-medium transition-colors ${
        primary ? 'bg-accent text-on-accent hover:bg-accent-ink' : 'text-ink-800 ring-1 ring-ink-900/15 ring-inset hover:bg-ink-900/[0.04] dark:text-ink-100 dark:ring-white/15 dark:hover:bg-white/5'
      }`}
    >
      {download.label}
      <ArrowUpRight className="size-4" />
    </a>
  )
}

function Step({ n, children }: { n: number; children: React.ReactNode }) {
  return (
    <li className="grid grid-cols-[24px_minmax(0,1fr)] items-start gap-3">
      <span className="mt-2.5 grid size-6 place-items-center rounded-full bg-ink-900/[0.06] font-mono text-xs text-ink-600 dark:bg-white/10 dark:text-ink-200">{n}</span>
      <div>{children}</div>
    </li>
  )
}

/** One way to install: numbered download-then-run steps, or a command with an optional download alternative. */
function MethodBody({ method }: { method: Method }) {
  const { command, download, steps } = method
  if (steps && command && download) {
    return (
      <ol className="space-y-3">
        <Step n={1}>
          <DownloadLink download={download} primary />
        </Step>
        <Step n={2}>
          <CopyCommand command={command} tone="light" />
        </Step>
      </ol>
    )
  }
  return (
    <div className="space-y-4">
      {command ? <CopyCommand command={command} tone="light" /> : null}
      {download ? <DownloadLink download={download} primary={!command} /> : null}
    </div>
  )
}

export function Install({ options, footer }: { options: Option[]; footer: string }) {
  const tabs = useTabs(options.length)
  const option = options[tabs.selected] ?? options[0]
  const variants = useTabs(option?.variants?.length ?? 0)
  const id = useId()

  // Open the visitor's platform; the server render shows the first tab.
  const { setSelected } = tabs
  useEffect(() => {
    const index = options.findIndex((o) => o.os === detectOs())
    if (index > 0) setSelected(index)
  }, [options, setSelected])

  if (!option) return null
  const variant = option.variants?.[variants.selected]

  return (
    <div className="self-start overflow-hidden rounded-3xl bg-white shadow-[0_20px_60px_-30px_rgba(0,0,0,0.3)] ring-1 ring-ink-900/10 dark:bg-ink-900 dark:shadow-none dark:ring-white/10">
      <div role="tablist" aria-label="Platform" onKeyDown={tabs.onKeyDown} className="grid grid-cols-3 gap-1 border-b border-ink-900/[0.07] p-1.5 dark:border-white/[0.07]">
        {options.map((o, i) => (
          <button
            key={o.label}
            ref={(el) => {
              tabs.refs.current[i] = el
            }}
            id={`${id}-tab-${i}`}
            role="tab"
            type="button"
            tabIndex={i === tabs.selected ? 0 : -1}
            aria-selected={i === tabs.selected}
            aria-controls={`${id}-panel`}
            onClick={() => tabs.setSelected(i)}
            className={`min-h-11 rounded-lg px-3 text-sm font-medium transition-colors sm:min-h-10 ${
              i === tabs.selected ? 'bg-accent/10 text-accent-fg dark:bg-accent/25' : 'text-ink-500 hover:bg-ink-900/[0.04] hover:text-ink-900 dark:text-ink-300 dark:hover:bg-white/5 dark:hover:text-white'
            }`}
          >
            {o.label}
          </button>
        ))}
      </div>

      <div id={`${id}-panel`} role="tabpanel" aria-labelledby={`${id}-tab-${tabs.selected}`} className="space-y-4 p-5 sm:p-6">
        <p className="text-sm leading-relaxed text-ink-600 dark:text-ink-300">{option.note}</p>

        {option.variants && variant ? (
          <>
            <div
              role="tablist"
              aria-label={`${option.label} package`}
              onKeyDown={variants.onKeyDown}
              className="inline-flex gap-1 rounded-lg bg-ink-900/[0.05] p-1 dark:bg-black/25"
            >
              {option.variants.map((v, i) => (
                <button
                  key={v.label}
                  ref={(el) => {
                    variants.refs.current[i] = el
                  }}
                  id={`${id}-variant-tab-${i}`}
                  role="tab"
                  type="button"
                  tabIndex={i === variants.selected ? 0 : -1}
                  aria-selected={i === variants.selected}
                  aria-controls={`${id}-variant-panel`}
                  onClick={() => variants.setSelected(i)}
                  className={`min-h-10 rounded-md px-3 font-mono text-[13px] transition-colors sm:min-h-8 ${
                    i === variants.selected ? 'bg-white text-ink-900 shadow-sm dark:bg-white/15 dark:text-white' : 'text-ink-500 hover:text-ink-900 dark:text-ink-300 dark:hover:text-white'
                  }`}
                >
                  {v.label}
                </button>
              ))}
            </div>
            <div
              id={`${id}-variant-panel`}
              role="tabpanel"
              aria-labelledby={`${id}-variant-tab-${variants.selected}`}
              className="space-y-4"
            >
              <p className="text-sm text-ink-600 dark:text-ink-300">{variant.note}</p>
              <MethodBody method={variant} />
            </div>
          </>
        ) : (
          <MethodBody method={option} />
        )}
      </div>

      <div
        className="border-t border-ink-900/[0.07] px-5 py-4 text-sm text-ink-500 sm:px-6 dark:border-white/[0.07] dark:text-ink-400 [&_a]:font-medium [&_a]:text-accent-fg [&_a]:underline [&_a]:underline-offset-2 [&_p]:m-0"
        dangerouslySetInnerHTML={{ __html: footer }}
      />
    </div>
  )
}
