# Privacy Policy

**Last Updated:** November 25, 2026

This Privacy Policy explains how **Playzi** ([https://github.com/Shionebash/Playzi](https://github.com/Shionebash/Playzi)) handles information when you use the Playzi desktop application, its Discord integration, and its optional Watch Party / Social Hub features.

## 1. Summary

Playzi is a **local-first, open-source desktop application**. We do **not** run analytics, telemetry, or advertising trackers, and we do **not** sell, rent, or trade your personal data.

## 2. Data Stored Locally on Your Device

Playzi stores application data locally on your computer (inside its configured `data/` and `media/` directories), including:

- Application settings and preferences (`.env`)
- Local playlists, collections, playback history, and thumbnail cache
- Downloaded audio/video files
- Optional browser/session cookies if you choose to sign in to YouTube / YouTube Music for personalized recommendations or authenticated playback

This data remains on your device and is under your sole control.

## 3. Discord Integration Data

If you enable Playzi's optional Discord integration, Playzi communicates locally with your running Discord desktop client via Discord IPC:

- **Information read from Discord:** Basic account identifiers provided by the local Discord IPC handshake (`user_id`, `username`, `global_name`) and, when another user requests to join your Watch Party, their Discord `user_id`, `username`, `global_name`, and `avatar` hash.
- **How it is used:** Solely in memory during your active session to display your connection status in the Playzi UI and to let you accept or decline Watch Party join requests.
- **Information sent to Discord:** Optional Rich Presence activity details (such as currently playing media title, channel/artist, playback timestamps, and Watch Party room status or join secrets) according to your Playzi settings.
- **Retention:** Discord profile data read via IPC is kept only in volatile memory and is not persisted to external servers by Playzi.

You can disable Discord Rich Presence, media display, or party invites at any time in Playzi's settings.

## 4. Watch Party and Optional Social Hub

Playzi includes an optional Watch Party and Friends feature that connects to a user-configured hub server (`SOCIAL_HUB_URL`, such as a self-hosted Cloudflare Workers or Vercel deployment):

- **Data transmitted to the hub:** Lightweight JSON control messages required for real-time synchronization and social features (such as room codes, playback sync states, friend requests, and online/room presence). Media streams directly from YouTube to each participant's device.
- **Storage on the hub:** Friend relationships and active room states are stored only on the hub instance you configure, in accordance with that hub operator's infrastructure (e.g., Cloudflare Durable Objects or Upstash Redis).

## 5. Third-Party Services

When you browse, stream, download, or connect accounts through Playzi, your device communicates directly with third-party services such as YouTube, YouTube Music, and Discord. Those services process your data under their own privacy policies:

- [Discord Privacy Policy](https://discord.com/privacy)
- [Google Privacy Policy](https://policies.google.com/privacy)

## 6. Children's Privacy

Playzi is not directed to children under the age of 13 (or the minimum digital consent age in your jurisdiction), and we do not knowingly collect personal information from children.

## 7. Your Choices and Data Deletion

- **Local data:** You may delete your local Playzi data at any time by clearing or removing the application's `data/` directory.
- **Discord integration:** You may disable the integration in Playzi's settings at any time to stop sending Rich Presence updates and disconnect IPC.
- **Hub data:** You may remove friends, leave rooms, or clear `SOCIAL_HUB_URL` in Playzi's settings.

## 8. Changes to This Privacy Policy

We may update this Privacy Policy periodically by posting the revised version in the Playzi GitHub repository.

## 9. Contact

If you have questions or privacy requests regarding Playzi, please open an issue on GitHub:

- GitHub Issues: [https://github.com/Shionebash/Playzi/issues](https://github.com/Shionebash/Playzi/issues)
