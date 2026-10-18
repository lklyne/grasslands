import { defineConfig, fontProviders } from 'astro/config'
import sitemap from '@astrojs/sitemap'

export default defineConfig({
  site: 'https://grasslands.dev',
  output: 'static',
  server: { port: 4323 },
  fonts: [
    {
      provider: fontProviders.google(),
      name: 'Inter',
      cssVariable: '--font-inter',
      weights: ['400 700'],
    },
  ],
  integrations: [sitemap()],
})
