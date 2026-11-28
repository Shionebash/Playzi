// Playzi Watch Party HTTP Relay on Vercel Serverless Functions.
// Bridges host and guest JSON control/sync messages via a lightweight per-room queue.

import { hashSecret, kvCasJson, kvGetWithRaw, setCors, timingSafeEqualStr } from "../lib/store.js";

const ROOM_TTL_SECS = 120; // 2 min idle expiry (refreshed on every host_poll)
const GUEST_STALE_MS = 25_000;
const SAFE_KEY_RE = /^[a-zA-Z0-9_-]{1,80}$/;

function hasOwnSafeKey(obj, key) {
  return Boolean(obj && typeof key === "string" && SAFE_KEY_RE.test(key) && Object.prototype.hasOwnProperty.call(obj, key));
}

function roomKey(code) {
  return `playzi:room:v1:${String(code || "").trim().toUpperCase()}`;
}

function emptyRoom(code) {
  return {
    code,
    seq: 0,
    closed: false,
    hostLastSeenMs: 0,
    hostSecretHash: "",
    guests: {},       // connId -> { lastSeenMs }
    hostInbox: [],    // [{ seq, event }]  (relayOpen, relayMessage, relayClose)
    guestInboxes: {}, // connId -> [{ seq, payload }]
  };
}

function pruneGuests(room) {
  const now = Date.now();
  for (const [cid, info] of Object.entries(room.guests || {})) {
    if (now - Number(info?.lastSeenMs || 0) > GUEST_STALE_MS) {
      delete room.guests[cid];
      delete room.guestInboxes[cid];
      room.seq += 1;
      room.hostInbox.push({ seq: room.seq, event: { relayClose: cid } });
    }
  }
  if (room.hostInbox.length > 200) {
    room.hostInbox = room.hostInbox.slice(-200);
  }
}

export default async function handler(req, res) {
  setCors(res);
  if (req.method === "OPTIONS") return res.status(204).end();
  if (req.method !== "POST") return res.status(405).json({ error: "Method not allowed" });

  try {
    const body = typeof req.body === "string" ? JSON.parse(req.body || "{}") : (req.body || {});
    const code = String(body.room || req.query?.room || "").trim().toUpperCase();
    if (!code) return res.status(400).json({ error: "Missing room code" });
    if (!SAFE_KEY_RE.test(code)) return res.status(400).json({ error: "Invalid room code" });

    const action = String(body.action || "").trim();
    const key = roomKey(code);
    const MAX_CAS_RETRIES = 3;
    for (let attempt = 0; attempt < MAX_CAS_RETRIES; attempt += 1) {
    const { value: loadedRoom, raw: roomRaw } = await kvGetWithRaw(key, null);
    const room = loadedRoom || emptyRoom(code);
    pruneGuests(room);

    if (action === "host_poll" || action === "host_send" || action === "host_close") {
      const rawHostSecret = String(req.headers["x-playzi-host-secret"] || body.hostSecret || "").trim();
      if (rawHostSecret.length < 16) {
        return res.status(401).json({ error: "Missing or invalid hostSecret" });
      }
      const secretHash = await hashSecret(rawHostSecret);
      const hostActive =
        !room.closed &&
        Number(room.hostLastSeenMs || 0) > 0 &&
        (Date.now() - Number(room.hostLastSeenMs || 0)) <= GUEST_STALE_MS;

      if (action === "host_poll") {
        if (room.hostSecretHash && hostActive) {
          if (!timingSafeEqualStr(room.hostSecretHash, secretHash)) {
            return res.status(403).json({ error: "Invalid hostSecret" });
          }
        } else {
          room.hostSecretHash = secretHash;
        }
        room.closed = false;
        room.hostLastSeenMs = Date.now();
        const since = Number(body.since || 0);
        const events = room.hostInbox.filter((e) => e.seq > since);
        // Optionally enqueue outgoing host messages in the same round-trip
        const outgoing = Array.isArray(body.messages) ? body.messages : [];
        for (const item of outgoing) {
          const target = String(item?.relayTo || "");
          const payload = String(item?.payload || "");
          if (target && payload && hasOwnSafeKey(room.guests, target)) {
            room.seq += 1;
            if (!hasOwnSafeKey(room.guestInboxes, target)) room.guestInboxes[target] = [];
            room.guestInboxes[target].push({ seq: room.seq, payload });
            if (room.guestInboxes[target].length > 120) {
              room.guestInboxes[target] = room.guestInboxes[target].slice(-120);
            }
          }
        }
        if (!(await kvCasJson(key, roomRaw, room, ROOM_TTL_SECS))) {
          if (attempt + 1 < MAX_CAS_RETRIES) continue;
          return res.status(409).json({ error: "Concurrent room update conflict" });
        }
        return res.status(200).json({
          ok: true,
          seq: room.seq,
          events: events.map((e) => e.event),
        });
      }

      if (action === "host_send") {
        if (!room.hostSecretHash || !timingSafeEqualStr(room.hostSecretHash, secretHash)) {
          return res.status(403).json({ error: "Invalid hostSecret" });
        }
        room.closed = false;
        room.hostLastSeenMs = Date.now();
        const outgoing = Array.isArray(body.messages)
          ? body.messages
          : (body.relayTo ? [{ relayTo: body.relayTo, payload: body.payload }] : []);
        for (const item of outgoing) {
          const target = String(item?.relayTo || "");
          const payload = String(item?.payload || "");
          if (target && payload && hasOwnSafeKey(room.guests, target)) {
            room.seq += 1;
            if (!hasOwnSafeKey(room.guestInboxes, target)) room.guestInboxes[target] = [];
            room.guestInboxes[target].push({ seq: room.seq, payload });
            if (room.guestInboxes[target].length > 120) {
              room.guestInboxes[target] = room.guestInboxes[target].slice(-120);
            }
          }
        }
        if (!(await kvCasJson(key, roomRaw, room, ROOM_TTL_SECS))) {
          if (attempt + 1 < MAX_CAS_RETRIES) continue;
          return res.status(409).json({ error: "Concurrent room update conflict" });
        }
        return res.status(200).json({ ok: true, seq: room.seq });
      }

      if (action === "host_close") {
        if (room.hostSecretHash) {
          if (!timingSafeEqualStr(room.hostSecretHash, secretHash)) {
            return res.status(403).json({ error: "Invalid hostSecret" });
          }
        } else if (hostActive) {
          return res.status(403).json({ error: "Invalid hostSecret" });
        } else {
          return res.status(200).json({ ok: true, closed: true });
        }
        room.closed = true;
        room.hostLastSeenMs = 0;
        room.hostSecretHash = "";
        const closePayload = JSON.stringify({
          type: "room_closed",
          reason: "El host cerró la sesión",
        });
        for (const cid of Object.keys(room.guests || {})) {
          room.seq += 1;
          if (!room.guestInboxes[cid]) room.guestInboxes[cid] = [];
          room.guestInboxes[cid].push({ seq: room.seq, payload: closePayload });
        }
        room.guests = {};
        if (!(await kvCasJson(key, roomRaw, room, 60))) {
          if (attempt + 1 < MAX_CAS_RETRIES) continue;
          return res.status(409).json({ error: "Concurrent room update conflict" });
        }
        return res.status(200).json({ ok: true, closed: true });
      }
    }

    if (action === "guest_join") {
      const hostOnline =
        !room.closed &&
        Number(room.hostLastSeenMs || 0) > 0 &&
        (Date.now() - Number(room.hostLastSeenMs || 0)) <= GUEST_STALE_MS;
      if (!hostOnline) {
        return res.status(410).json({
          ok: false,
          hostOnline: false,
          error: "La sesión ya no existe o el host cerró la sala.",
        });
      }
      const connId = `g_${globalThis.crypto.randomUUID().replace(/-/g, "").slice(0, 16)}`;
      room.guests[connId] = { lastSeenMs: Date.now() };
      room.guestInboxes[connId] = [];
      room.seq += 1;
      room.hostInbox.push({ seq: room.seq, event: { relayOpen: connId } });
      if (!(await kvCasJson(key, roomRaw, room, ROOM_TTL_SECS))) {
        if (attempt + 1 < MAX_CAS_RETRIES) continue;
        return res.status(409).json({ error: "Concurrent room update conflict" });
      }
      return res.status(200).json({
        ok: true,
        connId,
        seq: room.seq,
        hostOnline: true,
      });
    }

    if (action === "guest_poll") {
      const connId = String(body.connId || "").trim();
      const hostOnline =
        !room.closed &&
        Number(room.hostLastSeenMs || 0) > 0 &&
        (Date.now() - Number(room.hostLastSeenMs || 0)) <= GUEST_STALE_MS;
      if (!hostOnline) {
        if (connId && hasOwnSafeKey(room.guests, connId)) {
          delete room.guests[connId];
          delete room.guestInboxes[connId];
          if (!(await kvCasJson(key, roomRaw, room, 60))) {
            if (attempt + 1 < MAX_CAS_RETRIES) continue;
          }
        }
        return res.status(200).json({
          ok: true,
          roomClosed: true,
          seq: room.seq,
          messages: [],
        });
      }
      if (!connId || !hasOwnSafeKey(room.guests, connId)) {
        return res.status(404).json({ error: "Guest connection not found", roomClosed: true });
      }
      room.guests[connId].lastSeenMs = Date.now();
      const since = Number(body.since || 0);
      const msgs = (hasOwnSafeKey(room.guestInboxes, connId) ? room.guestInboxes[connId] : []).filter((m) => m.seq > since);
      const outgoing = Array.isArray(body.messages) ? body.messages : [];
      for (const payload of outgoing) {
        room.seq += 1;
        room.hostInbox.push({
          seq: room.seq,
          event: {
            relayFrom: connId,
            payload: typeof payload === "string" ? payload : JSON.stringify(payload),
          },
        });
      }
      if (!(await kvCasJson(key, roomRaw, room, ROOM_TTL_SECS))) {
        if (attempt + 1 < MAX_CAS_RETRIES) continue;
        return res.status(409).json({ error: "Concurrent room update conflict" });
      }
      return res.status(200).json({
        ok: true,
        roomClosed: false,
        seq: room.seq,
        messages: msgs.map((m) => m.payload),
      });
    }

    if (action === "guest_send") {
      const connId = String(body.connId || "").trim();
      if (!connId || !hasOwnSafeKey(room.guests, connId)) {
        return res.status(404).json({ error: "Guest connection not found" });
      }
      room.guests[connId].lastSeenMs = Date.now();
      const payload = typeof body.payload === "string" ? body.payload : JSON.stringify(body.payload || {});
      room.seq += 1;
      room.hostInbox.push({
        seq: room.seq,
        event: { relayFrom: connId, payload },
      });
      if (!(await kvCasJson(key, roomRaw, room, ROOM_TTL_SECS))) {
        if (attempt + 1 < MAX_CAS_RETRIES) continue;
        return res.status(409).json({ error: "Concurrent room update conflict" });
      }
      return res.status(200).json({ ok: true, seq: room.seq });
    }

    if (action === "guest_leave") {
      const connId = String(body.connId || "").trim();
      if (connId && hasOwnSafeKey(room.guests, connId)) {
        delete room.guests[connId];
        delete room.guestInboxes[connId];
        room.seq += 1;
        room.hostInbox.push({ seq: room.seq, event: { relayClose: connId } });
        if (!(await kvCasJson(key, roomRaw, room, ROOM_TTL_SECS))) {
          if (attempt + 1 < MAX_CAS_RETRIES) continue;
          return res.status(409).json({ error: "Concurrent room update conflict" });
        }
      }
      return res.status(200).json({ ok: true });
    }

    return res.status(400).json({ error: `Unsupported action: ${action}` });
    }
    return res.status(409).json({ error: "Concurrent room update conflict" });
  } catch (err) {
    return res.status(500).json({ error: String(err?.message || err) });
  }
}
