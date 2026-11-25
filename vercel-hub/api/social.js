// Playzi Social Hub on Vercel Serverless Functions (HTTP polling transport).
// Supports mutual friends, friend requests, online presence, open rooms, invites, and join requests.

import { kvDel, kvGetJson, kvMGetJson, kvSetJson, normalizeUserId, setCors } from "../lib/store.js";

const STATE_KEY = "playzi:social:state:v1";
const PRESENCE_TTL_SECS = 35;
const PRESENCE_TTL_MS = PRESENCE_TTL_SECS * 1000;
const USER_ROOM_TTL_SECS = 45;
const ROOM_HOST_STALE_MS = 20_000;
const ROOM_STARTUP_GRACE_MS = 12_000;
const NON_RELAY_ROOM_TTL_MS = 45_000;
const INVITE_TTL_MS = 10 * 60_000;
const JOIN_REQ_TTL_MS = 3 * 60_000;

function presenceKey(uid) {
  return `playzi:presence:v1:${uid}`;
}

function userRoomKey(uid) {
  return `playzi:user_room:v1:${uid}`;
}

function relayRoomKey(code) {
  return `playzi:room:v1:${String(code || "").trim().toUpperCase()}`;
}

function emptySocialState() {
  return {
    profiles: {},        // userId -> { userId, displayName, googleAccountName }
    friends: {},         // userId -> [friendUserId]
    friendRequests: {},  // id -> { id, fromUserId, fromDisplayName, toUserId, toDisplayName, createdAt }
    rooms: {},           // legacy fallback map (auto-migrated/cleaned to per-user keys)
    roomInvites: {},     // id -> { id, fromUserId, fromDisplayName, toUserId, roomCode, inviteUrl, roomAccess, createdAtMs }
    joinRequests: {},    // id -> { id, fromUserId, fromDisplayName, hostUserId, roomCode, createdAtMs }
    inbox: {},           // userId -> [eventObj] (transient one-shot notifications like join approval/rejection)
  };
}

function isRelayRoomUrl(url) {
  const u = String(url || "").trim().toLowerCase();
  return !u || u.includes("/api/room") || u.includes(".vercel.app");
}

function normalizeRoomPayload(room, now) {
  if (!room || !room.roomCode) return null;
  const code = String(room.roomCode).trim().toUpperCase();
  if (!code) return null;
  const rawAccess = String(room.roomAccess || "request").trim();
  const roomAccess = rawAccess === "open" || rawAccess === "invite_only" ? rawAccess : "request";
  return {
    roomCode: code,
    roomAccess,
    inviteUrl: String(room.inviteUrl || "").trim(),
    currentTitle: String(room.currentTitle ?? room.trackTitle ?? "").trim().slice(0, 120),
    participantsCount: Number(room.participantsCount ?? room.guestCount ?? 1),
    updatedAt: new Date(now).toISOString(),
    updatedAtMs: now,
  };
}

function isRoomLive(room, relayRoom, now) {
  if (!room || !room.roomCode) return false;
  const updatedAtMs = Number(room.updatedAtMs || Date.parse(room.updatedAt || "") || 0);
  if (isRelayRoomUrl(room.inviteUrl)) {
    if (relayRoom) {
      if (relayRoom.closed) return false;
      const hostLastSeen = Number(relayRoom.hostLastSeenMs || 0);
      return hostLastSeen > 0 && (now - hostLastSeen) <= ROOM_HOST_STALE_MS;
    }
    // Allow a brief grace window right after room creation before the first host_poll lands.
    return updatedAtMs > 0 && (now - updatedAtMs) <= ROOM_STARTUP_GRACE_MS;
  }
  return updatedAtMs > 0 && (now - updatedAtMs) <= NON_RELAY_ROOM_TTL_MS;
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

async function loadLivePresenceAndRooms(st, userIds, now) {
  const uniqueIds = Array.from(new Set(userIds.filter(Boolean)));
  const onlineMap = new Map();
  const roomMap = new Map();
  let stateDirty = false;

  if (!uniqueIds.length) {
    return { onlineMap, roomMap, stateDirty };
  }

  const keys = [
    ...uniqueIds.map((id) => presenceKey(id)),
    ...uniqueIds.map((id) => userRoomKey(id)),
  ];
  const values = await kvMGetJson(keys);
  const n = uniqueIds.length;

  const candidateRooms = new Map(); // uid -> { room, fromUserKey }
  for (let i = 0; i < n; i++) {
    const id = uniqueIds[i];
    const pres = values[i];
    const online = Boolean(pres && (now - Number(pres.lastSeenMs || 0)) <= PRESENCE_TTL_MS);
    onlineMap.set(id, online);

    const userRoom = values[n + i];
    const legacyRoom = st.rooms?.[id] || null;
    if (online) {
      if (userRoom && userRoom.roomCode) {
        candidateRooms.set(id, { room: userRoom, fromUserKey: true });
      } else if (legacyRoom && legacyRoom.roomCode) {
        candidateRooms.set(id, { room: legacyRoom, fromUserKey: false });
      }
    } else if (legacyRoom) {
      delete st.rooms[id];
      stateDirty = true;
    }
  }

  // Batch-verify all candidate Vercel relay rooms against playzi:room:v1:<CODE>
  const relayEntries = [];
  for (const [id, entry] of candidateRooms.entries()) {
    if (isRelayRoomUrl(entry.room.inviteUrl)) {
      relayEntries.push({ id, code: String(entry.room.roomCode).trim().toUpperCase() });
    }
  }
  const relayRoomByUser = new Map();
  if (relayEntries.length) {
    const relayValues = await kvMGetJson(relayEntries.map((e) => relayRoomKey(e.code)));
    for (let i = 0; i < relayEntries.length; i++) {
      relayRoomByUser.set(relayEntries[i].id, relayValues[i]);
    }
  }

  const staleDeletes = [];
  for (const [id, entry] of candidateRooms.entries()) {
    const relayRoom = relayRoomByUser.get(id) || null;
    if (isRoomLive(entry.room, relayRoom, now)) {
      roomMap.set(id, entry.room);
    } else {
      if (entry.fromUserKey) {
        staleDeletes.push(kvDel(userRoomKey(id)));
      }
      if (st.rooms?.[id]) {
        delete st.rooms[id];
        stateDirty = true;
      }
    }
  }
  if (staleDeletes.length) {
    await Promise.all(staleDeletes);
  }

  return { onlineMap, roomMap, stateDirty };
}

function pruneTransientCollections(st, roomMap, now) {
  let dirty = false;
  for (const [id, inv] of Object.entries(st.roomInvites || {})) {
    if (now - Number(inv?.createdAtMs || 0) > INVITE_TTL_MS) {
      delete st.roomInvites[id];
      dirty = true;
    }
  }
  for (const [id, req] of Object.entries(st.joinRequests || {})) {
    const hostId = req?.toUserId || req?.hostUserId || "";
    const expired = now - Number(req?.createdAtMs || 0) > JOIN_REQ_TTL_MS;
    const hostChecked = hostId && roomMap.has(hostId);
    const hostRoom = hostChecked ? roomMap.get(hostId) : null;
    const roomMissing = hostChecked && (!hostRoom || (req?.roomCode && hostRoom.roomCode !== req.roomCode));
    if (expired || roomMissing) {
      delete st.joinRequests[id];
      dirty = true;
    }
  }
  return dirty;
}

function buildSyncFor(st, uid, onlineMap, roomMap) {
  const friendIds = Array.isArray(st.friends?.[uid]) ? st.friends[uid] : [];
  const friends = friendIds.map((fid) => {
    const prof = st.profiles?.[fid] || {};
    const online = Boolean(onlineMap.get(fid));
    const room = online ? (roomMap.get(fid) || null) : null;
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

  const myActiveRoom = roomMap.get(uid) || null;
  const incomingJoinRequests = myActiveRoom
    ? Object.values(st.joinRequests || {})
        .filter((req) => {
          if ((req.toUserId || req.hostUserId) !== uid) return false;
          return !req.roomCode || req.roomCode === myActiveRoom.roomCode;
        })
        .map(({ createdAtMs, hostUserId, ...rest }) => ({
          id: rest.id,
          fromUserId: rest.fromUserId,
          fromDisplayName: rest.fromDisplayName || st.profiles?.[rest.fromUserId]?.displayName || rest.fromUserId,
          fromGoogleAccountName: rest.fromGoogleAccountName ?? st.profiles?.[rest.fromUserId]?.googleAccountName ?? "",
          toUserId: rest.toUserId || hostUserId || uid,
          roomCode: rest.roomCode || "",
          createdAt: rest.createdAt || "",
        }))
    : [];

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

    const now = Date.now();
    const action = String(body.action || body.type || "sync").trim();
    const msg = body.payload || body;
    const isDisconnect = action === "disconnect" || action === "client_disconnect" || action === "offline";

    const st = (await kvGetJson(STATE_KEY, null)) || emptySocialState();
    if (!st.profiles) st.profiles = {};
    if (!st.friends) st.friends = {};
    if (!st.friendRequests) st.friendRequests = {};
    if (!st.rooms) st.rooms = {};
    if (!st.roomInvites) st.roomInvites = {};
    if (!st.joinRequests) st.joinRequests = {};
    if (!st.inbox) st.inbox = {};

    let stateDirty = false;

    const prevProf = st.profiles[uid] || {};
    const nextDisplayName = (action === "profile_update" && msg.displayName !== undefined
      ? String(msg.displayName || "").trim()
      : String(body.displayName || "").trim()) || prevProf.displayName || uid;
    const nextGoogleName = action === "profile_update" && msg.googleAccountName !== undefined
      ? String(msg.googleAccountName || "").trim()
      : String(body.googleAccountName ?? prevProf.googleAccountName ?? "").trim();

    if (
      prevProf.userId !== uid
      || prevProf.displayName !== nextDisplayName
      || (prevProf.googleAccountName || "") !== nextGoogleName
    ) {
      st.profiles[uid] = {
        userId: uid,
        displayName: nextDisplayName,
        googleAccountName: nextGoogleName,
      };
      stateDirty = true;
    }

    if (isDisconnect) {
      await Promise.all([kvDel(presenceKey(uid)), kvDel(userRoomKey(uid))]);
      if (st.rooms[uid]) {
        delete st.rooms[uid];
        stateDirty = true;
      }
      for (const [reqId, jr] of Object.entries(st.joinRequests || {})) {
        if ((jr.toUserId || jr.hostUserId) === uid || jr.fromUserId === uid) {
          delete st.joinRequests[reqId];
          stateDirty = true;
        }
      }
      if (stateDirty) {
        await kvSetJson(STATE_KEY, st);
      }
      return res.status(200).json({ ok: true });
    }

    // Atomic per-user presence heartbeat (auto-expires in Redis after PRESENCE_TTL_SECS)
    await kvSetJson(presenceKey(uid), { lastSeenMs: now }, PRESENCE_TTL_SECS);

    // Handle explicit room presence updates or authoritative sync room state atomically per user
    const hasRoomUpdate = action === "room_presence_update" || (action === "sync" && body.hasRoomState === true);
    if (hasRoomUpdate) {
      const rawRoom = action === "room_presence_update" ? msg.room : body.room;
      const normalized = normalizeRoomPayload(rawRoom, now);
      if (normalized) {
        await kvSetJson(userRoomKey(uid), normalized, USER_ROOM_TTL_SECS);
        if (st.rooms[uid]) {
          delete st.rooms[uid];
          stateDirty = true;
        }
      } else {
        await kvDel(userRoomKey(uid));
        if (st.rooms[uid]) {
          delete st.rooms[uid];
          stateDirty = true;
        }
        for (const [reqId, jr] of Object.entries(st.joinRequests || {})) {
          if ((jr.toUserId || jr.hostUserId) === uid) {
            delete st.joinRequests[reqId];
            stateDirty = true;
          }
        }
      }
    }

    const friendIds = Array.isArray(st.friends?.[uid]) ? st.friends[uid] : [];
    const targetCandidate = normalizeUserId(msg.toUserId || msg.targetUserId || msg.hostUserId || msg.friendUserId);
    const idsToCheck = [uid, ...friendIds];
    if (targetCandidate) idsToCheck.push(targetCandidate);

    const { onlineMap, roomMap, stateDirty: liveDirty } = await loadLivePresenceAndRooms(st, idsToCheck, now);
    if (liveDirty) stateDirty = true;
    if (pruneTransientCollections(st, roomMap, now)) stateDirty = true;

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
          stateDirty = true;
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
            stateDirty = true;
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
        stateDirty = true;
      }
    } else if (action === "friend_request_cancel") {
      const reqItem = st.friendRequests[msg.requestId];
      if (reqItem && (reqItem.toUserId === uid || reqItem.fromUserId === uid)) {
        delete st.friendRequests[reqItem.id];
        notice = { level: "info", message: "Solicitud cancelada." };
        stateDirty = true;
      }
    } else if (action === "friend_remove") {
      const targetId = normalizeUserId(msg.friendUserId || msg.targetUserId);
      if (targetId) {
        removeMutualFriend(st, uid, targetId);
        notice = { level: "info", message: "Amigo eliminado." };
        stateDirty = true;
      }
    } else if (action === "room_invite_send") {
      const targetId = normalizeUserId(msg.toUserId || msg.targetUserId);
      const roomCode = String(msg.roomCode || "").trim().toUpperCase();
      const inviteUrl = String(msg.inviteUrl || "").trim();
      const targetRoom = roomMap.get(targetId) || null;
      if (!targetId || !roomCode || !inviteUrl) {
        notice = { level: "error", message: "Faltan datos para enviar la invitación." };
      } else if (!onlineMap.get(targetId)) {
        notice = { level: "error", message: "Tu amigo no está conectado en este momento." };
      } else if (targetRoom?.roomCode && String(targetRoom.roomCode).toUpperCase() !== roomCode) {
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
          createdAtMs: now,
        };
        pushInbox(st, targetId, { type: "room_invite_received", invite });
        pushInbox(st, targetId, {
          type: "notice",
          level: "info",
          message: `${st.profiles[uid].displayName} te invitó a la sala ${roomCode}.`,
        });
        notice = { level: "ok", message: `Invitación enviada a ${st.profiles[targetId]?.displayName || targetId}.` };
        stateDirty = true;
      }
    } else if (action === "room_invite_dismiss") {
      if (msg.inviteId && st.roomInvites[msg.inviteId]?.toUserId === uid) {
        delete st.roomInvites[msg.inviteId];
        stateDirty = true;
      }
    } else if (action === "room_join_request") {
      const hostId = normalizeUserId(msg.toUserId || msg.hostUserId);
      const room = roomMap.get(hostId) || null;
      if (!hostId || !room || !onlineMap.get(hostId)) {
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
        stateDirty = true;
      } else {
        const duplicate = Object.values(st.joinRequests || {}).find(
          (r) => r.fromUserId === uid && (r.toUserId || r.hostUserId) === hostId && (!r.roomCode || r.roomCode === room.roomCode),
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
            createdAtMs: now,
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
          stateDirty = true;
        }
      }
    } else if (action === "room_join_respond" || action === "room_join_response") {
      const jr = st.joinRequests[msg.requestId];
      const targetHost = jr ? (jr.toUserId || jr.hostUserId) : "";
      if (jr && targetHost === uid) {
        delete st.joinRequests[jr.id];
        stateDirty = true;
        const accept = msg.accept !== undefined ? Boolean(msg.accept) : Boolean(msg.approved);
        const activeRoom = roomMap.get(uid) || null;
        const roomMatches = Boolean(activeRoom && (!jr.roomCode || activeRoom.roomCode === jr.roomCode));
        const inviteUrl = roomMatches ? String(msg.inviteUrl || activeRoom.inviteUrl || "").trim() : "";
        if (accept && roomMatches && inviteUrl) {
          pushInbox(st, jr.fromUserId, {
            type: "room_join_approved",
            hostUserId: uid,
            hostDisplayName: st.profiles[uid].displayName,
            roomCode: activeRoom.roomCode || jr.roomCode,
            inviteUrl,
          });
          notice = { level: "ok", message: `Aceptaste a ${jr.fromDisplayName || jr.fromUserId} en la sala.` };
        } else if (accept) {
          pushInbox(st, jr.fromUserId, {
            type: "notice",
            level: "error",
            message: "La sesión ya no está activa.",
          });
          notice = { level: "error", message: "No tienes una sesión activa para aceptar esta solicitud." };
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
      stateDirty = true;
    }

    if (stateDirty) {
      await kvSetJson(STATE_KEY, st);
    }

    return res.status(200).json({
      ok: true,
      sync: buildSyncFor(st, uid, onlineMap, roomMap),
      events,
      notice,
    });
  } catch (err) {
    return res.status(500).json({ error: String(err?.message || err) });
  }
}
