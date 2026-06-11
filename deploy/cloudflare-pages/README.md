# Cloudflare Pages — static landing (optional)

Deploy the contents of `landing/` as a static site on [Cloudflare Pages](https://developers.cloudflare.com/pages/).

Use this when you want a public marketing or status page **without** running the
mint API on the same hostname.

## Prerequisites

- Cloudflare account with Pages enabled
- API token with **Cloudflare Pages — Edit** (and DNS edit if attaching custom domains)
- Node.js / `npx` for manual deploys, or GitHub Actions (see below)

## Manual deploy

Set credentials in your environment (never commit tokens):

```bash
export CLOUDFLARE_API_TOKEN="your-token"
export CLOUDFLARE_ACCOUNT_ID="your-account-id"

npx wrangler@4 pages deploy landing \
  --project-name=your-pages-project \
  --branch=main
```

Create the Pages project once (if it does not exist):

```bash
curl -sS -X POST \
  -H "Authorization: Bearer $CLOUDFLARE_API_TOKEN" \
  -H "Content-Type: application/json" \
  "https://api.cloudflare.com/client/v4/accounts/${CLOUDFLARE_ACCOUNT_ID}/pages/projects" \
  -d '{"name":"your-pages-project","production_branch":"main"}'
```

## Custom domain

Attach domains via the Cloudflare dashboard or API. Point DNS CNAME records at
your `*.pages.dev` hostname.

Example API attach:

```bash
for host in mint.example.com www.mint.example.com; do
  curl -sS -X POST \
    -H "Authorization: Bearer $CLOUDFLARE_API_TOKEN" \
    -H "Content-Type: application/json" \
    "https://api.cloudflare.com/client/v4/accounts/${CLOUDFLARE_ACCOUNT_ID}/pages/projects/your-pages-project/domains" \
    -d "{\"name\":\"$host\"}"
done
```

## Mint API on a subdomain (optional)

A common production layout:

| Host | Serves |
| ---- | ------ |
| `mint.example.com` | Static landing (Pages) |
| `api.mint.example.com` | Mint HTTP API (tunnel or reverse proxy → `:3338`) |

See `deploy/cloudflared/config.yml.example` for tunnel ingress patterns.

## GitHub Actions

`.github/workflows/deploy-landing.yml` deploys when `landing/**` changes on `main`.

Add **repository secrets** (Settings → Secrets → Actions):

| Secret | Value |
| ------ | ----- |
| `CLOUDFLARE_API_TOKEN` | Pages deploy token |
| `CLOUDFLARE_ACCOUNT_ID` | Your Cloudflare account ID |

Do not store these in the repository.

## Verify

```bash
curl -sI "https://your-pages-project.pages.dev/" | head -5
```
