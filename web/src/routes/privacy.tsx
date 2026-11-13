import { createFileRoute } from '@tanstack/react-router'
import { ContentPage, getPage, pageHead } from '#/components/content-page'

const page = getPage('privacy')

export const Route = createFileRoute('/privacy')({
  head: () => pageHead(page),
  component: () => <ContentPage page={page} />,
})
