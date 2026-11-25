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
            inviteUrl: room.roomAccess === "open" ? (room.inviteUrl || "") : "",
            roomAccess: room.roomAccess || "request",
            participantsCount: Number(room.participantsCount ?? room.guestCount ?? 1),
            currentTitle: String(room.currentTitle ?? room.trackTitle ?? ""),
            updatedAt: String(room.updatedAt || ""),
          }
        : null,
    };
  });

  const incomingFriendRequests = [];
  const outgoingFriendRequests = [];
  for (const req of Object.values(st.friendRequests || {})) {
    const item = {
      id: req.id,
      fromUserId: req.fromUserId,
      fromDisplayName: req.fromDisplayName || st.profiles?.[req.fromUserId]?.displayName || req.fromUserId,
      fromGoogleAccountName: req.fromGoogleAccountName ?? st.profiles?.[req.fromUserId]?.googleAccountName ?? "",
      toUserId: req.toUserId,
      toDisplayName: req.toDisplayName || st.profiles?.[req.toUserId]?.displayName || req.toUserId,
      toGoogleAccountName: req.toGoogleAccountName ?? st.profiles?.[req.toUserId]?.googleAccountName ?? "",
      createdAt: req.createdAt || "",
    };
    if (req.toUserId === uid) incomingFriendRequests.push(item);
    else if (req.fromUserId === uid) outgoingFriendRequests.push(item);
  }

  const incomingRoomInvites = Object.values(st.roomInvites || {})
    .filter((inv) => inv.toUserId === uid)
    .map(({ createdAtMs, ...rest }) => rest);

  const incomingJoinRequests = Object.values(st.joinRequests || {})
    .filter((req) => (req.toUserId || req.hostUserId) === uid)
    .map(({ createdAtMs, hostUserId, ...rest }) => ({
      id: rest.id,
      fromUserId: rest.fromUserId,
      fromDisplayName: rest.fromDisplayName || st.profiles?.[rest.fromUserId]?.displayName || rest.fromUserId,
      fromGoogleAccountName: rest.fromGoogleAccountName ?? st.profiles?.[rest.fromUserId]?.googleAccountName ?? "",
      toUserId: rest.toUserId || hostUserId || uid,
      roomCode: rest.roomCode || "",
      createdAt: rest.createdAt || "",
    }));

  return {
    type: "social_sync",
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

    const action = String(body.action || body.type || "sync").trim();
    const msg = body.payload || body;

    const prevProf = st.profiles[uid] || {};
    const nextDisplayName = action === "profile_update" && msg.displayName !== undefined
      ? String(msg.displayName || "").trim()
      : String(body.displayName || "").trim();
    const nextGoogleName = action === "profile_update" && msg.googleAccountName !== undefined
      ? String(msg.googleAccountName || "").trim()
      : String(body.googleAccountName ?? prevProf.googleAccountName ?? "").trim();

    st.profiles[uid] = {
      userId: uid,
      displayName: nextDisplayName || prevProf.displayName || uid,
      googleAccountName: nextGoogleName,
      lastSeenMs: Date.now(),
    };

    let notice = null;

    if (action === "friend_request_send") {
      const targetId = normalizeUserId(msg.toUserId || msg.targetUserId);
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
          pushInbox(st, targetId, {
            type: "notice",
            level: "ok",
            message: `${st.profiles[uid].displayName} aceptó tu solicitud de amistad.`,
          });
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
              fromGoogleAccountName: st.profiles[uid].googleAccountName || "",
              toUserId: targetId,
              toDisplayName: st.profiles[targetId]?.displayName || targetId,
              toGoogleAccountName: st.profiles[targetId]?.googleAccountName || "",
              createdAt: new Date().toISOString(),
            };
            pushInbox(st, targetId, {
              type: "notice",
              level: "info",
              message: `${st.profiles[uid].displayName} te envió una solicitud de amistad.`,
            });
          }
          notice = { level: "ok", message: `Solicitud de amistad enviada a ${st.profiles[targetId]?.displayName || targetId}.` };
        }
      }
    } else if (action === "friend_request_respond" || action === "friend_request_accept" || action === "friend_request_decline") {
      const reqItem = st.friendRequests[msg.requestId];
      const accept = action === "friend_request_accept"
        ? true
        : action === "friend_request_decline"
          ? false
          : Boolean(msg.accept);
      if (reqItem && reqItem.toUserId === uid) {
        delete st.friendRequests[reqItem.id];
        if (accept) {
          addMutualFriend(st, reqItem.fromUserId, uid);
          notice = { level: "ok", message: `Ahora eres amigo de ${reqItem.fromDisplayName || reqItem.fromUserId}.` };
          pushInbox(st, reqItem.fromUserId, {
            type: "notice",
            level: "ok",
            message: `${st.profiles[uid].displayName} aceptó tu solicitud de amistad.`,
          });
        } else {
          notice = { level: "info", message: "Solicitud de amistad rechazada." };
          pushInbox(st, reqItem.fromUserId, {
            type: "notice",
            level: "info",
            message: `${st.profiles[uid].displayName} rechazó tu solicitud de amistad.`,
          });
        }
      }
    } else if (action === "friend_request_cancel") {
      const reqItem = st.friendRequests[msg.requestId];
      if (reqItem && (reqItem.toUserId === uid || reqItem.fromUserId === uid)) {
        delete st.friendRequests[reqItem.id];
        notice = { level: "info", message: "Solicitud cancelada." };
      }
    } else if (action === "friend_remove") {
      const targetId = normalizeUserId(msg.friendUserId || msg.targetUserId);
      if (targetId) {
        removeMutualFriend(st, uid, targetId);
        notice = { level: "info", message: "Amigo eliminado." };
      }
    } else if (action === "room_presence_update") {
      if (msg.room && msg.room.roomCode) {
        const rawAccess = String(msg.room.roomAccess || "request").trim();
        const roomAccess = rawAccess === "open" || rawAccess === "invite_only" ? rawAccess : "request";
        st.rooms[uid] = {
          roomCode: String(msg.room.roomCode).trim().toUpperCase(),
          roomAccess,
          inviteUrl: String(msg.room.inviteUrl || "").trim(),
          currentTitle: String(msg.room.currentTitle ?? msg.room.trackTitle ?? "").trim().slice(0, 120),
          participantsCount: Number(msg.room.participantsCount ?? msg.room.guestCount ?? 1),
          updatedAt: new Date().toISOString(),
        };
      } else {
        delete st.rooms[uid];
        for (const [reqId, jr] of Object.entries(st.joinRequests || {})) {
          if ((jr.toUserId || jr.hostUserId) === uid) {
            delete st.joinRequests[reqId];
          }
        }
      }
    } else if (action === "room_invite_send") {
      const targetId = normalizeUserId(msg.toUserId || msg.targetUserId);
      const roomCode = String(msg.roomCode || "").trim().toUpperCase();
      const inviteUrl = String(msg.inviteUrl || "").trim();
      if (!targetId || !roomCode || !inviteUrl) {
        notice = { level: "error", message: "Faltan datos para enviar la invitación." };
      } else if (!isOnline(st, targetId)) {
        notice = { level: "error", message: "Tu amigo no está conectado en este momento." };
      } else if (st.rooms?.[targetId]?.roomCode && String(st.rooms[targetId].roomCode).toUpperCase() !== roomCode) {
        notice = { level: "error", message: `${st.profiles[targetId]?.displayName || targetId} está en una sesión activa.` };
      } else {
        const id = `inv_${Date.now()}_${Math.random().toString(36).slice(2, 8)}`;
        const invite = {
          id,
          fromUserId: uid,
          fromDisplayName: st.profiles[uid].displayName,
          fromGoogleAccountName: st.profiles[uid].googleAccountName || "",
          roomCode,
          inviteUrl,
          createdAt: new Date().toISOString(),
        };
        st.roomInvites[id] = {
          ...invite,
          toUserId: targetId,
          createdAtMs: Date.now(),
        };
        pushInbox(st, targetId, { type: "room_invite_received", invite });
        pushInbox(st, targetId, {
          type: "notice",
          level: "info",
          message: `${st.profiles[uid].displayName} te invitó a la sala ${roomCode}.`,
        });
        notice = { level: "ok", message: `Invitación enviada a ${st.profiles[targetId]?.displayName || targetId}.` };
      }
    } else if (action === "room_invite_dismiss") {
      if (msg.inviteId && st.roomInvites[msg.inviteId]?.toUserId === uid) {
        delete st.roomInvites[msg.inviteId];
      }
    } else if (action === "room_join_request") {
      const hostId = normalizeUserId(msg.toUserId || msg.hostUserId);
      const room = st.rooms[hostId];
      if (!hostId || !room || !isOnline(st, hostId)) {
        notice = { level: "error", message: "Ese amigo no tiene una sala abierta en este momento." };
      } else if (!areFriends(st, uid, hostId)) {
        notice = { level: "error", message: "Deben ser amigos para solicitar acceso a la sala." };
      } else if (!room.inviteUrl) {
        notice = { level: "error", message: "La sala del amigo aún no tiene enlace público listo." };
      } else if (room.roomAccess === "invite_only") {
        notice = { level: "error", message: "Esta sala está configurada como solo por invitación." };
      } else if (room.roomAccess === "open") {
        pushInbox(st, uid, {
          type: "room_join_approved",
          hostUserId: hostId,
          hostDisplayName: st.profiles[hostId]?.displayName || hostId,
          roomCode: room.roomCode,
          inviteUrl: room.inviteUrl,
        });
      } else {
        const duplicate = Object.values(st.joinRequests || {}).find(
          (r) => r.fromUserId === uid && (r.toUserId || r.hostUserId) === hostId,
        );
        if (duplicate) {
          notice = { level: "info", message: "Ya enviaste una solicitud para unirte a esta sala." };
        } else {
          const id = `jr_${Date.now()}_${Math.random().toString(36).slice(2, 8)}`;
          const jrPublic = {
            id,
            fromUserId: uid,
            fromDisplayName: st.profiles[uid].displayName,
            fromGoogleAccountName: st.profiles[uid].googleAccountName || "",
            toUserId: hostId,
            roomCode: room.roomCode,
            createdAt: new Date().toISOString(),
          };
          st.joinRequests[id] = {
            ...jrPublic,
            createdAtMs: Date.now(),
          };
          pushInbox(st, hostId, { type: "room_join_request_received", request: jrPublic });
          pushInbox(st, hostId, {
            type: "notice",
            level: "info",
            message: `${st.profiles[uid].displayName} pide unirse a tu sala ${room.roomCode}.`,
          });
          notice = {
            level: "ok",
            message: `Solicitud enviada a ${st.profiles[hostId]?.displayName || hostId}. Esperando aprobación...`,
          };
        }
      }
    } else if (action === "room_join_respond" || action === "room_join_response") {
      const jr = st.joinRequests[msg.requestId];
      const targetHost = jr ? (jr.toUserId || jr.hostUserId) : "";
      if (jr && targetHost === uid) {
        delete st.joinRequests[jr.id];
        const accept = msg.accept !== undefined ? Boolean(msg.accept) : Boolean(msg.approved);
        const inviteUrl = String(msg.inviteUrl || st.rooms[uid]?.inviteUrl || "").trim();
        if (accept && inviteUrl) {
          pushInbox(st, jr.fromUserId, {
            type: "room_join_approved",
            hostUserId: uid,
            hostDisplayName: st.profiles[uid].displayName,
            roomCode: st.rooms[uid]?.roomCode || jr.roomCode,
            inviteUrl,
          });
          notice = { level: "ok", message: `Aceptaste a ${jr.fromDisplayName || jr.fromUserId} en la sala.` };
        } else {
          pushInbox(st, jr.fromUserId, {
            type: "notice",
            level: "error",
            message: `${st.profiles[uid].displayName} rechazó tu solicitud para unirte a la sala.`,
          });
          notice = { level: "info", message: "Solicitud de ingreso rechazada." };
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
