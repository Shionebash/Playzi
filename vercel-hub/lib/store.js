// Zero-dependency key-value store for Playzi Vercel Hub.
// Uses Upstash Redis / Vercel KV REST API in production, with an in-memory fallback for local dev.

const memoryStore = new Map();

function getRedisConfig() {
  const url = (
    process.env.UPSTASH_REDIS_REST_URL
    || process.env.KV_REST_API_URL
    || ""
  ).trim().replace(/\/+$/, "");
  const token = (
    process.env.UPSTASH_REDIS_REST_TOKEN
    || process.env.KV_REST_API_TOKEN
    || ""
  ).trim();
  if (url && token) return { url, token };
  return null;
}

export function isPersistentStorageConfigured() {
  return Boolean(getRedisConfig());
}

async function redisCommand(args) {
  const cfg = getRedisConfig();
  if (!cfg) return null;
  const res = await fetch(cfg.url, {
    method: "POST",
    headers: {
      Authorization: `Bearer ${cfg.token}`,
      "Content-Type": "application/json",
    },
    body: JSON.stringify(args),
  });
  if (!res.ok) {
    throw new Error(`Redis REST error: HTTP ${res.status}`);
  }
  const data = await res.json();
  if (data.error) throw new Error(data.error);
  return data.result;
}

export async function kvGetJson(key, fallback = null) {
  const cfg = getRedisConfig();
  if (cfg) {
    const raw = await redisCommand(["GET", key]);
    if (raw == null) return fallback;
    try {
      return typeof raw === "string" ? JSON.parse(raw) : raw;
    } catch {
      return fallback;
    }
  }
  const entry = memoryStore.get(key);
  if (!entry) return fallback;
  if (entry.expiresAt && Date.now() > entry.expiresAt) {
    memoryStore.delete(key);
    return fallback;
  }
  return entry.value;
}

export async function kvMGetJson(keys) {
  if (!Array.isArray(keys) || keys.length === 0) return [];
  const cfg = getRedisConfig();
  if (cfg) {
    const rawList = await redisCommand(["MGET", ...keys]);
    if (!Array.isArray(rawList)) return keys.map(() => null);
    return rawList.map((raw) => {
      if (raw == null) return null;
      try {
        return typeof raw === "string" ? JSON.parse(raw) : raw;
      } catch {
        return null;
      }
    });
  }
  const now = Date.now();
  return keys.map((key) => {
    const entry = memoryStore.get(key);
    if (!entry) return null;
    if (entry.expiresAt && now > entry.expiresAt) {
      memoryStore.delete(key);
      return null;
    }
    return entry.value;
  });
}

export async function kvSetJson(key, value, ttlSeconds = 0) {
  const cfg = getRedisConfig();
  if (cfg) {
    const encoded = JSON.stringify(value);
    if (ttlSeconds > 0) {
      await redisCommand(["SET", key, encoded, "EX", String(ttlSeconds)]);
    } else {
      await redisCommand(["SET", key, encoded]);
    }
    return;
  }
  memoryStore.set(key, {
    value,
    expiresAt: ttlSeconds > 0 ? Date.now() + ttlSeconds * 1000 : 0,
  });
}

export async function kvDel(key) {
  const cfg = getRedisConfig();
  if (cfg) {
    await redisCommand(["DEL", key]);
    return;
  }
  memoryStore.delete(key);
}

export function setCors(res) {
  res.setHeader("Access-Control-Allow-Origin", "*");
  res.setHeader("Access-Control-Allow-Methods", "GET, POST, OPTIONS");
  res.setHeader("Access-Control-Allow-Headers", "Content-Type");
  res.setHeader("Cache-Control", "no-store");
}

export function normalizeUserId(raw) {
  return String(raw || "").trim().toUpperCase();
}
