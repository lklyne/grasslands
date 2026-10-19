import { readFile } from 'node:fs/promises'
import type { APIRoute, GetStaticPaths } from 'astro'
import satori from 'satori'
import sharp from 'sharp'

// Figma "Twitter post - 2" (agent-canvas 754:96) is the reference at 1200x675.
// Every size is composited from the same proportions so new widths stay on-model.
const SIZES = {
  '1200x630': { width: 1200, height: 630 },
  '1200x675': { width: 1200, height: 675 },
} as const

type Size = keyof typeof SIZES

// The meadow overshoots a width-fit by 1301/1200, centered, with 36 of its 155px overflow above.
const MEADOW_ZOOM = 1301 / 1200
const MEADOW_TOP = 36 / 155
// The wordmark is 128px on a 1200px card and its box is centered at 457.5/675.
const TITLE_SCALE = 128 / 1200
const TITLE_CENTER_Y = 457.5 / 675

export const getStaticPaths = (() =>
  Object.keys(SIZES).map((size) => ({ params: { size } }))) satisfies GetStaticPaths

export const GET: APIRoute = async ({ params }) => {
  const { width, height } = SIZES[params.size as Size]
  const [meadow, garamond] = await Promise.all([
    readFile('public/meadow.webp'),
    readFile('src/assets/fonts/EBGaramond-Italic.ttf'),
  ])

  const source = await sharp(meadow).metadata()
  const sourceWidth = source.width ?? width
  const sourceHeight = source.height ?? height
  const scale = Math.max(width / sourceWidth, height / sourceHeight) * MEADOW_ZOOM
  const scaledWidth = Math.round(sourceWidth * scale)
  const scaledHeight = Math.round(sourceHeight * scale)
  const background = sharp(meadow)
    .resize(scaledWidth, scaledHeight)
    .extract({
      left: Math.round((scaledWidth - width) / 2),
      top: Math.round((scaledHeight - height) * MEADOW_TOP),
      width,
      height,
    })

  const fontSize = Math.round(width * TITLE_SCALE)
  const title = await satori(
    {
      type: 'div',
      props: {
        style: {
          position: 'absolute',
          top: Math.round(height * TITLE_CENTER_Y - fontSize * 0.6),
          left: 0,
          width,
          display: 'flex',
          justifyContent: 'center',
          fontFamily: 'EB Garamond',
          fontStyle: 'italic',
          fontSize,
          lineHeight: 1.2,
          color: 'white',
          textShadow: `0 ${fontSize * (3 / 128)}px ${fontSize * (1 / 128)}px rgba(0, 0, 0, 0.25)`,
        },
        children: 'Grasslands',
      },
    },
    {
      width,
      height,
      fonts: [{ name: 'EB Garamond', data: garamond, weight: 400, style: 'italic' }],
    },
  )

  const png = await background
    .composite([{ input: Buffer.from(title) }])
    .png()
    .toBuffer()

  return new Response(new Uint8Array(png), { headers: { 'Content-Type': 'image/png' } })
}
