// Playzi Watch Party Relay + Social Hub (Cloudflare Workers + Durable Objects)

const CORS_HEADERS = {
  "Access-Control-Allow-Origin": "*",
  "Access-Control-Allow-Methods": "GET, POST, OPTIONS",
  "Access-Control-Allow-Headers": "Content-Type, X-Playzi-Auth, X-Playzi-Host-Secret",
};

function jsonResponse(data, status = 200) {
  return new Response(JSON.stringify(data), {
    status,
    headers: {
      "Content-Type": "application/json",
      ...CORS_HEADERS,
    },
  });
}

const SAFE_USER_ID_RE = /^[A-Z0-9_-]{1,80}$/;

function normalizeUserId(raw) {
  return String(raw || "").trim().toUpperCase();
}

function safeSend(ws, obj) {
  try {
    ws.send(typeof obj === "string" ? obj : JSON.stringify(obj));
    return true;
  } catch (_) {
    return false;
  }
}

async function hashSecret(raw) {
  const bytes = new TextEncoder().encode(String(raw || ""));
  const digest = await crypto.subtle.digest("SHA-256", bytes);
  return Array.from(new Uint8Array(digest), (b) => b.toString(16).padStart(2, "0")).join("");
}

function timingSafeEqualStr(a, b) {
  const sa = String(a || "");
  const sb = String(b || "");
  if (sa.length !== sb.length || sa.length === 0) return false;
  let diff = 0;
  for (let i = 0; i < sa.length; i += 1) {
    diff |= sa.charCodeAt(i) ^ sb.charCodeAt(i);
  }
  return diff === 0;
}

export default {
  async fetch(request, env) {
    const url = new URL(request.url);

    if (request.method === "OPTIONS") {
      return new Response(null, { status: 204, headers: CORS_HEADERS });
    }

    if (url.pathname === "/" || url.pathname === "/health") {
      return jsonResponse({
        ok: true,
        service: "playzi-hub",
        endpoints: {
          social: "/social/ws",
          roomRelay: "/ws?room=CODE",
        },
      });
    }

    if (url.pathname === "/social/ws") {
      const id = env.SOCIAL_HUB.idFromName("global");
      const stub = env.SOCIAL_HUB.get(id);
      return stub.fetch(request);
    }

    if (url.pathname === "/ws") {
      const room = String(url.searchParams.get("room") || "").trim().toUpperCase();
      if (!room) {
        return jsonResponse({ error: "Missing room query parameter" }, 400);
      }
      const id = env.PARTY_ROOM.idFromName(room);
      const stub = env.PARTY_ROOM.get(id);
      return stub.fetch(request);
    }

    return jsonResponse({ error: "Not found" }, 404);
  },
};

// ── Watch Party Room Relay Durable Object ───────────────────────────────────

export class PartyRoom {
  constructor(ctx, env) {
    this.ctx = ctx;
    this.env = env;
    this.hostSocket = null;
    this.hostSecretHash = null;
    this.guests = new Map(); // connId -> WebSocket
  }

  async fetch(request) {
    if (request.headers.get("Upgrade")?.toLowerCase() !== "websocket") {
      return jsonResponse({ error: "Expected WebSocket upgrade" }, 426);
    }

    const url = new URL(request.url);
    const role = String(url.searchParams.get("role") || "guest").toLowerCase();

    let hostSecretHash = null;
    if (role === "host") {
      const rawSecret = String(
        request.headers.get("X-Playzi-Host-Secret")
        || url.searchParams.get("hostSecret")
        || ""
      ).trim();
      if (rawSecret.length < 16) {
        return jsonResponse({ error: "Missing or invalid hostSecret" }, 401);
      }
      hostSecretHash = await hashSecret(rawSecret);
      if (
        this.hostSocket
        && this.hostSocket.readyState === 1
        && this.hostSecretHash
        && !timingSafeEqualStr(this.hostSecretHash, hostSecretHash)
      ) {
        return jsonResponse({ error: "Forbidden: invalid hostSecret" }, 403);
      }
    }

    const pair = new WebSocketPair();
    const [client, server] = Object.values(pair);
    server.accept();

    if (role === "host") {
      this.attachHost(server, hostSecretHash);
    } else {
      this.attachGuest(server);
    }

    return new Response(null, { status: 101, webSocket: client });
  }

  attachHost(ws, secretHash) {
    if (this.hostSocket && this.hostSocket !== ws && this.hostSocket.readyState === 1) {
      if (this.hostSecretHash && timingSafeEqualStr(this.hostSecretHash, secretHash)) {
        const prev = this.hostSocket;
        this.hostSocket = ws;
        this.hostSecretHash = secretHash;
        try {
          prev.close(1000, "Host reconnected");
        } catch (_) {}
      } else {
        safeSend(ws, {
          type: "room_closed",
          reason: "La sala ya tiene un anfitrión conectado.",
        });
        try {
          ws.close(4003, "Room already has an active host");
        } catch (_) {}
        return;
      }
    } else {
      this.hostSocket = ws;
      this.hostSecretHash = secretHash;
    }

    ws.addEventListener("message", (event) => {
      let msg;
      try {
        msg = JSON.parse(String(event.data || ""));
      } catch (_) {
        return;
      }

      if (msg.relayPing) {
        safeSend(ws, { relayPong: Date.now() });
        return;
      }

      if (msg.relayKick) {
        const target = this.guests.get(String(msg.relayKick));
        if (target) {
          this.guests.delete(String(msg.relayKick));
          try {
            target.close(1000, "Removed by host");
          } catch (_) {}
        }
        return;
      }

      if (msg.relayTo && typeof msg.payload === "string") {
        if (msg.relayTo === "all") {
          for (const [connId, guestWs] of this.guests.entries()) {
            if (!safeSend(guestWs, msg.payload)) {
              this.guests.delete(connId);
            }
          }
        } else {
          const guestWs = this.guests.get(String(msg.relayTo));
          if (guestWs && !safeSend(guestWs, msg.payload)) {
            this.guests.delete(String(msg.relayTo));
          }
        }
      }
    });

    const onHostClose = () => {
      if (this.hostSocket !== ws) return;
      this.hostSocket = null;
      this.hostSecretHash = null;
      const closeMsg = JSON.stringify({
        type: "room_closed",
        reason: "El host cerró la sesión.",
      });
      for (const guestWs of this.guests.values()) {
        safeSend(guestWs, closeMsg);
        try {
          guestWs.close(1000, "Host disconnected");
        } catch (_) {}
      }
      this.guests.clear();
    };

    ws.addEventListener("close", onHostClose);
    ws.addEventListener("error", onHostClose);
  }

  attachGuest(ws) {
    if (!this.hostSocket) {
      safeSend(ws, {
        type: "room_closed",
        reason: "La sala no está abierta o el host no está conectado.",
      });
      try {
        ws.close(1000, "Room not active");
      } catch (_) {}
      return;
    }

    const connId = crypto.randomUUID();
    this.guests.set(connId, ws);
    safeSend(this.hostSocket, { relayOpen: connId });

    ws.addEventListener("message", (event) => {
      if (!this.hostSocket) {
        safeSend(ws, {
          type: "room_closed",
          reason: "El host se desconectó.",
        });
        try {
          ws.close(1000, "Host gone");
        } catch (_) {}
        return;
      }
      if (typeof event.data === "string") {
        safeSend(this.hostSocket, {
          relayFrom: connId,
          payload: event.data,
        });
      }
    });

    const onGuestClose = () => {
      if (this.guests.delete(connId) && this.hostSocket) {
        safeSend(this.hostSocket, { relayClosed: connId });
      }
    };

    ws.addEventListener("close", onGuestClose);
    ws.addEventListener("error", onGuestClose);
  }
}

// ── Social Hub Durable Object ───────────────────────────────────────────────

export class SocialHub {
  constructor(ctx, env) {
    this.ctx = ctx;
    this.env = env;
    this.loaded = false;
    this.users = {}; // userId -> { userId, displayName, googleAccountName, updatedAt }
    this.authTokens = {}; // userId -> sha256(authToken)
    this.friendships = {}; // userId -> string[]
    this.friendRequests = []; // { id, fromUserId, fromDisplayName, fromGoogleAccountName, toUserId, createdAt }
    this.sockets = new Map(); // userId -> Set<WebSocket>
    this.rooms = new Map(); // userId -> { roomCode, inviteUrl, roomAccess, participantsCount, currentTitle, updatedAt }
    this.joinRequests = new Map(); // requestId -> { id, fromUserId, fromDisplayName, fromGoogleAccountName, toUserId, roomCode, createdAt }
  }

  async ensureLoaded() {
    if (this.loaded) return;
    const [sharded, legacyUsers, legacyAuth, legacyFriends, friendRequests] = await Promise.all([
      this.ctx.storage.list(),
      this.ctx.storage.get("users"),
      this.ctx.storage.get("authTokens"),
      this.ctx.storage.get("friendships"),
      this.ctx.storage.get("friendRequests"),
    ]);

    this.users = {};
    this.authTokens = {};
    this.friendships = {};

    let hasSharded = false;
    for (const [k, v] of sharded.entries()) {
      if (k.startsWith("user:")) {
        this.users[k.slice(5)] = v;
        hasSharded = true;
      } else if (k.startsWith("auth:")) {
        this.authTokens[k.slice(5)] = v;
        hasSharded = true;
      } else if (k.startsWith("friends:")) {
        this.friendships[k.slice(8)] = Array.isArray(v) ? v : [];
        hasSharded = true;
      }
    }

    if (!hasSharded) {
      if (legacyUsers && typeof legacyUsers === "object") this.users = legacyUsers;
      if (legacyAuth && typeof legacyAuth === "object") this.authTokens = legacyAuth;
      if (legacyFriends && typeof legacyFriends === "object") this.friendships = legacyFriends;
      const migrated = {};
      for (const [uid, val] of Object.entries(this.users)) migrated[`user:${uid}`] = val;
      for (const [uid, val] of Object.entries(this.authTokens)) migrated[`auth:${uid}`] = val;
      for (const [uid, val] of Object.entries(this.friendships)) migrated[`friends:${uid}`] = val;
      if (Object.keys(migrated).length > 0) {
        const entries = Object.entries(migrated);
        for (let i = 0; i < entries.length; i += 100) {
          await this.ctx.storage.put(Object.fromEntries(entries.slice(i, i + 100)));
        }
        await this.ctx.storage.delete(["users", "authTokens", "friendships"]);
      }
    }

    this.friendRequests = Array.isArray(friendRequests) ? friendRequests : [];
    this.loaded = true;
  }

  async persistUser(userId) {
    const batch = {};
    if (Object.prototype.hasOwnProperty.call(this.users, userId)) {
      batch[`user:${userId}`] = this.users[userId];
    }
    if (Object.prototype.hasOwnProperty.call(this.authTokens, userId)) {
      batch[`auth:${userId}`] = this.authTokens[userId];
    }
    if (Object.prototype.hasOwnProperty.call(this.friendships, userId)) {
      batch[`friends:${userId}`] = this.friendships[userId];
    }
    if (Object.keys(batch).length > 0) {
      await this.ctx.storage.put(batch);
    }
  }

  async persistFriendRequests() {
    await this.ctx.storage.put("friendRequests", this.friendRequests);
  }

  async persist(...affectedUserIds) {
    const uniqueIds = Array.from(new Set(affectedUserIds.filter(Boolean)));
    if (uniqueIds.length > 0) {
      const batch = { friendRequests: this.friendRequests };
      for (const uid of uniqueIds) {
        if (Object.prototype.hasOwnProperty.call(this.users, uid)) batch[`user:${uid}`] = this.users[uid];
        if (Object.prototype.hasOwnProperty.call(this.authTokens, uid)) batch[`auth:${uid}`] = this.authTokens[uid];
        if (Object.prototype.hasOwnProperty.call(this.friendships, uid)) batch[`friends:${uid}`] = this.friendships[uid];
      }
      await this.ctx.storage.put(batch);
      return;
    }
    await this.persistFriendRequests();
  }

  isOnline(userId) {
    const set = this.sockets.get(userId);
    return Boolean(set && set.size > 0);
  }

  sendToUser(userId, message) {
    const set = this.sockets.get(userId);
    if (!set || set.size === 0) return false;
    let delivered = false;
    for (const ws of Array.from(set)) {
      if (safeSend(ws, message)) {
        delivered = true;
      } else {
        set.delete(ws);
      }
    }
    if (set.size === 0) {
      this.sockets.delete(userId);
    }
    return delivered;
  }

  areFriends(a, b) {
    const list = this.friendships[a];
    return Array.isArray(list) && list.includes(b);
  }

  addMutualFriend(a, b) {
    if (!Array.isArray(this.friendships[a])) this.friendships[a] = [];
    if (!Array.isArray(this.friendships[b])) this.friendships[b] = [];
    if (!this.friendships[a].includes(b)) this.friendships[a].push(b);
    if (!this.friendships[b].includes(a)) this.friendships[b].push(a);
  }

  removeMutualFriend(a, b) {
    if (Array.isArray(this.friendships[a])) {
      this.friendships[a] = this.friendships[a].filter((x) => x !== b);
    }
    if (Array.isArray(this.friendships[b])) {
      this.friendships[b] = this.friendships[b].filter((x) => x !== a);
    }
  }

  buildSyncFor(userId) {
    const friendIds = Array.isArray(this.friendships[userId]) ? this.friendships[userId] : [];
    const friends = friendIds.map((fid) => {
      const profile = this.users[fid] || {
        userId: fid,
        displayName: fid,
        googleAccountName: "",
      };
      const online = this.isOnline(fid);
      const rawRoom = online ? this.rooms.get(fid) || null : null;
      const room = rawRoom
        ? {
            ...rawRoom,
            inviteUrl: rawRoom.roomAccess === "open" ? (rawRoom.inviteUrl || "") : "",
          }
        : null;
      return {
        userId: fid,
        displayName: profile.displayName || fid,
        googleAccountName: profile.googleAccountName || "",
        online,
        room,
      };
    });

    const incomingFriendRequests = this.friendRequests
      .filter((r) => r.toUserId === userId)
      .map((r) => {
        const sender = this.users[r.fromUserId];
        return {
          ...r,
          fromDisplayName: sender?.displayName || r.fromDisplayName || r.fromUserId,
          fromGoogleAccountName: sender?.googleAccountName || r.fromGoogleAccountName || "",
        };
      });

    const outgoingFriendRequests = this.friendRequests
      .filter((r) => r.fromUserId === userId)
      .map((r) => {
        const target = this.users[r.toUserId];
        return {
          ...r,
          toDisplayName: target?.displayName || r.toUserId,
          toGoogleAccountName: target?.googleAccountName || "",
        };
      });

    const myRoom = this.rooms.get(userId) || null;
    const incomingJoinRequests = myRoom
      ? Array.from(this.joinRequests.values()).filter(
          (r) => r.toUserId === userId && (!r.roomCode || r.roomCode === myRoom.roomCode)
        )
      : [];

    return {
      type: "social_sync",
      friends,
      incomingFriendRequests,
      outgoingFriendRequests,
      incomingJoinRequests,
    };
  }

  syncUser(userId) {
    if (!this.isOnline(userId)) return;
    this.sendToUser(userId, this.buildSyncFor(userId));
  }

  syncFriendsOf(userId) {
    const friendIds = Array.isArray(this.friendships[userId]) ? this.friendships[userId] : [];
    for (const fid of friendIds) {
      this.syncUser(fid);
    }
  }

  async fetch(request) {
    await this.ensureLoaded();

    if (request.headers.get("Upgrade")?.toLowerCase() !== "websocket") {
      return jsonResponse({ error: "Expected WebSocket upgrade" }, 426);
    }

    const url = new URL(request.url);
    const userId = normalizeUserId(url.searchParams.get("userId"));
    if (!userId) {
      return jsonResponse({ error: "Missing userId" }, 400);
    }
    if (!SAFE_USER_ID_RE.test(userId)) {
      return jsonResponse({ error: "Invalid userId" }, 400);
    }

    const rawAuthToken = String(
      request.headers.get("X-Playzi-Auth")
      || url.searchParams.get("authToken")
      || ""
    ).trim();
    if (rawAuthToken.length < 16) {
      return jsonResponse({ error: "Missing or invalid authToken" }, 401);
    }
    const tokenHash = await hashSecret(rawAuthToken);
    const storedHash = Object.prototype.hasOwnProperty.call(this.authTokens, userId)
      ? this.authTokens[userId]
      : null;
    if (storedHash) {
      if (!timingSafeEqualStr(storedHash, tokenHash)) {
        return jsonResponse({ error: "Invalid authToken for userId" }, 403);
      }
    } else {
      this.authTokens[userId] = tokenHash;
    }

    const displayName = String(url.searchParams.get("displayName") || userId)
      .trim()
      .slice(0, 48) || userId;
    const googleAccountName = String(url.searchParams.get("googleAccountName") || "")
      .trim()
      .slice(0, 80);

    const pair = new WebSocketPair();
    const [client, server] = Object.values(pair);
    server.accept();

    this.users[userId] = {
      userId,
      displayName,
      googleAccountName,
      updatedAt: new Date().toISOString(),
    };
    await this.persistUser(userId);

    if (!this.sockets.has(userId)) {
      this.sockets.set(userId, new Set());
    }
    this.sockets.get(userId).add(server);

    this.syncUser(userId);
    this.syncFriendsOf(userId);

    server.addEventListener("message", (event) => {
      this.handleMessage(userId, server, String(event.data || "")).catch((err) => {
        safeSend(server, {
          type: "notice",
          level: "error",
          message: `Error en Hub Social: ${err?.message || err}`,
        });
      });
    });

    const onClose = () => {
      const set = this.sockets.get(userId);
      if (set) {
        set.delete(server);
        if (set.size === 0) {
          this.sockets.delete(userId);
          this.rooms.delete(userId);
          for (const [reqId, jr] of this.joinRequests.entries()) {
            if (jr.toUserId === userId || jr.fromUserId === userId) {
              this.joinRequests.delete(reqId);
            }
          }
          this.syncFriendsOf(userId);
        }
      }
    };

    server.addEventListener("close", onClose);
    server.addEventListener("error", onClose);

    return new Response(null, { status: 101, webSocket: client });
  }

  async handleMessage(userId, ws, raw) {
    let msg;
    try {
      msg = JSON.parse(raw);
    } catch (_) {
      return;
    }

    const type = msg?.type;
    if (!type) return;

    if (type === "ping") {
      safeSend(ws, { type: "pong", ts: Date.now() });
      return;
    }

    if (type === "sync") {
      this.syncUser(userId);
      return;
    }

    if (type === "client_disconnect" || type === "disconnect") {
      const set = this.sockets.get(userId);
      if (set) {
        set.delete(ws);
        if (set.size === 0) {
          this.sockets.delete(userId);
        }
      }
      this.rooms.delete(userId);
      for (const [reqId, jr] of this.joinRequests.entries()) {
        if (jr.toUserId === userId || jr.fromUserId === userId) {
          this.joinRequests.delete(reqId);
        }
      }
      this.syncFriendsOf(userId);
      return;
    }

    if (type === "profile_update") {
      const displayName = String(msg.displayName || "").trim().slice(0, 48);
      const googleAccountName = String(msg.googleAccountName || "").trim().slice(0, 80);
      const existing = this.users[userId] || { userId, displayName: userId, googleAccountName: "" };
      this.users[userId] = {
        userId,
        displayName: displayName || existing.displayName || userId,
        googleAccountName: googleAccountName || existing.googleAccountName || "",
        updatedAt: new Date().toISOString(),
      };
      await this.persistUser(userId);
      this.syncUser(userId);
      this.syncFriendsOf(userId);
      return;
    }

    if (type === "friend_request_send") {
      const toUserId = normalizeUserId(msg.toUserId);
      if (!toUserId) {
        safeSend(ws, { type: "notice", level: "error", message: "Ingresa el ID de Playzi de tu amigo." });
        return;
      }
      if (toUserId === userId) {
        safeSend(ws, { type: "notice", level: "error", message: "No puedes enviarte una solicitud a ti mismo." });
        return;
      }
      if (this.areFriends(userId, toUserId)) {
        safeSend(ws, { type: "notice", level: "info", message: "Ese usuario ya está en tu lista de amigos." });
        return;
      }

      // If the target already sent us a request, accept it automatically!
      const reverse = this.friendRequests.find(
        (r) => r.fromUserId === toUserId && r.toUserId === userId
      );
      if (reverse) {
        this.friendRequests = this.friendRequests.filter((r) => r.id !== reverse.id);
        this.addMutualFriend(userId, toUserId);
        await this.persist(userId, toUserId);
        this.syncUser(userId);
        this.syncUser(toUserId);
        safeSend(ws, { type: "notice", level: "ok", message: "Solicitud mutua aceptada. ¡Ya son amigos!" });
        this.sendToUser(toUserId, {
          type: "notice",
          level: "ok",
          message: `${this.users[userId]?.displayName || userId} aceptó tu solicitud de amistad.`,
        });
        return;
      }

      const duplicate = this.friendRequests.find(
        (r) => r.fromUserId === userId && r.toUserId === toUserId
      );
      if (duplicate) {
        safeSend(ws, { type: "notice", level: "info", message: "Ya enviaste una solicitud pendiente a ese usuario." });
        return;
      }

      const sender = this.users[userId] || { userId, displayName: userId, googleAccountName: "" };
      const req = {
        id: crypto.randomUUID(),
        fromUserId: userId,
        fromDisplayName: sender.displayName || userId,
        fromGoogleAccountName: sender.googleAccountName || "",
        toUserId,
        createdAt: new Date().toISOString(),
      };
      this.friendRequests.push(req);
      await this.persist();
      this.syncUser(userId);
      this.syncUser(toUserId);

      safeSend(ws, {
        type: "notice",
        level: "ok",
        message: `Solicitud de amistad enviada a ${this.users[toUserId]?.displayName || toUserId}.`,
      });
      this.sendToUser(toUserId, {
        type: "notice",
        level: "info",
        message: `${sender.displayName || userId} te envió una solicitud de amistad.`,
      });
      return;
    }

    if (type === "friend_request_respond") {
      const requestId = String(msg.requestId || "").trim();
      const accept = Boolean(msg.accept);
      const idx = this.friendRequests.findIndex(
        (r) => r.id === requestId && r.toUserId === userId
      );
      if (idx === -1) {
        safeSend(ws, { type: "notice", level: "error", message: "La solicitud ya no existe." });
        this.syncUser(userId);
        return;
      }
      const [req] = this.friendRequests.splice(idx, 1);
      const me = this.users[userId] || { userId, displayName: userId };

      if (accept) {
        this.addMutualFriend(userId, req.fromUserId);
        await this.persist(userId, req.fromUserId);
        this.syncUser(userId);
        this.syncUser(req.fromUserId);
        safeSend(ws, {
          type: "notice",
          level: "ok",
          message: `Ahora eres amigo de ${req.fromDisplayName || req.fromUserId}.`,
        });
        this.sendToUser(req.fromUserId, {
          type: "notice",
          level: "ok",
          message: `${me.displayName || userId} aceptó tu solicitud de amistad.`,
        });
      } else {
        await this.persist();
        this.syncUser(userId);
        this.syncUser(req.fromUserId);
        safeSend(ws, { type: "notice", level: "info", message: "Solicitud de amistad rechazada." });
      }
      return;
    }

    if (type === "friend_request_cancel") {
      const requestId = String(msg.requestId || "").trim();
      const idx = this.friendRequests.findIndex(
        (r) => r.id === requestId && r.fromUserId === userId
      );
      if (idx !== -1) {
        const [req] = this.friendRequests.splice(idx, 1);
        await this.persist();
        this.syncUser(userId);
        this.syncUser(req.toUserId);
        safeSend(ws, { type: "notice", level: "info", message: "Solicitud cancelada." });
      }
      return;
    }

    if (type === "friend_remove") {
      const friendUserId = normalizeUserId(msg.friendUserId);
      if (!friendUserId) return;
      this.removeMutualFriend(userId, friendUserId);
      await this.persist(userId, friendUserId);
      this.syncUser(userId);
      this.syncUser(friendUserId);
      safeSend(ws, { type: "notice", level: "info", message: "Amigo eliminado." });
      return;
    }

    if (type === "room_presence_update") {
      if (!msg.room || !msg.room.roomCode) {
        this.rooms.delete(userId);
        for (const [reqId, jr] of this.joinRequests.entries()) {
          if (jr.toUserId === userId) {
            this.joinRequests.delete(reqId);
          }
        }
      } else {
        this.rooms.set(userId, {
          roomCode: String(msg.room.roomCode).trim().toUpperCase(),
          inviteUrl: String(msg.room.inviteUrl || "").trim(),
          roomAccess: String(msg.room.roomAccess || "request").trim(),
          participantsCount: Number(msg.room.participantsCount || 1),
          currentTitle: String(msg.room.currentTitle || "").slice(0, 120),
          updatedAt: new Date().toISOString(),
        });
      }
      this.syncFriendsOf(userId);
      return;
    }

    if (type === "room_invite_send") {
      const toUserId = normalizeUserId(msg.toUserId);
      const roomCode = String(msg.roomCode || "").trim().toUpperCase();
      const inviteUrl = String(msg.inviteUrl || "").trim();
      if (!toUserId || !roomCode || !inviteUrl) {
        safeSend(ws, { type: "notice", level: "error", message: "Faltan datos para enviar la invitación." });
        return;
      }
      const myFriends = Array.isArray(this.friendships[userId]) ? this.friendships[userId] : [];
      if (!myFriends.includes(toUserId)) {
        safeSend(ws, { type: "notice", level: "error", message: "Solo puedes invitar a usuarios que estén en tu lista de amigos." });
        return;
      }
      if (!this.isOnline(toUserId)) {
        safeSend(ws, { type: "notice", level: "error", message: "Tu amigo no está conectado en este momento." });
        return;
      }
      const me = this.users[userId] || { userId, displayName: userId, googleAccountName: "" };
      const invite = {
        id: crypto.randomUUID(),
        fromUserId: userId,
        fromDisplayName: me.displayName || userId,
        fromGoogleAccountName: me.googleAccountName || "",
        roomCode,
        inviteUrl,
        createdAt: new Date().toISOString(),
      };
      this.sendToUser(toUserId, { type: "room_invite_received", invite });
      this.sendToUser(toUserId, {
        type: "notice",
        level: "info",
        message: `${me.displayName || userId} te invitó a la sala ${roomCode}.`,
      });
      safeSend(ws, {
        type: "notice",
        level: "ok",
        message: `Invitación enviada a ${this.users[toUserId]?.displayName || toUserId}.`,
      });
      return;
    }

    if (type === "room_join_request") {
      const toUserId = normalizeUserId(msg.toUserId);
      const targetRoom = this.rooms.get(toUserId);
      if (!targetRoom || !this.isOnline(toUserId)) {
        safeSend(ws, { type: "notice", level: "error", message: "Ese amigo no tiene una sala abierta en este momento." });
        this.syncUser(userId);
        return;
      }
      if (!this.areFriends(userId, toUserId)) {
        safeSend(ws, { type: "notice", level: "error", message: "Deben ser amigos para solicitar acceso a la sala." });
        return;
      }
      if (!targetRoom.inviteUrl) {
        safeSend(ws, { type: "notice", level: "error", message: "La sala del amigo aún no tiene enlace público listo." });
        return;
      }

      const hostProfile = this.users[toUserId] || { userId: toUserId, displayName: toUserId };
      if (targetRoom.roomAccess === "open") {
        safeSend(ws, {
          type: "room_join_approved",
          hostUserId: toUserId,
          hostDisplayName: hostProfile.displayName || toUserId,
          roomCode: targetRoom.roomCode,
          inviteUrl: targetRoom.inviteUrl,
        });
        return;
      }

      if (targetRoom.roomAccess === "invite_only") {
        safeSend(ws, {
          type: "notice",
          level: "error",
          message: "Esta sala está configurada como solo por invitación.",
        });
        return;
      }

      // Check duplicate pending join request
      for (const existing of this.joinRequests.values()) {
        if (existing.fromUserId === userId && existing.toUserId === toUserId) {
          safeSend(ws, {
            type: "notice",
            level: "info",
            message: "Ya enviaste una solicitud para unirte a esta sala.",
          });
          return;
        }
      }

      const me = this.users[userId] || { userId, displayName: userId, googleAccountName: "" };
      const req = {
        id: crypto.randomUUID(),
        fromUserId: userId,
        fromDisplayName: me.displayName || userId,
        fromGoogleAccountName: me.googleAccountName || "",
        toUserId,
        roomCode: targetRoom.roomCode,
        createdAt: new Date().toISOString(),
      };
      this.joinRequests.set(req.id, req);
      this.sendToUser(toUserId, { type: "room_join_request_received", request: req });
      this.sendToUser(toUserId, {
        type: "notice",
        level: "info",
        message: `${me.displayName || userId} pide unirse a tu sala ${targetRoom.roomCode}.`,
      });
      this.syncUser(toUserId);
      safeSend(ws, {
        type: "notice",
        level: "ok",
        message: `Solicitud enviada a ${hostProfile.displayName || toUserId}. Esperando aprobación...`,
      });
      return;
    }

    if (type === "room_join_respond") {
      const requestId = String(msg.requestId || "").trim();
      const accept = Boolean(msg.accept);
      const req = this.joinRequests.get(requestId);
      if (!req || req.toUserId !== userId) {
        this.syncUser(userId);
        return;
      }
      this.joinRequests.delete(requestId);
      this.syncUser(userId);

      const me = this.users[userId] || { userId, displayName: userId };
      const myRoom = this.rooms.get(userId);
      const roomMatches = Boolean(myRoom && (!req.roomCode || myRoom.roomCode === req.roomCode));
      const inviteUrl = roomMatches ? String(myRoom.inviteUrl || "").trim() : "";

      if (accept && roomMatches && inviteUrl) {
        this.sendToUser(req.fromUserId, {
          type: "room_join_approved",
          hostUserId: userId,
          hostDisplayName: me.displayName || userId,
          roomCode: myRoom.roomCode || req.roomCode,
          inviteUrl,
        });
        safeSend(ws, {
          type: "notice",
          level: "ok",
          message: `Aceptaste a ${req.fromDisplayName || req.fromUserId} en la sala.`,
        });
      } else if (accept) {
        this.sendToUser(req.fromUserId, {
          type: "notice",
          level: "error",
          message: "La sesión ya no está activa.",
        });
        safeSend(ws, {
          type: "notice",
          level: "error",
          message: "No tienes una sesión activa para aceptar esta solicitud.",
        });
      } else {
        this.sendToUser(req.fromUserId, {
          type: "notice",
          level: "error",
          message: `${me.displayName || userId} rechazó tu solicitud para unirte a la sala.`,
        });
        safeSend(ws, {
          type: "notice",
          level: "info",
          message: "Solicitud de ingreso rechazada.",
        });
      }
    }
  }
}
