import { createFileRoute } from '@tanstack/react-router'
import { ContentPage, getPage, pageHead } from '#/components/content-page'

const page = getPage('tos')

export const Route = createFileRoute('/tos')({
  head: () => pageHead(page),
  component: () => <ContentPage page={page} />,
})
