import type { APIRoute } from 'astro'
import { ICON_DARK, ICON_LIGHT } from '../lib/favicon'
import { iconSvg } from '../lib/turf'

export const GET: APIRoute = () => {
  const style = `<style>@media (prefers-color-scheme: dark) { path { fill: ${ICON_DARK} } }</style>`
  return new Response(iconSvg(ICON_LIGHT, style), { headers: { 'Content-Type': 'image/svg+xml' } })
}
