import { defineCollection, defineConfig } from '@content-collections/core'
import { compileMarkdown } from '@content-collections/markdown'
import GithubSlugger from 'github-slugger'
import rehypeSlug from 'rehype-slug'
import { z } from 'zod'

// Each kind has an icon in src/routes/index.tsx.
const featureKinds = ['search', 'sorting', 'compat', 'safe', 'transfer', 'eml'] as const

const link = z.object({ label: z.string(), href: z.url() })
// A real capture of the app in web/public/screenshots: `<name>-light.webp` and `<name>-dark.webp`,
// at twice `width` × `height`.
const screenshot = z.object({ name: z.string(), alt: z.string(), width: z.number(), height: z.number() })
const feature = z.object({ kind: z.enum(featureKinds), title: z.string(), body: z.string(), screenshot })
const installMethod = {
  command: z.string().optional(),
  download: link.optional(),
  // Number the download and the command as steps 1 and 2 (download first).
  steps: z.boolean().optional(),
}

// The landing page: copy lives in frontmatter so the layout stays in code.
const home = defineCollection({
  name: 'home',
  directory: 'content',
  include: 'home.md',
  schema: z.object({
    title: z.string(),
    description: z.string(),
    headline: z.string(),
    intro: z.string(),
    facts: z.array(z.string()),
    heroCommand: z.string(),
    heroCommandNote: z.string(),
    heroScreenshot: screenshot,
    featuresTitle: z.string(),
    featuresIntro: z.string(),
    features: z.array(feature),
    privacy: z.object({
      title: z.string(),
      intro: z.string(),
      // Everything the app connects to, by how often: always, sometimes, never.
      groups: z.array(z.object({ title: z.string(), items: z.array(z.object({ name: z.string(), body: z.string() })) })),
      website: z.string(),
    }),
    installTitle: z.string(),
    installIntro: z.string(),
    install: z.array(
      z.object({
        label: z.string(),
        // Which tab to open first for visitors on this OS.
        os: z.enum(['mac', 'windows', 'linux']),
        note: z.string(),
        ...installMethod,
        // Package choices within the tab, such as .deb, .rpm and Homebrew on Linux.
        variants: z.array(z.object({ label: z.string(), note: z.string(), ...installMethod })).optional(),
      }),
    ),
    content: z.string(),
  }),
  transform: async (document, context) => ({
    ...document,
    html: await compileMarkdown(context, document),
  }),
})

/** The page's `## ` headings, with the same ids rehype-slug gives them. */
function headings(markdown: string) {
  const slugger = new GithubSlugger()
  return [...markdown.matchAll(/^## (.+)$/gm)].map(([, text]) => {
    const title = text.trim()
    return { title, id: slugger.slug(title) }
  })
}

// Long-form pages such as the terms and the privacy policy, one Markdown file each.
const pages = defineCollection({
  name: 'pages',
  directory: 'content/pages',
  include: '*.md',
  schema: z.object({
    title: z.string(),
    description: z.string(),
    // Bump on every substantive change; git history keeps the earlier versions.
    updated: z.iso.date(),
    content: z.string(),
  }),
  transform: async (document, context) => ({
    ...document,
    slug: document._meta.path,
    headings: headings(document.content),
    html: await compileMarkdown(context, document, { rehypePlugins: [rehypeSlug] }),
  }),
})

export default defineConfig({ content: [home, pages] })
