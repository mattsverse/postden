/**
 * A real screenshot of the app from web/public/screenshots, in the visitor's color scheme.
 * Each name has a `-light.webp` and a `-dark.webp`, captured with the app set to that theme.
 */
export function Screenshot({
  name,
  alt,
  width,
  height,
  className = '',
  priority = false,
}: {
  name: string
  alt: string
  width: number
  height: number
  className?: string
  priority?: boolean
}) {
  return (
    <picture>
      <source srcSet={`/screenshots/${name}-dark.webp`} media="(prefers-color-scheme: dark)" />
      <img
        src={`/screenshots/${name}-light.webp`}
        alt={alt}
        width={width}
        height={height}
        loading={priority ? 'eager' : 'lazy'}
        decoding="async"
        className={`block h-auto w-full ${className}`}
      />
    </picture>
  )
}
