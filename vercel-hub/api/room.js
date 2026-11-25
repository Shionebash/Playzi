// Playzi Watch Party HTTP Relay on Vercel Serverless Functions.
// Bridges host and guest JSON control/sync messages via a lightweight per-room queue.

import { kvGetJson, kvSetJson, setCors } from "../lib/store.js";

const ROOM_TTL_SECS = 60 * 60; // 1 hour idle expiry
const GUEST_STALE_MS = 25_000;

function roomKey(code) {
  return `playzi:room:v1:${String(code || "").trim().toUpperCase()}`;
}

function emptyRoom(code) {
  return {
    code,
    seq: 0,
    hostLastSeenMs: 0,
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

    const action = String(body.action || "").trim();
    const key = roomKey(code);
    const room = (await kvGetJson(key, null)) || emptyRoom(code);
    pruneGuests(room);

    if (action === "host_poll") {
      room.hostLastSeenMs = Date.now();
      const since = Number(body.since || 0);
      const events = room.hostInbox.filter((e) => e.seq > since);
      // Optionally enqueue outgoing host messages in the same round-trip
      const outgoing = Array.isArray(body.messages) ? body.messages : [];
      for (const item of outgoing) {
        const target = String(item?.relayTo || "");
        const payload = String(item?.payload || "");
        if (target && payload && room.guests[target]) {
          room.seq += 1;
          if (!room.guestInboxes[target]) room.guestInboxes[target] = [];
          room.guestInboxes[target].push({ seq: room.seq, payload });
          if (room.guestInboxes[target].length > 120) {
            room.guestInboxes[target] = room.guestInboxes[target].slice(-120);
          }
        }
      }
      await kvSetJson(key, room, ROOM_TTL_SECS);
      return res.status(200).json({
        ok: true,
        seq: room.seq,
        events: events.map((e) => e.event),
      });
    }

    if (action === "host_send") {
      room.hostLastSeenMs = Date.now();
      const outgoing = Array.isArray(body.messages)
        ? body.messages
        : (body.relayTo ? [{ relayTo: body.relayTo, payload: body.payload }] : []);
      for (const item of outgoing) {
        const target = String(item?.relayTo || "");
        const payload = String(item?.payload || "");
        if (target && payload && room.guests[target]) {
          room.seq += 1;
          if (!room.guestInboxes[target]) room.guestInboxes[target] = [];
          room.guestInboxes[target].push({ seq: room.seq, payload });
          if (room.guestInboxes[target].length > 120) {
            room.guestInboxes[target] = room.guestInboxes[target].slice(-120);
          }
        }
      }
      await kvSetJson(key, room, ROOM_TTL_SECS);
      return res.status(200).json({ ok: true, seq: room.seq });
    }

    if (action === "guest_join") {
      const connId = String(body.connId || `g_${Date.now()}_${Math.random().toString(36).slice(2, 8)}`);
      room.guests[connId] = { lastSeenMs: Date.now() };
      room.guestInboxes[connId] = [];
      room.seq += 1;
      room.hostInbox.push({ seq: room.seq, event: { relayOpen: connId } });
      await kvSetJson(key, room, ROOM_TTL_SECS);
      return res.status(200).json({
        ok: true,
        connId,
        seq: room.seq,
        hostOnline: (Date.now() - Number(room.hostLastSeenMs || 0)) <= GUEST_STALE_MS,
      });
    }

    if (action === "guest_poll") {
      const connId = String(body.connId || "").trim();
      if (!connId || !room.guests[connId]) {
        return res.status(404).json({ error: "Guest connection not found" });
      }
      room.guests[connId].lastSeenMs = Date.now();
      const since = Number(body.since || 0);
      const msgs = (room.guestInboxes[connId] || []).filter((m) => m.seq > since);
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
      await kvSetJson(key, room, ROOM_TTL_SECS);
      return res.status(200).json({
        ok: true,
        seq: room.seq,
        messages: msgs.map((m) => m.payload),
      });
    }

    if (action === "guest_send") {
      const connId = String(body.connId || "").trim();
      if (!connId || !room.guests[connId]) {
        return res.status(404).json({ error: "Guest connection not found" });
      }
      room.guests[connId].lastSeenMs = Date.now();
      const payload = typeof body.payload === "string" ? body.payload : JSON.stringify(body.payload || {});
      room.seq += 1;
      room.hostInbox.push({
        seq: room.seq,
        event: { relayFrom: connId, payload },
      });
      await kvSetJson(key, room, ROOM_TTL_SECS);
      return res.status(200).json({ ok: true, seq: room.seq });
    }

    if (action === "guest_leave") {
      const connId = String(body.connId || "").trim();
      if (connId && room.guests[connId]) {
        delete room.guests[connId];
        delete room.guestInboxes[connId];
        room.seq += 1;
        room.hostInbox.push({ seq: room.seq, event: { relayClose: connId } });
        await kvSetJson(key, room, ROOM_TTL_SECS);
      }
      return res.status(200).json({ ok: true });
    }

    return res.status(400).json({ error: `Unsupported action: ${action}` });
  } catch (err) {
    return res.status(500).json({ error: String(err?.message || err) });
  }
}
