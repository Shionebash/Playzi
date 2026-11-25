# Playzi Hub (Cloudflare Workers + Durable Objects)

Lightweight, free-tier compatible WebSocket relay and social hub for Playzi Watch Party.

## Features
- **Social Hub (`/social/ws`)**:
  - Mutual friend requests (send, accept, decline, cancel, remove) persisted in Durable Object storage.
  - Real-time online presence and active open-room status for mutual friends.
  - Direct room invitations and room join requests (with instant join for `open` rooms or host approval for `request` rooms).
- **Watch Party Relay (`/ws?room=CODE`)**:
  - Bridges host and guest WebSockets over `wss://` so online sessions work with zero port forwarding or LAN IP issues.
  - Only lightweight JSON control/sync messages pass through the relay; media streams directly from YouTube on each client.

## Deploy (Free Tier)

1. Install dependencies:
   ```bash
   cd cloudflare-hub
   npm install
   ```
2. Log in to Cloudflare:
   ```bash
   npx wrangler login
   ```
3. Deploy:
   ```bash
   npx wrangler deploy
   ```
4. Copy the deployed URL (e.g. `https://playzi-hub.<your-subdomain>.workers.dev`) and paste it into Playzi -> **Sesión** -> **Servidor Hub Online** (or set `SOCIAL_HUB_URL` in `.env`).
