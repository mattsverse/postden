import { createFileRoute } from '@tanstack/react-router'
import { NotFound } from '#/components/not-found'

// Matches every address that isn't a page. The build renders it once, at /404, into 404.html,
// which static hosts such as Cloudflare Pages serve with a 404 status for unknown URLs (without
// that file they answer with the homepage and a 200). Because the browser matches this same
// route at whatever address was asked for, the page hydrates cleanly there.
export const Route = createFileRoute('/$')({
  head: () => ({
    meta: [{ title: 'Page not found · Postden' }, { name: 'robots', content: 'noindex' }],
  }),
  component: NotFound,
})
