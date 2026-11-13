import { defineConfig } from 'vite'
import { devtools } from '@tanstack/devtools-vite'

import { tanstackStart } from '@tanstack/react-start/plugin/vite'

import viteReact from '@vitejs/plugin-react'
import tailwindcss from '@tailwindcss/vite'
import contentCollections from '@content-collections/vite'

import { SITE_URL } from './src/site.ts'

const config = defineConfig({
  resolve: { tsconfigPaths: true },
  plugins: [
    devtools(),
    contentCollections(),
    tailwindcss(),
    tanstackStart({
      // Every page is static content, so build plain HTML files any static host can serve.
      // `privacy.html` rather than `privacy/index.html`, so hosts serve /privacy without redirecting to /privacy/.
      prerender: {
        enabled: true,
        crawlLinks: true,
        failOnError: true,
        autoSubfolderIndex: false,
        // A #section link points into a page that's already built. The crawler has already queued it
        // for the sitemap by the time this runs, so mark it excluded there too.
        filter: (page) => {
          // The alternate homepage design is built for comparison, but isn't a page to list.
          if (page.path === '/alt') page.sitemap = { exclude: true }
          if (!page.path.includes('#')) return true
          page.sitemap = { exclude: true }
          return false
        },
      },
      sitemap: { enabled: true, host: SITE_URL },
    }),
    viteReact(),
  ],
})

export default config
