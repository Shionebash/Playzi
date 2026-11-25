# Playzi Hub on Vercel (Host Relay + Friends / Social)

Lightweight, zero-dependency Serverless HTTP Hub for Playzi Watch Party and Friends, ready to deploy on **Vercel Free Tier**.

## What Gets Uploaded to Vercel
Only the `vercel-hub/` directory is deployed to Vercel — **do NOT upload the Rust desktop app, `data/`, or `media/`**:
- `vercel-hub/package.json`
- `vercel-hub/vercel.json`
- `vercel-hub/api/health.js`
- `vercel-hub/api/social.js`
- `vercel-hub/api/room.js`
- `vercel-hub/lib/store.js`

## Persistent Storage (Recommended for Production)
Vercel Serverless Functions are stateless across cold starts, so connect a free **Turso (SQLite)** database (500M reads/month + 10M writes/month free, no card required):
1. Create a free database at [turso.tech](https://turso.tech).
2. Copy its database URL (`libsql://...`) and generate an auth token (Read & Write).
3. In Vercel -> Project -> **Settings -> Environment Variables**, add:
   - `TURSO_DATABASE_URL`
   - `TURSO_AUTH_TOKEN`
4. Redeploy. The `playzi_kv` table is created automatically on first request, and `/api/health` will report `"storageBackend": "turso"`.

(Upstash Redis via `UPSTASH_REDIS_REST_URL` + `UPSTASH_REDIS_REST_TOKEN` is also supported as a fallback; if neither is set, the hub falls back to in-memory storage for local testing.)

## Deploy Steps

### Option A: Vercel Dashboard (GitHub)
1. Import your repository in [vercel.com/new](https://vercel.com/new).
2. Set **Root Directory** to `vercel-hub`.
3. Leave Framework Preset as **Other** and click **Deploy**.
4. Connect **Upstash Redis** in the project's **Storage** tab and redeploy.

### Option B: Vercel CLI
```bash
cd vercel-hub
npx vercel --prod
```

## Configure in Playzi
Copy your deployed URL (e.g. `https://playzi-hub.vercel.app`) and paste it into:
- Playzi -> **Sesión** -> **Servidor Hub Online** (or set `SOCIAL_HUB_URL=https://playzi-hub.vercel.app` in `.env`).

Playzi automatically detects Vercel's HTTP hub transport via `/api/health` and uses it for both Friends (`POST /api/social`) and Watch Party Host/Guest sync (`POST /api/room`).
