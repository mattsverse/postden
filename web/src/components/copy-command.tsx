import { Check, Copy } from 'lucide-react'
import { Fragment, useEffect, useState } from 'react'

/** A shell command with a copy button; wraps on narrow screens instead of scrolling. */
export function CopyCommand({ command, tone = 'dark' }: { command: string; tone?: 'dark' | 'light' }) {
  const [copied, setCopied] = useState(false)

  useEffect(() => {
    if (!copied) return
    const timer = setTimeout(() => setCopied(false), 1600)
    return () => clearTimeout(timer)
  }, [copied])

  async function copy() {
    try {
      await navigator.clipboard.writeText(command)
      setCopied(true)
    } catch {
      // Clipboard access can be denied; the command stays selectable.
    }
  }

  const light = tone === 'light'
  return (
    <div
      className={`flex items-start gap-3 rounded-lg py-1.5 pr-1.5 pl-4 font-mono text-[13px] ${
        light
          ? 'bg-ink-900/[0.04] ring-1 ring-ink-900/10 ring-inset dark:bg-white/[0.04] dark:ring-white/10'
          : 'bg-black/30'
      }`}
    >
      <span className="py-1.5 text-ink-400 select-none" aria-hidden="true">
        $
      </span>
      <code className={`min-w-0 flex-1 py-1.5 ${light ? 'text-ink-800 dark:text-ink-100' : 'text-white'}`}>
        {/* Wrap between words, keeping each path or package name on one line. */}
        {command.split(' ').map((word, i) => (
          <Fragment key={i}>
            {i > 0 ? ' ' : null}
            <span className="whitespace-nowrap">{word}</span>
          </Fragment>
        ))}
      </code>
      <button
        type="button"
        onClick={copy}
        aria-label="Copy command"
        className={`grid size-11 shrink-0 place-items-center rounded-md transition-colors sm:size-9 ${
          light
            ? 'text-ink-500 hover:bg-ink-900/[0.06] hover:text-ink-900 dark:text-ink-300 dark:hover:bg-white/10 dark:hover:text-white'
            : 'text-ink-300 hover:bg-white/10 hover:text-white'
        }`}
      >
        {copied ? <Check className="size-4" /> : <Copy className="size-4" />}
      </button>
      <span className="sr-only" aria-live="polite">
        {copied ? 'Copied to clipboard' : ''}
      </span>
    </div>
  )
}
