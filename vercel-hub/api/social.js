// Playzi Social Hub on Vercel Serverless Functions (HTTP polling transport).
// Supports mutual friends, friend requests, online presence, open rooms, invites, and join requests.

import { kvGetJson, kvSetJson, normalizeUserId, setCors } from "../lib/store.js";

const STATE_KEY = "playzi:social:state:v1";
const PRESENCE_TTL_MS = 35_000;
const INVITE_TTL_MS = 10 * 60_000;
const JOIN_REQ_TTL_MS = 3 * 60_000;

function emptySocialState() {
  return {
    profiles: {},        // userId -> { userId, displayName, googleAccountName, lastSeenMs }
    friends: {},         // userId -> [friendUserId]
    friendRequests: {},  // id -> { id, fromUserId, fromDisplayName, toUserId, toDisplayName, createdAt }
    rooms: {},           // userId -> { roomCode, roomAccess, inviteUrl, trackTitle, guestCount, updatedAt }
    roomInvites: {},     // id -> { id, fromUserId, fromDisplayName, toUserId, roomCode, inviteUrl, roomAccess, createdAtMs }
    joinRequests: {},    // id -> { id, fromUserId, fromDisplayName, hostUserId, roomCode, createdAtMs }
    inbox: {},           // userId -> [eventObj] (transient one-shot notifications like join approval/rejection)
  };
}

function pruneState(st) {
  const now = Date.now();
  for (const [uid, prof] of Object.entries(st.profiles || {})) {
    if (now - Number(prof?.lastSeenMs || 0) > PRESENCE_TTL_MS) {
      delete st.rooms[uid];
    }
  }
  for (const [id, inv] of Object.entries(st.roomInvites || {})) {
    if (now - Number(inv?.createdAtMs || 0) > INVITE_TTL_MS) {
      delete st.roomInvites[id];
    }
  }
  for (const [id, req] of Object.entries(st.joinRequests || {})) {
    if (now - Number(req?.createdAtMs || 0) > JOIN_REQ_TTL_MS) {
      delete st.joinRequests[id];
    }
  }
}

function isOnline(st, uid) {
  const prof = st.profiles?.[uid];
  if (!prof) return false;
  return (Date.now() - Number(prof.lastSeenMs || 0)) <= PRESENCE_TTL_MS;
}

function areFriends(st, a, b) {
  return Array.isArray(st.friends?.[a]) && st.friends[a].includes(b);
}

function addMutualFriend(st, a, b) {
  st.friends[a] = Array.from(new Set([...(st.friends[a] || []), b]));
  st.friends[b] = Array.from(new Set([...(st.friends[b] || []), a]));
}

function removeMutualFriend(st, a, b) {
  st.friends[a] = (st.friends[a] || []).filter((id) => id !== b);
  st.friends[b] = (st.friends[b] || []).filter((id) => id !== a);
}

function pushInbox(st, uid, event) {
  if (!st.inbox) st.inbox = {};
  const list = Array.isArray(st.inbox[uid]) ? st.inbox[uid] : [];
  list.push(event);
  st.inbox[uid] = list.slice(-20);
}

function buildSyncFor(st, uid) {
  const friendIds = Array.isArray(st.friends?.[uid]) ? st.friends[uid] : [];
  const friends = friendIds.map((fid) => {
    const prof = st.profiles?.[fid] || {};
    const online = isOnline(st, fid);
    const room = online ? (st.rooms?.[fid] || null) : null;
    return {
      userId: fid,
      displayName: prof.displayName || fid,
      googleAccountName: prof.googleAccountName || "",
      online,
      room: room
        ? {
            roomCode: room.roomCode || "",
            roomAccess: room.roomAccess || "request",
            inviteUrl: room.roomAccess === "open" ? (room.inviteUrl || "") : "",
            trackTitle: room.trackTitle || "",
            guestCount: Number(room.guestCount || 0),
          }
        : null,
    };
  });

  const incomingFriendRequests = [];
  const outgoingFriendRequests = [];
  for (const req of Object.values(st.friendRequests || {})) {
    if (req.toUserId === uid) incomingFriendRequests.push(req);
    else if (req.fromUserId === uid) outgoingFriendRequests.push(req);
  }

  const incomingRoomInvites = Object.values(st.roomInvites || {})
    .filter((inv) => inv.toUserId === uid)
    .map(({ createdAtMs, ...rest }) => rest);

  const incomingJoinRequests = Object.values(st.joinRequests || {})
    .filter((req) => req.hostUserId === uid)
    .map(({ createdAtMs, ...rest }) => rest);

  return {
    type: "sync",
    friends,
    incomingFriendRequests,
    outgoingFriendRequests,
    incomingRoomInvites,
    incomingJoinRequests,
  };
}

export default async function handler(req, res) {
  setCors(res);
  if (req.method === "OPTIONS") return res.status(204).end();
  if (req.method !== "POST") return res.status(405).json({ error: "Method not allowed" });

  try {
    const body = typeof req.body === "string" ? JSON.parse(req.body || "{}") : (req.body || {});
    const uid = normalizeUserId(body.userId);
    if (!uid) return res.status(400).json({ error: "Missing userId" });

    const st = (await kvGetJson(STATE_KEY, null)) || emptySocialState();
    pruneState(st);

    const prevProf = st.profiles[uid] || {};
    st.profiles[uid] = {
      userId: uid,
      displayName: String(body.displayName || prevProf.displayName || uid).trim() || uid,
      googleAccountName: String(body.googleAccountName ?? prevProf.googleAccountName ?? "").trim(),
      lastSeenMs: Date.now(),
    };

    const action = String(body.action || body.type || "sync").trim();
    const msg = body.payload || body;
    let notice = null;

    if (action === "friend_request_send") {
      const targetId = normalizeUserId(msg.targetUserId);
      if (!targetId || targetId === uid) {
        notice = { level: "warn", message: "ID de amigo inválido." };
      } else if (areFriends(st, uid, targetId)) {
        notice = { level: "info", message: "Ya son amigos." };
      } else {
        const reverse = Object.values(st.friendRequests).find(
          (r) => r.fromUserId === targetId && r.toUserId === uid,
        );
        if (reverse) {
          delete st.friendRequests[reverse.id];
          addMutualFriend(st, uid, targetId);
          notice = { level: "ok", message: `Ahora eres amigo de ${reverse.fromDisplayName || targetId}.` };
        } else {
          const exists = Object.values(st.friendRequests).find(
            (r) => r.fromUserId === uid && r.toUserId === targetId,
          );
          if (!exists) {
            const id = `fr_${Date.now()}_${Math.random().toString(36).slice(2, 8)}`;
            st.friendRequests[id] = {
              id,
              fromUserId: uid,
              fromDisplayName: st.profiles[uid].displayName,
              toUserId: targetId,
              toDisplayName: st.profiles[targetId]?.displayName || targetId,
              createdAt: new Date().toISOString(),
            };
          }
          notice = { level: "ok", message: `Solicitud de amistad enviada a ${targetId}.` };
        }
      }
    } else if (action === "friend_request_accept") {
      const reqItem = st.friendRequests[msg.requestId];
      if (reqItem && reqItem.toUserId === uid) {
        delete st.friendRequests[reqItem.id];
        addMutualFriend(st, reqItem.fromUserId, uid);
        notice = { level: "ok", message: `Aceptaste a ${reqItem.fromDisplayName || reqItem.fromUserId}.` };
      }
    } else if (action === "friend_request_decline" || action === "friend_request_cancel") {
      const reqItem = st.friendRequests[msg.requestId];
      if (reqItem && (reqItem.toUserId === uid || reqItem.fromUserId === uid)) {
        delete st.friendRequests[reqItem.id];
      }
    } else if (action === "friend_remove") {
      const targetId = normalizeUserId(msg.targetUserId);
      if (targetId) removeMutualFriend(st, uid, targetId);
    } else if (action === "room_presence_update") {
      if (msg.room && msg.room.roomCode) {
        st.rooms[uid] = {
          roomCode: String(msg.room.roomCode).trim().toUpperCase(),
          roomAccess: msg.room.roomAccess === "open" ? "open" : "request",
          inviteUrl: String(msg.room.inviteUrl || "").trim(),
          trackTitle: String(msg.room.trackTitle || "").trim(),
          guestCount: Number(msg.room.guestCount || 0),
          updatedAt: Date.now(),
        };
      } else {
        delete st.rooms[uid];
      }
    } else if (action === "room_invite_send") {
      const targetId = normalizeUserId(msg.targetUserId);
      if (targetId && areFriends(st, uid, targetId)) {
        const id = `inv_${Date.now()}_${Math.random().toString(36).slice(2, 8)}`;
        st.roomInvites[id] = {
          id,
          fromUserId: uid,
          fromDisplayName: st.profiles[uid].displayName,
          toUserId: targetId,
          roomCode: String(msg.roomCode || "").trim().toUpperCase(),
          inviteUrl: String(msg.inviteUrl || "").trim(),
          roomAccess: msg.roomAccess === "open" ? "open" : "request",
          createdAtMs: Date.now(),
        };
        notice = { level: "ok", message: `Invitación enviada a ${st.profiles[targetId]?.displayName || targetId}.` };
      }
    } else if (action === "room_invite_dismiss") {
      if (msg.inviteId && st.roomInvites[msg.inviteId]?.toUserId === uid) {
        delete st.roomInvites[msg.inviteId];
      }
    } else if (action === "room_join_request") {
      const hostId = normalizeUserId(msg.hostUserId);
      const room = st.rooms[hostId];
      if (!hostId || !areFriends(st, uid, hostId) || !room || !isOnline(st, hostId)) {
        notice = { level: "warn", message: "La sala de tu amigo no está disponible ahora mismo." };
      } else if (room.roomAccess === "open" && room.inviteUrl) {
        pushInbox(st, uid, {
          type: "room_join_approved",
          hostUserId: hostId,
          hostDisplayName: st.profiles[hostId]?.displayName || hostId,
          roomCode: room.roomCode,
          inviteUrl: room.inviteUrl,
        });
      } else {
        const id = `jr_${Date.now()}_${Math.random().toString(36).slice(2, 8)}`;
        st.joinRequests[id] = {
          id,
          fromUserId: uid,
          fromDisplayName: st.profiles[uid].displayName,
          hostUserId: hostId,
          roomCode: room.roomCode,
          createdAtMs: Date.now(),
        };
        notice = { level: "info", message: "Solicitud enviada al host para unirte a la sala." };
      }
    } else if (action === "room_join_response") {
      const jr = st.joinRequests[msg.requestId];
      if (jr && jr.hostUserId === uid) {
        delete st.joinRequests[jr.id];
        if (msg.approved) {
          const inviteUrl = String(msg.inviteUrl || st.rooms[uid]?.inviteUrl || "").trim();
          pushInbox(st, jr.fromUserId, {
            type: "room_join_approved",
            hostUserId: uid,
            hostDisplayName: st.profiles[uid].displayName,
            roomCode: jr.roomCode,
            inviteUrl,
          });
        } else {
          pushInbox(st, jr.fromUserId, {
            type: "room_join_rejected",
            hostUserId: uid,
            hostDisplayName: st.profiles[uid].displayName,
            roomCode: jr.roomCode,
          });
        }
      }
    }

    const events = Array.isArray(st.inbox?.[uid]) ? st.inbox[uid] : [];
    if (events.length) {
      st.inbox[uid] = [];
    }

    await kvSetJson(STATE_KEY, st);

    return res.status(200).json({
      ok: true,
      sync: buildSyncFor(st, uid),
      events,
      notice,
    });
  } catch (err) {
    return res.status(500).json({ error: String(err?.message || err) });
  }
}
