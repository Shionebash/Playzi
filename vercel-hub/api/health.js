import { getStorageBackend, isPersistentStorageConfigured, setCors } from "../lib/store.js";

export default function handler(req, res) {
  setCors(res);
  if (req.method === "OPTIONS") {
    return res.status(204).end();
  }
  return res.status(200).json({
    ok: true,
    service: "playzi-vercel-hub",
    transport: "http",
    persistentStorage: isPersistentStorageConfigured(),
    storageBackend: getStorageBackend(),
    endpoints: {
      health: "/api/health",
      social: "/api/social",
      room: "/api/room",
    },
  });
}
