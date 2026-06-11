# Cloudflare Pages — landing page

Static landing page for [minervamnt.xyz](https://minervamnt.xyz). Served from Cloudflare Pages; no Pi or tunnel required.

## Project

| Item | Value |
|------|-------|
| Pages project | `minervamnt` |
| Default URL | `https://minervamnt.pages.dev` |
| Custom domains | `minervamnt.xyz`, `www.minervamnt.xyz` |
| Source directory | `landing/` (contains `index.html`) |
| Cloudflare account ID | `dfc6e38d5b254f0f8ffac8a0e554112a` |

## Manual deploy (Mac)

Requires Node.js / `npx` (or portable Node in `/tmp`).

```bash
# From repo root — inject token from Doppler (dojopop / prd_zorie)
doppler run --project dojopop --config prd_zorie -- bash -c '
  export CLOUDFLARE_ACCOUNT_ID=dfc6e38d5b254f0f8ffac8a0e554112a
  npx wrangler@4 pages deploy landing \
    --project-name=minervamnt \
    --branch=main \
    --commit-dirty=true
'
```

First-time project creation (if missing):

```bash
doppler run --project dojopop --config prd_zorie -- bash -c '
  curl -sS -X POST \
    -H "Authorization: Bearer $CLOUDFLARE_API_TOKEN" \
    -H "Content-Type: application/json" \
    "https://api.cloudflare.com/client/v4/accounts/dfc6e38d5b254f0f8ffac8a0e554112a/pages/projects" \
    -d "{\"name\":\"minervamnt\",\"production_branch\":\"main\"}"
'
```

## Custom domain & DNS

Custom domains are attached to the Pages project via API or dashboard. DNS for `minervamnt.xyz` must point at Pages, not the Pi tunnel.

**Current DNS** (zone `minervamnt.xyz`):

| Name | Type | Content | Proxied |
|------|------|---------|---------|
| `minervamnt.xyz` | CNAME | `minervamnt.pages.dev` | yes |
| `www.minervamnt.xyz` | CNAME | `minervamnt.pages.dev` | yes |

**Previous DNS** (tunnel — removed):

| Name | Type | Content |
|------|------|---------|
| `minervamnt.xyz` | CNAME | `<tunnel-id>.cfargotunnel.com` |

Update DNS with the DNS-scoped token (`CLOUDFLARE_DNS_TOKEN` in Doppler):

```bash
doppler run --project dojopop --config prd_zorie -- bash -c '
  API="https://api.cloudflare.com/client/v4"
  AUTH="Authorization: Bearer $CLOUDFLARE_DNS_TOKEN"
  ZONE="$CLOUDFLARE_ZONE_ID_MINERVAMNT"

  # Patch apex record (replace RECORD_ID from list call)
  curl -sS -X PATCH -H "$AUTH" -H "Content-Type: application/json" \
    "$API/zones/$ZONE/dns_records/<RECORD_ID>" \
    -d "{\"type\":\"CNAME\",\"name\":\"minervamnt.xyz\",\"content\":\"minervamnt.pages.dev\",\"proxied\":true,\"ttl\":1}"
'
```

Attach domains to Pages (uses `CLOUDFLARE_API_TOKEN`):

```bash
doppler run --project dojopop --config prd_zorie -- bash -c '
  API="https://api.cloudflare.com/client/v4"
  AUTH="Authorization: Bearer $CLOUDFLARE_API_TOKEN"
  ACCOUNT=dfc6e38d5b254f0f8ffac8a0e554112a

  for host in minervamnt.xyz www.minervamnt.xyz; do
    curl -sS -X POST -H "$AUTH" -H "Content-Type: application/json" \
      "$API/accounts/$ACCOUNT/pages/projects/minervamnt/domains" \
      -d "{\"name\":\"$host\"}"
  done
'
```

## Verify

```bash
curl -s -o /dev/null -w "root: %{http_code}\n" https://minervamnt.xyz/
curl -s https://minervamnt.xyz/ | head -5
curl -s -o /dev/null -w "health: %{http_code}\n" https://minervamnt.xyz/health
curl -s -o /dev/null -w "v1/info: %{http_code}\n" https://minervamnt.xyz/v1/info
```

Expected: HTTP 200 on `/` with HTML landing page. `/health` and `/v1/info` return the same static page (no mint JSON).

## Pi tunnel (optional)

The `cloudflared` tunnel on pi5 can stay **stopped**. DNS no longer routes to the tunnel, so the site works even when Pi SSH is down.

When re-enabling the mint API later:

- Use a subdomain (e.g. `api.minervamnt.xyz` → tunnel → `:3338`), or
- Switch apex DNS back to the tunnel and disable Pages custom domain.

Do **not** re-enable `minerva-mint` until you intend to serve the API again.

## GitHub Actions

`.github/workflows/deploy-landing.yml` deploys on push to `main` when `landing/**` changes.

Add repository secrets:

| Secret | Source |
|--------|--------|
| `CLOUDFLARE_API_TOKEN` | Doppler `CLOUDFLARE_API_TOKEN` |
| `CLOUDFLARE_ACCOUNT_ID` | `dfc6e38d5b254f0f8ffac8a0e554112a` |
