// Zero-dependency key-value store for Playzi Vercel Hub.
// Priority:
//   1. Turso (libSQL over Hrana HTTP v2) via TURSO_DATABASE_URL + TURSO_AUTH_TOKEN
//   2. Upstash Redis / Vercel KV REST API via UPSTASH_REDIS_REST_URL + UPSTASH_REDIS_REST_TOKEN
//   3. In-memory fallback for local dev

const memoryStore = new Map();
let tursoSchemaReady = false;
let tursoInitPromise = null;

function getTursoConfig() {
  const rawUrl = (
    process.env.TURSO_DATABASE_URL
    || process.env.LIBSQL_URL
    || ""
  ).trim();
  const token = (
    process.env.TURSO_AUTH_TOKEN
    || process.env.LIBSQL_AUTH_TOKEN
    || ""
  ).trim();
  if (!rawUrl || !token) return null;

  let httpUrl = rawUrl;
  if (httpUrl.startsWith("libsql://")) {
    httpUrl = `https://${httpUrl.slice("libsql://".length)}`;
  } else if (httpUrl.startsWith("ws://")) {
    httpUrl = `http://${httpUrl.slice("ws://".length)}`;
  } else if (httpUrl.startsWith("wss://")) {
    httpUrl = `https://${httpUrl.slice("wss://".length)}`;
  } else if (!httpUrl.startsWith("https://") && !httpUrl.startsWith("http://")) {
    httpUrl = `https://${httpUrl}`;
  }
  httpUrl = httpUrl.replace(/\/+$/, "").replace(/\/v2\/pipeline$/, "");
  return { pipelineUrl: `${httpUrl}/v2/pipeline`, token };
}

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

export function getStorageBackend() {
  if (getTursoConfig()) return "turso";
  if (getRedisConfig()) return "redis";
  return "memory";
}

export function isPersistentStorageConfigured() {
  return Boolean(getTursoConfig() || getRedisConfig());
}

// ── Turso (libSQL Hrana HTTP v2) ────────────────────────────────────────────

function toHranaArg(value) {
  if (value === null || value === undefined) {
    return { type: "null" };
  }
  if (typeof value === "number" && Number.isInteger(value)) {
    return { type: "integer", value: String(value) };
  }
  return { type: "text", value: String(value) };
}

function fromHranaCell(cell) {
  if (!cell || cell.type === "null") return null;
  if (cell.type === "integer") return Number(cell.value);
  if (cell.type === "float") return Number(cell.value);
  return cell.value ?? null;
}

async function tursoExecute(cfg, sql, args = []) {
  const res = await fetch(cfg.pipelineUrl, {
    method: "POST",
    headers: {
      Authorization: `Bearer ${cfg.token}`,
      "Content-Type": "application/json",
    },
    body: JSON.stringify({
      requests: [
        {
          type: "execute",
          stmt: {
            sql,
            args: args.map(toHranaArg),
          },
        },
        { type: "close" },
      ],
    }),
  });

  if (!res.ok) {
    const bodyText = await res.text().catch(() => "");
    let detail = "";
    try {
      const parsed = JSON.parse(bodyText);
      detail = parsed?.error || parsed?.message || bodyText;
    } catch {
      detail = bodyText;
    }
    const cleanDetail = String(detail || "").trim().slice(0, 200);
    throw new Error(
      cleanDetail
        ? `Turso HTTP error ${res.status} (${cleanDetail})`
        : `Turso HTTP error ${res.status}`
    );
  }

  const data = await res.json();
  const first = Array.isArray(data?.results) ? data.results[0] : null;
  if (!first) {
    throw new Error("Turso returned an empty pipeline response");
  }
  if (first.type === "error") {
    throw new Error(`Turso SQL error: ${first.error?.message || "unknown error"}`);
  }
  return first.response?.result || { cols: [], rows: [] };
}

async function ensureTursoSchema(cfg) {
  if (tursoSchemaReady) return;
  if (!tursoInitPromise) {
    tursoInitPromise = tursoExecute(
      cfg,
      "CREATE TABLE IF NOT EXISTS playzi_kv (k TEXT PRIMARY KEY, v TEXT NOT NULL, expires_at INTEGER NOT NULL DEFAULT 0)"
    )
      .then(() => {
        tursoSchemaReady = true;
      })
      .finally(() => {
        tursoInitPromise = null;
      });
  }
  await tursoInitPromise;
}

// ── Upstash Redis REST fallback ─────────────────────────────────────────────

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
    const bodyText = await res.text().catch(() => "");
    let detail = "";
    try {
      const parsed = JSON.parse(bodyText);
      detail = parsed?.error || bodyText;
    } catch {
      detail = bodyText;
    }
    const cleanDetail = String(detail || "").trim().slice(0, 200);
    throw new Error(
      cleanDetail
        ? `Redis REST error: HTTP ${res.status} (${cleanDetail})`
        : `Redis REST error: HTTP ${res.status}`
    );
  }
  const data = await res.json();
  if (data.error) throw new Error(data.error);
  return data.result;
}

// ── Unified KV API ──────────────────────────────────────────────────────────

export async function kvGetJson(key, fallback = null) {
  const turso = getTursoConfig();
  if (turso) {
    await ensureTursoSchema(turso);
    const result = await tursoExecute(
      turso,
      "SELECT v, expires_at FROM playzi_kv WHERE k = ? LIMIT 1",
      [key]
    );
    const row = result.rows?.[0];
    if (!row) return fallback;
    const raw = fromHranaCell(row[0]);
    const expiresAt = Number(fromHranaCell(row[1]) || 0);
    if (expiresAt > 0 && Date.now() > expiresAt) {
      return fallback;
    }
    if (raw == null) return fallback;
    try {
      return typeof raw === "string" ? JSON.parse(raw) : raw;
    } catch {
      return fallback;
    }
  }

  const redis = getRedisConfig();
  if (redis) {
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

  const turso = getTursoConfig();
  if (turso) {
    await ensureTursoSchema(turso);
    const uniqueKeys = Array.from(new Set(keys));
    const now = Date.now();
    const found = new Map();
    const CHUNK_SIZE = 100;

    for (let i = 0; i < uniqueKeys.length; i += CHUNK_SIZE) {
      const chunk = uniqueKeys.slice(i, i + CHUNK_SIZE);
      const placeholders = chunk.map(() => "?").join(", ");
      const result = await tursoExecute(
        turso,
        `SELECT k, v, expires_at FROM playzi_kv WHERE k IN (${placeholders})`,
        chunk
      );
      for (const row of result.rows || []) {
        const k = fromHranaCell(row[0]);
        const raw = fromHranaCell(row[1]);
        const expiresAt = Number(fromHranaCell(row[2]) || 0);
        if (!k || raw == null) continue;
        if (expiresAt > 0 && now > expiresAt) continue;
        try {
          found.set(k, typeof raw === "string" ? JSON.parse(raw) : raw);
        } catch {
          // ignore malformed JSON entry
        }
      }
    }

    return keys.map((k) => (found.has(k) ? found.get(k) : null));
  }

  const redis = getRedisConfig();
  if (redis) {
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
  const turso = getTursoConfig();
  if (turso) {
    await ensureTursoSchema(turso);
    const encoded = JSON.stringify(value);
    const expiresAt = ttlSeconds > 0 ? Date.now() + Math.trunc(ttlSeconds * 1000) : 0;
    await tursoExecute(
      turso,
      "INSERT INTO playzi_kv (k, v, expires_at) VALUES (?, ?, ?) ON CONFLICT(k) DO UPDATE SET v = excluded.v, expires_at = excluded.expires_at",
      [key, encoded, expiresAt]
    );
    return;
  }

  const redis = getRedisConfig();
  if (redis) {
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
  const turso = getTursoConfig();
  if (turso) {
    await ensureTursoSchema(turso);
    await tursoExecute(turso, "DELETE FROM playzi_kv WHERE k = ?", [key]);
    return;
  }

  const redis = getRedisConfig();
  if (redis) {
    await redisCommand(["DEL", key]);
    return;
  }

  memoryStore.delete(key);
}

export function setCors(res) {
  res.setHeader("Access-Control-Allow-Origin", "*");
  res.setHeader("Access-Control-Allow-Methods", "GET, POST, OPTIONS");
  res.setHeader("Access-Control-Allow-Headers", "Content-Type, X-Playzi-Auth, X-Playzi-Host-Secret");
  res.setHeader("Cache-Control", "no-store");
}

export function normalizeUserId(raw) {
  return String(raw || "").trim().toUpperCase();
}

export async function hashSecret(raw) {
  const bytes = new TextEncoder().encode(String(raw || ""));
  const digest = await globalThis.crypto.subtle.digest("SHA-256", bytes);
  return Array.from(new Uint8Array(digest), (b) => b.toString(16).padStart(2, "0")).join("");
}

export function timingSafeEqualStr(a, b) {
  const sa = String(a || "");
  const sb = String(b || "");
  if (sa.length !== sb.length || sa.length === 0) return false;
  let diff = 0;
  for (let i = 0; i < sa.length; i += 1) {
    diff |= sa.charCodeAt(i) ^ sb.charCodeAt(i);
  }
  return diff === 0;
}
