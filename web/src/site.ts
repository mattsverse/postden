/** Where the website is published. Used for canonical URLs, social cards and the sitemap. */
export const SITE_URL = 'https://postden.app'

/** The card shown when a page is shared or linked: 1200×630, in web/public. */
const SOCIAL_IMAGE = { path: '/og.png', width: 1200, height: 630, alt: 'Postden, a desktop email client, showing its inbox.' }

/**
 * Search and social tags for one page: title, description, its canonical URL, and the
 * Open Graph and Twitter cards. `path` is the page's URL path, such as `/privacy`.
 */
export function seo({ path, title, description }: { path: string; title: string; description: string }) {
  const url = `${SITE_URL}${path}`
  const image = `${SITE_URL}${SOCIAL_IMAGE.path}`
  return {
    meta: [
      { title },
      { name: 'description', content: description },
      { property: 'og:type', content: 'website' },
      { property: 'og:site_name', content: 'Postden' },
      { property: 'og:url', content: url },
      { property: 'og:title', content: title },
      { property: 'og:description', content: description },
      { property: 'og:image', content: image },
      { property: 'og:image:width', content: String(SOCIAL_IMAGE.width) },
      { property: 'og:image:height', content: String(SOCIAL_IMAGE.height) },
      { property: 'og:image:alt', content: SOCIAL_IMAGE.alt },
      { name: 'twitter:card', content: 'summary_large_image' },
    ],
    links: [{ rel: 'canonical', href: url }],
  }
}
