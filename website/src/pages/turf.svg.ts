import type { APIRoute } from 'astro'
import { stripSvg } from '../lib/turf'

export const GET: APIRoute = () =>
  new Response(stripSvg(), { headers: { 'Content-Type': 'image/svg+xml' } })
