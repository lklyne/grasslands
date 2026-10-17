import { defineConfig } from 'astro/config'
import sitemap from '@astrojs/sitemap'

export default defineConfig({
  site: 'https://grasslands.dev',
  output: 'static',
  server: { port: 4323 },
  integrations: [sitemap()],
})
