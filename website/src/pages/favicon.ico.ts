import type { APIRoute } from 'astro'
import sharp from 'sharp'
import { ICON_LIGHT } from '../lib/favicon'
import { iconSvg } from '../lib/turf'

// Multiples of the 16px grid, so every size stays pixel-sharp.
const SIZES = [16, 32, 48]

export const GET: APIRoute = async () => {
  const svg = Buffer.from(iconSvg(ICON_LIGHT))
  const pngs = await Promise.all(
    SIZES.map((size) => sharp(svg, { density: 72 * (size / 16) }).resize(size, size).png().toBuffer()),
  )

  // ICO container: a 6-byte header, a 16-byte entry per image, then the PNGs themselves.
  const header = Buffer.alloc(6 + 16 * pngs.length)
  header.writeUInt16LE(1, 2)
  header.writeUInt16LE(pngs.length, 4)
  let offset = header.length
  pngs.forEach((png, i) => {
    const entry = 6 + 16 * i
    header.writeUInt8(SIZES[i], entry)
    header.writeUInt8(SIZES[i], entry + 1)
    header.writeUInt16LE(1, entry + 4)
    header.writeUInt16LE(32, entry + 6)
    header.writeUInt32LE(png.length, entry + 8)
    header.writeUInt32LE(offset, entry + 12)
    offset += png.length
  })

  return new Response(new Uint8Array(Buffer.concat([header, ...pngs])), {
    headers: { 'Content-Type': 'image/x-icon' },
  })
}
