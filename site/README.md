# conbus site

The landing page and waitlist: static files under `public/` behind one Worker
(`src/index.ts`) that answers `POST /api/click`, `POST /api/waitlist` and
`GET /api/contact`, with a D1 database for the signups and the fake-door
price clicks.

## Deploy

One-time setup (an operator step — creates real Cloudflare resources):

```bash
cd site
npm install
npx wrangler login
npx wrangler d1 create conbus-waitlist          # paste the printed id into
                                              # wrangler.jsonc -> database_id
npx wrangler d1 execute conbus-waitlist --remote --file ./schema.sql
# Set the real contact address:
#   wrangler.jsonc -> vars.CONTACT_EMAIL
```

Then deploy:

```bash
npx wrangler deploy
```

To check the bundle and config without deploying:

```bash
npx wrangler deploy --dry-run
```

## Test

```bash
npm test
```

Vitest runs the Worker against a local D1 via
`@cloudflare/vitest-pool-workers`; no Cloudflare account needed.

## What it stores

`schema.sql` is the whole data model. A signup keeps the UTC **day** (never a
timestamp), the email, the form answers and the consent flag; a click keeps a
`(day, variant)` counter. No IP address, user agent or cookie is ever stored.
A duplicate email returns the same `{ok: true}` as a fresh signup — the list
cannot be enumerated through the form. See `public/privacy.html`.
