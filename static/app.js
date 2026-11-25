const state = {
  view: "home",
  query: "",
  searchScope: "all",
  musicSearchFilter: "",
  results: [],
  downloads: [],
  library: [],
  history: [],
  channels: [],
  feed: [],
  collections: [],
  playlists: [],
  recommendations: [],
  recommendationsLastSync: "",
  musicRecommendations: [],
  musicRecomLastSync: "",
  recomExpanded: { songs: false, videos: false, mixes: false },
  recomLoadingMore: { songs: false, videos: false },
  homeExpanded: { music: false, videos: false },
  musicPlaylists: [],
  musicPlaylistsLastSync: "",
  channelsLastSync: "",
  musicAuthState: "",
  musicAccountName: "",
  musicLibrary: { songs: [], liked: [], albums: [], artists: [], playlists: [], history: [], lastSync: "", error: "" },
  syncStatus: {},
  player: {
    vjs: null,
    queue: [],
    index: 0,
    current: null,
    loading: false,
    sourceContext: "search",
    audioOnly: false,
    shuffle: false,
    repeat: "none",
    autoplay: localStorage.getItem("playzi-autoplay") !== "0",
    _originalQueue: [],
    _listenerAbort: null,
    _loadingId: 0,
    audioTracks: null,
    audioLang: null,
    availableQualities: null,
    activeQuality: null,
    currentHeight: null,
    native: null,
    capabilities: null,
  },
  config: null,
  updateStatus: null,
  theme: localStorage.getItem("playzi-theme") || "dark",
  sidebarCollapsed: localStorage.getItem("playzi-sidebar-collapsed") === "1",
  watchSidebarCollapsed: { users: false, queue: false, related: false },
  channelView: { url: "", name: "", tab: "videos", items: [], page: 1, hasMore: false, loading: false, meta: null, error: "" },
  artistView: { channelId: "", name: "", data: null, loading: false, error: "" },
  userProfile: { userId: "", displayName: "", googleAccountName: "" },
  friends: [],
  social: {
    configured: false,
    connected: false,
    hubUrl: "",
    lastError: "",
    friends: [],
    incomingFriendRequests: [],
    outgoingFriendRequests: [],
    incomingRoomInvites: [],
    incomingJoinRequests: [],
  },
  discord: {
    enabled: true,
    configured: false,
    connected: false,
    showMedia: true,
    showParty: true,
    inviteActive: false,
    userId: "",
    username: "",
    globalName: "",
    lastError: "",
    pendingJoinRequests: [],
  },
  party: {
    active: false,
    role: "idle",
    connected: false,
    roomCode: "",
    hostUserId: "",
    hostName: "",
    bindPort: null,
    localWsUrl: "",
    publicWsUrl: "",
    inviteUrl: "",
    tunnelRunning: false,
    tunnelError: "",
    relayConnected: false,
    relayError: "",
    roomAccess: "request",
    queuePolicy: "all",
    allowedQueueUserIds: [],
    canSelfQueue: false,
    selfUserId: "",
    participants: [],
    queue: [],
    playback: { currentIndex: null, isPlaying: false, positionSecs: 0, updatedAtMs: 0, playbackRate: 1, repeat: "none", shuffle: false, version: 0 },
    lastError: "",
  },
};

const COLOR_PRESETS = [
  ["pink",   "Rosa",    "oklch(.64 .23 10)"],
  ["amber",  "Ámbar",   "oklch(.70 .17 65)"],
  ["blue",   "Azul",    "oklch(.64 .20 240)"],
  ["purple", "Violeta", "oklch(.65 .20 285)"],
  ["green",  "Verde",   "oklch(.60 .19 155)"],
  ["teal",   "Teal",    "oklch(.61 .19 196)"],
];

let _miniInterval = null;
let _hadActiveDownloads = false;
let _lastSeenSyncSuccess = "";
let _libraryLimit = 100;
let _historyLimit = 100;
let _dragSrc = null;

const $ = (sel) => document.querySelector(sel);
const $$ = (sel) => [...document.querySelectorAll(sel)];
const DEFAULT_MUSIC_SEARCH_LIMIT = 30;

const IS_TAURI = Boolean(window.__TAURI__?.core?.invoke);

async function tauriRequest(method, path, body = null) {
  try {
    return await window.__TAURI__.core.invoke("api_request", {
      method,
      path,
      body,
    });
  } catch (err) {
    if (err instanceof Error) throw err;
    throw new Error(typeof err === "string" ? err : err?.message || String(err));
  }
}

const api = {
  async get(path) {
    if (IS_TAURI) return tauriRequest("GET", path, null);
    const res = await fetch(path);
    if (!res.ok) throw new Error(await errorText(res));
    return res.json();
  },
  async post(path, body = {}) {
    if (IS_TAURI) return tauriRequest("POST", path, body);
    const res = await fetch(path, {
      method: "POST",
      headers: { "Content-Type": "application/json" },
      body: JSON.stringify(body),
    });
    if (!res.ok) throw new Error(await errorText(res));
    return res.json();
  },
  async patch(path, body = {}) {
    if (IS_TAURI) return tauriRequest("PATCH", path, body);
    const res = await fetch(path, {
      method: "PATCH",
      headers: { "Content-Type": "application/json" },
      body: JSON.stringify(body),
    });
    if (!res.ok) throw new Error(await errorText(res));
    return res.json();
  },
  async delete(path) {
    if (IS_TAURI) return tauriRequest("DELETE", path, null);
    const res = await fetch(path, { method: "DELETE" });
    if (!res.ok) throw new Error(await errorText(res));
    return res.json();
  },
};

async function errorText(res) {
  try {
    const json = await res.json();
    return json.detail || res.statusText;
  } catch {
    return res.statusText;
  }
}

function _isMembersOnlyError(msg) {
  if (!msg) return false;
  return /members[- ]only|join this channel|subscriber_only|solo para miembros|solo miembros/i.test(String(msg));
}

function _markMembersOnlyByUrl(url) {
  if (!url) return;
  const lists = [
    state.feed,
    state.recom,
    state.results,
    state.channelView?.items,
    state.player?.queue,
    state.player?.related,
  ];
  for (const list of lists) {
    if (!Array.isArray(list)) continue;
    for (const entry of list) {
      if (entry && entry.url === url) entry.membersOnly = true;
    }
  }
  if (state.player?.current?.url === url) {
    state.player.current.membersOnly = true;
  }
}

function escapeHtml(value) {
  return String(value ?? "").replace(/[&<>"']/g, (ch) => ({
    "&": "&amp;",
    "<": "&lt;",
    ">": "&gt;",
    '"': "&quot;",
    "'": "&#039;",
  }[ch]));
}

function normalizeThumbUrl(url) {
  const value = String(url || "").trim();
  if (!value) return "";
  if (value.startsWith("//")) return `https:${value}`;
  return value;
}

function thumbSrc(url) {
  const normalized = normalizeThumbUrl(url);
  if (!normalized) return "";
  if (normalized.startsWith("/") && !normalized.startsWith("//")) return normalized;
  if (IS_TAURI) {
    const base = (location.protocol === "http:" && location.hostname.endsWith("localhost"))
      ? "http://playzi-thumb.localhost/"
      : "playzi-thumb://localhost/";
    return `${base}?url=${encodeURIComponent(normalized)}`;
  }
  return `/api/thumbnail?url=${encodeURIComponent(normalized)}`;
}

function streamProxyUrl(rawUrl) {
  if (!rawUrl) return "";
  if (IS_TAURI) {
    const base = (location.protocol === "http:" && location.hostname.endsWith("localhost"))
      ? "http://playzi-stream.localhost/"
      : "playzi-stream://localhost/";
    return `${base}?url=${encodeURIComponent(rawUrl)}`;
  }
  return rawUrl;
}

function streamMasterProxyUrl(videoUrl, audioUrl) {
  if (!videoUrl || !audioUrl) return streamProxyUrl(videoUrl || audioUrl || "");
  if (IS_TAURI) {
    const base = (location.protocol === "http:" && location.hostname.endsWith("localhost"))
      ? "http://playzi-stream.localhost/"
      : "playzi-stream://localhost/";
    return `${base}master.m3u8?v=${encodeURIComponent(videoUrl)}&a=${encodeURIComponent(audioUrl)}`;
  }
  return videoUrl;
}

function thumbAttrs(url, eager = false) {
  const rawUrl = normalizeThumbUrl(url);
  return `src="${escapeHtml(thumbSrc(rawUrl))}" data-thumb-url="${escapeHtml(rawUrl)}" alt="" loading="${eager ? "eager" : "lazy"}" referrerpolicy="no-referrer" onerror="fallbackThumb(this)"`;
}

function fallbackThumb(img) {
  const raw = normalizeThumbUrl(img.dataset.thumbUrl || "");
  if (raw.includes("/hqdefault.jpg")) {
    const next = raw.replace("/hqdefault.jpg", "/mqdefault.jpg");
    img.dataset.thumbUrl = next;
    img.src = thumbSrc(next);
    return;
  }
  if (raw.includes("/mqdefault.jpg")) {
    const next = raw.replace("/mqdefault.jpg", "/default.jpg");
    img.dataset.thumbUrl = next;
    img.src = thumbSrc(next);
    return;
  }
  img.remove();
}

function isUrl(value) {
  return /(youtube\.com|youtu\.be|music\.youtube\.com)/i.test(value || "");
}

function deriveThumbnail(url) {
  const m = /(?:v=|youtu\.be\/|shorts\/)([A-Za-z0-9_-]{11})/.exec(url || "");
  return m ? `https://i.ytimg.com/vi/${m[1]}/hqdefault.jpg` : "";
}

function toast(message) {
  const el = $("#toast");
  el.textContent = message;
  el.classList.remove("hidden");
  clearTimeout(toast.timer);
  toast.timer = setTimeout(() => el.classList.add("hidden"), 4200);
}

function _formatTime(sec) {
  if (!isFinite(sec) || isNaN(sec) || sec < 0) return "0:00";
  const m = Math.floor(sec / 60);
  const s = Math.floor(sec % 60);
  return `${m}:${s.toString().padStart(2, "0")}`;
}

function _destroyPlayer() {
  if (state.player._listenerAbort) {
    state.player._listenerAbort.abort();
    state.player._listenerAbort = null;
  }
  if (state.player.vjs && !state.player.vjs.isDisposed()) {
    state.player.vjs.dispose();
    state.player.vjs = null;
  }
  if (state.player.audioVjs && !state.player.audioVjs.isDisposed()) {
    state.player.audioVjs.dispose();
    state.player.audioVjs = null;
  }
  state.player.current = null;
  state.player.queue = [];
  state.player._originalQueue = [];
  state.player.loading = false;
  state.player.audioOnly = false;
  state.player.sourceContext = "search";
  _syncDiscordMediaPresence(true);
}

function _isShort(item) {
  if (!item) return false;
  if (item.resultType === "short") return true;
  const url = String(item.url || item.target || "");
  if (/\/shorts\/[A-Za-z0-9_-]{11}/i.test(url)) return true;
  const durText = String(item.durationText || "").trim().toLowerCase();
  if (durText === "short" || durText === "shorts") return true;
  return false;
}

function _shortsEnabled() {
  return Boolean(state.config?.showShorts);
}

function applyShortsVisibility() {
  const enabled = _shortsEnabled();
  const navBtn = $("#navShorts");
  if (navBtn) navBtn.classList.toggle("hidden", !enabled);
  if (!enabled && state.view === "shorts") {
    setView("home");
  }
}

function setView(view) {
  if (view === "shorts" && !_shortsEnabled()) {
    view = "home";
  }
  if (state.view === "watch" && view !== "watch") {
    if (state.player.current) {
      // keep player alive — show mini player
      _showMiniPlayer();
    } else {
      _destroyPlayer();
      $("#view-watch").innerHTML = "";
    }
  }
  if (view === "watch") _hideMiniPlayer();
  state.view = view;
  $$(".view").forEach((el) => el.classList.add("hidden"));
  $(`#view-${view}`).classList.remove("hidden");
  $$(".nav-item[data-view]").forEach((el) => el.classList.toggle("active", el.dataset.view === view));
  render();
  _ensurePartyViewPolling();
  if (view === "party") {
    _syncPartyViewNow();
  }
  if (view === "music") _autoTriggerMusicSync();
  if (view === "recom" || view === "music") _enrichMusicRecomDurationsIfNeeded();
}

function applySidebarState() {
  document.body.classList.toggle("sidebar-collapsed", state.sidebarCollapsed);
  const toggleBtn = $("#sidebarToggle");
  if (toggleBtn) {
    toggleBtn.setAttribute("aria-expanded", String(!state.sidebarCollapsed));
    toggleBtn.title = state.sidebarCollapsed ? "Expandir barra lateral" : "Colapsar barra lateral";
  }
}

function toggleSidebar() {
  state.sidebarCollapsed = !state.sidebarCollapsed;
  localStorage.setItem("playzi-sidebar-collapsed", state.sidebarCollapsed ? "1" : "0");
  applySidebarState();
}

const MOON_SVG = `<svg width="15" height="15" viewBox="0 0 24 24" fill="currentColor"><path d="M21 12.79A9 9 0 1 1 11.21 3 7 7 0 0 0 21 12.79z"/></svg>`;
const SUN_SVG  = `<svg width="15" height="15" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round"><circle cx="12" cy="12" r="5"/><path d="M12 1v2M12 21v2M4.22 4.22l1.42 1.42M18.36 18.36l1.42 1.42M1 12h2M21 12h2M4.22 19.78l1.42-1.42M18.36 5.64l1.42-1.42"/></svg>`;

// Modern mixed player icon set (filled rounded primary + crisp 2px stroke secondary)
const PLAYER_ICONS = {
  play: (s = 20) => `<svg class="pz-ico" width="${s}" height="${s}" viewBox="0 0 24 24" fill="currentColor" aria-hidden="true"><path d="M7.5 4.86c0-.88.96-1.42 1.71-.96l11.2 6.86a1.12 1.12 0 0 1 0 1.92l-11.2 6.86c-.75.46-1.71-.08-1.71-.96V4.86Z"/></svg>`,
  pause: (s = 20) => `<svg class="pz-ico" width="${s}" height="${s}" viewBox="0 0 24 24" fill="currentColor" aria-hidden="true"><rect x="5.5" y="4.5" width="4.8" height="15" rx="1.6"/><rect x="13.7" y="4.5" width="4.8" height="15" rx="1.6"/></svg>`,
  stop: (s = 15) => `<svg class="pz-ico" width="${s}" height="${s}" viewBox="0 0 24 24" fill="currentColor" aria-hidden="true"><rect x="5" y="5" width="14" height="14" rx="2.5"/></svg>`,
  prev: (s = 20) => `<svg class="pz-ico" width="${s}" height="${s}" viewBox="0 0 24 24" fill="currentColor" aria-hidden="true"><rect x="4.5" y="5" width="2.6" height="14" rx="1.2"/><path d="M18.3 5.4c.74-.45 1.7.09 1.7.96v11.28c0 .87-.96 1.41-1.7.96l-8.95-5.64a1.12 1.12 0 0 1 0-1.92L18.3 5.4Z"/></svg>`,
  next: (s = 20) => `<svg class="pz-ico" width="${s}" height="${s}" viewBox="0 0 24 24" fill="currentColor" aria-hidden="true"><path d="M5.7 5.4c-.74-.45-1.7.09-1.7.96v11.28c0 .87.96 1.41 1.7.96l8.95-5.64a1.12 1.12 0 0 0 0-1.92L5.7 5.4Z"/><rect x="16.9" y="5" width="2.6" height="14" rx="1.2"/></svg>`,
  shuffle: (s = 17) => `<svg class="pz-ico" width="${s}" height="${s}" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round" aria-hidden="true"><path d="M16 3h5v5"/><path d="M4 20 21 3"/><path d="M21 16v5h-5"/><path d="M15 15l6 6"/><path d="M4 4l5 5"/></svg>`,
  repeat: (s = 17) => `<svg class="pz-ico" width="${s}" height="${s}" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round" aria-hidden="true"><path d="m17 2 4 4-4 4"/><path d="M3 11v-1a4 4 0 0 1 4-4h14"/><path d="m7 22-4-4 4-4"/><path d="M21 13v1a4 4 0 0 1-4 4H3"/></svg>`,
  repeatOne: (s = 17) => `<svg class="pz-ico" width="${s}" height="${s}" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round" aria-hidden="true"><path d="m17 2 4 4-4 4"/><path d="M3 11v-1a4 4 0 0 1 4-4h14"/><path d="m7 22-4-4 4-4"/><path d="M21 13v1a4 4 0 0 1-4 4H3"/><path d="M11 10h1v4"/></svg>`,
  volHigh: (s = 18) => `<svg class="pz-ico" width="${s}" height="${s}" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round" aria-hidden="true"><polygon points="11 5 6 9 2 9 2 15 6 15 11 19 11 5" fill="currentColor" stroke="none"/><path d="M15.5 8.5a5 5 0 0 1 0 7"/><path d="M18.8 5.2a9.5 9.5 0 0 1 0 13.6"/></svg>`,
  volLow: (s = 18) => `<svg class="pz-ico" width="${s}" height="${s}" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round" aria-hidden="true"><polygon points="11 5 6 9 2 9 2 15 6 15 11 19 11 5" fill="currentColor" stroke="none"/><path d="M15.5 8.5a5 5 0 0 1 0 7"/></svg>`,
  volMute: (s = 18) => `<svg class="pz-ico" width="${s}" height="${s}" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round" aria-hidden="true"><polygon points="11 5 6 9 2 9 2 15 6 15 11 19 11 5" fill="currentColor" stroke="none"/><path d="m22 9-6 6"/><path d="m16 9 6 6"/></svg>`,
  expand: (s = 16) => `<svg class="pz-ico" width="${s}" height="${s}" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round" aria-hidden="true"><path d="M15 3h6v6"/><path d="M9 21H3v-6"/><path d="M21 3l-7 7"/><path d="M3 21l7-7"/></svg>`,
  close: (s = 16) => `<svg class="pz-ico" width="${s}" height="${s}" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2.2" stroke-linecap="round" aria-hidden="true"><path d="M18 6 6 18"/><path d="m6 6 12 12"/></svg>`,
  download: (s = 14) => `<svg class="pz-ico" width="${s}" height="${s}" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round" aria-hidden="true"><path d="M12 3v12"/><path d="m7 10 5 5 5-5"/><path d="M5 20h14"/></svg>`,
  headphones: (s = 14) => `<svg class="pz-ico" width="${s}" height="${s}" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round" aria-hidden="true"><path d="M3 14v-3a9 9 0 0 1 18 0v3"/><path d="M21 19a2 2 0 0 1-2 2h-1a2 2 0 0 1-2-2v-3a2 2 0 0 1 2-2h3Z"/><path d="M3 19a2 2 0 0 0 2 2h1a2 2 0 0 0 2-2v-3a2 2 0 0 0-2-2H3Z"/></svg>`,
  external: (s = 14) => `<svg class="pz-ico" width="${s}" height="${s}" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round" aria-hidden="true"><path d="M15 3h6v6"/><path d="M10 14 21 3"/><path d="M18 13v6a2 2 0 0 1-2 2H5a2 2 0 0 1-2-2V8a2 2 0 0 1 2-2h6"/></svg>`,
  back: (s = 14) => `<svg class="pz-ico" width="${s}" height="${s}" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round" aria-hidden="true"><path d="m12 19-7-7 7-7"/><path d="M19 12H5"/></svg>`,
  musicNote: (s = 56) => `<svg class="pz-ico" width="${s}" height="${s}" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="1.6" stroke-linecap="round" stroke-linejoin="round" aria-hidden="true"><path d="M9 18V5l12-2v13"/><circle cx="6" cy="18" r="3" fill="currentColor"/><circle cx="18" cy="16" r="3" fill="currentColor"/></svg>`,
};

function _volIconSvg(vol, muted = false, size = 18) {
  if (muted || vol <= 0) return PLAYER_ICONS.volMute(size);
  if (vol < 0.5) return PLAYER_ICONS.volLow(size);
  return PLAYER_ICONS.volHigh(size);
}

function updateThemeBtn() {
  const el = $("#themeIcon");
  if (el) el.innerHTML = state.theme === "dark" ? MOON_SVG : SUN_SVG;
}

function setTheme(t) {
  state.theme = t;
  document.documentElement.dataset.theme = t;
  localStorage.setItem("playzi-theme", t);
  updateThemeBtn();
  if (state.view === "config") renderConfig();
}

async function loadAll() {
  document.documentElement.dataset.theme = state.theme;
  updateThemeBtn();
  try {
    applyBootstrap(await api.get("/api/bootstrap"));
    render();
    _silentAuthRefresh();
  } catch (err) {
    toast(err.message);
  }
}

// On startup, silently revalidate the session against the persistent browser
// profile. Never opens a visible browser (that's only the on-click fallback).
async function _silentAuthRefresh() {
  const hasCookies = state.config?.cookiesBrowser || state.config?.cookiesFile;
  if (!hasCookies) return;
  try {
    const resp = await api.post("/api/music/auth/refresh");
    await _pollTask(resp?.task?.id, 60);
    await refreshLists();
  } catch (_) {}
}

function applyBootstrap(data) {
  _libraryLimit = 100;
  _historyLimit = 100;
  state.config = data.config || state.config;
  state.downloads = data.downloads?.items || [];
  state.library = data.library?.items || [];
  state.history = data.history?.items || [];
  state.channels = data.channels?.items || [];
  state.feed = data.feed?.items || [];
  state.collections = data.collections?.items || [];
  state.playlists = data.playlists?.items || [];
  state.recommendations = data.recommendations?.items || [];
  state.recommendationsLastSync = data.recommendations?.lastSync || "";
  state.musicRecommendations = data.musicRecommendations?.items || [];
  state.musicRecomLastSync = data.musicRecommendations?.lastSync || "";
  state.musicPlaylists = data.musicPlaylists?.items || [];
  state.musicPlaylistsLastSync = data.musicPlaylists?.lastSync || "";
  state.musicLibrary = data.musicLibrary || state.musicLibrary;
  state.channelsLastSync = data.channelsLastSync || "";
  state.musicAuthState = data.musicAuthState || "";
  state.musicAccountName = data.musicAccountName || "";
  state.syncStatus = data.syncStatus || {};
  state.userProfile = data.userProfile || state.userProfile;
  state.friends = data.friends || state.friends;
  _lastSeenSyncSuccess = state.syncStatus.lastSuccess || _lastSeenSyncSuccess;
  applyShortsVisibility();
  _enrichMusicRecomDurationsIfNeeded();
}

let _enrichingMusicDurations = false;
let _lastMusicDurationEnrichAt = 0;

function _hasMissingMusicRecomDurations() {
  return (state.musicRecommendations || []).some((item) => (
    !_isRecommendedMix(item)
    && Boolean(item?.id)
    && (!item.duration || !item.durationText || item.durationText === "--:--")
  ));
}

async function _enrichMusicRecomDurationsIfNeeded() {
  if (_enrichingMusicDurations || !_hasMissingMusicRecomDurations()) return;
  const now = Date.now();
  if (now - _lastMusicDurationEnrichAt < 15000) return;
  _enrichingMusicDurations = true;
  _lastMusicDurationEnrichAt = now;
  try {
    const res = await api.get("/api/music/recommendations");
    if (Array.isArray(res?.items) && res.items.length) {
      state.musicRecommendations = res.items;
      if (res.lastSync) state.musicRecomLastSync = res.lastSync;
      if (["recom", "music", "home"].includes(state.view)) render();
    }
  } catch (_) {
    // Non-blocking background enrichment
  } finally {
    _enrichingMusicDurations = false;
  }
}

async function refreshLists() {
  const preserveConfigForm = state.view === "config";
  applyBootstrap(await api.get("/api/bootstrap"));
  if (!preserveConfigForm) render();
}

async function refreshSection(key, endpoint) {
  const data = await api.get(endpoint);
  if (data.items !== undefined) state[key] = data.items;
  else Object.assign(state, data);
  render();
}

async function refreshSummary() {
  const data = await api.get("/api/state/summary");
  state.downloads = data.downloads?.items || state.downloads;
  state.syncStatus = data.syncStatus || state.syncStatus;
  if (state.view === "downloads") renderDownloads();
  $("#downloadBadge").textContent = state.downloads.filter((d) => !["done", "error"].includes(d.status)).length || "";
}

async function startBackgroundTask(response, message = "Tarea iniciada") {
  if (response?.task?.id) {
    toast(message);
    await refreshSummary();
    return response.task;
  }
  await refreshLists();
  return null;
}

async function doSearch() {
  const q = state.query.trim();
  if (!q) {
    state.results = [];
    if (state.view === "search") renderSearch();
    return;
  }
  if (state.view !== "search") setView("search");
  const isLocalScope = state.searchScope === "local" || state.searchScope === "playlists";
  if (!isLocalScope) renderSearch(true);
  try {
    if (state.searchScope === "local") {
      state.results = state.library.filter((item) => _matchesQuery(item, q)).map((item) => ({ ...item, source: "local", url: item.mediaUrl || item.path }));
      renderSearch();
      return;
    }
    if (state.searchScope === "playlists") {
      state.results = state.playlists.filter((pl) => _matchesQuery(pl, q)).map(playlistResultItem);
      renderSearch();
      return;
    }
    const scope = state.searchScope === "all" ? "" : `&scope=${encodeURIComponent(state.searchScope)}`;
    const filter = (state.searchScope === "music" && state.musicSearchFilter)
      ? `&search_filter=${encodeURIComponent(state.musicSearchFilter)}` : "";
    const limit = `&limit=${encodeURIComponent(searchLimitForScope(state.searchScope))}`;
    const data = await api.get(`/api/search?q=${encodeURIComponent(q)}${scope}${filter}${limit}`);
    state.results = data.items || [];
  } catch (err) {
    toast(err.message);
    state.results = [];
  }
  renderSearch();
}

function _matchesQuery(item, q) {
  const hay = `${item.title || item.name || ""} ${item.channel || ""} ${item.collection || ""}`.toLowerCase();
  return hay.includes(q.toLowerCase());
}

function searchLimitForScope(scope) {
  const configured = Number(state.config?.searchLimit || 0);
  if (scope === "youtube") return configured || 12;
  if (scope === "music") return Math.max(DEFAULT_MUSIC_SEARCH_LIMIT, configured || 0);
  return Math.max(DEFAULT_MUSIC_SEARCH_LIMIT, configured || 0);
}

function playlistResultItem(pl) {
  return {
    id: pl.id,
    url: pl.externalUrl || `playlist:${pl.id}`,
    title: pl.name,
    channel: `${pl.itemCount || pl.items?.length || 0} items`,
    thumbnail: pl.thumbnail || "",
    durationText: pl.importMode === "linked" ? "Vinculada" : "Playlist",
    source: pl.source || "playlist",
    resultType: "playlist",
    playlistId: pl.id,
  };
}

function render() {
  $("#downloadBadge").textContent = state.downloads.filter((d) => !["done", "error"].includes(d.status)).length || "";
  _updatePartyBadge();
  applyShortsVisibility();
  renderAuthStatus();
  if (state.view === "home") renderHome();
  if (state.view === "search") renderSearch();
  if (state.view === "recom") renderRecom();
  if (state.view === "shorts") renderShorts();
  if (state.view === "channels") renderChannels();
  if (state.view === "library") renderLibrary();
  if (state.view === "music") renderMusic();
  if (state.view === "collections") renderCollections();
  if (state.view === "downloads") renderDownloads();
  if (state.view === "history") renderHistory();
  if (state.view === "party") renderParty();
  if (state.view === "config") renderConfig();
  if (state.view === "watch") renderWatch();
  if (state.view === "channel") renderChannelView();
  if (state.view === "artist") renderArtistView();
}

function syncNotice(section) {
  const status = state.syncStatus || {};
  const sectionStatus = status.sections?.[section] || {};
  if (status.running && status.section === section) {
    return `<div class="url-banner" style="margin-bottom:16px"><strong>...</strong><div style="min-width:0;flex:1"><div class="url-banner-title">Actualizando automáticamente</div><div class="url-banner-sub">Playzi está sincronizando esta sección.</div></div></div>`;
  }
  const warning = sectionStatus.warning || (status.section === section ? status.warning : "");
  if (warning) {
    return `<div class="url-banner" style="margin-bottom:16px"><strong>!</strong><div style="min-width:0;flex:1"><div class="url-banner-title">Cookies en cache</div><div class="url-banner-sub">${escapeHtml(warning)}</div></div></div>`;
  }
  if (sectionStatus.lastError) {
    return `<div class="url-banner" style="margin-bottom:16px"><strong>!</strong><div style="min-width:0;flex:1"><div class="url-banner-title">No se pudo sincronizar automáticamente</div><div class="url-banner-sub">${escapeHtml(sectionStatus.lastError)}</div></div></div>`;
  }
  return "";
}

function renderHome() {
  const recent = state.history.filter((item) => item.url || item.target).slice(0, 8);
  const allMusic = _uniqueByUrl([...(state.musicLibrary?.liked || []), ...state.musicRecommendations]);
  const allVideos = _uniqueByUrl([...state.recommendations, ...state.feed]).filter((item) => !_isShort(item));
  const musicLimit = state.homeExpanded.music ? allMusic.length : 8;
  const videosLimit = state.homeExpanded.videos ? allVideos.length : 8;
  const music = allMusic.slice(0, musicLimit);
  const videos = allVideos.slice(0, videosLimit);
  const imported = state.playlists.filter((pl) => pl.externalUrl || pl.importMode !== "local").slice(0, 6);
  const musicMoreBtn = allMusic.length > 8
    ? `<button class="btn btn-ghost btn-sm" type="button" data-home-expand="music">${state.homeExpanded.music ? "Mostrar menos" : `Mostrar más (${allMusic.length})`}</button>`
    : "";
  const videosMoreBtn = allVideos.length > 8
    ? `<button class="btn btn-ghost btn-sm" type="button" data-home-expand="videos">${state.homeExpanded.videos ? "Mostrar menos" : `Mostrar más (${allVideos.length})`}</button>`
    : "";
  $("#view-home").innerHTML = `
    <div class="home-hero">
      <div>
        <span class="section-title">Playzi</span>
        <h1>YouTube + YouTube Music</h1>
        <p>Un inicio para seguir viendo, escuchar música, importar playlists y manejar tu biblioteca local.</p>
      </div>
      <div class="home-actions">
        <button class="btn btn-primary" type="button" data-view-go="search">Buscar</button>
        <button class="btn btn-ghost" type="button" data-open-import-playlist>Importar playlist</button>
        <button class="btn btn-ghost" type="button" id="refreshMusicLibrary">Sincronizar Music</button>
      </div>
    </div>

    ${recent.length ? homeSection("Continuar", recent.map(historyCard).join("")) : ""}
    ${music.length ? homeSection("Música para ti", `<div class="grid">${music.map(videoCard).join("")}</div>`, musicMoreBtn) : ""}
    ${videos.length ? homeSection("Videos y canales", `<div class="grid">${videos.map(videoCard).join("")}</div>`, videosMoreBtn) : ""}
    ${imported.length ? homeSection("Playlists importadas", playlistGrid(imported)) : ""}
    ${state.library.length ? homeSection("Biblioteca local", `<div class="grid">${state.library.slice(0, 8).map(libraryCard).join("")}</div>`) : ""}
  `;
}

function homeSection(title, body, actionHtml = "") {
  return `
    <section class="home-section">
      <div class="section-hdr"><span class="section-title">${escapeHtml(title)}</span>${actionHtml}</div>
      ${body}
    </section>
  `;
}

function renderSearch(loading = false) {
  const root = $("#view-search");
  const q = state.query.trim();
  const direct = isUrl(q);
  const visibleResults = (_shortsEnabled() || direct)
    ? state.results
    : state.results.filter((item) => !_isShort(item));
  root.innerHTML = `
    ${direct ? `
      <div class="url-banner">
        <strong>Link</strong>
        <div style="min-width:0;flex:1">
          <div class="url-banner-title">URL de YouTube detectada</div>
          <div class="url-banner-sub">${escapeHtml(q)}</div>
        </div>
        <button class="btn btn-primary" type="button" data-url-play>▶ Reproducir</button>
        <button class="btn btn-ghost" type="button" data-url-download="video">Descargar</button>
        <button class="btn btn-ghost" type="button" data-url-download="audio">Audio</button>
      </div>` : ""}
    <div class="section-hdr">
      <span class="section-title">${q ? `Resultados para "${escapeHtml(q)}"` : "Buscar"}</span>
      <span class="meta">${loading ? "Buscando..." : `${visibleResults.length} resultados`}</span>
    </div>
    <div class="search-scopes">
      ${[
        ["all", "Todo"],
        ["youtube", "YouTube"],
        ["music", "Music"],
        ["local", "Local"],
        ["playlists", "Playlists"],
      ].map(([value, label]) => `<button class="pill ${state.searchScope === value ? "active" : ""}" type="button" data-search-scope="${value}">${label}</button>`).join("")}
    </div>
    ${state.searchScope === "music" ? `
    <div class="search-scopes" style="margin-top:6px">
      ${[
        ["", "Todo"],
        ["songs", "Canciones"],
        ["artists", "Artistas"],
        ["albums", "Álbumes"],
        ["videos", "Videos"],
        ["playlists", "Listas"],
      ].map(([v, l]) => `<button class="pill ${state.musicSearchFilter === v ? "active" : ""}" type="button" data-music-filter="${v}">${l}</button>`).join("")}
    </div>` : ""}
    ${!q ? empty("Busca por texto o pega una URL", "También puedes agregar canales para mejorar tus recomendaciones locales.") : ""}
    ${q && loading ? empty("Consultando YouTube", "yt-dlp está obteniendo metadata.") : ""}
    ${q && !loading && visibleResults.length === 0 ? empty("Sin resultados", "Prueba otra búsqueda o revisa la URL.") : ""}
    ${visibleResults.length ? searchResultsHtml(visibleResults, direct) : ""}
  `;
}

function searchResultsHtml(items, allowDirectShort = false) {
  const shorts = items.filter(_isShort);
  const regular = items.filter((item) => !_isShort(item));
  const showShortsGroup = (_shortsEnabled() || allowDirectShort) && shorts.length > 0;
  const shouldGroup = regular.some((item) => item.source === "music" && ["track", "video", "artist", "playlist", "album"].includes(item.resultType || ""));
  if (!shouldGroup) {
    const regularHtml = regular.length ? `<div class="grid">${regular.map(videoCard).join("")}</div>` : "";
    const shortsHtml = showShortsGroup ? `
      <section class="search-result-group">
        <div class="section-hdr"><span class="section-title">Shorts</span><span class="meta">${shorts.length}</span></div>
        <div class="grid grid-shorts">${shorts.map(videoCard).join("")}</div>
      </section>` : "";
    return regularHtml + shortsHtml;
  }
  const groups = [
    ["Canciones", regular.filter((item) => item.source === "music" && ["track", "video"].includes(item.resultType || ""))],
    ["Artistas", regular.filter((item) => item.source === "music" && item.resultType === "artist")],
    ["Playlists", regular.filter((item) => item.source === "music" && item.resultType === "playlist")],
    ["Albumes", regular.filter((item) => item.source === "music" && item.resultType === "album")],
    ["YouTube", regular.filter((item) => item.source !== "music")],
  ].filter(([, groupItems]) => groupItems.length);
  const regularHtml = groups.map(([title, groupItems]) => `
    <section class="search-result-group">
      <div class="section-hdr"><span class="section-title">${escapeHtml(title)}</span><span class="meta">${groupItems.length}</span></div>
      <div class="grid">${groupItems.map(videoCard).join("")}</div>
    </section>
  `).join("");
  const shortsHtml = showShortsGroup ? `
    <section class="search-result-group">
      <div class="section-hdr"><span class="section-title">Shorts</span><span class="meta">${shorts.length}</span></div>
      <div class="grid grid-shorts">${shorts.map(videoCard).join("")}</div>
    </section>` : "";
  return regularHtml + shortsHtml;
}

function _isRecommendedMix(item) {
  return item?.resultType === "mix"
    || item?.resultType === "playlist"
    || /[?&]list=/.test(String(item?.url || ""));
}

function _resolveItemChannelUrl(item) {
  if (!item) return "";
  if (item.channelUrl) return item.channelUrl;
  if (item.sourceChannelId) {
    const ch = state.channels.find((c) => c.id === item.sourceChannelId);
    if (ch?.url) return ch.url;
  }
  const url = String(item.url || "");
  const isPlaylist = _isRecommendedMix(item);
  if (!isPlaylist && /(?:youtube\.com\/watch\?v=|youtu\.be\/)/i.test(url)) {
    return url;
  }
  return "";
}

function _isNonChannelPart(part) {
  const lower = String(part || "").trim().toLowerCase();
  if (!lower) return true;
  if ([
    "canción", "cancion", "song", "video", "vídeo", "episodio", "episode",
    "podcast", "álbum", "album", "sencillo", "single", "ep",
  ].includes(lower)) return true;
  if (/^\d{1,2}:\d{2}(:\d{2})?$/.test(lower)) return true;
  if (/^\d{4}$/.test(lower)) return true;
  if (/(visualizaciones|visualización|vistas|views|reproducciones|plays|listeners|oyentes)/i.test(lower)) {
    return true;
  }
  return false;
}

function _splitChannelMeta(raw) {
  const text = String(raw || "").trim();
  if (!text) return { prefix: "", name: "", suffix: "" };
  const parts = text.split(/\s*[•·]\s*/).map((s) => s.trim()).filter(Boolean);
  if (parts.length <= 1) return { prefix: "", name: text, suffix: "" };
  const nameIdx = parts.findIndex((p) => !_isNonChannelPart(p));
  if (nameIdx === -1) return { prefix: "", name: text, suffix: "" };
  const prefixParts = parts.slice(0, nameIdx);
  const suffixParts = parts.slice(nameIdx + 1);
  return {
    prefix: prefixParts.length ? `${prefixParts.join(" • ")} • ` : "",
    name: parts[nameIdx],
    suffix: suffixParts.length ? ` • ${suffixParts.join(" • ")}` : "",
  };
}

function _renderChannelMetaHtml(rawChannelName, resolvedChannelUrl, allowLink) {
  const { prefix, name, suffix } = _splitChannelMeta(rawChannelName);
  const clickable = Boolean(
    allowLink
    && resolvedChannelUrl
    && name
    && !["YouTube", "YouTube Music", "YouTube Mix", "Playlist de YouTube"].includes(name)
  );
  const nameHtml = clickable
    ? `<span class="channel-link" data-watch-channel="${escapeHtml(resolvedChannelUrl)}" data-watch-channel-name="${escapeHtml(name)}" title="Ver canal (YouTube + YT Music)">${escapeHtml(name)}</span>`
    : escapeHtml(name);
  return `${escapeHtml(prefix)}${nameHtml}${escapeHtml(suffix)}`;
}

async function _expandRecomSection(kind) {
  if (kind === "mixes") {
    state.recomExpanded.mixes = !state.recomExpanded.mixes;
    renderRecom();
    return;
  }
  const currentlyExpanded = Boolean(state.recomExpanded[kind]);
  if (currentlyExpanded) {
    state.recomExpanded[kind] = false;
    renderRecom();
    return;
  }
  state.recomExpanded[kind] = true;
  state.recomLoadingMore[kind] = true;
  renderRecom();
  try {
    if (kind === "songs") {
      const res = await api.post("/api/music/recommendations/more");
      if (Array.isArray(res?.items) && res.items.length) {
        state.musicRecommendations = res.items;
        if (res.lastSync) state.musicRecomLastSync = res.lastSync;
      }
    } else if (kind === "videos") {
      const res = await api.post("/api/recommendations/more");
      if (Array.isArray(res?.items) && res.items.length) {
        state.recommendations = res.items;
        if (res.lastSync) state.recommendationsLastSync = res.lastSync;
      }
    }
  } catch (err) {
    toast(errorText(err));
  } finally {
    state.recomLoadingMore[kind] = false;
    renderRecom();
  }
}

function renderRecom() {
  const hasCookies = state.config?.cookiesBrowser || state.config?.cookiesFile;
  const songsAll = _uniqueByUrl(state.musicRecommendations.filter((item) => !_isRecommendedMix(item)));
  const videosAll = _uniqueByUrl([
    ...state.recommendations.filter((item) => !_isRecommendedMix(item) && !_isShort(item)),
    ...state.feed.filter((item) => !_isShort(item)),
  ]).filter((item) => !_isWatchedVideo(item));
  const mixesAll = _uniqueByUrl([
    ...state.recommendations.filter(_isRecommendedMix),
    ...state.musicRecommendations.filter(_isRecommendedMix),
  ]);

  const INITIAL_LIMIT = 8;
  const songsVisible = state.recomExpanded.songs ? songsAll : songsAll.slice(0, INITIAL_LIMIT);
  const videosVisible = state.recomExpanded.videos ? videosAll : videosAll.slice(0, INITIAL_LIMIT);
  const mixesVisible = state.recomExpanded.mixes ? mixesAll : mixesAll.slice(0, INITIAL_LIMIT);

  const songsMoreLabel = state.recomLoadingMore.songs
    ? "Cargando más..."
    : (state.recomExpanded.songs ? "Mostrar menos" : "Mostrar más");
  const videosMoreLabel = state.recomLoadingMore.videos
    ? "Cargando más..."
    : (state.recomExpanded.videos ? "Mostrar menos" : "Mostrar más");

  $("#view-recom").innerHTML = `
    <div class="section-hdr">
      <span class="section-title">Para ti ${state.recommendationsLastSync ? `<span class="meta" style="font-size:10px;margin-left:8px">· ${escapeHtml(state.recommendationsLastSync)}</span>` : ""}</span>
      <div style="display:flex;gap:8px;align-items:center">
        <button class="btn btn-ghost" type="button" id="refreshAllChannels">Canales</button>
        <button class="btn btn-primary" type="button" id="refreshRecom">Actualizar</button>
      </div>
    </div>
    ${!hasCookies ? `
      <div class="url-banner" style="margin-bottom:20px">
        <strong>⚙</strong>
        <div style="min-width:0;flex:1">
          <div class="url-banner-title">Cookies requeridas</div>
          <div class="url-banner-sub">Ve a Configuración → selecciona tu navegador con sesión de YouTube iniciada.</div>
        </div>
        <button class="btn btn-ghost" type="button" data-view-go="config">Configurar</button>
      </div>` : ""}
    ${hasCookies ? syncNotice("recommendations") : ""}
    ${songsAll.length ? `
      <div class="section-hdr" style="margin-top:4px;margin-bottom:12px">
        <span class="section-title">Canciones para ti <span class="meta" style="font-size:11px;margin-left:6px">${songsAll.length}</span></span>
        <button class="btn btn-ghost btn-sm" type="button" data-recom-expand="songs" ${state.recomLoadingMore.songs ? "disabled" : ""}>${songsMoreLabel}</button>
      </div>
      <div class="grid" style="margin-bottom:28px">${songsVisible.map(videoCard).join("")}</div>
    ` : ""}
    ${videosAll.length ? `
      <div class="section-hdr" style="margin-top:4px;margin-bottom:12px">
        <span class="section-title">Videos para ti <span class="meta" style="font-size:11px;margin-left:6px">${videosAll.length}</span></span>
        <button class="btn btn-ghost btn-sm" type="button" data-recom-expand="videos" ${state.recomLoadingMore.videos ? "disabled" : ""}>${videosMoreLabel}</button>
      </div>
      <div class="grid" style="margin-bottom:28px">${videosVisible.map(videoCard).join("")}</div>
    ` : ""}
    ${mixesAll.length ? `
      <div class="section-hdr" style="margin-top:4px;margin-bottom:12px">
        <span class="section-title">Mixes y playlists para ti <span class="meta" style="font-size:11px;margin-left:6px">${mixesAll.length}</span></span>
        ${mixesAll.length > INITIAL_LIMIT ? `<button class="btn btn-ghost btn-sm" type="button" data-recom-expand="mixes">${state.recomExpanded.mixes ? "Mostrar menos" : "Mostrar más"}</button>` : ""}
      </div>
      <div class="grid" style="margin-bottom:28px">${mixesVisible.map(videoCard).join("")}</div>
    ` : ""}
    ${(!songsAll.length && !videosAll.length && !mixesAll.length)
      ? (hasCookies
        ? empty("Sin feed cargado", "Actualiza recomendaciones o canales para llenar Para ti.")
        : empty("Configura cookies primero", "Necesitas seleccionar el navegador con tu sesión de YouTube."))
      : ""}
  `;
}

function renderShorts() {
  const root = $("#view-shorts");
  if (!root) return;
  if (!_shortsEnabled()) {
    setView("home");
    return;
  }
  const hasCookies = state.config?.cookiesBrowser || state.config?.cookiesFile;
  const shortsAll = _uniqueByUrl([
    ...state.recommendations.filter(_isShort),
    ...state.feed.filter(_isShort),
  ]);
  root.innerHTML = `
    <div class="section-hdr">
      <span class="section-title">YouTube Shorts <span class="meta" style="font-size:11px;margin-left:6px">${shortsAll.length}</span></span>
      <div style="display:flex;gap:8px;align-items:center">
        <button class="btn btn-ghost" type="button" id="refreshAllChannels">Canales</button>
        <button class="btn btn-primary" type="button" id="refreshRecom">Actualizar</button>
      </div>
    </div>
    ${hasCookies ? syncNotice("recommendations") : ""}
    ${shortsAll.length
      ? `<div class="grid grid-shorts">${shortsAll.map(videoCard).join("")}</div>`
      : empty("Sin Shorts cargados", "Actualiza tus recomendaciones o canales para ver YouTube Shorts aquí.")}
  `;
}

function _uniqueByUrl(items) {
  const seen = new Set();
  const result = [];
  for (const item of items) {
    const key = item?.url || item?.feedId || item?.id;
    if (!key || seen.has(key)) continue;
    seen.add(key);
    result.push(item);
  }
  return result;
}

function renderChannels() {
  const lastSync = state.channelsLastSync || "";
  $("#view-channels").innerHTML = `
    <div class="section-hdr">
      <span class="section-title">Canales ${lastSync ? `<span class="meta" style="font-size:10px;margin-left:8px">· sync: ${escapeHtml(lastSync)}</span>` : ""}</span>
      <span class="meta">${state.channels.length} canales</span>
    </div>
    <form class="inline-form" id="channelForm">
      <input class="form-input" name="url" placeholder="@canal o URL del canal" required>
      <input class="form-input" name="name" placeholder="Nombre opcional">
      <button class="btn btn-primary" type="submit">Agregar</button>
    </form>
    <div class="list-stack">
      ${state.channels.map((c) => {
        const count = state.feed.filter((f) => f.sourceChannelId === c.id).length;
        return `
        <div class="row-card">
          <div class="row-main" data-view-channel="${escapeHtml(c.id)}" style="cursor:pointer" title="Ver videos del canal">
            <strong>${escapeHtml(c.name)}</strong>
            <span>${escapeHtml(c.url)}</span>
            ${c.error ? `<span class="error-text">${escapeHtml(c.error)}</span>` : `<span>Última sync: ${escapeHtml(c.lastSync || "nunca")}${count ? ` · ${count} videos recientes` : ""}</span>`}
          </div>
          <button class="btn btn-ghost" type="button" data-refresh-channel="${escapeHtml(c.id)}">Actualizar</button>
          <button class="btn btn-danger" type="button" data-delete-channel="${escapeHtml(c.id)}">Eliminar</button>
        </div>`;
      }).join("")}
    </div>
  `;
}

// Normalize a channel URL like the backend `_videos_url` so we can match it
// against followed channels (state.channels[].url is stored normalized).
function _normalizeChannelUrl(url) {
  let clean = (url || "").trim();
  if (!clean) return "";
  if (!clean.startsWith("http")) clean = "https://www.youtube.com/@" + clean.replace(/^@/, "");
  if (clean.includes("youtube.com") && !/\/(videos|streams|shorts)\/?$/.test(clean)) {
    clean = clean.replace(/\/$/, "") + "/videos";
  }
  return clean;
}

function findFollowedChannel(channelUrl) {
  if (!channelUrl) return null;
  const norm = _normalizeChannelUrl(channelUrl);
  const bare = channelUrl.replace(/\/$/, "");
  return state.channels.find((c) => c.url === norm)
    || state.channels.find((c) => c.url && c.url.replace(/\/videos$/, "") === bare)
    || null;
}

function _followButtonHtml(channelUrl, name) {
  if (!channelUrl) return "";
  const ch = findFollowedChannel(channelUrl);
  if (ch) {
    return `<button class="btn btn-ghost" type="button" data-unfollow-channel="${escapeHtml(ch.id)}">Siguiendo ✓</button>`;
  }
  return `<button class="btn btn-ghost" type="button" data-add-channel="${escapeHtml(channelUrl)}" data-add-channel-name="${escapeHtml(name || "")}">＋ Seguir canal</button>`;
}

// ── Channel view (browse full uploads) ───────────────────────────────────────

const CHANNEL_TABS = [
  ["videos", "Videos"],
  ["streams", "Directos"],
  ["shorts", "Shorts"],
  ["playlists", "Playlists"],
  ["releases", "Lanzamientos"],
  ["music", "YT Music"],
];
let _channelObserver = null;
let _channelLoadId = 0;

async function openChannelView(url, name, tab = "videos", opts = {}) {
  if (!url) return;
  const cv = state.channelView;
  if (cv.url !== url || !opts._autoFallback) {
    cv._fallbackStep = 0;
  }
  cv.url = url;
  cv.name = name || "";
  cv.tab = tab;
  cv.items = [];
  cv.page = 1;
  cv.hasMore = false;
  cv.loading = true;
  cv.meta = null;
  cv.error = "";
  setView("channel");
  renderChannelView();
  await _fetchChannelPage(1);
}

async function _fetchChannelPage(page) {
  const cv = state.channelView;
  const loadId = ++_channelLoadId;
  cv.loading = true;
  try {
    const data = await api.get(`/api/channel?url=${encodeURIComponent(cv.url)}&tab=${encodeURIComponent(cv.tab)}&page=${page}`);
    if (loadId !== _channelLoadId) return;
    cv.meta = data;
    cv.name = data.name || cv.name;
    // Canonical channel URL by UC id so cache keys stay stable between @handle and UC id
    if (data.url) cv.url = data.url;
    cv.page = page;
    cv.hasMore = !!data.hasMore;
    cv.error = data.tabError || "";
    cv.loading = false;
    if (page === 1) {
      cv.items = data.items || [];
      // Dual YouTube + YT Music fallback chain: if Videos is empty, check Directos -> Lanzamientos -> YT Music,
      // staying inside the unified channel view so all tabs (YouTube & YT Music) remain accessible.
      const fallbackOrder = ["videos", "streams", "releases", "music"];
      if (!cv.items.length && cv._fallbackStep < fallbackOrder.length - 1) {
        const curIdx = fallbackOrder.indexOf(cv.tab);
        if (curIdx !== -1 && curIdx === cv._fallbackStep) {
          cv._fallbackStep += 1;
          const nextTab = fallbackOrder[cv._fallbackStep];
          openChannelView(cv.url, cv.name, nextTab, { _autoFallback: true });
          return;
        }
      }
      renderChannelView();
    } else {
      cv.items = [...cv.items, ...(data.items || [])];
      _appendChannelItems(data.items || []);
    }
  } catch (err) {
    if (loadId !== _channelLoadId) return;
    cv.loading = false;
    cv.error = err.message;
    if (page === 1) renderChannelView();
    else _updateChannelSentinel();
  }
}

function renderChannelView() {
  const cv = state.channelView;
  const root = $("#view-channel");
  if (!root) return;
  const meta = cv.meta;
  const avatar = meta?.avatar ? `<img class="channel-avatar" ${thumbAttrs(meta.avatar)}>` : `<div class="channel-avatar channel-avatar--empty"></div>`;
  const subs = meta?.subscriberCount ? `${Number(meta.subscriberCount).toLocaleString()} suscriptores` : "";
  const followBtn = meta ? (meta.alreadyFollowed
    ? `<button class="btn btn-ghost" type="button" data-unfollow-channel="${escapeHtml(meta.followedChannelId || "")}">Siguiendo ✓</button>`
    : `<button class="btn btn-primary" type="button" data-add-channel="${escapeHtml(meta.url || cv.url)}" data-add-channel-name="${escapeHtml(cv.name)}">＋ Seguir canal</button>`) : "";
  const visibleTabs = CHANNEL_TABS.filter(([key]) => key !== "shorts" || _shortsEnabled());
  const visibleItems = cv.tab === "videos"
    ? cv.items.filter((item) => !_isShort(item))
    : cv.items;
  root.innerHTML = `
    <div class="channel-header">
      ${avatar}
      <div class="channel-header-info">
        <h2 class="channel-name">${escapeHtml(cv.name || "Canal")}</h2>
        <div class="meta">${escapeHtml(meta?.handle || "")}${meta?.handle && subs ? " · " : ""}${escapeHtml(subs)}</div>
      </div>
      <div style="display:flex;gap:8px;margin-left:auto">
        ${followBtn}
        <button class="btn btn-ghost" type="button" id="backToChannels">← Volver</button>
      </div>
    </div>
    <div class="channel-tabs">
      ${visibleTabs.map(([key, label]) => `<button class="channel-tab ${key === cv.tab ? "active" : ""}" type="button" data-channel-tab="${key}">${label}</button>`).join("")}
    </div>
    <div id="channelGrid" class="grid${cv.tab === "shorts" ? " grid-shorts" : ""}">
      ${visibleItems.length ? visibleItems.map(videoCard).join("") : (cv.loading ? _skeletonCards(8) : "")}
    </div>
    ${(!cv.loading && !visibleItems.length) ? empty(cv.error ? "Sin contenido" : "Sin videos", cv.error || "Este canal no tiene contenido en esta pestaña.") : ""}
    <div id="channelSentinel" class="channel-sentinel">${cv.loading && visibleItems.length ? `<span class="spinner"></span>` : ""}</div>
  `;
  const back = $("#backToChannels");
  if (back) back.onclick = () => setView("channels");
  root.querySelectorAll("[data-channel-tab]").forEach((btn) => {
    btn.onclick = () => {
      if (btn.dataset.channelTab === cv.tab) return;
      openChannelView(cv.url, cv.name, btn.dataset.channelTab);
    };
  });
  _setupChannelObserver();
}

function _skeletonCards(n) {
  return Array.from({ length: n }, () => `
    <article class="card card--skeleton">
      <div class="thumb skeleton-box"></div>
      <div class="card-body">
        <div class="skeleton-line" style="width:90%"></div>
        <div class="skeleton-line" style="width:55%"></div>
      </div>
    </article>`).join("");
}

function _appendChannelItems(items) {
  const grid = $("#channelGrid");
  const filtered = state.channelView.tab === "videos" ? items.filter((item) => !_isShort(item)) : items;
  if (grid && filtered.length) grid.insertAdjacentHTML("beforeend", filtered.map(videoCard).join(""));
  _updateChannelSentinel();
}

function _updateChannelSentinel() {
  const sentinel = $("#channelSentinel");
  if (sentinel) sentinel.innerHTML = state.channelView.loading ? `<span class="spinner"></span>` : "";
}

function _setupChannelObserver() {
  if (_channelObserver) _channelObserver.disconnect();
  const sentinel = $("#channelSentinel");
  if (!sentinel) return;
  _channelObserver = new IntersectionObserver((entries) => {
    const cv = state.channelView;
    if (entries.some((e) => e.isIntersecting) && cv.hasMore && !cv.loading && state.view === "channel") {
      _updateChannelSentinel();
      _fetchChannelPage(cv.page + 1).then(_updateChannelSentinel);
    }
  }, { rootMargin: "600px" });
  _channelObserver.observe(sentinel);
}

// ── Artist view (YT Music) ────────────────────────────────────────────────────

async function openArtistView(channelId, name) {
  if (!channelId) return;
  const url = channelId.startsWith("http")
    ? channelId
    : `https://www.youtube.com/channel/${channelId}`;
  await openChannelView(url, name, "music");
}

async function _legacyOpenArtistView(channelId, name) {
  if (!channelId) return;
  const av = state.artistView;
  av.channelId = channelId;
  av.name = name || "";
  av.data = null;
  av.loading = true;
  av.error = "";
  setView("artist");
  renderArtistView();
  try {
    const data = await api.get(`/api/music/artist?channel_id=${encodeURIComponent(channelId)}`);
    if (state.artistView.channelId !== channelId) return;
    av.data = data;
    av.name = data.name || av.name;
    av.loading = false;
  } catch (err) {
    if (state.artistView.channelId !== channelId) return;
    av.loading = false;
    av.error = err.message;
  }
  renderArtistView();
}

function renderArtistView() {
  const av = state.artistView;
  const root = $("#view-artist");
  if (!root) return;
  const d = av.data;
  if (av.loading) {
    root.innerHTML = `
      <div class="channel-header">
        <div class="channel-avatar channel-avatar--empty skeleton-box"></div>
        <div class="channel-header-info"><h2 class="channel-name">${escapeHtml(av.name || "Artista")}</h2></div>
      </div>
      <div class="grid">${_skeletonCards(8)}</div>`;
    return;
  }
  if (!d) {
    root.innerHTML = `
      <div class="section-hdr">
        <span class="section-title">${escapeHtml(av.name || "Artista")}</span>
        <button class="btn btn-ghost" type="button" id="backFromArtist">← Volver</button>
      </div>
      ${empty("Error", av.error || "No se pudo cargar el artista.")}`;
    const b = $("#backFromArtist");
    if (b) b.onclick = () => setView("music");
    return;
  }
  const songRows = (d.songs || []).map((song, i) => `
    <div class="queue-item" data-artist-play="${i}">
      <span class="queue-idx">${i + 1}</span>
      ${song.thumbnail ? `<img class="queue-thumb" ${thumbAttrs(song.thumbnail)}>` : `<div class="queue-thumb"></div>`}
      <div class="queue-info">
        <div class="queue-title">${escapeHtml(song.title)}</div>
        <div class="queue-meta">${escapeHtml(song.channel || "")} · ${escapeHtml(song.durationText || "")}</div>
      </div>
    </div>`).join("");
  const releaseCard = (rel) => `
    <article class="card">
      <div class="thumb">${rel.thumbnail ? `<img ${thumbAttrs(rel.thumbnail)}>` : ""}<span class="duration">${escapeHtml(rel.durationText || "")}</span></div>
      <div class="card-body">
        <div class="card-title">${escapeHtml(rel.title)}</div>
        <div class="meta">${escapeHtml(rel.channel || "")}</div>
      </div>
      <div class="card-actions">
        <button class="btn btn-primary" type="button" data-album-play="${escapeHtml(rel.url)}">▶ Reproducir</button>
      </div>
    </article>`;
  const relatedCard = (a) => `
    <article class="card">
      <div class="thumb">${a.thumbnail ? `<img ${thumbAttrs(a.thumbnail)}>` : ""}</div>
      <div class="card-body">
        <div class="card-title">${escapeHtml(a.title)}</div>
        <div class="meta">${escapeHtml(a.channel || "")}</div>
      </div>
      <div class="card-actions">
        <button class="btn btn-ghost" type="button" data-open-artist="${escapeHtml(a.id)}" data-open-artist-name="${escapeHtml(a.title)}">Ver artista</button>
      </div>
    </article>`;
  root.innerHTML = `
    <div class="channel-header">
      ${d.thumbnail ? `<img class="channel-avatar" ${thumbAttrs(d.thumbnail)}>` : `<div class="channel-avatar channel-avatar--empty"></div>`}
      <div class="channel-header-info">
        <h2 class="channel-name">${escapeHtml(d.name)}</h2>
        <div class="meta">${escapeHtml(d.subscriberCount ? `${d.subscriberCount} suscriptores` : "Artista · YT Music")}</div>
      </div>
      <div style="display:flex;gap:8px;margin-left:auto">
        ${d.songs?.length ? `<button class="btn btn-primary" type="button" data-artist-play="0">▶ Reproducir top</button>` : ""}
        <button class="btn btn-ghost" type="button" id="backFromArtist">← Volver</button>
      </div>
    </div>
    ${d.songs?.length ? `<section class="artist-section"><div class="section-hdr"><span class="section-title">Canciones top</span></div><div class="list-stack">${songRows}</div></section>` : ""}
    ${d.albums?.length ? `<section class="artist-section"><div class="section-hdr"><span class="section-title">Álbumes</span></div><div class="grid">${d.albums.map(releaseCard).join("")}</div></section>` : ""}
    ${d.singles?.length ? `<section class="artist-section"><div class="section-hdr"><span class="section-title">Singles</span></div><div class="grid">${d.singles.map(releaseCard).join("")}</div></section>` : ""}
    ${d.related?.length ? `<section class="artist-section"><div class="section-hdr"><span class="section-title">Artistas similares</span></div><div class="grid">${d.related.map(relatedCard).join("")}</div></section>` : ""}
  `;
  const back = $("#backFromArtist");
  if (back) back.onclick = () => setView("music");
}

function _refreshAfterFollowChange() {
  if (state.view === "watch") {
    _updateWatchMeta();
  } else if (state.view === "channel" && state.channelView.meta) {
    // Recalcular seguimiento contra state.channels (meta del backend quedo stale)
    const meta = state.channelView.meta;
    const followed = findFollowedChannel(meta.url || state.channelView.url);
    meta.alreadyFollowed = !!followed;
    meta.followedChannelId = followed?.id || null;
    renderChannelView();
  }
}

function renderLibrary() {
  const visible = state.library.slice(0, _libraryLimit);
  const hasMore = state.library.length > _libraryLimit;
  $("#view-library").innerHTML = `
    <div class="section-hdr"><span class="section-title">Biblioteca</span><span class="meta">${state.library.length} archivos</span></div>
    ${visible.length ? `<div class="grid">${visible.map(libraryCard).join("")}</div>` : empty("Biblioteca vacía", "Los archivos descargados aparecerán aquí.")}
    ${hasMore ? `<div style="text-align:center;padding:16px 0"><button class="btn btn-ghost" type="button" onclick="_libraryLimit += 100; renderLibrary()">Ver más (${state.library.length - _libraryLimit} restantes)</button></div>` : ""}
  `;
}

async function _pollTask(taskId, maxTicks = 600) {
  // Poll a background task until it finishes; returns the final task or null.
  if (!taskId) return null;
  for (let i = 0; i < maxTicks; i++) {
    await new Promise((r) => setTimeout(r, 2000));
    let task;
    try {
      task = await api.get(`/api/tasks/${taskId}`);
    } catch (_) { continue; }
    if (task.status === "done" || task.status === "error") return task;
  }
  return null;
}

async function _onAuthSettled(silent = false) {
  // Refresh state after an auth task; if logged in, pull music content.
  await refreshLists();
  _musicSyncTriggered = false;
  if (state.musicAuthState === "ok") {
    toast(state.musicAccountName ? `Conectado como ${state.musicAccountName}` : "Sesión iniciada.");
    try {
      await api.post("/api/music/recommendations/refresh");
      await api.post("/api/music/playlists/refresh");
    } catch (_) {}
  } else if (!silent) {
    toast("La sesión sigue sin validar. Asegúrate de iniciar sesión completamente.");
  }
  if (state.view === "music") renderMusic();
}

async function _awaitLoginTask(taskId) {
  const task = await _pollTask(taskId);
  if (!task) return;
  if (task.status === "error") {
    toast(task.error || "Error al iniciar sesión");
    return;
  }
  await _onAuthSettled();
}

async function logoutMusicAccount() {
  try {
    await api.post("/api/music/logout");
    state.musicAuthState = "expired";
    state.musicAccountName = "";
    _musicSyncTriggered = false;
    renderAuthStatus();
    await refreshLists();
    render();
    toast("Sesión cerrada.");
  } catch (err) {
    toast(err.message);
  }
}

async function openConnectedAccountModal() {
  try {
    const resp = await api.post("/api/music/auth/refresh");
    const accounts = Array.isArray(resp?.accounts) && resp.accounts.length
      ? resp.accounts
      : [{ authUser: resp?.activeAuthUser || 0, name: state.musicAccountName || "Cuenta actual", handle: "", photo: "" }];
    openAccountPickerModal(accounts, resp?.activeAuthUser || 0, resp?.browser || "", true);
  } catch (err) {
    openAccountPickerModal(
      [{ authUser: 0, name: state.musicAccountName || "Cuenta actual", handle: "", photo: "" }],
      0,
      "",
      true,
    );
  }
}

function openAccountPickerModal(accounts, activeAuthUser = 0, browserName = "", showLogout = false) {
  const root = $("#modalRoot");
  if (!root) return;
  const close = () => { root.innerHTML = ""; };
  const subtitle = browserName
    ? `Cuentas detectadas en ${escapeHtml(browserName)}. Elige cuál usar en Playzi:`
    : "Elige qué cuenta de YouTube usar en Playzi:";
  const rows = (accounts || []).map((acc) => {
    const isActive = Number(acc.authUser) === Number(activeAuthUser);
    const avatar = acc.photo
      ? `<img src="${escapeHtml(acc.photo)}" alt="" style="width:36px;height:36px;border-radius:50%;object-fit:cover;flex-shrink:0">`
      : `<div style="width:36px;height:36px;border-radius:50%;background:var(--surface-2);display:flex;align-items:center;justify-content:center;font-weight:700;flex-shrink:0">${escapeHtml((acc.name || "?").slice(0, 1).toUpperCase())}</div>`;
    return `
      <button type="button" class="btn btn-ghost" data-pick-authuser="${Number(acc.authUser)}" style="width:100%;justify-content:flex-start;gap:12px;padding:10px 12px;text-align:left;border:1px solid ${isActive ? "var(--accent)" : "var(--border)"};margin-bottom:8px">
        ${avatar}
        <div style="flex:1;min-width:0">
          <div style="font-weight:600;overflow:hidden;text-overflow:ellipsis;white-space:nowrap">${escapeHtml(acc.name || `Cuenta ${acc.authUser}`)}</div>
          ${acc.handle ? `<div class="meta" style="font-size:12px;overflow:hidden;text-overflow:ellipsis;white-space:nowrap">${escapeHtml(acc.handle)}</div>` : ""}
        </div>
        ${isActive ? `<span class="badge" style="font-size:11px">Activa</span>` : ""}
      </button>
    `;
  }).join("");

  const canLogout = showLogout || state.musicAuthState === "ok";
  root.innerHTML = `
    <div class="modal-backdrop" id="accountPickerBackdrop">
      <div class="modal" style="max-width:440px">
        <div class="modal-header">
          <h3>Cuenta de YouTube</h3>
          <button class="icon-btn" id="accountPickerClose" type="button">×</button>
        </div>
        <div class="modal-body">
          <p class="meta" style="margin-bottom:12px">${subtitle}</p>
          <div>${rows}</div>
        </div>
        <div class="modal-actions" style="justify-content:space-between;flex-wrap:wrap;gap:8px;margin-top:12px">
          <button class="btn btn-ghost" id="accountPickerOpenBrowser" type="button">Usar otra cuenta (navegador)</button>
          <div style="display:flex;gap:8px">
            ${canLogout ? `<button class="btn btn-ghost" id="accountPickerLogout" type="button" style="color:var(--danger, #f87171)">Cerrar sesión</button>` : ""}
            <button class="btn btn-ghost" id="accountPickerCancel" type="button">Cerrar</button>
          </div>
        </div>
      </div>
    </div>
  `;

  $("#accountPickerClose").onclick = close;
  $("#accountPickerCancel").onclick = close;
  $("#accountPickerBackdrop").onclick = (ev) => {
    if (ev.target.id === "accountPickerBackdrop") close();
  };
  const logoutBtn = $("#accountPickerLogout");
  if (logoutBtn) {
    logoutBtn.onclick = async () => {
      close();
      await logoutMusicAccount();
    };
  }
  $("#accountPickerOpenBrowser").onclick = async () => {
    close();
    try {
      const visible = await api.post("/api/music/login");
      const b = visible?.browser ? ` (${visible.browser})` : "";
      toast(`Se abrió el selector de Google en tu navegador${b}. Inicia sesión y Playzi la detectará automáticamente.`);
    } catch (err) {
      toast(err.message);
    }
  };
  root.querySelectorAll("[data-pick-authuser]").forEach((btn) => {
    btn.onclick = async () => {
      const idx = Number(btn.dataset.pickAuthuser || 0);
      close();
      try {
        toast("Cambiando de cuenta...");
        await api.post("/api/music/accounts/select", { authUser: idx });
        await _onAuthSettled(false);
      } catch (err) {
        toast(err.message);
      }
    };
  });
}

// Silent-first login: import cookies from the default browser; if multiple
// Google accounts are signed in, show an account picker; otherwise open the
// Google AccountChooser in the default browser.
let _loginFlowBusy = false;
async function startLoginFlow() {
  if (_loginFlowBusy) return;
  _loginFlowBusy = true;
  try {
    toast("Buscando sesión en tu navegador predeterminado...");
    const resp = await api.post("/api/music/auth/refresh", { forceImport: true });
    if (resp?.task?.id) {
      await _pollTask(resp.task.id, 60);
    }
    await refreshLists();

    const accounts = Array.isArray(resp?.accounts) ? resp.accounts : [];
    if (accounts.length > 1) {
      openAccountPickerModal(accounts, resp.activeAuthUser || 0, resp.browser || "");
      return;
    }
    if (state.musicAuthState === "ok" || resp?.ok) {
      await _onAuthSettled(true);
      return;
    }
    if (resp?.error) {
      toast(resp.error);
      return;
    }
    // Not signed in yet -> open Google AccountChooser in the system default browser.
    const visible = await api.post("/api/music/login");
    if (visible?.mode === "external-browser") {
      const b = visible.browser ? ` (${visible.browser})` : "";
      toast(`Se abrió tu navegador predeterminado${b}. Inicia sesión en YouTube Music y Playzi detectará tu sesión automáticamente.`);
    } else {
      toast("Abriendo navegador... inicia sesión en YouTube y cierra la ventana al terminar.");
      await _awaitLoginTask(visible?.task?.id);
    }
  } catch (err) {
    toast(err.message);
  } finally {
    _loginFlowBusy = false;
  }
}

let _musicSyncTriggered = false;
async function _autoTriggerMusicSync() {
  const hasCookies = state.config?.cookiesBrowser || state.config?.cookiesFile;
  if (!hasCookies || _musicSyncTriggered || state.musicAuthState === "expired") return;
  const needsRecom = !state.musicRecomLastSync && !state.syncStatus?.sections?.musicRecommendations?.running;
  const needsPlaylists = !state.musicPlaylistsLastSync && !state.syncStatus?.sections?.musicPlaylists?.running;
  if (!needsRecom && !needsPlaylists) return;
  _musicSyncTriggered = true;
  try {
    if (needsRecom) await api.post("/api/music/recommendations/refresh");
    if (needsPlaylists) await api.post("/api/music/playlists/refresh");
    toast("Cargando YT Music en segundo plano...");
  } catch (_) {
    _musicSyncTriggered = false;
  }
}

function renderAuthStatus() {
  const el = $("#authStatus");
  if (!el) return;
  const ok = state.musicAuthState === "ok";
  const name = state.musicAccountName || "";
  el.classList.toggle("auth-ok", ok);
  el.classList.toggle("auth-off", !ok);
  const label = ok ? (name || "Conectado") : "Iniciar sesión";
  el.title = ok ? `Conectado como ${name || "tu cuenta"}` : "Iniciar sesión con Google";
  el.innerHTML = `<span class="auth-dot"></span><span class="auth-label">${escapeHtml(label)}</span>`;
}

function _musicAuthBanner() {
  if (state.musicAuthState !== "expired") return "";
  return `<div class="url-banner" style="margin-bottom:16px;border-color:oklch(.54 .20 25)">
    <strong style="color:oklch(.62 .20 25)">⚠</strong>
    <div style="min-width:0;flex:1">
      <div class="url-banner-title" style="color:oklch(.62 .20 25)">Sesión de YouTube expirada</div>
      <div class="url-banner-sub">Tu sesión ya no es válida. Vuelve a iniciar sesión para ver tus recomendaciones personalizadas.</div>
    </div>
    <button class="btn btn-primary" type="button" data-music-login>Iniciar sesión</button>
  </div>`;
}

function renderMusic() {
  const hasCookies = state.config?.cookiesBrowser || state.config?.cookiesFile;
  const expired = state.musicAuthState === "expired";
  const local = state.library.filter((item) => item.kind === "audio" || ["MP3", "OPUS", "M4A"].includes(String(item.format || "").toUpperCase()));
  const musicMixes = _uniqueByUrl(state.musicRecommendations.filter(_isRecommendedMix));
  const musicTracks = _uniqueByUrl(state.musicRecommendations.filter((item) => !_isRecommendedMix(item)));
  $("#view-music").innerHTML = `
    <div class="section-hdr">
      <span class="section-title">YT Music — Recomendadas ${state.musicRecomLastSync ? `<span class="meta" style="font-size:10px;margin-left:6px">· ${escapeHtml(state.musicRecomLastSync)}</span>` : ""}</span>
      <button class="btn btn-primary" type="button" id="refreshMusicRecom">Actualizar</button>
    </div>
    ${!hasCookies ? `<div class="url-banner" style="margin-bottom:16px"><strong>⚙</strong><div style="min-width:0;flex:1"><div class="url-banner-title">Cookies requeridas</div><div class="url-banner-sub">Configura el navegador en Configuración.</div></div><button class="btn btn-ghost" type="button" data-view-go="config">Configurar</button></div>` : ""}
    ${_musicAuthBanner()}
    ${hasCookies && !expired ? syncNotice("musicRecommendations") : ""}
    ${!expired && musicMixes.length ? `
      <div class="section-hdr" style="margin-top:4px;margin-bottom:12px">
        <span class="section-title">Mixes y playlists para ti</span>
        <span class="meta">${musicMixes.length} mixes</span>
      </div>
      <div class="grid" style="margin-bottom:28px">${musicMixes.map(videoCard).join("")}</div>
    ` : ""}
    ${expired
      ? `<div style="margin-bottom:32px">${empty("Contenido no personalizado", "Inicia sesión arriba para cargar tus recomendaciones reales.")}</div>`
      : musicTracks.length
      ? `<div class="grid" style="margin-bottom:32px">${musicTracks.map(videoCard).join("")}</div>`
      : (!musicMixes.length && hasCookies) ? `<div style="margin-bottom:32px">${
          state.musicRecomLastSync
            ? empty("Sin recomendaciones", "El sync no devolvió contenido. Intenta de nuevo con Actualizar.")
            : empty("Recomendaciones no cargadas", "Haz clic en Actualizar para cargar tu feed de YouTube Music.")
        }</div>` : ""}

    <div class="section-hdr" style="margin-top:8px">
      <span class="section-title">Mis Playlists ${state.musicPlaylistsLastSync ? `<span class="meta" style="font-size:10px;margin-left:6px">· ${escapeHtml(state.musicPlaylistsLastSync)}</span>` : ""}</span>
      <button class="btn btn-ghost" type="button" id="refreshMusicPlaylists">Actualizar</button>
    </div>
    ${hasCookies ? syncNotice("musicPlaylists") : ""}
    ${state.musicPlaylists.length
      ? `<div class="collection-grid" style="margin-bottom:32px">
          ${state.musicPlaylists.map((p) => `
            <div style="position:relative">
              <button class="collection-card" type="button" data-music-playlist="${escapeHtml(p.url)}" data-music-playlist-title="${escapeHtml(p.title)}" style="width:100%">
                ${p.thumbnail ? `<img src="${escapeHtml(thumbSrc(p.thumbnail))}" style="width:38px;height:38px;border-radius:8px;object-fit:cover" onerror="this.style.display='none'">` : `<span>♪</span>`}
                <strong>${escapeHtml(p.title)}</strong>
                <small>${p.count ? `${p.count} canciones` : "Playlist"}</small>
              </button>
            </div>`).join("")}
         </div>`
      : hasCookies ? `<div style="margin-bottom:32px">${empty("Sin playlists", "Playzi las cargará automáticamente cuando el sync termine.")}</div>` : ""}

    <div class="section-hdr" style="margin-top:8px">
      <span class="section-title">Biblioteca local</span>
      <span class="meta">${local.length} audios</span>
    </div>
    ${local.length ? `<div class="grid">${local.map(libraryCard).join("")}</div>` : empty("Sin música guardada", "Descarga como Audio para llenar esta sección.")}
  `;
}

function renderDownloads() {
  $("#view-downloads").innerHTML = `
    <div class="section-hdr"><span class="section-title">Descargas</span><span class="meta">${state.downloads.length} tareas</span></div>
    ${state.downloads.length ? state.downloads.map(downloadItem).join("") : empty("Sin descargas", "Inicia una descarga desde Buscar o Para ti.")}
  `;
}

function playlistGrid(playlists) {
  return `
    <div class="collection-grid">
      ${playlists.map((pl) => `
        <div style="position:relative">
          <button class="collection-card" type="button" data-playlist-id="${escapeHtml(pl.id)}" style="width:100%">
            ${pl.thumbnail ? `<img class="collection-thumb" ${thumbAttrs(pl.thumbnail)}>` : `<span>▶</span>`}
            <strong>${escapeHtml(pl.name)}</strong>
            <small>${pl.itemCount || pl.items?.length || 0} items · ${escapeHtml(sourceLabel(pl.source || "playzi"))}${pl.importMode === "linked" ? " · vinculada" : ""}</small>
          </button>
          ${pl.importMode === "linked" ? `<button class="btn btn-ghost" type="button" data-sync-playlist="${escapeHtml(pl.id)}" style="position:absolute;top:8px;left:8px;padding:3px 7px;font-size:11px" title="Sincronizar">Sync</button>` : ""}
          <button class="btn btn-danger" type="button" data-delete-playlist="${escapeHtml(pl.id)}" style="position:absolute;top:8px;right:8px;padding:3px 7px;font-size:11px" title="Eliminar playlist">✕</button>
        </div>`).join("")}
      ${playlists.length === 0 ? `<p class="meta" style="padding:8px 0">Sin playlists. Crea una, importa una o usa "+ Lista" en cualquier video.</p>` : ""}
    </div>
  `;
}

function renderCollections() {
  $("#view-collections").innerHTML = `
    <div class="section-hdr">
      <span class="section-title">Playlists</span>
      <div style="display:flex;gap:8px;align-items:center">
        <span class="meta">${state.playlists.length} playlists</span>
        <button class="btn btn-primary" type="button" data-open-import-playlist>Importar</button>
      </div>
    </div>
    <form class="inline-form" id="playlistForm">
      <input class="form-input" name="name" placeholder="Nueva playlist..." required>
      <button class="btn btn-primary" type="submit">Crear</button>
    </form>
    ${playlistGrid(state.playlists)}
    <hr style="margin:24px 0; border-color: var(--br)">
    <div class="section-hdr"><span class="section-title">Colecciones (descargados)</span><span class="meta">${state.collections.length} colecciones</span></div>
    <form class="inline-form" id="collectionForm">
      <input class="form-input" name="name" placeholder="Nueva colección" required>
      <button class="btn btn-primary" type="submit">Crear</button>
    </form>
    <div class="collection-grid">
      ${state.collections.map((c) => `
        <div style="position:relative">
          <button class="collection-card" type="button" data-collection="${escapeHtml(c.name)}" style="width:100%">
            <span>▥</span>
            <strong>${escapeHtml(c.name)}</strong>
            <small>${Number(c.count || 0)} archivos</small>
          </button>
          <button class="btn btn-danger" type="button" data-delete-collection="${escapeHtml(c.id)}" style="position:absolute;top:8px;right:8px;padding:3px 7px;font-size:11px" title="Eliminar colección">✕</button>
        </div>`).join("")}
    </div>
  `;
}

function historyCard(item) {
  const url = item.url || item.target || "";
  const isRemoteUrl = /^https?:\/\//i.test(item.url || "");
  const canPlay = Boolean(url);
  const thumb = item.thumbnail || deriveThumbnail(url);
  return `
    <div class="hist-item hist-item--rich">
      ${thumb ? `<img class="mini-thumb" ${thumbAttrs(thumb)}>` : `<div class="mini-thumb mini-thumb--empty"></div>`}
      <div class="hist-info">
        <div class="hist-title">${escapeHtml(item.title || item.target || item.url || "Sin titulo")}</div>
        <div class="meta">
          <span class="tag">${escapeHtml(actionLabel(item.action || item.kind || "evento"))}</span>
          <span class="tag tag-muted">${escapeHtml(sourceLabel(item.source || ""))}</span>
          ${escapeHtml(item.channel || item.player || "")} ${escapeHtml(item.date || item.createdAt || "")}
        </div>
      </div>
      <div class="hist-actions">
        ${canPlay ? `<button class="btn btn-primary" type="button" data-history-play="${escapeHtml(url)}" data-history-title="${escapeHtml(item.title || "")}">▶ Reproducir</button>` : ""}
        ${isRemoteUrl ? `<button class="btn btn-ghost" type="button" data-download-history="audio" data-history-id="${escapeHtml(item.id)}">Audio</button>` : ""}
        ${isRemoteUrl ? `<button class="btn btn-ghost" type="button" data-add-history-playlist="${escapeHtml(item.id)}">+ Lista</button>` : ""}
        ${isRemoteUrl ? `<a class="btn btn-ghost" href="${escapeHtml(item.url)}" target="_blank">Abrir</a>` : ""}
        <button class="btn btn-danger" type="button" data-delete-history="${escapeHtml(item.id)}">Eliminar</button>
      </div>
    </div>
  `;
}

function renderHistory() {
  const visible = state.history.slice(0, _historyLimit);
  const hasMore = state.history.length > _historyLimit;
  $("#view-history").innerHTML = `
    <div class="section-hdr"><span class="section-title">Historial</span><span class="meta">${state.history.length} eventos</span></div>
    <div class="history-list">
      ${visible.length ? visible.map(historyCard).join("") : empty("Sin historial", "Las descargas y reproducciones aparecerán aquí.")}
    </div>
    ${hasMore ? `<div style="text-align:center;padding:16px 0"><button class="btn btn-ghost" type="button" onclick="_historyLimit += 100; renderHistory()">Ver más (${state.history.length - _historyLimit} restantes)</button></div>` : ""}
  `;
}

function sourceLabel(source) {
  return ({ youtube: "YouTube", music: "YT Music", local: "Local", playzi: "Playzi", playlist: "Playlist" }[source] || source || "Playzi");
}

function actionLabel(action) {
  return ({ play: "Reproducción", download: "Descarga", import: "Importación", sync: "Sync", playlist: "Playlist" }[action] || action);
}

function _formatUpdateStatusHtml() {
  if (!IS_TAURI) return "";
  const u = state.updateStatus || {};
  const curVer = u.currentVersion ? `v${escapeHtml(u.currentVersion)}` : "Desconocida";
  const latestVer = u.latestVersion ? `v${escapeHtml(u.latestVersion)}` : "";
  const busy = u.status === "checking" || u.status === "downloading" || u.status === "installing";
  let msg = "Las actualizaciones se revisan e instalan automáticamente al iniciar.";
  if (u.status === "checking") msg = "Buscando nuevas versiones en GitHub Releases...";
  else if (u.status === "up-to-date") msg = `Tienes la última versión disponible (${curVer}).`;
  else if (u.status === "available") msg = `Nueva versión ${latestVer} disponible. Preparando descarga...`;
  else if (u.status === "downloading") msg = `Descargando ${latestVer} (${Math.round(u.progress || 0)}%)...`;
  else if (u.status === "installing") msg = `Instalando ${latestVer}... La aplicación se reiniciará automáticamente.`;
  else if (u.status === "error") msg = `Error al revisar actualizaciones: ${escapeHtml(u.error || "desconocido")}`;

  const progressBar = u.status === "downloading" ? `
    <div style="margin-top:10px;height:6px;background:var(--bg3);border-radius:999px;overflow:hidden">
      <div style="height:100%;width:${Math.min(100, Math.max(0, Number(u.progress || 0)))}%;background:var(--accent);transition:width .2s ease"></div>
    </div>` : "";

  return `
    <div class="section-hdr"><span class="section-title">Actualizaciones</span></div>
    <div class="config-panel" style="margin-bottom:16px">
      <div style="display:flex;align-items:center;justify-content:space-between;gap:12px;flex-wrap:wrap">
        <div>
          <div style="font-weight:600;margin-bottom:4px">Playzi ${curVer}</div>
          <div style="font-size:13px;color:var(--tx2)">${msg}</div>
        </div>
        <button type="button" class="btn btn-primary" onclick="checkForAppUpdates()" ${busy ? "disabled" : ""}>
          ${busy ? "Actualizando..." : "Buscar actualizaciones"}
        </button>
      </div>
      ${progressBar}
    </div>
  `;
}

async function checkForAppUpdates() {
  if (!IS_TAURI) return;
  try {
    const status = await api.post("/api/update/check", { autoInstall: true });
    state.updateStatus = status;
    if (state.view === "config") renderConfig();
  } catch (err) {
    toast(errorText(err));
  }
}

function renderConfig() {
  const c = state.config || {};
  const playbackQuality = c.playbackQuality || "best";
  const curColor = document.documentElement.dataset.color || "pink";
  $("#view-config").innerHTML = `
    ${_formatUpdateStatusHtml()}
    <div class="section-hdr"><span class="section-title">Apariencia</span></div>
    <div class="config-panel" style="margin-bottom:16px">
      <div style="margin-bottom:16px">
        <div class="section-title" style="margin-bottom:10px">Tema</div>
        <div style="display:flex;gap:8px">
          <button type="button" class="btn ${state.theme === "dark" ? "btn-primary" : "btn-ghost"}" onclick="setTheme('dark')" style="gap:7px">${MOON_SVG} Oscuro</button>
          <button type="button" class="btn ${state.theme === "light" ? "btn-primary" : "btn-ghost"}" onclick="setTheme('light')" style="gap:7px">${SUN_SVG} Claro</button>
        </div>
      </div>
      <div>
        <div class="section-title" style="margin-bottom:10px">Color de acento</div>
        <div style="display:flex;gap:14px;flex-wrap:wrap">
          ${COLOR_PRESETS.map(([id, label]) => `
            <div style="display:flex;flex-direction:column;align-items:center;gap:6px">
              <button class="color-dot${curColor === id ? " active" : ""}" data-color-preset="${id}" style="width:36px;height:36px"></button>
              <span style="font-size:11px;color:var(--tx2)">${label}</span>
            </div>`).join("")}
        </div>
      </div>
    </div>
    <div class="section-hdr"><span class="section-title">Configuración general</span></div>
    <form class="config-panel edit-config" id="configForm">
      <label>MEDIA_ROOT<input class="form-input" name="mediaRoot" value="${escapeHtml(c.mediaRoot || "")}"></label>
      <label>Reproductor por defecto
        <select class="form-input" name="defaultPlayer">
          <option value="mpv" ${c.defaultPlayer === "mpv" ? "selected" : ""}>mpv</option>
          <option value="vlc" ${c.defaultPlayer === "vlc" ? "selected" : ""}>VLC</option>
        </select>
      </label>
      <label>Ruta mpv<input class="form-input" name="mpvPath" value="${escapeHtml(c.players?.mpv || "")}"></label>
      <label>Ruta VLC<input class="form-input" name="vlcPath" value="${escapeHtml(c.players?.vlc || "")}"></label>
      <label>Ruta ffmpeg<input class="form-input" name="ffmpegPath" value="${escapeHtml(c.ffmpeg || "")}"></label>
      <label>Formato audio
        <select class="form-input" name="audioFormat">
          ${["opus", "mp3", "m4a"].map((fmt) => `<option value="${fmt}" ${c.audioFormat === fmt ? "selected" : ""}>${fmt}</option>`).join("")}
        </select>
      </label>
      <label>Calidad reproduccion
        <select class="form-input" name="playbackQuality">
          ${["best", "2160p", "1440p", "1080p", "720p", "480p", "360p"].map((q) => `<option value="${q}" ${playbackQuality === q ? "selected" : ""}>${q}</option>`).join("")}
        </select>
      </label>
      <label>Límite búsqueda<input class="form-input" name="searchLimit" type="number" min="1" max="50" value="${escapeHtml(c.searchLimit || 12)}"></label>
      <label>Navegador para cookies automaticas
        <select class="form-input" name="cookiesBrowser">
          ${["", "playzi", "zen", "firefox", "chrome", "edge", "brave", "chromium", "opera", "vivaldi"].map((b) =>
            `<option value="${b}" ${(c.cookiesBrowser || "") === b ? "selected" : ""}>${b === "playzi" ? "Navegador predeterminado (automático)" : (b || "Ninguno")}</option>`
          ).join("")}
        </select>
      </label>
      <label>Archivo cookies.txt (alternativo; también puedes dejarlo en D:\\ytlocal\\cookies)
        <input class="form-input" name="cookiesFile" value="${escapeHtml(c.cookiesFile || "")}" placeholder="C:\\ruta\\cookies.txt">
      </label>
      <label class="config-toggle">
        <input type="checkbox" name="showShorts" ${c.showShorts ? "checked" : ""}>
        <div>
          <div class="config-toggle-title">Mostrar sección de YouTube Shorts</div>
          <div class="config-toggle-sub">Por defecto los Shorts se ocultan y se separan de los videos normales. Activa esta opción para ver la sección de Shorts.</div>
        </div>
      </label>
      <div style="border-top:1px solid var(--br);padding-top:14px;margin-top:6px">
        <div style="display:flex;align-items:center;justify-content:space-between;gap:10px;margin-bottom:10px;flex-wrap:wrap">
          <div class="section-title" style="margin:0">Integración con Discord</div>
          <div style="display:flex;align-items:center;gap:8px">
            ${(() => {
              const d = state.discord || {};
              if (!c.discordEnabled) return `<span class="party-pill">Desactivado</span>`;
              if (d.connected) {
                const label = d.globalName || d.username || "Conectado";
                return `<span class="party-pill party-pill--ok">Conectado: ${escapeHtml(label)}</span>`;
              }
              if (d.configured === false) return `<span class="party-pill party-pill--warn">No disponible</span>`;
              return `<span class="party-pill party-pill--warn">Desconectado</span>`;
            })()}
            <button class="btn btn-ghost" type="button" data-discord-reconnect="1" style="padding:4px 10px;font-size:.78rem">Reconectar</button>
          </div>
        </div>
        ${state.discord?.lastError && c.discordEnabled && !state.discord?.connected ? `<div class="muted" style="font-size:.8rem;color:#fbbf24;margin-bottom:10px">${escapeHtml(state.discord.lastError)}</div>` : ""}
        <div style="display:flex;flex-direction:column;gap:10px">
          <label class="config-toggle">
            <input type="checkbox" name="discordEnabled" ${c.discordEnabled !== false ? "checked" : ""}>
            <div>
              <div class="config-toggle-title">Activar integración con Discord (Rich Presence)</div>
              <div class="config-toggle-sub">Conecta Playzi con tu aplicación de escritorio de Discord para mostrar tu actividad y habilitar invitaciones.</div>
            </div>
          </label>
          <label class="config-toggle">
            <input type="checkbox" name="discordShowMedia" ${c.discordShowMedia !== false ? "checked" : ""}>
            <div>
              <div class="config-toggle-title">Mostrar en el perfil lo que estás viendo o escuchando</div>
              <div class="config-toggle-sub">Muestra el título, canal/artista, miniatura y progreso de reproducción en tu perfil de Discord.</div>
            </div>
          </label>
          <label class="config-toggle">
            <input type="checkbox" name="discordShowParty" ${c.discordShowParty !== false ? "checked" : ""}>
            <div>
              <div class="config-toggle-title">Mostrar sala activa y permitir invitaciones / solicitudes de unión</div>
              <div class="config-toggle-sub">Muestra que estás en una sala con tus amigos, habilita el botón de invitar (+) en Discord y permite recibir solicitudes de "Pedir unirse".</div>
            </div>
          </label>
        </div>
      </div>
      <button class="btn btn-primary" type="submit">Guardar configuración</button>
    </form>
  `;
}

// ── Watch view ────────────────────────────────────────────────────────────────

function _watchSidebarHtml() {
  const c = state.watchSidebarCollapsed || {};
  const partyActive = Boolean(state.party?.active);
  return `
      <div class="watch-sidebar">
        <div class="watch-sidebar-section watch-sidebar-section--users${c.users ? " is-collapsed" : ""}" id="watch-users-section"${partyActive ? "" : ' style="display:none"'}>
          <button class="watch-queue-hdr" type="button" data-watch-section-toggle="users" aria-expanded="${c.users ? "false" : "true"}">
            <span id="watch-users-hdr">Usuarios</span>
            <span class="watch-section-chevron" aria-hidden="true">▾</span>
          </button>
          <div id="watch-users" class="watch-queue-list watch-users-list"></div>
        </div>
        <div class="watch-sidebar-section watch-sidebar-section--queue${c.queue ? " is-collapsed" : ""}" id="watch-queue-section">
          <button class="watch-queue-hdr" type="button" data-watch-section-toggle="queue" aria-expanded="${c.queue ? "false" : "true"}">
            <span id="watch-queue-hdr">Cola</span>
            <span class="watch-section-chevron" aria-hidden="true">▾</span>
          </button>
          <div id="watch-queue" class="watch-queue-list"></div>
        </div>
        <div class="watch-sidebar-section watch-sidebar-section--related${c.related ? " is-collapsed" : ""}" id="watch-related-section">
          <button class="watch-queue-hdr" type="button" data-watch-section-toggle="related" aria-expanded="${c.related ? "false" : "true"}">
            <span id="watch-related-hdr">Relacionados</span>
            <span class="watch-section-chevron" aria-hidden="true">▾</span>
          </button>
          <div id="watch-related" class="watch-queue-list"></div>
        </div>
      </div>`;
}

function _videoLayoutHtml() {
  return `
    <div id="watch-party-bar"></div>
    <div class="watch-layout watch-layout--video">
      <div class="watch-main">
        <div class="watch-video-wrap">
          <video id="webPlayerVideo" class="video-js vjs-default-skin vjs-big-play-centered" preload="auto" playsinline></video>
          <div id="webPlayerOverlay" class="player-overlay" style="display:none">
            <div class="player-overlay-spinner"></div>
          </div>
        </div>
        <div id="watch-info"></div>
      </div>
      ${_watchSidebarHtml()}
    </div>
  `;
}

function _musicLayoutHtml() {
  return `
    <div id="watch-party-bar"></div>
    <div class="watch-layout watch-layout--music">
      <div class="watch-main watch-main--music">
        <div class="music-player-card">
          <div class="music-art-wrap">
            <img id="musicArt" class="music-art" src="" alt="" style="display:none"
              onerror="this.style.display='none';var f=document.getElementById('musicArtFallback');if(f)f.style.display=''">
            <div class="music-art-fallback" id="musicArtFallback">${PLAYER_ICONS.musicNote(58)}</div>
          </div>
          <div id="musicTitle" class="music-title">...</div>
          <div id="musicArtist" class="music-artist"></div>
          <div class="music-progress">
            <span id="musicCurrent" class="music-time">0:00</span>
            <div class="music-track" id="musicProgressWrap">
              <div class="music-fill" id="musicProgressFill" style="width:0%"></div>
            </div>
            <span id="musicDuration" class="music-time">0:00</span>
          </div>
          <div class="music-controls">
            <button class="music-ctrl-sm ${state.player.shuffle ? "btn-player-ctrl-on" : ""}" id="ctrlShuffle" type="button" title="${state.player.shuffle ? "Aleatorio: activo" : "Aleatorio"}">${PLAYER_ICONS.shuffle(18)}</button>
            <button class="music-btn" id="watchPrev" type="button" title="Anterior">${PLAYER_ICONS.prev(21)}</button>
            <button class="music-btn music-btn--play" id="musicPlayPause" type="button" title="Play/Pausa">${PLAYER_ICONS.play(23)}</button>
            <button class="music-btn" id="watchNext" type="button" title="Siguiente">${PLAYER_ICONS.next(21)}</button>
            <button class="music-ctrl-sm ${state.player.repeat !== "none" ? "btn-player-ctrl-on" : ""}" id="ctrlRepeat" type="button" title="${state.player.repeat === "none" ? "Repetir" : state.player.repeat === "one" ? "Repetir: una" : "Repetir: todas"}">${state.player.repeat === "one" ? PLAYER_ICONS.repeatOne(18) : PLAYER_ICONS.repeat(18)}</button>
          </div>
          <div class="music-extra-ctrl">
            <button class="btn btn-ghost btn-player-ctrl btn-player-ctrl--label ${state.player.autoplay ? "btn-player-ctrl-on" : ""}" type="button" id="ctrlAutoplay" aria-pressed="${state.player.autoplay ? "true" : "false"}" title="${state.player.autoplay ? "Autoplay: activo" : "Autoplay: inactivo"}">Autoplay</button>
          </div>
          <div class="music-volume">
            <button type="button" class="music-vol-icon" id="musicVolIcon" title="Silenciar">${PLAYER_ICONS.volHigh(18)}</button>
            <input type="range" id="musicVolume" class="volume-slider" min="0" max="1" step="0.01" value="1">
          </div>
          <div class="music-actions">
            <span id="musicFollow"></span>
            <button class="btn btn-ghost btn-with-ico" type="button" data-watch-download="audio">${PLAYER_ICONS.download(14)}<span>Descargar</span></button>
            <button class="btn btn-ghost btn-with-ico" type="button" id="toggleAudioOnly">${PLAYER_ICONS.headphones(14)}<span>Solo audio</span></button>
            <button class="btn btn-ghost btn-with-ico" type="button" id="playerExternal">${PLAYER_ICONS.external(14)}<span>Abrir externo</span></button>
            <button class="btn btn-ghost btn-with-ico" type="button" id="watchBack">${PLAYER_ICONS.back(14)}<span>Volver</span></button>
          </div>
          <div id="musicStatus" class="player-status" style="display:none"></div>
          <audio id="webPlayerAudio" autoplay></audio>
        </div>
      </div>
      ${_watchSidebarHtml()}
    </div>
  `;
}

function renderWatch() {
  const root = $("#view-watch");
  const isMusicCtx = state.player.sourceContext === "music";
  const useMusicLayout = isMusicCtx || state.player.audioOnly;
  const hasCorrectLayout = useMusicLayout
    ? Boolean(root.querySelector(".watch-layout--music") && _getAudioEl())
    : Boolean(root.querySelector(".watch-layout--video") && document.getElementById("webPlayerVideo"));

  if (!hasCorrectLayout) {
    // Abort any existing listeners before rebuilding DOM
    if (state.player._listenerAbort) {
      state.player._listenerAbort.abort();
      state.player._listenerAbort = null;
    }
    if (state.player.vjs && !state.player.vjs.isDisposed()) {
      state.player.vjs.dispose();
      state.player.vjs = null;
    }
    if (state.player.audioVjs && !state.player.audioVjs.isDisposed()) {
      state.player.audioVjs.dispose();
      state.player.audioVjs = null;
    }
    root.innerHTML = useMusicLayout ? _musicLayoutHtml() : _videoLayoutHtml();
    if (useMusicLayout) {
      const ac = new AbortController();
      state.player._listenerAbort = ac;
      const sig = { signal: ac.signal };
      // music: ended listener on native <audio>
      const media = $("#webPlayerAudio");
      media.addEventListener("ended", () => { _onTrackEnded(); }, sig);
      _setupMusicListeners(sig);
    }
    // video: ended listener added inside ensureVideoPlayer()
  }

  _updateWatchMeta();
  _updatePlayerControls();
  _updateWatchUsers();
  _updateWatchQueue();
  _updateWatchRelated();
  _updateWatchPartyBar();
  _applyPartyPlayerRestrictions();
}

function _updateWatchMeta(error = "") {
  const useMusicLayout = state.player.sourceContext === "music" || state.player.audioOnly;
  if (useMusicLayout && $("#musicTitle")) {
    _updateMusicMeta(error);
  } else {
    _updateVideoMeta(error);
  }
}

function _updateVideoOverlay(error = "") {
  const overlay = $("#webPlayerOverlay");
  if (!overlay) return;
  if (state.player.loading && !error) {
    const thumb = state.player.current?.thumbnail;
    overlay.style.backgroundImage = thumb ? `url("${thumbSrc(thumb)}")` : "";
    overlay.style.display = "flex";
  } else {
    overlay.style.display = "none";
  }
}

function _updateVideoMeta(error = "") {
  _updateVideoOverlay(error);
  const el = $("#watch-info");
  if (!el) return;
  const item = state.player.current;
  const title = item?.title || item?.url || "...";
  const channel = item?.channel || "";
  const channelUrl = item?.channelUrl || "";
  const browseChannelUrl = _resolveItemChannelUrl(item);
  const duration = item?.durationText || "";
  const channelHtml = _renderChannelMetaHtml(channel, browseChannelUrl, true);
  const isPartyGuest = _partyIsGuest();
  const hasPrev = !isPartyGuest && state.player.index > 0;
  const hasNext = !isPartyGuest && state.player.index < state.player.queue.length - 1;
  const membersOnlyErr = _isMembersOnlyError(error);
  const isMembers = Boolean(item?.membersOnly || membersOnlyErr);
  const errorBannerHtml = !error
    ? ""
    : membersOnlyErr
      ? `<div class="player-info-card player-info-card--members">
          <div class="player-info-card-body">
            <span class="tag tag-members">★ Solo miembros</span>
            <span>Este video es exclusivo para miembros del canal. Necesitas una membresía activa en tu cuenta de YouTube para reproducirlo.</span>
          </div>
          <div class="player-info-card-actions">
            ${item?.url ? `<a class="btn btn-ghost" href="${escapeHtml(item.url)}" target="_blank" rel="noopener">Abrir en YouTube</a>` : ""}
            <button class="btn btn-ghost" type="button" onclick="loadWebPlayerItem(state.player.index)">Reintentar</button>
          </div>
        </div>`
      : `<div class="player-error-card"><span>⚠ ${escapeHtml(error)}</span><button class="btn btn-ghost" type="button" onclick="loadWebPlayerItem(state.player.index)">Reintentar</button></div>`;
  el.innerHTML = `
    <div class="watch-title">${escapeHtml(title)}</div>
    <div class="watch-channel">${channelHtml}${duration ? ` · ${escapeHtml(duration)}` : ""}${isMembers ? ` <span class="tag tag-members" title="Video exclusivo para miembros del canal">★ Solo miembros</span>` : ""}</div>
    <div class="watch-actions">
      <button class="btn btn-ghost btn-with-ico" type="button" id="watchPrev" ${hasPrev ? "" : "disabled"}>${PLAYER_ICONS.prev(15)}<span>Anterior</span></button>
      <button class="btn btn-ghost btn-with-ico" type="button" id="watchNext" ${hasNext ? "" : "disabled"}><span>Siguiente</span>${PLAYER_ICONS.next(15)}</button>
      ${_followButtonHtml(channelUrl, channel)}
      ${_playerCtrlsHtml()}
      <button class="btn btn-ghost btn-with-ico" type="button" data-watch-download="video">${PLAYER_ICONS.download(14)}<span>Descargar</span></button>
      <button class="btn btn-ghost btn-with-ico" type="button" data-watch-download="audio">${PLAYER_ICONS.headphones(14)}<span>Audio</span></button>
      <button class="btn btn-ghost btn-with-ico" type="button" id="toggleAudioOnly">${PLAYER_ICONS.headphones(14)}<span>Solo audio</span></button>
      <button class="btn btn-ghost btn-with-ico" type="button" id="playerExternal">${PLAYER_ICONS.external(14)}<span>Abrir externo</span></button>
      <button class="btn btn-ghost btn-with-ico" type="button" id="watchBack">${PLAYER_ICONS.back(14)}<span>Volver</span></button>
    </div>
    ${_nativePlayerBarHtml()}
    ${state.player.loading ? `<div class="player-status">Cargando...</div>` : ""}
    ${errorBannerHtml}
  `;
  _applyPartyPlayerRestrictions();
}

function _nativePlayerBarHtml() {
  if (!IS_TAURI || !state.player.native?.active) return "";
  const st = state.player.native;
  const isMpv = (st.backend || "mpv") === "mpv";
  const pauseLabel = st.paused ? `${PLAYER_ICONS.play(14)}<span>Reanudar</span>` : `${PLAYER_ICONS.pause(14)}<span>Pausar</span>`;
  const muteLabel = st.muted ? `${PLAYER_ICONS.volMute(15)}<span>Con sonido</span>` : `${PLAYER_ICONS.volHigh(15)}<span>Silenciar</span>`;
  const ipcBtns = isMpv
    ? `
      <button class="btn btn-ghost btn-with-ico" type="button" data-native-ctrl="toggle">${pauseLabel}</button>
      <button class="btn btn-ghost" type="button" data-native-ctrl="seek-back">-10s</button>
      <button class="btn btn-ghost" type="button" data-native-ctrl="seek-fwd">+10s</button>
      <button class="btn btn-ghost btn-with-ico" type="button" data-native-ctrl="mute">${muteLabel}</button>
    `
    : "";
  return `
    <div class="watch-actions" style="margin-top:8px;padding:8px 10px;border-radius:10px;background:var(--surface-2,rgba(255,255,255,0.04))">
      <span class="badge">${escapeHtml(st.backend || "mpv")} · ${escapeHtml(st.hwdec || "auto-safe")}</span>
      ${ipcBtns}
      <button class="btn btn-ghost btn-with-ico" type="button" data-native-ctrl="stop">${PLAYER_ICONS.stop(13)}<span>Detener</span></button>
    </div>
  `;
}

async function refreshNativePlayerStatus() {
  if (!IS_TAURI) return null;
  try {
    const status = await window.__TAURI__.core.invoke("player_status");
    state.player.native = status?.state || null;
    state.player.capabilities = status?.capabilities || null;
    return status;
  } catch (_) {
    return null;
  }
}

async function sendNativePlayerControl(command) {
  if (!IS_TAURI) return;
  try {
    const updated = await window.__TAURI__.core.invoke("player_control", { command });
    state.player.native = updated;
    if (state.view === "watch") _updateWatchMeta();
  } catch (err) {
    toast(String(err?.message || err));
  }
}

function _updateMusicMeta(error = "") {
  const item = state.player.current;
  const titleEl = $("#musicTitle");
  const artistEl = $("#musicArtist");
  const artEl = $("#musicArt");
  const artFallback = $("#musicArtFallback");
  const statusEl = $("#musicStatus");
  const prevBtn = $("#watchPrev");
  const nextBtn = $("#watchNext");

  const channel = item?.channel || "";
  const channelUrl = item?.channelUrl || "";
  const browseChannelUrl = _resolveItemChannelUrl(item);
  if (titleEl) titleEl.textContent = item?.title || "...";
  if (artistEl) {
    artistEl.innerHTML = _renderChannelMetaHtml(channel, browseChannelUrl, true);
  }
  const followEl = $("#musicFollow");
  if (followEl) followEl.innerHTML = _followButtonHtml(channelUrl, channel);

  if (artEl) {
    const thumb = item?.thumbnail;
    if (thumb) {
      artEl.src = thumbSrc(thumb);
      artEl.style.display = "block";
      if (artFallback) artFallback.style.display = "none";
    } else {
      artEl.style.display = "none";
      if (artFallback) artFallback.style.display = "";
    }
  }

  if (prevBtn) prevBtn.disabled = state.player.index <= 0;
  if (nextBtn) nextBtn.disabled = state.player.index >= state.player.queue.length - 1;
  _updatePlayerControls();

  const toggleBtn = $("#toggleAudioOnly");
  if (toggleBtn) toggleBtn.textContent = state.player.audioOnly ? "Ver video" : "Solo audio";

  if (statusEl) {
    if (state.player.loading) {
      statusEl.textContent = "Cargando...";
      statusEl.style.display = "";
      statusEl.className = "player-status";
    } else if (error) {
      if (_isMembersOnlyError(error)) {
        statusEl.innerHTML = `<span class="tag tag-members">★ Solo miembros</span> Este contenido es exclusivo para miembros del canal. <button class="btn btn-ghost" type="button" style="padding:2px 8px;font-size:11px" onclick="loadWebPlayerItem(state.player.index)">Reintentar</button>`;
        statusEl.style.display = "";
        statusEl.className = "player-status";
      } else {
        statusEl.innerHTML = `⚠ ${escapeHtml(error)} <button class="btn btn-ghost" type="button" style="padding:2px 8px;font-size:11px" onclick="loadWebPlayerItem(state.player.index)">Reintentar</button>`;
        statusEl.style.display = "";
        statusEl.className = "player-status error-text";
      }
    } else {
      statusEl.style.display = "none";
    }
  }
}

const _VOL_KEY = "playzi-volume";
const _MUTE_KEY = "playzi-muted";
let _suppressVolumeSaveUntil = 0;

function _markProgrammaticVolumeApply(ms = 150) {
  _suppressVolumeSaveUntil = Math.max(_suppressVolumeSaveUntil, performance.now() + ms);
}

function _clampVolume(v, fallback = 1) {
  const n = Number(v);
  if (!Number.isFinite(n)) return fallback;
  return Math.max(0, Math.min(1, n));
}

// Perceptual quadratic curve for the music slider so low positions
// match human logarithmic hearing instead of sounding disproportionately loud.
function _sliderToLinearVolume(sliderVal) {
  const s = _clampVolume(sliderVal, 1);
  return Math.round(s * s * 10000) / 10000;
}

function _linearToSliderVolume(linearVal) {
  const g = _clampVolume(linearVal, 1);
  return Math.round(Math.sqrt(g) * 100) / 100;
}

function _getSavedVolumeState() {
  const rawVol = localStorage.getItem(_VOL_KEY);
  const volume = rawVol !== null ? _clampVolume(parseFloat(rawVol), 1) : 1;
  const rawMuted = localStorage.getItem(_MUTE_KEY);
  const muted = rawMuted === "1" || (rawMuted === null && volume === 0);
  return { volume, muted };
}

function _saveVolumeState(volume, muted) {
  const v = _clampVolume(volume, 1);
  const m = Boolean(muted);
  if (!m && v > 0) {
    localStorage.setItem(_VOL_KEY, String(v));
  } else if (localStorage.getItem(_VOL_KEY) === null) {
    localStorage.setItem(_VOL_KEY, String(v > 0 ? v : 0.5));
  }
  localStorage.setItem(_MUTE_KEY, (m || v === 0) ? "1" : "0");
}

function _syncMusicVolumeUi(volume, muted) {
  const sliderVal = muted ? 0 : _linearToSliderVolume(volume);
  const volSlider = $("#musicVolume");
  const volIcon = $("#musicVolIcon");
  if (volSlider) {
    volSlider.value = String(sliderVal);
    volSlider.title = `Volumen: ${Math.round(sliderVal * 100)}%`;
  }
  if (volIcon) {
    volIcon.innerHTML = _volIconSvg(sliderVal, muted || volume === 0, 18);
    volIcon.title = (muted || volume === 0) ? "Activar sonido" : "Silenciar";
  }
}

function _applySavedVolumeToMusic() {
  const { volume, muted } = _getSavedVolumeState();
  const effectiveVol = muted ? 0 : volume;
  _markProgrammaticVolumeApply();
  if (state.player.audioVjs && !state.player.audioVjs.isDisposed()) {
    state.player.audioVjs.muted(muted);
    state.player.audioVjs.volume(effectiveVol);
  }
  const audio = _getAudioEl();
  if (audio) {
    audio.muted = muted;
    audio.volume = effectiveVol;
  }
  _syncMusicVolumeUi(volume, muted);
}

function _applySavedVolumeToVideo(player, videoEl = null) {
  const { volume, muted } = _getSavedVolumeState();
  _markProgrammaticVolumeApply();
  if (videoEl) {
    videoEl.muted = muted;
    videoEl.volume = volume;
  }
  if (player && !player.isDisposed()) {
    player.muted(muted);
    player.volume(volume);
  }
}

function _setupMusicListeners(sig) {
  const audio = $("#webPlayerAudio");
  if (!audio) return;

  _applySavedVolumeToMusic();

  const volSlider = $("#musicVolume");
  const volIcon = $("#musicVolIcon");

  if (volSlider) {
    volSlider.addEventListener("input", () => {
      const s = _clampVolume(parseFloat(volSlider.value), 0);
      const linear = _sliderToLinearVolume(s);
      _saveVolumeState(linear, s === 0);
      _applySavedVolumeToMusic();
    }, sig);
  }
  if (volIcon) {
    volIcon.addEventListener("click", () => {
      const { volume, muted } = _getSavedVolumeState();
      if (muted || volume === 0) {
        const restoreVol = volume > 0 ? volume : _sliderToLinearVolume(0.7);
        _saveVolumeState(restoreVol, false);
      } else {
        _saveVolumeState(volume, true);
      }
      _applySavedVolumeToMusic();
    }, sig);
  }

  audio.addEventListener("timeupdate", () => {
    const fill = $("#musicProgressFill");
    const cur = $("#musicCurrent");
    if (fill && audio.duration) fill.style.width = `${(audio.currentTime / audio.duration) * 100}%`;
    if (cur) cur.textContent = _formatTime(audio.currentTime);
    _maybePrefetchNextItem(audio.currentTime);
  }, sig);

  audio.addEventListener("loadstart", () => {
    _applySavedVolumeToMusic();
  }, sig);

  audio.addEventListener("loadedmetadata", () => {
    _applySavedVolumeToMusic();
    const dur = $("#musicDuration");
    if (dur) dur.textContent = _formatTime(audio.duration);
  }, sig);

  audio.addEventListener("play", () => {
    const btn = $("#musicPlayPause");
    if (btn) btn.innerHTML = PLAYER_ICONS.pause(23);
    if (navigator.mediaSession) navigator.mediaSession.playbackState = "playing";
    _onLocalMediaEvent("play");
  }, sig);

  audio.addEventListener("pause", () => {
    const btn = $("#musicPlayPause");
    if (btn) btn.innerHTML = PLAYER_ICONS.play(23);
    if (navigator.mediaSession) navigator.mediaSession.playbackState = "paused";
    _onLocalMediaEvent("pause");
  }, sig);

  audio.addEventListener("seeked", () => {
    _onLocalMediaEvent("seeked");
  }, sig);

  const wrap = $("#musicProgressWrap");
  if (wrap) {
    wrap.addEventListener("click", (ev) => {
      if (!_partyCanSeek()) return;
      if (!audio.duration) return;
      const rect = wrap.getBoundingClientRect();
      audio.currentTime = Math.max(0, Math.min(1, (ev.clientX - rect.left) / rect.width)) * audio.duration;
    }, sig);
  }

  const playBtn = $("#musicPlayPause");
  if (playBtn) {
    playBtn.addEventListener("click", () => {
      if (!_partyCanPlayPause()) return;
      if (audio.paused) audio.play().catch(() => {});
      else audio.pause();
    }, sig);
  }
}

function _updateWatchRelated() {
  const el = $("#watch-related");
  const hdr = $("#watch-related-hdr");
  if (!el) return;
  const ctx = state.player.sourceContext;
  const rawItems = ctx === "music"
    ? state.musicRecommendations
    : (state.recommendations.length ? state.recommendations : state.feed);
  const items = ctx === "music" ? rawItems : rawItems.filter((item) => !_isShort(item));
  if (hdr) hdr.textContent = `Relacionados · ${items.length}`;
  el.innerHTML = items.slice(0, 30).map((item) => {
    const channelName = item.sourceChannelName
      || (item.sourceChannelId ? state.channels.find((c) => c.id === item.sourceChannelId)?.name : null)
      || item.channel || "";
    const itemUrl = item.url || item.feedId || "";
    return `
      <div class="queue-item" data-related-url="${escapeHtml(itemUrl)}">
        ${item.thumbnail ? `<img class="queue-thumb" ${thumbAttrs(item.thumbnail)}>` : `<div class="queue-thumb"></div>`}
        <div class="queue-info">
          <div class="queue-title">${escapeHtml(item.title || "")}</div>
          <div class="queue-meta">${escapeHtml(channelName)}${item.durationText ? ` · ${escapeHtml(item.durationText)}` : ""}</div>
        </div>
        <button class="queue-add" type="button" data-related-queue-url="${escapeHtml(itemUrl)}" title="Añadir a la cola">＋</button>
      </div>
    `;
  }).join("");
}

function _updateWatchQueue() {
  const el = $("#watch-queue");
  const hdr = $("#watch-queue-hdr");
  if (!el) return;
  const q = state.player.queue;
  const idx = state.player.index;
  const isParty = Boolean(state.party?.active);
  const canRemoveFromQueue = isParty && _partyCanRemoveQueue();
  if (hdr) {
    hdr.textContent = isParty
      ? `Cola compartida · ${q.length ? `${idx + 1} / ${q.length}` : "0"}`
      : `Cola · ${idx + 1} / ${q.length}`;
  }
  el.innerHTML = q.map((item, i) => {
    const addedBy = item.addedBy;
    const addedLabel = addedBy?.displayName
      ? `<div class="queue-added-by" title="${escapeHtml(addedBy.googleAccountName ? `${addedBy.displayName} (Google: ${addedBy.googleAccountName} · ${addedBy.userId})` : `${addedBy.displayName} (${addedBy.userId || ""})`)}">Agregado por ${escapeHtml(addedBy.displayName)}</div>`
      : "";
    const removeBtn = canRemoveFromQueue
      ? `<button class="queue-remove-btn" type="button" data-party-remove-idx="${i}" title="Quitar de la cola">×</button>`
      : "";
    return `
      <div class="queue-item ${i === idx ? "active" : ""}" data-queue-idx="${i}">
        <span class="queue-idx">${i === idx ? "▶" : i + 1}</span>
        ${item.thumbnail ? `<img class="queue-thumb" ${thumbAttrs(item.thumbnail)}>` : `<div class="queue-thumb"></div>`}
        <div class="queue-info">
          <div class="queue-title">${escapeHtml(item.title || item.url)}</div>
          <div class="queue-meta">${escapeHtml(item.channel || "")}${item.durationText ? ` · ${escapeHtml(item.durationText)}` : ""}</div>
          ${addedLabel}
        </div>
        ${removeBtn}
      </div>
    `;
  }).join("");
  const activeEl = el.querySelector(".queue-item.active");
  if (activeEl) activeEl.scrollIntoView({ block: "nearest" });
}

function _updateWatchUsers() {
  const section = $("#watch-users-section");
  const el = $("#watch-users");
  const hdr = $("#watch-users-hdr");
  if (!section || !el) return;
  const p = state.party;
  if (!p?.active) {
    section.style.display = "none";
    el.innerHTML = "";
    return;
  }
  section.style.display = "";
  const participants = p.participants || [];
  const prof = state.userProfile || {};
  if (hdr) hdr.textContent = `Usuarios · ${participants.length}`;
  if (!participants.length) {
    el.innerHTML = `<div class="watch-users-empty">Sin usuarios conectados</div>`;
    return;
  }
  el.innerHTML = participants.map((part) => {
    const displayName = part.displayName || part.userId || "Usuario";
    const initial = displayName.trim().charAt(0).toUpperCase() || "?";
    const isSelf = part.userId === prof.userId || part.userId === p.selfUserId;
    let roleMeta = "";
    if (part.isHost) {
      roleMeta = "Control total";
    } else {
      const perms = [];
      if (part.canQueue) perms.push("Cola");
      if (part.canPlayPause) perms.push("Play/Pausa");
      if (part.canSeek) perms.push("Buscar");
      if (part.canRemoveQueue) perms.push("Quitar");
      roleMeta = perms.length ? perms.join(" · ") : "Solo ver";
    }
    const tooltip = part.googleAccountName
      ? `${displayName} (Google: ${part.googleAccountName} · ${part.userId || ""})`
      : `${displayName}${part.userId ? ` (${part.userId})` : ""}`;
    return `
      <div class="watch-user-item" title="${escapeHtml(tooltip)}">
        <div class="watch-user-avatar${part.isHost ? " is-host" : ""}">
          <span>${escapeHtml(initial)}</span>
          <span class="watch-user-dot" aria-hidden="true"></span>
        </div>
        <div class="watch-user-info">
          <div class="watch-user-name-row">
            <span class="watch-user-name">${escapeHtml(displayName)}</span>
            ${part.isHost ? `<span class="watch-user-badge watch-user-badge--host">Host</span>` : ""}
            ${isSelf ? `<span class="watch-user-badge">Tú</span>` : ""}
          </div>
          <div class="watch-user-meta">${escapeHtml(roleMeta)}</div>
        </div>
      </div>
    `;
  }).join("");
}

function _toggleWatchSidebarSection(sectionKey) {
  if (!state.watchSidebarCollapsed) {
    state.watchSidebarCollapsed = { users: false, queue: false, related: false };
  }
  const next = !state.watchSidebarCollapsed[sectionKey];
  state.watchSidebarCollapsed[sectionKey] = next;
  const sec = $(`#watch-${sectionKey}-section`);
  if (!sec) return;
  sec.classList.toggle("is-collapsed", next);
  const btn = sec.querySelector(`[data-watch-section-toggle="${sectionKey}"]`);
  if (btn) btn.setAttribute("aria-expanded", next ? "false" : "true");
}

// ── Player logic ──────────────────────────────────────────────────────────────

function _itemToQueueEntry(item) {
  return {
    url: item.url,
    title: item.title || item.url,
    thumbnail: item.thumbnail || "",
    channel: item.channel || item.sourceChannelName || "",
    channelUrl: item.channelUrl || "",
    duration: item.duration || null,
    durationText: item.durationText || "--:--",
    source: item.source || "",
    resultType: item.resultType || "",
  };
}

// ── Player controls: shuffle / repeat / autoplay / queue ─────────────────────

function toggleShuffle() {
  if (!state.player.shuffle) {
    state.player._originalQueue = [...state.player.queue];
    const current = state.player.queue[state.player.index];
    const rest = state.player.queue.filter((_, i) => i !== state.player.index);
    for (let i = rest.length - 1; i > 0; i--) {
      const j = Math.floor(Math.random() * (i + 1));
      [rest[i], rest[j]] = [rest[j], rest[i]];
    }
    state.player.queue = [current, ...rest];
    state.player.index = 0;
    state.player.shuffle = true;
  } else {
    const current = state.player.current;
    state.player.queue = [...state.player._originalQueue];
    state.player.index = state.player.queue.findIndex((item) => item.url === current?.url);
    if (state.player.index === -1) state.player.index = 0;
    state.player._originalQueue = [];
    state.player.shuffle = false;
  }
  _updateWatchQueue();
  _updatePlayerControls();
}

function cycleRepeat() {
  const modes = ["none", "one", "all"];
  const i = modes.indexOf(state.player.repeat);
  state.player.repeat = modes[(i + 1) % 3];
  _updatePlayerControls();
}

function toggleAutoplay() {
  state.player.autoplay = !state.player.autoplay;
  localStorage.setItem("playzi-autoplay", state.player.autoplay ? "1" : "0");
  _updatePlayerControls();
}

function _updatePlayerControls() {
  const shuffleBtn = $("#ctrlShuffle");
  const repeatBtn = $("#ctrlRepeat");
  const autoplayBtn = $("#ctrlAutoplay");
  if (shuffleBtn) {
    shuffleBtn.classList.toggle("btn-player-ctrl-on", state.player.shuffle);
    shuffleBtn.title = state.player.shuffle ? "Aleatorio: activo" : "Aleatorio";
  }
  if (repeatBtn) {
    const repeat = state.player.repeat;
    const icoSize = repeatBtn.classList.contains("music-ctrl-sm") ? 18 : 16;
    repeatBtn.classList.toggle("btn-player-ctrl-on", repeat !== "none");
    repeatBtn.innerHTML = repeat === "one" ? PLAYER_ICONS.repeatOne(icoSize) : PLAYER_ICONS.repeat(icoSize);
    repeatBtn.title = repeat === "none" ? "Repetir" : repeat === "one" ? "Repetir: una" : "Repetir: todas";
  }
  if (autoplayBtn) {
    autoplayBtn.classList.toggle("btn-player-ctrl-on", state.player.autoplay);
    autoplayBtn.setAttribute("aria-pressed", state.player.autoplay ? "true" : "false");
    autoplayBtn.title = state.player.autoplay
      ? "Autoplay: activo (añade canciones relacionadas al terminar)"
      : "Autoplay: inactivo";
  }
}

function _playerCtrlsHtml() {
  const sh = state.player.shuffle;
  const rep = state.player.repeat;
  const ap = state.player.autoplay;
  return `
    <button class="btn btn-ghost btn-player-ctrl ${sh ? "btn-player-ctrl-on" : ""}" type="button" id="ctrlShuffle" title="${sh ? "Aleatorio: activo" : "Aleatorio"}">${PLAYER_ICONS.shuffle(16)}</button>
    <button class="btn btn-ghost btn-player-ctrl ${rep !== "none" ? "btn-player-ctrl-on" : ""}" type="button" id="ctrlRepeat" title="${rep === "none" ? "Repetir" : rep === "one" ? "Repetir: una" : "Repetir: todas"}">${rep === "one" ? PLAYER_ICONS.repeatOne(16) : PLAYER_ICONS.repeat(16)}</button>
    <button class="btn btn-ghost btn-player-ctrl btn-player-ctrl--label ${ap ? "btn-player-ctrl-on" : ""}" type="button" id="ctrlAutoplay" aria-pressed="${ap ? "true" : "false"}" title="${ap ? "Autoplay: activo (añade canciones relacionadas al terminar)" : "Autoplay: inactivo"}">Autoplay</button>
  `;
}

async function _onTrackEnded() {
  _finishYtWatchReporting();
  if (state.player.repeat === "one") {
    loadWebPlayerItem(state.player.index);
    return;
  }
  if (state.player.index < state.player.queue.length - 1) {
    loadWebPlayerItem(state.player.index + 1);
    return;
  }
  if (state.player.repeat === "all" && state.player.queue.length > 0) {
    loadWebPlayerItem(0);
    return;
  }
  if (state.player.autoplay && state.player.current?.url) {
    try {
      const data = await api.get(`/api/radio?url=${encodeURIComponent(state.player.current.url)}`);
      if (data.items?.length) {
        toast("Autoplay: cargando mix relacionado...");
        state.player.queue.push(...data.items);
        if (state.player.shuffle) state.player._originalQueue.push(...data.items);
        _updateWatchQueue();
        loadWebPlayerItem(state.player.index + 1);
      }
    } catch (_) {}
  }
}

function _extractVideoIdFromUrl(url) {
  const m = String(url || "").match(/[?&]v=([A-Za-z0-9_-]{11})/)
    || String(url || "").match(/youtu\.be\/([A-Za-z0-9_-]{11})/);
  return m ? m[1] : "";
}

function _isExpandablePlaylistOrMix(item) {
  const url = String(item?.url || "");
  if (!isUrl(url) || !/[?&]list=/.test(url)) return false;
  return !url.includes("watch?v=")
    || _isRecommendedMix(item)
    || /[?&]list=RD/.test(url);
}

async function _resolvePlaylistOrMixItems(item) {
  const data = await api.post("/api/player/playlist", {
    url: item.url,
    quality: state.config?.playbackQuality || "best",
  });
  if (data.requiresLogin || !data.items?.length) {
    throw new Error(data.error || "No se pudo cargar la playlist o mix");
  }
  return data.items.map(_itemToQueueEntry);
}

async function addToQueue(item) {
  const isPlaylistOrMix = _isExpandablePlaylistOrMix(item);
  let entries;
  if (isPlaylistOrMix) {
    toast("Cargando cola de la playlist...");
    try {
      entries = await _resolvePlaylistOrMixItems(item);
    } catch (err) {
      toast(err.message);
      return;
    }
  } else {
    entries = [_itemToQueueEntry(item)];
  }
  if (!entries.length) return;

  if (state.party?.active) {
    if (state.party.role !== "host" && !state.party.canSelfQueue) {
      toast("El host no ha habilitado permisos de cola para tu usuario");
      return;
    }
    try {
      let snap = null;
      for (const entry of entries) {
        const ctx = String(entry.url || "").includes("music.youtube.com") ? "music" : (state.view === "watch" ? state.player.sourceContext : state.view);
        snap = await api.post("/api/party/queue/add", {
          item: { ...entry, sourceContext: ctx || "search" },
          playNext: false,
        });
      }
      if (snap) _onPartyStateUpdate(snap);
      toast(entries.length > 1 ? `Añadidas ${entries.length} pistas a la cola compartida` : "Añadido a la cola compartida");
    } catch (err) {
      toast(err.message);
    }
    return;
  }
  if (!state.player.current) {
    const ctx = String(item.url || "").includes("music.youtube.com") ? "music" : state.view;
    openWatchQueue(entries, 0, ctx);
    return;
  }
  state.player.queue.push(...entries);
  if (state.player.shuffle) state.player._originalQueue.push(...entries);
  _updateWatchQueue();
  toast(entries.length > 1 ? `Añadidas ${entries.length} pistas a la cola` : "Añadido a la cola");
}

async function playNext(item) {
  const isPlaylistOrMix = _isExpandablePlaylistOrMix(item);
  let entries;
  if (isPlaylistOrMix) {
    toast("Cargando cola de la playlist...");
    try {
      entries = await _resolvePlaylistOrMixItems(item);
    } catch (err) {
      toast(err.message);
      return;
    }
  } else {
    entries = [_itemToQueueEntry(item)];
  }
  if (!entries.length) return;

  if (state.party?.active) {
    if (state.party.role !== "host" && !state.party.canSelfQueue) {
      toast("El host no ha habilitado permisos de cola para tu usuario");
      return;
    }
    try {
      let snap = null;
      for (let i = entries.length - 1; i >= 0; i--) {
        const entry = entries[i];
        const ctx = String(entry.url || "").includes("music.youtube.com") ? "music" : (state.view === "watch" ? state.player.sourceContext : state.view);
        snap = await api.post("/api/party/queue/add", {
          item: { ...entry, sourceContext: ctx || "search" },
          playNext: true,
        });
      }
      if (snap) _onPartyStateUpdate(snap);
      toast(entries.length > 1 ? `Se reproducirán ${entries.length} pistas a continuación` : "Se reproducirá a continuación en la sesión");
    } catch (err) {
      toast(err.message);
    }
    return;
  }
  if (!state.player.current) {
    const ctx = String(item.url || "").includes("music.youtube.com") ? "music" : state.view;
    openWatchQueue(entries, 0, ctx);
    return;
  }
  state.player.queue.splice(state.player.index + 1, 0, ...entries);
  if (state.player.shuffle) state.player._originalQueue.push(...entries);
  _updateWatchQueue();
  toast(entries.length > 1 ? `Se reproducirán ${entries.length} pistas a continuación` : "Se reproducirá a continuación");
}

function _updateMediaSession() {
  if (!navigator.mediaSession) return;
  const item = state.player.current;
  if (!item) return;
  navigator.mediaSession.metadata = new MediaMetadata({
    title: item.title || "",
    artist: item.channel || "",
    artwork: item.thumbnail
      ? [{ src: thumbSrc(item.thumbnail), sizes: "480x360", type: "image/jpeg" }]
      : [],
  });
  const isAudio = state.player.sourceContext === "music" || state.player.audioOnly;
  const audioEl = isAudio ? _getAudioEl() : null;
  const vjs = !isAudio ? state.player.vjs : null;
  navigator.mediaSession.setActionHandler("play", () => {
    if (audioEl) audioEl.play().catch(() => {});
    else if (vjs) vjs.play();
    navigator.mediaSession.playbackState = "playing";
  });
  navigator.mediaSession.setActionHandler("pause", () => {
    if (audioEl) audioEl.pause();
    else if (vjs) vjs.pause();
    navigator.mediaSession.playbackState = "paused";
  });
  navigator.mediaSession.setActionHandler("previoustrack", () => {
    if (state.player.index > 0) loadWebPlayerItem(state.player.index - 1);
  });
  navigator.mediaSession.setActionHandler("nexttrack", () => {
    if (state.player.index < state.player.queue.length - 1) loadWebPlayerItem(state.player.index + 1);
    else _onTrackEnded();
  });
  navigator.mediaSession.setActionHandler("seekbackward", (d) => {
    const s = d?.seekOffset ?? 10;
    if (audioEl) audioEl.currentTime = Math.max(0, audioEl.currentTime - s);
    else if (vjs) vjs.currentTime(Math.max(0, vjs.currentTime() - s));
  });
  navigator.mediaSession.setActionHandler("seekforward", (d) => {
    const s = d?.seekOffset ?? 10;
    if (audioEl) audioEl.currentTime = Math.min(audioEl.duration || Infinity, audioEl.currentTime + s);
    else if (vjs) vjs.currentTime(Math.min(vjs.duration() || Infinity, vjs.currentTime() + s));
  });
  navigator.mediaSession.playbackState = "playing";
  _syncDiscordMediaPresence(true);
}

async function openWatchQueue(queue, index = 0, sourceContext = "search") {
  const singleItem = queue.length === 1 ? queue[0] : null;
  if (singleItem && _isExpandablePlaylistOrMix(singleItem)) {
    toast("Cargando cola...");
    try {
      const seedVid = _extractVideoIdFromUrl(singleItem.url);
      const resolved = await _resolvePlaylistOrMixItems(singleItem);
      queue = resolved;
      if (seedVid) {
        const matchIdx = queue.findIndex((it) => _extractVideoIdFromUrl(it.url) === seedVid);
        index = matchIdx >= 0 ? matchIdx : 0;
      } else {
        index = 0;
      }
    } catch (err) {
      toast(err.message);
      return;
    }
  }

  if (state.party?.active) {
    const chosenList = queue.length ? queue : [];
    const chosen = chosenList[index] || chosenList[0];
    if (!chosen) return;
    if (state.party.role === "guest") {
      if (state.party.canSelfQueue) {
        await addToQueue(chosen);
        setView("watch");
      } else {
        toast("Solo el host controla la reproducción y la cola en esta sesión");
      }
      return;
    }
    // Host: enqueue and immediately play it synchronized for everyone
    try {
      let snap = null;
      let startIdx = 0;
      for (let i = 0; i < chosenList.length; i++) {
        const entry = chosenList[i];
        snap = await api.post("/api/party/queue/add", {
          item: { ..._itemToQueueEntry(entry), sourceContext: sourceContext || "search" },
          playNext: false,
        });
        if (i === index) {
          startIdx = Math.max(0, (snap.queue?.length || 1) - 1);
        }
      }
      if (!snap) return;
      state.player.sourceContext = sourceContext;
      state.player.audioOnly = false;
      _onPartyStateUpdate(snap, { skipAutoLoad: true });
      setView("watch");
      await loadWebPlayerItem(startIdx);
      await _pushHostPlaybackSync({ currentIndex: startIdx, positionSecs: 0, isPlaying: true });
    } catch (err) {
      toast(err.message);
    }
    return;
  }

  state.player.audioOnly = false;
  state.player.sourceContext = sourceContext;
  state.player.shuffle = false;
  state.player._originalQueue = [];
  state.player.queue = queue;
  state.player.index = index;
  state.player.current = null;
  state.player.loading = true;
  setView("watch");
  await loadWebPlayerItem(index);
}

let _ytWatchSession = null;

function _randomCpn() {
  const chars = "ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789-_";
  let out = "";
  for (let i = 0; i < 16; i += 1) {
    out += chars[Math.floor(Math.random() * chars.length)];
  }
  return out;
}

function _currentPlaybackPosition() {
  try {
    if (state.player.vjs && !state.player.vjs.isDisposed()) {
      return {
        currentTime: Number(state.player.vjs.currentTime() || 0),
        duration: Number(state.player.vjs.duration() || 0) || null,
        paused: Boolean(state.player.vjs.paused()),
      };
    }
    const audio = _getAudioEl();
    if (audio) {
      return {
        currentTime: Number(audio.currentTime || 0),
        duration: Number(audio.duration || 0) || null,
        paused: Boolean(audio.paused),
      };
    }
  } catch (_) {}
  return { currentTime: 0, duration: null, paused: false };
}

const _sessionWatchedIds = new Set();

function _extractYtVideoId(raw) {
  const s = String(raw || "").trim();
  if (!s) return "";
  if (/^[A-Za-z0-9_-]{11}$/.test(s)) return s;
  const m = s.match(/(?:[?&]v=|youtu\.be\/|\/shorts\/|\/embed\/)([A-Za-z0-9_-]{11})/);
  return m ? m[1] : "";
}

function _isLocallyWatchedId(vid) {
  if (!vid) return false;
  if (_sessionWatchedIds.has(vid)) return true;
  const hist = state.history || [];
  for (let i = 0; i < hist.length; i++) {
    const h = hist[i];
    if (_extractYtVideoId(h.url || h.target || h.id) === vid) return true;
  }
  return false;
}

function _getWatchedPercent(item) {
  if (!item) return 0;
  if (_isRecommendedMix(item) || item.resultType === "artist" || item.resultType === "album") return 0;
  const ytPct = Number(item.watchedPercent || 0);
  if (ytPct > 0) return Math.min(100, Math.round(ytPct));
  const vid = _extractYtVideoId(item.url || item.id);
  if (_isLocallyWatchedId(vid)) return 100;
  return 0;
}

function _isWatchedVideo(item) {
  return _getWatchedPercent(item) >= 80;
}

function _stopYtWatchReporting(sendFinalPause = true) {
  const sess = _ytWatchSession;
  if (sess?.timer) {
    clearInterval(sess.timer);
  }
  _ytWatchSession = null;
  if (sendFinalPause && sess) {
    const pos = _currentPlaybackPosition();
    if (pos.currentTime >= 5 && pos.currentTime > sess.lastSentAt + 2) {
      api.post("/api/youtube/watch-report", {
        url: sess.url,
        eventType: "pause",
        startTime: sess.lastSentAt,
        currentTime: pos.currentTime,
        duration: pos.duration || sess.duration,
        cpn: sess.cpn,
      }).catch(() => {});
    }
  }
}

function _finishYtWatchReporting() {
  if (!_ytWatchSession) return;
  const sess = _ytWatchSession;
  _stopYtWatchReporting(false);
  const pos = _currentPlaybackPosition();
  const dur = pos.duration || sess.duration || null;
  api.post("/api/youtube/watch-report", {
    url: sess.url,
    eventType: "ended",
    startTime: sess.lastSentAt,
    currentTime: Math.max(pos.currentTime || 0, dur || 15),
    duration: dur,
    cpn: sess.cpn,
  }).catch(() => {});
}

function _startYtWatchReporting(item) {
  _stopYtWatchReporting(true);
  const url = String(item?.url || "");
  if (!/(?:youtube\.com|youtu\.be)/i.test(url)) return;
  const vid = _extractYtVideoId(url);
  if (vid) _sessionWatchedIds.add(vid);
  const cpn = _randomCpn();
  const sess = {
    url,
    cpn,
    duration: Number(item?.duration || 0) || null,
    lastSentAt: 0,
    timer: null,
  };
  _ytWatchSession = sess;
  // Report initial playback start so YouTube registers the view in Watch History immediately
  api.post("/api/youtube/watch-report", {
    url,
    eventType: "start",
    startTime: 0,
    currentTime: 0.5,
    duration: sess.duration,
    cpn,
  }).catch(() => {});
  // Periodically send watchtime pings while playing so YouTube's recommendation algorithm learns watch duration
  sess.timer = setInterval(() => {
    if (_ytWatchSession !== sess) return;
    const pos = _currentPlaybackPosition();
    if (pos.paused || pos.currentTime < 5) return;
    if (pos.duration && !sess.duration) {
      sess.duration = pos.duration;
      if (state.player.current && (!state.player.current.durationText || state.player.current.durationText === "--:--")) {
        state.player.current.duration = Math.round(pos.duration);
        state.player.current.durationText = _formatTime(pos.duration);
      }
    }
    if (pos.currentTime - sess.lastSentAt < 12) return;
    const prevSent = sess.lastSentAt;
    sess.lastSentAt = pos.currentTime;
    api.post("/api/youtube/watch-report", {
      url: sess.url,
      eventType: "progress",
      startTime: prevSent,
      currentTime: pos.currentTime,
      duration: pos.duration || sess.duration,
      cpn: sess.cpn,
    }).catch(() => {});
  }, 15000);
}

async function loadWebPlayerItem(index, opts = {}) {
  const item = state.player.queue[index];
  if (!item) return;
  const loadId = ++state.player._loadingId;
  state.player.index = index;
  state.player.current = item;
  state.player.loading = true;
  if (item.sourceContext && state.party?.active) {
    state.player.sourceContext = item.sourceContext;
  }
  renderWatch();
  _updateWatchMeta();
  _updateWatchQueue();
  _updateWatchRelated();
  if (state.view !== "watch") {
    _showMiniPlayer();
  }

  if (opts?.fromPartySync) {
    _partyMediaLoadingLock = true;
    _lockPartySync(1500);
  }
  try {
    _startYtWatchReporting(item);
    if (state.player.sourceContext === "music" || state.player.audioOnly) {
      await _loadMusicItem(item, loadId);
    } else {
      await _loadVideoItem(item, loadId);
    }
    if (state.player._loadingId !== loadId) {
      if (opts?.fromPartySync) _partyMediaLoadingLock = false;
      return;
    }
    _partyLastSyncedUrl = item.url;
    if (opts?.fromPartySync) {
      setTimeout(() => {
        _partyMediaLoadingLock = false;
        if (state.player._loadingId !== loadId) return;
        _applyMediaSync(Boolean(opts.targetPlaying), Number(opts.targetPos || 0), Number(opts.targetRate || 1));
      }, 350);
    } else if (state.party?.active && state.party.role === "host") {
      _pushHostPlaybackSync({ currentIndex: index, positionSecs: 0, isPlaying: true });
    }
  } catch (err) {
    if (opts?.fromPartySync) _partyMediaLoadingLock = false;
    if (state.player._loadingId !== loadId) return;
    state.player.loading = false;
    const msg = err?.message || String(err || "Error inesperado");
    if (_isMembersOnlyError(msg)) {
      _markMembersOnlyByUrl(item.url);
    }
    _updateWatchMeta(msg);
  }
}

function _resolveDesktopPlayer() {
  const preferred = (state.config?.defaultPlayer || "mpv").toLowerCase();
  const caps = state.player.capabilities || {};
  if (preferred === "vlc") {
    return {
      preferred: "vlc",
      available: !!caps.vlcAvailable,
      path: caps.vlcPath || state.config?.players?.vlc || "vlc",
    };
  }
  return {
    preferred: "mpv",
    available: !!caps.mpvAvailable,
    path: caps.mpvPath || state.config?.players?.mpv || "mpv",
  };
}

function _getAudioEl() {
  return document.getElementById("webPlayerAudio_html5_api") || $("#webPlayerAudio");
}

async function _loadMusicItem(item, loadId) {
  const info = await api.get(`/api/player/audio?url=${encodeURIComponent(item.url)}`);
  if (state.player._loadingId !== loadId) return;
  const audio = _getAudioEl();
  if (!audio) return;
  state.player.current = { ...item, ...(info.item || {}) };
  state.player.loading = false;
  const rawSrc = info.streamUrl || info.proxyUrl || "";
  const isHls = rawSrc.includes(".m3u8") || rawSrc.includes("hls_playlist");
  const proxiedSrc = streamProxyUrl(rawSrc);
  if (IS_TAURI && isHls && window.videojs) {
    if (!state.player.audioVjs || state.player.audioVjs.isDisposed()) {
      _applySavedVolumeToMusic();
      state.player.audioVjs = videojs("webPlayerAudio", {
        controls: false,
        autoplay: true,
        preload: "auto",
        html5: { vhs: { overrideNative: true } },
      });
      state.player.audioVjs.ready(() => {
        _applySavedVolumeToMusic();
      });
      state.player.audioVjs.on("loadstart", () => {
        _applySavedVolumeToMusic();
      });
      state.player.audioVjs.on("loadedmetadata", () => {
        _applySavedVolumeToMusic();
      });
      state.player.audioVjs.on("error", () => {
        const err = state.player.audioVjs?.error();
        _updateWatchMeta(err?.message || "Error al reproducir el audio.");
      });
      state.player.audioVjs.on("timeupdate", () => {
        _maybePrefetchNextItem(state.player.audioVjs?.currentTime() || 0);
      });
    }
    _applySavedVolumeToMusic();
    state.player.audioVjs.src({ src: proxiedSrc, type: "application/x-mpegURL" });
    _applySavedVolumeToMusic();
    state.player.audioVjs.play().catch(() => {});
  } else {
    let targetAudio = audio;
    if (state.player.audioVjs && !state.player.audioVjs.isDisposed()) {
      try { state.player.audioVjs.dispose(); } catch (_) {}
      state.player.audioVjs = null;
      // videojs.dispose() removes the <audio> element from the DOM; recreate it for native playback.
      const card = document.querySelector(".music-player-card");
      if (card && !document.getElementById("webPlayerAudio")) {
        const fresh = document.createElement("audio");
        fresh.id = "webPlayerAudio";
        fresh.autoplay = true;
        card.appendChild(fresh);
        if (state.player._listenerAbort) {
          state.player._listenerAbort.abort();
        }
        const ac = new AbortController();
        state.player._listenerAbort = ac;
        const sig = { signal: ac.signal };
        fresh.addEventListener("ended", () => { _onTrackEnded(); }, sig);
        _setupMusicListeners(sig);
        targetAudio = fresh;
      }
    }
    if (!targetAudio) return;
    targetAudio.onerror = () => {
      const code = targetAudio.error?.code;
      _updateWatchMeta(`Error de audio (código ${code || "?"}). Prueba con 'Abrir externo'.`);
    };
    _applySavedVolumeToMusic();
    targetAudio.src = proxiedSrc;
    targetAudio.load();
    _applySavedVolumeToMusic();
    targetAudio.play().catch(() => {});
  }
  _updateWatchMeta();
  _updateWatchQueue();
  _updateWatchRelated();
  _updateMediaSession();
}

// Warm the backend stream cache for the next queue item after 30s of playback
// so transitioning to the next track or video is smooth and near-instant.
const PREFETCH_TRIGGER_SECONDS = 30;
const PREFETCH_CACHE_TTL_MS = 5 * 60 * 1000;
const _prefetchedStreamMap = new Map();

function _maybePrefetchNextItem(currentTime) {
  if (!Number.isFinite(currentTime) || currentTime < PREFETCH_TRIGGER_SECONDS) return;
  _prefetchNextItem();
}

function _prefetchNextItem() {
  const next = state.player.queue[state.player.index + 1];
  if (!next || !next.url || !isUrl(next.url)) return;
  const isAudio = state.player.sourceContext === "music" || state.player.audioOnly;
  const rawQuality = state.config?.playbackQuality || "best";
  const quality = encodeURIComponent(rawQuality);
  const cacheKey = `${next.url}|${isAudio ? "audio" : rawQuality}`;
  const now = Date.now();
  const prevTs = _prefetchedStreamMap.get(cacheKey);
  if (prevTs && (now - prevTs) < PREFETCH_CACHE_TTL_MS) return;
  _prefetchedStreamMap.set(cacheKey, now);
  if (_prefetchedStreamMap.size > 64) {
    const oldestKey = _prefetchedStreamMap.keys().next().value;
    if (oldestKey) _prefetchedStreamMap.delete(oldestKey);
  }

  if (IS_TAURI) {
    const endpoint = isAudio
      ? `/api/player/audio?url=${encodeURIComponent(next.url)}`
      : `/api/player/info?url=${encodeURIComponent(next.url)}&quality=${quality}`;
    api.get(endpoint).catch(() => {
      _prefetchedStreamMap.delete(cacheKey);
    });
    return;
  }

  const endpoint = isAudio
    ? `/api/player/audio?url=${encodeURIComponent(next.url)}`
    : next.manifestUrl
      ? next.manifestUrl
      : `/api/player/manifest.mpd?url=${encodeURIComponent(next.url)}&quality=${quality}`;
  fetch(endpoint).catch(() => {
    _prefetchedStreamMap.delete(cacheKey);
  });
}

async function _loadVideoItem(item, loadId) {
  state.player.audioTracks = null;
  state.player.audioLang = null;
  state.player.availableQualities = null;
  state.player.currentHeight = null;
  const rawQuality = state.config?.playbackQuality || "best";
  state.player.activeQuality = rawQuality;
  const quality = encodeURIComponent(rawQuality);

  // Desktop integrated mode: play inside Playzi via Video.js + local playzi-stream:// HLS proxy.
  // External players (VLC / mpv) remain available via the "Abrir externo" button and local files.
  if (IS_TAURI) {
    const info = await api.get(`/api/player/info?url=${encodeURIComponent(item.url)}&quality=${quality}`);
    if (state.player._loadingId !== loadId) return;
    state.player.current = { ...item, ...(info.item || {}) };
    state.player.availableQualities = Array.isArray(info.availableQualities) && info.availableQualities.length
      ? info.availableQualities
      : null;
    state.player.currentHeight = info.video?.height || null;
    state.player.loading = false;
    const rawSrc = info.streamUrl || info.video?.url || "";
    if (rawSrc) {
      await ensureVideoPlayer();
      if (state.player._loadingId !== loadId) return;
      const player = state.player.vjs;
      const qualityBtn = player.controlBar?.getChild("QualityButton");
      if (qualityBtn) qualityBtn.update();
      const isHls = rawSrc.includes(".m3u8") || rawSrc.includes("hls_playlist") || rawSrc.includes("/manifest/hls");
      const videoHasAudio = Boolean(info.video?.acodec && info.video.acodec !== "none");
      const hlsAudioUrl = info.hlsAudio?.url || "";
      const proxiedSrc = (isHls && !videoHasAudio && hlsAudioUrl)
        ? streamMasterProxyUrl(rawSrc, hlsAudioUrl)
        : streamProxyUrl(rawSrc);
      const mimeType = isHls
        ? "application/x-mpegURL"
        : (info.video?.ext === "webm" ? "video/webm" : "video/mp4");
      _applySavedVolumeToVideo(player);
      player.src({ src: proxiedSrc, type: mimeType });
      _applySavedVolumeToVideo(player);
      player.play().catch(() => {});
    } else {
      _updateWatchMeta("No se pudo resolver un stream reproducible. Puedes intentar con 'Abrir externo'.");
      return;
    }
    _updateWatchMeta();
    _updateWatchQueue();
    _updateWatchRelated();
    _updateMediaSession();
    return;
  }

  // Warm the manifest in parallel with /info: both share the same yt-dlp
  // extraction via the backend single-flight, so the player's manifest
  // request later is a cache hit instead of a second round-trip.
  if (!item.manifestUrl) {
    fetch(`/api/player/manifest.mpd?url=${encodeURIComponent(item.url)}&quality=${quality}`).catch(() => {});
  }
  const info = item.manifestUrl
    ? { manifestUrl: item.manifestUrl, item }
    : await api.get(`/api/player/info?url=${encodeURIComponent(item.url)}&quality=${quality}`);
  if (state.player._loadingId !== loadId) return;
  state.player.availableQualities = Array.isArray(info.availableQualities) && info.availableQualities.length
    ? info.availableQualities
    : null;
  state.player.currentHeight = info.video?.height || null;
  await ensureVideoPlayer();
  if (state.player._loadingId !== loadId) return;
  const player = state.player.vjs;
  const qualityBtn = player.controlBar?.getChild("QualityButton");
  if (qualityBtn) qualityBtn.update();
  const audioBtn = player.controlBar?.getChild("AudioLangButton");
  if (audioBtn) { audioBtn.show(); audioBtn.update(); }
  _applySavedVolumeToVideo(player);
  player.src({ type: "application/dash+xml", src: info.manifestUrl });
  _applySavedVolumeToVideo(player);
  player.play().catch(() => {});
  state.player.current = { ...item, ...(info.item || {}) };
  state.player.loading = false;
  _updateWatchMeta();
  _updateWatchQueue();
  _updateWatchRelated();
  _updateMediaSession();
}

async function ensureVideoPlayer() {
  if (!window.videojs) throw new Error("Video.js no está cargado.");
  if (state.player.vjs && !state.player.vjs.isDisposed()) return;
  let videoEl = document.getElementById("webPlayerVideo");
  if (!videoEl) {
    renderWatch();
    videoEl = document.getElementById("webPlayerVideo");
  }
  if (!videoEl) throw new Error("No se encontró el contenedor de video.");
  // Apply saved volume & mute synchronously before Video.js initializes
  // so autoplay never blasts at 100% or clobbers localStorage on startup.
  _applySavedVolumeToVideo(null, videoEl);
  const player = videojs(videoEl, {
    controls: true,
    autoplay: true,
    preload: "auto",
    fill: true,
    playbackRates: [0.25, 0.5, 0.75, 1, 1.25, 1.5, 1.75, 2],
    html5: { vhs: { overrideNative: true } },
    userActions: {
      click: () => {
        if (!_partyCanPlayPause()) return;
        if (player.paused()) player.play().catch(() => {});
        else player.pause();
      },
      doubleClick: true,
      hotkeys: false,
    },
  });
  state.player.vjs = player;
  let volumeReady = false;
  player.on("error", () => {
    const err = player.error();
    const messages = {
      1: "Reproducción cancelada.",
      2: "Error de red al cargar el video. Revisa tu conexión o reintenta.",
      3: "Error al decodificar el video. Prueba con otra calidad.",
      4: "Formato no compatible con el reproductor web. Prueba con un reproductor externo (MPV/VLC) o baja la calidad.",
      5: "El video está cifrado y no se puede reproducir.",
    };
    const msg = messages[err?.code] || err?.message || "Error de reproducción";
    toast(msg);
    _updateWatchMeta(msg);
  });
  player.on("ended", () => { _onTrackEnded(); });
  player.on("timeupdate", () => { _maybePrefetchNextItem(player.currentTime()); });
  player.on("play", () => { _onLocalMediaEvent("play"); });
  player.on("pause", () => { _onLocalMediaEvent("pause"); });
  player.on("seeked", () => { _onLocalMediaEvent("seeked"); });
  player.on("ratechange", () => { _onLocalMediaEvent("ratechange"); });
  player.on("loadstart", () => { _applySavedVolumeToVideo(player); });
  player.on("loadedmetadata", () => { _applySavedVolumeToVideo(player); });
  // Restore / persist volume without letting startup events overwrite localStorage
  player.ready(() => {
    _applySavedVolumeToVideo(player);
    volumeReady = true;
    _applyPartyPlayerRestrictions();
  });
  player.on("volumechange", () => {
    if (!volumeReady || performance.now() < _suppressVolumeSaveUntil) return;
    _saveVolumeState(player.volume(), player.muted());
  });
  _registerQualityMenu(player);
  _registerAudioMenu(player);
  _registerTheaterBtn(player);
  _applyPartyPlayerRestrictions();
}

function _registerAudioMenu(player) {
  const MenuButton = videojs.getComponent("MenuButton");
  const MenuItem = videojs.getComponent("MenuItem");

  class AudioTrackMenuItem extends MenuItem {
    constructor(p, opts) {
      super(p, { ...opts, selectable: !!opts.track, multiSelectable: false });
      if (opts.track) {
        const active = state.player.audioLang
          ? state.player.audioLang === opts.track.lang
          : opts.track.default;
        this.selected(active);
      }
    }
    handleClick() {
      const track = this.options_.track;
      const p = this.player();
      if (!track || !p || p.isDisposed()) return;
      state.player.audioLang = track.lang;
      const curTime = p.currentTime();
      let src = p.currentSrc().replace(/&audio_lang=[^&]*/, "");
      if (!track.default) src += `&audio_lang=${encodeURIComponent(track.lang)}`;
      toast(`Cambiando audio a ${track.name}...`);
      p.src({ type: "application/dash+xml", src });
      p.one("canplay", () => {
        _applyingPartySync = true;
        p.currentTime(curTime);
        p.play().catch(() => {});
        setTimeout(() => { _applyingPartySync = false; }, 250);
      });
      const btn = p.controlBar.getChild("AudioLangButton");
      if (btn) btn.items.forEach((it) => it.options_.track && it.selected(it.options_.track.lang === track.lang));
    }
  }

  class AudioLangButton extends MenuButton {
    buildCSSClass() { return `vjs-audio-lang-button ${super.buildCSSClass()}`; }
    createItems() {
      const p = this.player();
      const tracks = state.player.audioTracks;
      if (tracks === null) return [new AudioTrackMenuItem(p, { label: "Cargando idiomas..." })];
      return tracks.map((t) => new AudioTrackMenuItem(p, { label: t.name, track: t }));
    }
    handleClick() {
      super.handleClick?.();
      if (state.player.audioTracks !== null || this._fetching) return;
      this._fetching = true;
      const url = state.player.current?.url;
      if (!url) { this._fetching = false; return; }
      api.get(`/api/player/audio-tracks?url=${encodeURIComponent(url)}`)
        .then((data) => {
          state.player.audioTracks = data.tracks || [];
          this.update();
          if (state.player.audioTracks.length <= 1) {
            toast("Este video no tiene otras pistas de audio");
            this.hide();
          }
        })
        .catch((err) => { toast(err.message); })
        .finally(() => { this._fetching = false; });
    }
  }

  if (!videojs.getComponent("AudioLangButton")) videojs.registerComponent("AudioLangButton", AudioLangButton);
  if (!videojs.getComponent("AudioTrackMenuItem")) videojs.registerComponent("AudioTrackMenuItem", AudioTrackMenuItem);
  player.controlBar.addChild("AudioLangButton", {}, player.controlBar.children().length - 1);
}

function _formatQualityLabel(qualityValue) {
  if (qualityValue === "best") {
    const h = state.player.currentHeight;
    return h ? `Auto (${h}p)` : "Auto";
  }
  if (qualityValue === "2160p") return "2160p (4K)";
  if (qualityValue === "1440p") return "1440p (2K)";
  return qualityValue;
}

function _registerQualityMenu(player) {
  const defaultQualities = ["2160p", "1440p", "1080p", "720p", "480p", "360p", "240p"];
  const MenuButton = videojs.getComponent("MenuButton");
  const MenuItem = videojs.getComponent("MenuItem");

  class QualityMenuItem extends MenuItem {
    constructor(p, opts) {
      super(p, { ...opts, selectable: true, multiSelectable: false });
      const activeQ = state.player.activeQuality || state.config?.playbackQuality || "best";
      this.selected(opts.qualityValue === activeQ);
    }
    async handleClick() {
      const p = this.player();
      if (!p || p.isDisposed()) return;
      const selectedQ = this.options_.qualityValue;
      state.player.activeQuality = selectedQ;
      const curTime = p.currentTime();
      const wasPaused = p.paused();

      if (IS_TAURI) {
        const url = state.player.current?.url;
        if (!url) return;
        toast(`Cambiando calidad a ${_formatQualityLabel(selectedQ)}...`);
        try {
          const info = await api.get(`/api/player/info?url=${encodeURIComponent(url)}&quality=${encodeURIComponent(selectedQ)}`);
          if (!p || p.isDisposed()) return;
          if (Array.isArray(info.availableQualities) && info.availableQualities.length) {
            state.player.availableQualities = info.availableQualities;
          }
          state.player.currentHeight = info.video?.height || null;
          const rawSrc = info.streamUrl || info.video?.url || "";
          if (!rawSrc) {
            toast("No se pudo cambiar la calidad del video");
            return;
          }
          const isHls = rawSrc.includes(".m3u8") || rawSrc.includes("hls_playlist") || rawSrc.includes("/manifest/hls");
          const videoHasAudio = Boolean(info.video?.acodec && info.video.acodec !== "none");
          const hlsAudioUrl = info.hlsAudio?.url || "";
          const proxiedSrc = (isHls && !videoHasAudio && hlsAudioUrl)
            ? streamMasterProxyUrl(rawSrc, hlsAudioUrl)
            : streamProxyUrl(rawSrc);
          const mimeType = isHls
            ? "application/x-mpegURL"
            : (info.video?.ext === "webm" ? "video/webm" : "video/mp4");
          p.src({ src: proxiedSrc, type: mimeType });
          p.one("canplay", () => {
            _applyingPartySync = true;
            p.currentTime(curTime);
            if (!wasPaused) p.play().catch(() => {});
            setTimeout(() => { _applyingPartySync = false; }, 250);
          });
          const btn = p.controlBar.getChild("QualityButton");
          if (btn) btn.update();
        } catch (err) {
          toast(err.message || "Error al cambiar la calidad");
        }
        return;
      }

      const curSrc = p.currentSrc();
      const newSrc = /quality=[^&]*/.test(curSrc)
        ? curSrc.replace(/quality=[^&]*/, `quality=${encodeURIComponent(selectedQ)}`)
        : `${curSrc}${curSrc.includes("?") ? "&" : "?"}quality=${encodeURIComponent(selectedQ)}`;
      p.src({ type: "application/dash+xml", src: newSrc });
      p.one("canplay", () => {
        _applyingPartySync = true;
        p.currentTime(curTime);
        if (!wasPaused) p.play().catch(() => {});
        setTimeout(() => { _applyingPartySync = false; }, 250);
      });
      const btn = p.controlBar.getChild("QualityButton");
      if (btn) btn.items.forEach((it) => it.selected(it.options_.qualityValue === selectedQ));
    }
  }

  class QualityButton extends MenuButton {
    constructor(p, opts) {
      super(p, opts);
      this.controlText("Calidad");
    }
    buildCSSClass() { return `vjs-quality-button ${super.buildCSSClass()}`; }
    createItems() {
      const p = this.player();
      const detected = Array.isArray(state.player.availableQualities) && state.player.availableQualities.length
        ? state.player.availableQualities
        : defaultQualities;
      const list = ["best", ...detected.filter((q) => q !== "best")];
      return list.map((q) => new QualityMenuItem(p, {
        label: _formatQualityLabel(q),
        qualityValue: q,
      }));
    }
  }

  if (!videojs.getComponent("QualityButton")) videojs.registerComponent("QualityButton", QualityButton);
  if (!videojs.getComponent("QualityMenuItem")) videojs.registerComponent("QualityMenuItem", QualityMenuItem);
  player.controlBar.addChild("QualityButton", {}, player.controlBar.children().length - 1);
}

function _registerTheaterBtn(player) {
  const Button = videojs.getComponent("Button");

  class TheaterButton extends Button {
    buildCSSClass() { return "vjs-theater-button vjs-control vjs-button"; }
    handleClick() {
      const layout = document.querySelector(".watch-layout--video");
      if (layout) layout.classList.toggle("watch-layout--theater");
      this.el_.classList.toggle("vjs-theater-active");
    }
  }

  if (!videojs.getComponent("TheaterButton")) videojs.registerComponent("TheaterButton", TheaterButton);
  player.controlBar.addChild("TheaterButton", {}, player.controlBar.children().length - 1);
}

// ── Mini player ───────────────────────────────────────────────────────────────

function _miniPlayerHtml() {
  const item = state.player.current;
  const hasPrev = state.player.index > 0;
  const hasNext = state.player.index < state.player.queue.length - 1;
  return `
    <div class="mini-player" id="miniPlayer">
      <div class="mini-thumb-wrap">
        ${item?.thumbnail ? `<img class="mini-thumb" src="${escapeHtml(thumbSrc(item.thumbnail))}" alt="">` : `<div class="mini-thumb mini-thumb--empty"></div>`}
      </div>
      <div class="mini-info">
        <div class="mini-title" id="miniTitle">${escapeHtml(item?.title || "")}</div>
        <div class="mini-artist" id="miniArtist">${escapeHtml(item?.channel || "")}</div>
        <div class="mini-prog-wrap"><div class="mini-prog-fill" id="miniProgFill" style="width:0%"></div></div>
      </div>
      <div class="mini-btns">
        <button class="mini-btn" id="miniPrev" title="Anterior" ${hasPrev ? "" : "disabled"}>${PLAYER_ICONS.prev(16)}</button>
        <button class="mini-btn mini-btn--play" id="miniPlayPause" data-state="playing" title="Play/Pausa">${PLAYER_ICONS.pause(16)}</button>
        <button class="mini-btn" id="miniNext" title="Siguiente" ${hasNext ? "" : "disabled"}>${PLAYER_ICONS.next(16)}</button>
        <span class="mini-sep" aria-hidden="true"></span>
        <button class="mini-btn" id="miniExpand" title="Abrir reproductor">${PLAYER_ICONS.expand(15)}</button>
        <button class="mini-btn" id="miniClose" title="Cerrar">${PLAYER_ICONS.close(15)}</button>
      </div>
    </div>
  `;
}

function _updateMiniPlayer() {
  const item = state.player.current;
  if (!item) {
    // Reproductor destruido mientras el mini seguia visible: cortar el interval
    _hideMiniPlayer();
    return;
  }

  const titleEl = document.getElementById("miniTitle");
  if (titleEl) titleEl.textContent = item.title || "";
  const artistEl = document.getElementById("miniArtist");
  if (artistEl) artistEl.textContent = item.channel || "";

  let currentTime = 0, duration = 0, paused = true;
  if (state.player.sourceContext === "music" || state.player.audioOnly) {
    const audio = document.querySelector("#view-watch audio");
    if (audio) { currentTime = audio.currentTime; duration = audio.duration || 0; paused = audio.paused; }
  } else if (state.player.vjs && !state.player.vjs.isDisposed()) {
    currentTime = state.player.vjs.currentTime() || 0;
    duration = state.player.vjs.duration() || 0;
    paused = state.player.vjs.paused();
  }

  const fill = document.getElementById("miniProgFill");
  if (fill) fill.style.width = duration ? `${(currentTime / duration) * 100}%` : "0%";
  const playBtn = document.getElementById("miniPlayPause");
  if (playBtn) {
    const nextState = paused ? "paused" : "playing";
    if (playBtn.dataset.state !== nextState) {
      playBtn.dataset.state = nextState;
      playBtn.innerHTML = paused ? PLAYER_ICONS.play(16) : PLAYER_ICONS.pause(16);
    }
    playBtn.disabled = !_partyCanPlayPause();
  }
  const isPartyGuest = _partyIsGuest();
  const prevBtn = document.getElementById("miniPrev");
  if (prevBtn) prevBtn.disabled = isPartyGuest || state.player.index <= 0;
  const nextBtn = document.getElementById("miniNext");
  if (nextBtn) nextBtn.disabled = isPartyGuest || state.player.index >= state.player.queue.length - 1;
}

function _showMiniPlayer() {
  const root = document.getElementById("playerRoot");
  if (!root) return;
  root.innerHTML = _miniPlayerHtml();
  clearInterval(_miniInterval);
  _miniInterval = setInterval(_updateMiniPlayer, 800);
  root.addEventListener("click", _miniPlayerClick);
  document.body.classList.add("has-mini-player");
}

function _hideMiniPlayer() {
  clearInterval(_miniInterval);
  _miniInterval = null;
  const root = document.getElementById("playerRoot");
  if (root) {
    root.removeEventListener("click", _miniPlayerClick);
    root.innerHTML = "";
  }
  document.body.classList.remove("has-mini-player");
}

function _miniTogglePlay() {
  if (!_partyCanPlayPause()) return;
  if (state.player.sourceContext === "music" || state.player.audioOnly) {
    const audio = document.querySelector("#view-watch audio");
    if (!audio) return;
    if (audio.paused) audio.play().catch(() => {});
    else audio.pause();
  } else if (state.player.vjs && !state.player.vjs.isDisposed()) {
    if (state.player.vjs.paused()) state.player.vjs.play().catch(() => {});
    else state.player.vjs.pause();
  }
  setTimeout(_updateMiniPlayer, 50);
}

function _miniPlayerClick(ev) {
  if (ev.target.closest("#miniClose")) {
    _destroyPlayer();
    _hideMiniPlayer();
    const watchRoot = document.getElementById("view-watch");
    if (watchRoot) watchRoot.innerHTML = "";
    return;
  }
  if (ev.target.closest("#miniExpand")) {
    setView("watch");
    return;
  }
  if (ev.target.closest("#miniPlayPause")) {
    _miniTogglePlay();
    return;
  }
  if (ev.target.closest("#miniPrev") && !_partyIsGuest() && state.player.index > 0) {
    loadWebPlayerItem(state.player.index - 1);
    return;
  }
  if (ev.target.closest("#miniNext") && !_partyIsGuest() && state.player.index < state.player.queue.length - 1) {
    loadWebPlayerItem(state.player.index + 1);
    return;
  }
  // click on info/thumb → expand to watch view
  if (ev.target.closest(".mini-info") || ev.target.closest(".mini-thumb-wrap")) {
    setView("watch");
  }
}

// ── Playlists ─────────────────────────────────────────────────────────────────

function openAddToPlaylistModal(url, meta) {
  const playlists = state.playlists;
  const root = document.getElementById("modalRoot");
  if (!root) return;
  root.innerHTML = `
    <div class="modal-backdrop" id="modalBackdrop">
      <div class="modal">
        <div class="modal-header">
          <strong>Agregar a playlist</strong>
          <button class="icon-btn" id="modalClose" type="button">×</button>
        </div>
        <div class="modal-body">
          ${playlists.length ? `
            <div class="playlist-select-list">
              ${playlists.map((pl) => `
                <button class="playlist-select-item" type="button" data-add-to-pl-id="${escapeHtml(pl.id)}">
                  <span>▥ ${escapeHtml(pl.name)}</span>
                  <span class="meta">${pl.items?.length || 0} items</span>
                </button>
              `).join("")}
            </div>
          ` : `<p class="meta">Sin playlists. Crea una primero.</p>`}
          <hr style="margin:12px 0; border-color: var(--br)">
          <form id="newPlaylistInlineForm" class="inline-form">
            <input class="form-input" name="name" placeholder="Nueva playlist..." required>
            <button class="btn btn-primary" type="submit">Crear y agregar</button>
          </form>
        </div>
      </div>
    </div>
  `;

  const close = () => { root.innerHTML = ""; };
  document.getElementById("modalClose").onclick = close;
  document.getElementById("modalBackdrop").addEventListener("click", (e) => {
    if (e.target === document.getElementById("modalBackdrop")) close();
  });

  // add to existing playlist
  root.querySelectorAll("[data-add-to-pl-id]").forEach((btn) => {
    btn.addEventListener("click", async () => {
      try {
        await api.post(`/api/playlists/${btn.dataset.addToPlId}/items`, { url, ...meta });
        toast("Agregado a playlist ✓");
        close();
        const data = await api.get("/api/playlists");
        state.playlists = data.items || [];
      } catch (err) { toast(err.message); }
    });
  });

  // create new then add
  document.getElementById("newPlaylistInlineForm").addEventListener("submit", async (e) => {
    e.preventDefault();
    const name = e.target.name.value.trim();
    if (!name) return;
    try {
      const pl = await api.post("/api/playlists", { name });
      await api.post(`/api/playlists/${pl.id}/items`, { url, ...meta });
      toast(`Playlist "${name}" creada y item agregado ✓`);
      close();
      const data = await api.get("/api/playlists");
      state.playlists = data.items || [];
    } catch (err) { toast(err.message); }
  });
}

async function closeWebPlayer() {
  const ctx = state.player.sourceContext || "search";
  _destroyPlayer();
  _hideMiniPlayer();
  $("#view-watch").innerHTML = "";
  setView(ctx);
}

// ── Cards & items ─────────────────────────────────────────────────────────────

function videoCard(item) {
  const id = escapeHtml(item.feedId || item.id || item.url);
  const isPlaylist = _isRecommendedMix(item);
  const isArtist = item.resultType === "artist";
  const isAlbum = item.resultType === "album";
  const channelName = item.sourceChannelName
    || (item.sourceChannelId ? state.channels.find((c) => c.id === item.sourceChannelId)?.name : null)
    || (item.channel !== "YouTube" ? item.channel : null)
    || item.channel || "";

  let channelBtn = "";
  if (item.sourceChannelId) {
    channelBtn = `<button class="btn btn-ghost" type="button" data-view-channel="${escapeHtml(item.sourceChannelId)}">Ver canal</button>`;
  } else if (item.channelUrl) {
    channelBtn = `<button class="btn btn-ghost" type="button" data-add-channel="${escapeHtml(item.channelUrl)}" data-add-channel-name="${escapeHtml(channelName)}" title="Suscribirse al canal">＋ Canal</button>`;
  }

  const resolvedChannelUrl = _resolveItemChannelUrl(item);
  const channelNameHtml = _renderChannelMetaHtml(channelName, resolvedChannelUrl, !isPlaylist);

  const isShort = _isShort(item);
  const watchedPct = _getWatchedPercent(item);
  return `
    <article class="card${isShort ? " card--short" : ""}">
      <div class="thumb">
        ${item.thumbnail ? `<img ${thumbAttrs(item.thumbnail)}>` : ""}
        ${item.membersOnly ? `<span class="badge-members" title="Video exclusivo para miembros del canal">★ Solo miembros</span>` : ""}
        ${watchedPct >= 80 ? `<span class="badge-watched" title="Ya viste este video">Visto</span>` : ""}
        <span class="duration">${escapeHtml(item.durationText || (isShort ? "Short" : "--:--"))}</span>
        ${watchedPct > 0 ? `<div class="thumb-watched-bar" title="${watchedPct}% visto"><span class="thumb-watched-fill" style="width:${watchedPct}%"></span></div>` : ""}
      </div>
      <div class="card-body">
        <div class="card-title">${escapeHtml(item.title)}</div>
        <div class="meta"><span class="tag ${isShort ? "tag-short" : "tag-muted"}">${escapeHtml(isShort ? "Short" : sourceLabel(item.source || (String(item.url || "").includes("music.youtube.com") ? "music" : "youtube")))}</span> ${item.membersOnly ? `<span class="tag tag-members" title="Requiere membresía activa del canal en YouTube">Solo miembros</span> ` : ""}${channelNameHtml}${item.viewCount ? ` · ${Number(item.viewCount).toLocaleString()} vistas` : ""}</div>
      </div>
      <div class="card-actions">
        ${(!isArtist && !isAlbum) ? `<button class="btn btn-primary" type="button" data-play-default data-id="${id}">▶ Reproducir</button>` : ""}
        ${(isAlbum && item.url) ? `<button class="btn btn-primary" type="button" data-album-play="${escapeHtml(item.url)}">▶ Reproducir</button>` : ""}
        ${isArtist ? `<button class="btn btn-ghost" type="button" data-open-artist="${escapeHtml(item.id || "")}" data-open-artist-name="${escapeHtml(item.title || "")}">Ver artista</button>` : ""}
        ${isPlaylist ? `<button class="btn btn-ghost" type="button" data-import-result-playlist="${escapeHtml(item.url || "")}">Importar</button>` : ""}
        ${(!isPlaylist && !isArtist && !isAlbum) ? `<button class="btn btn-ghost" type="button" data-download="video" data-id="${id}">Descargar</button>` : ""}
        ${(!isPlaylist && !isArtist && !isAlbum) ? `<button class="btn btn-ghost" type="button" data-download="audio" data-id="${id}">Audio</button>` : ""}
        ${(!isArtist && !isAlbum) ? `<button class="btn btn-ghost" type="button" data-add-to-queue data-id="${id}" title="Añadir al final de la cola">+ Cola</button>` : ""}
        ${(!isArtist && !isAlbum) ? `<button class="btn btn-ghost" type="button"
          data-add-to-playlist="${escapeHtml(item.url || item.feedId || '')}"
          data-playlist-title="${escapeHtml(item.title || '')}"
          data-playlist-thumb="${escapeHtml(item.thumbnail || '')}"
          data-playlist-channel="${escapeHtml(typeof channelName === 'string' ? channelName : '')}"
          data-playlist-duration="${escapeHtml(item.durationText || '')}">+ Lista</button>` : ""}
        ${channelBtn}
      </div>
    </article>
  `;
}

function resultLabel(item) {
  if (item.source === "music") {
    return ({
      track: "YT Music - Cancion",
      video: "YT Music - Video",
      artist: "YT Music - Artista",
      playlist: "YT Music - Playlist",
      album: "YT Music - Album",
    }[item.resultType] || "YT Music");
  }
  return sourceLabel(item.source || (String(item.url || "").includes("music.youtube.com") ? "music" : "youtube"));
}

function libraryCard(item) {
  return `
    <article class="card">
      <div class="thumb">
        ${item.thumbnail ? `<img ${thumbAttrs(item.thumbnail)}>` : ""}
        <span class="duration">${escapeHtml(item.durationText || item.format || "")}</span>
      </div>
      <div class="card-body">
        <div class="card-title">${escapeHtml(item.title)}</div>
        <div class="meta"><span class="tag">${escapeHtml(item.format || item.kind)}</span> ${escapeHtml(item.size || "")} ${item.collection ? ` · ${escapeHtml(item.collection)}` : ""}</div>
      </div>
      <div class="card-actions">
        <button class="btn btn-primary" type="button" data-local-play-default data-path="${escapeHtml(item.path)}">▶ Reproducir</button>
        ${item.mediaUrl ? `<a class="btn btn-ghost" href="${escapeHtml(item.mediaUrl)}" target="_blank">Navegador</a>` : ""}
      </div>
    </article>
  `;
}

function downloadItem(item) {
  const done = item.status === "done";
  const failed = item.status === "error";
  const cancelled = item.status === "cancelled";
  const active = ["queued", "starting", "downloading", "processing"].includes(item.status);
  const statusLabel = done ? "Listo" : failed ? "Error" : cancelled ? "Cancelado" : escapeHtml(item.status || "queued");
  const membersErr = failed && _isMembersOnlyError(item.error);
  return `
    <div class="dl-item">
      <div class="dl-header">
        ${item.thumbnail ? `<img class="mini-thumb" ${thumbAttrs(item.thumbnail)}>` : `<div class="mini-thumb"></div>`}
        <div class="dl-info">
          <div class="dl-title">${escapeHtml(item.title)}</div>
          <div class="meta">${escapeHtml(item.kind)}${item.quality ? ` · ${escapeHtml(item.quality)}` : ""} · ${escapeHtml(item.channel || "")}</div>
        </div>
        <div class="dl-status">${statusLabel}</div>
      </div>
      <div class="track"><div class="fill" style="width:${Number(item.progress || 0)}%"></div></div>
      <div class="prog-labels"><span>${Math.round(item.progress || 0)}%</span><span>${escapeHtml(item.speed || "")} ${escapeHtml(item.eta || "")}</span></div>
      ${failed ? (membersErr ? `<div style="margin-top:6px;display:flex;align-items:center;gap:8px;flex-wrap:wrap;font-size:12px"><span class="tag tag-members">★ Solo miembros</span><span>Este video es exclusivo para miembros del canal.</span></div>` : `<div class="error-text">${escapeHtml(item.error)}</div>`) : ""}
      <div style="display:flex;gap:7px;margin-top:8px">
        ${active ? `<button class="btn btn-ghost" type="button" data-cancel-download="${escapeHtml(item.id)}">Cancelar</button>` : ""}
        ${(done || failed || cancelled) ? `<button class="btn btn-danger" type="button" data-delete-download="${escapeHtml(item.id)}">Eliminar</button>` : ""}
      </div>
    </div>
  `;
}

function empty(title, sub) {
  return `<div class="empty"><strong>${escapeHtml(title)}</strong><span>${escapeHtml(sub)}</span></div>`;
}

function allVideoItems() {
  const playlistItems = state.playlists.flatMap((pl) => pl.items || []);
  const musicLibraryItems = [
    ...(state.musicLibrary?.songs || []),
    ...(state.musicLibrary?.liked || []),
    ...(state.musicLibrary?.history || []),
  ];
  const artistItems = state.artistView?.data
    ? [...(state.artistView.data.songs || []), ...(state.artistView.data.albums || []), ...(state.artistView.data.singles || [])]
    : [];
  return [...state.results, ...(state.channelView?.items || []), ...artistItems, ...state.feed, ...state.recommendations, ...state.musicRecommendations, ...musicLibraryItems, ...state.library, ...playlistItems, ...state.history];
}

function findVideo(id) {
  return allVideoItems().find((item) => String(item.feedId || item.id || item.url) === String(id));
}

// Warm the backend cache when the user hovers a result, so the click feels
// instant. Debounced; capped concurrency; never re-warms the same URL.
let _prefetchInFlight = 0;
const _PREFETCH_MAX_INFLIGHT = 3;

const _PREFETCH_TTL_MS = 4 * 60 * 60 * 1000;
const _PREFETCH_MAX_ENTRIES = 120;
const _prefetchTimestamps = new Map();

function _isPrefetchedFresh(url) {
  const ts = _prefetchTimestamps.get(url);
  if (!ts) return false;
  if (Date.now() - ts > _PREFETCH_TTL_MS) {
    _prefetchTimestamps.delete(url);
    _prefetchedUrls.delete(url);
    return false;
  }
  return true;
}

function _markPrefetched(url) {
  if (_prefetchTimestamps.size >= _PREFETCH_MAX_ENTRIES) {
    const oldest = _prefetchTimestamps.keys().next().value;
    if (oldest) {
      _prefetchTimestamps.delete(oldest);
      _prefetchedUrls.delete(oldest);
    }
  }
  _prefetchTimestamps.set(url, Date.now());
  _prefetchedUrls.add(url);
}

function prefetchPlayback(url, forceAudio = false) {
  if (!url || _isPrefetchedFresh(url) || _prefetchInFlight >= _PREFETCH_MAX_INFLIGHT) return;
  // Playlists/mixes resolve to many videos — nothing single to warm.
  if (String(url).includes("list=") && !String(url).includes("watch?v=")) return;
  _markPrefetched(url);
  _prefetchInFlight += 1;
  const isAudio = forceAudio || String(url).includes("music.youtube.com");
  const quality = encodeURIComponent(state.config?.playbackQuality || "best");
  const endpoint = isAudio
    ? `/api/player/audio?url=${encodeURIComponent(url)}`
    : `/api/player/manifest.mpd?url=${encodeURIComponent(url)}&quality=${quality}`;
  fetch(endpoint)
    .then((res) => {
      if (!res.ok) {
        _prefetchedUrls.delete(url);
        _prefetchTimestamps.delete(url);
      }
    })
    .catch(() => {
      _prefetchedUrls.delete(url);
      _prefetchTimestamps.delete(url);
    })
    .finally(() => { _prefetchInFlight -= 1; });
}

function _hoverPrefetchTarget(ev) {
  // Anywhere on a result card (thumbnail/title), not just the Play button.
  const card = ev.target.closest(".card");
  const playBtn = card?.querySelector("[data-play-default]");
  if (playBtn) {
    const item = findVideo(playBtn.dataset.id);
    if (item?.url) return { url: item.url, forceAudio: false };
  }
  // Artist view song rows play via the music/audio pipeline.
  const artistRow = ev.target.closest("[data-artist-play]");
  if (artistRow) {
    const song = state.artistView?.data?.songs?.[Number(artistRow.dataset.artistPlay) || 0];
    if (song?.url) return { url: song.url, forceAudio: true };
  }
  return null;
}

function _setupHoverPrefetch() {
  let hoverTimer = null;
  document.body.addEventListener("mouseover", (ev) => {
    const target = _hoverPrefetchTarget(ev);
    if (!target || _prefetchedUrls.has(target.url)) return;
    clearTimeout(hoverTimer);
    hoverTimer = setTimeout(() => prefetchPlayback(target.url, target.forceAudio), 200);
  });
  document.body.addEventListener("mouseout", (ev) => {
    if (ev.target.closest(".card") || ev.target.closest("[data-artist-play]")) clearTimeout(hoverTimer);
  });
  // No hover on touch devices: warm on touchstart, before the tap resolves to a click.
  document.body.addEventListener("touchstart", (ev) => {
    const target = _hoverPrefetchTarget(ev);
    if (target) prefetchPlayback(target.url, target.forceAudio);
  }, { passive: true });
}

function openDownloadModal(item, kind, presetCollection = "") {
  const root = $("#modalRoot");
  const isAudio = kind === "audio";
  root.innerHTML = `
    <div class="modal-backdrop">
      <div class="modal">
        <h2>${escapeHtml(isAudio ? "Descargar audio" : "Descargar video")}</h2>
        <p>${escapeHtml(item.title)}</p>
        <div class="field">
          <label>Calidad</label>
          <div class="pills">
            ${["4K", "1080p", "720p", "480p", "360p"].map((q) => `<button class="pill ${q === "1080p" ? "active" : ""}" type="button" data-quality="${q}" ${isAudio ? 'style="display:none"' : ""}>${q}</button>`).join("")}
          </div>
        </div>
        <div class="field">
          <label>Colección</label>
          <input id="collectionInput" class="form-input" placeholder="Opcional" value="${escapeHtml(presetCollection)}">
        </div>
        <div class="modal-actions">
          <button class="btn btn-ghost" type="button" id="cancelModal">Cancelar</button>
          <button class="btn btn-primary" type="button" id="confirmDownload">Descargar</button>
        </div>
      </div>
    </div>
  `;
  let quality = "1080p";
  root.querySelectorAll("[data-quality]").forEach((btn) => {
    btn.addEventListener("click", () => {
      quality = btn.dataset.quality;
      root.querySelectorAll("[data-quality]").forEach((b) => b.classList.toggle("active", b === btn));
    });
  });
  $("#cancelModal").onclick = () => root.innerHTML = "";
  $(".modal-backdrop").onclick = (ev) => { if (ev.target.classList.contains("modal-backdrop")) root.innerHTML = ""; };
  $("#confirmDownload").onclick = async () => {
    const collection = $("#collectionInput")?.value || "";
    root.innerHTML = "";
    await startDownload(item, kind, quality, collection);
  };
}

function openImportPlaylistModal(url = "") {
  const root = $("#modalRoot");
  root.innerHTML = `
    <div class="modal-backdrop">
      <div class="modal">
        <h2>Importar playlist</h2>
        <p>Pega una playlist de YouTube o YouTube Music y elige si quieres copiarla o mantenerla vinculada.</p>
        <div class="field">
          <label>URL</label>
          <input id="importPlaylistUrl" class="form-input" value="${escapeHtml(url)}" placeholder="https://youtube.com/playlist?list=...">
        </div>
        <div class="field">
          <label>Modo</label>
          <div class="pills">
            <button class="pill active" type="button" data-import-mode="copy">Copia local</button>
            <button class="pill" type="button" data-import-mode="linked">Vinculada</button>
          </div>
        </div>
        <div class="modal-actions">
          <button class="btn btn-ghost" type="button" id="cancelImportPlaylist">Cancelar</button>
          <button class="btn btn-primary" type="button" id="confirmImportPlaylist">Importar</button>
        </div>
      </div>
    </div>
  `;
  let mode = "copy";
  root.querySelectorAll("[data-import-mode]").forEach((btn) => {
    btn.addEventListener("click", () => {
      mode = btn.dataset.importMode;
      root.querySelectorAll("[data-import-mode]").forEach((b) => b.classList.toggle("active", b === btn));
    });
  });
  $("#cancelImportPlaylist").onclick = () => root.innerHTML = "";
  $(".modal-backdrop").onclick = (ev) => { if (ev.target.classList.contains("modal-backdrop")) root.innerHTML = ""; };
  $("#confirmImportPlaylist").onclick = async () => {
    const target = $("#importPlaylistUrl").value.trim();
    if (!target) {
      toast("Pega la URL de la playlist.");
      return;
    }
    root.innerHTML = "";
    try {
      toast("Importando playlist...");
      await api.post("/api/playlists/import", { url: target, mode });
      await refreshLists();
      setView("collections");
      toast("Playlist importada");
    } catch (err) {
      toast(err.message);
    }
  };
}

async function startDownload(item, kind, quality = "1080p", collection = "") {
  try {
    await api.post("/api/downloads", {
      url: item.url || state.query.trim(),
      title: item.title,
      channel: item.channel || item.sourceChannelName,
      thumbnail: item.thumbnail,
      kind,
      quality,
      collection,
    });
    toast("Descarga iniciada");
    setView("downloads");
    await refreshSummary();
  } catch (err) {
    toast(err.message);
  }
}

async function playTarget(target, player, title, thumbnail, channel) {
  if (!player && isUrl(target)) {
    const ctx = state.view !== "watch" ? state.view : (state.player.sourceContext || "search");
    await openWatchQueue([{ url: target, title: title || target, thumbnail: thumbnail || "", channel: channel || "", durationText: "" }], 0, ctx);
    return;
  }
  try {
    const body = { target, title };
    if (player) body.player = player;
    if (thumbnail) body.thumbnail = thumbnail;
    if (channel) body.channel = channel;
    const result = await api.post("/api/play", body);
    toast(`Abriendo en ${result.player || state.config?.defaultPlayer || "reproductor"}`);
    await refreshLists();
  } catch (err) {
    toast(err.message);
  }
}

async function directPlay() {
  const target = state.query.trim();
  if (!target) {
    toast("Pega una URL de YouTube en la búsqueda primero.");
    return;
  }
  await playTarget(target, null, target);
}

// ── Playlist edit helpers ─────────────────────────────────────────────────────

function _renderPlaylistDetail(plId) {
  const pl = state.playlists.find((p) => p.id === plId);
  if (!pl) { renderCollections(); return; }
  if (state.view !== "collections") setView("collections");
  const root = $("#view-collections");
  root.innerHTML = `
    <div class="section-hdr">
      <span class="section-title">▶ ${escapeHtml(pl.name)}</span>
      <div style="display:flex;gap:8px;align-items:center;flex-wrap:wrap">
        <button class="btn btn-ghost" type="button" data-rename-playlist="${escapeHtml(pl.id)}" title="Cambiar nombre">✎ Renombrar</button>
        <button class="btn btn-ghost" type="button" data-add-song-to-playlist="${escapeHtml(pl.id)}">＋ Agregar</button>
        ${pl.importMode === "linked" ? `<button class="btn btn-ghost" type="button" data-sync-playlist="${escapeHtml(pl.id)}">↺ Sync</button>` : ""}
        <button class="btn btn-primary" type="button" data-play-playlist-id="${escapeHtml(pl.id)}">▶ Reproducir todo</button>
        <button class="btn btn-ghost" type="button" onclick="renderCollections()">← Volver</button>
      </div>
    </div>
    ${pl.items?.length ? `
      <p class="meta" style="padding:4px 0 10px;font-size:11px">Arrastra ⠿ para reordenar · ${pl.items.length} canciones</p>
      <div class="playlist-item-list" id="plDetailList">
        ${pl.items.map((item, idx) => `
          <div class="queue-item pl-draggable" draggable="true" data-drag-idx="${idx}" data-drag-pl="${escapeHtml(plId)}">
            <span class="drag-handle" title="Arrastrar para reordenar">⠿</span>
            <span class="queue-idx">${idx + 1}</span>
            ${item.thumbnail ? `<img class="queue-thumb" src="${escapeHtml(thumbSrc(item.thumbnail))}" alt="" loading="lazy">` : `<div class="queue-thumb"></div>`}
            <div class="queue-info">
              <div class="queue-title">${escapeHtml(item.title || item.url)}</div>
              <div class="queue-meta">${escapeHtml(item.channel || "")}${item.durationText ? ` · ${escapeHtml(item.durationText)}` : ""}</div>
            </div>
            <button class="btn btn-ghost" type="button" style="padding:3px 8px;font-size:11px;flex-shrink:0"
              data-remove-from-playlist="${escapeHtml(plId)}"
              data-remove-item-url="${escapeHtml(item.url || '')}">✕</button>
          </div>
        `).join("")}
      </div>
    ` : `<p class="meta" style="padding:12px 0">Playlist vacía. Usa ＋ Agregar para añadir canciones.</p>`}
  `;
  _bindPlaylistDnD(plId);
}

function _bindPlaylistDnD(plId) {
  const list = document.getElementById("plDetailList");
  if (!list) return;
  list.querySelectorAll(".pl-draggable").forEach((row) => {
    row.addEventListener("dragstart", (ev) => {
      _dragSrc = Number(row.dataset.dragIdx);
      ev.dataTransfer.effectAllowed = "move";
      row.classList.add("dragging");
    });
    row.addEventListener("dragend", () => {
      row.classList.remove("dragging");
      list.querySelectorAll(".drag-over").forEach((el) => el.classList.remove("drag-over"));
    });
    row.addEventListener("dragover", (ev) => {
      ev.preventDefault();
      ev.dataTransfer.dropEffect = "move";
      list.querySelectorAll(".drag-over").forEach((el) => el.classList.remove("drag-over"));
      if (_dragSrc !== null && Number(row.dataset.dragIdx) !== _dragSrc) row.classList.add("drag-over");
    });
    row.addEventListener("dragleave", () => row.classList.remove("drag-over"));
    row.addEventListener("drop", async (ev) => {
      ev.preventDefault();
      row.classList.remove("drag-over");
      const destIdx = Number(row.dataset.dragIdx);
      if (_dragSrc === null || _dragSrc === destIdx) { _dragSrc = null; return; }
      const pl = state.playlists.find((p) => p.id === plId);
      if (!pl) { _dragSrc = null; return; }
      const items = [...pl.items];
      const [moved] = items.splice(_dragSrc, 1);
      items.splice(destIdx, 0, moved);
      pl.items = items;
      _dragSrc = null;
      _renderPlaylistDetail(plId);
      try {
        await api.post(`/api/playlists/${plId}/items/reorder`, { urls: items.map((i) => i.url) });
        await refreshSection("playlists", "/api/playlists");
        _renderPlaylistDetail(plId);
      } catch (err) {
        toast(err.message);
      }
    });
  });
}

function openAddSongToPlaylistModal(plId) {
  const root = $("#modalRoot");
  root.innerHTML = `
    <div class="modal-backdrop" id="addSongBackdrop">
      <div class="modal" style="max-width:560px;max-height:min(88vh,640px);display:flex;flex-direction:column;overflow:hidden">
        <div style="display:flex;align-items:center;justify-content:space-between;margin-bottom:12px;flex-shrink:0">
          <strong style="font-size:15px">＋ Agregar canciones</strong>
          <button class="icon-btn" id="addSongClose" type="button">×</button>
        </div>
        <input class="form-input" id="addSongSearch" placeholder="Artista, canción o URL..." autocomplete="off" style="margin-bottom:8px;flex-shrink:0">
        <div style="display:flex;gap:6px;margin-bottom:6px;flex-shrink:0">
          ${[["music","Music"],["youtube","YouTube"],["all","Todo"]].map(([v,l]) =>
            `<button class="pill${v === "music" ? " active" : ""}" type="button" data-add-scope="${v}">${l}</button>`
          ).join("")}
        </div>
        <div id="addSongMusicFilters" style="display:flex;gap:5px;margin-bottom:8px;flex-shrink:0;flex-wrap:wrap">
          ${[["songs","Canciones"],["artists","Artistas"],["albums","Álbumes"],["videos","Videos"],["playlists","Listas"],["","Todo"]].map(([v,l]) =>
            `<button class="pill${v === "songs" ? " active" : ""}" type="button" data-add-music-filter="${v}" style="font-size:11px">${l}</button>`
          ).join("")}
        </div>
        <div id="addSongResults" style="overflow-y:auto;flex:1;min-height:0"></div>
      </div>
    </div>
  `;
  const close = () => { root.innerHTML = ""; };
  document.getElementById("addSongClose").onclick = close;
  document.getElementById("addSongBackdrop").addEventListener("click", (e) => {
    if (e.target === document.getElementById("addSongBackdrop")) close();
  });
  let addScope = "music";
  let addMusicFilter = "songs";
  const musicFiltersEl = () => document.getElementById("addSongMusicFilters");
  root.querySelectorAll("[data-add-scope]").forEach((btn) => {
    btn.addEventListener("click", () => {
      addScope = btn.dataset.addScope;
      root.querySelectorAll("[data-add-scope]").forEach((b) => b.classList.toggle("active", b === btn));
      const filtersDiv = musicFiltersEl();
      if (filtersDiv) filtersDiv.style.display = addScope === "music" ? "flex" : "none";
      if (addScope === "music") { addMusicFilter = "songs"; filtersDiv?.querySelectorAll("[data-add-music-filter]").forEach((b) => b.classList.toggle("active", b.dataset.addMusicFilter === "songs")); }
      const q = document.getElementById("addSongSearch")?.value.trim();
      if (q) triggerSearch(q);
    });
  });
  root.querySelectorAll("[data-add-music-filter]").forEach((btn) => {
    btn.addEventListener("click", () => {
      addMusicFilter = btn.dataset.addMusicFilter;
      root.querySelectorAll("[data-add-music-filter]").forEach((b) => b.classList.toggle("active", b === btn));
      const q = document.getElementById("addSongSearch")?.value.trim();
      if (q) triggerSearch(q);
    });
  });
  const added = new Set((state.playlists.find((p) => p.id === plId)?.items || []).map((i) => i.url));
  const resultsEl = document.getElementById("addSongResults");
  let searchTimer = null;

  function triggerSearch(q) {
    clearTimeout(searchTimer);
    resultsEl.innerHTML = `<p class="meta" style="padding:8px 0">Buscando...</p>`;
    searchTimer = setTimeout(() => runSearch(q), 400);
  }

  async function runSearch(q) {
    try {
      let scopeParam = addScope === "all" ? "" : `&scope=${encodeURIComponent(addScope)}`;
      if (addScope === "music") {
        if (addMusicFilter) scopeParam += `&search_filter=${encodeURIComponent(addMusicFilter)}`;
        scopeParam += `&limit=${DEFAULT_MUSIC_SEARCH_LIMIT}`;
      }
      if (addScope === "youtube") scopeParam += "&limit=20";
      const data = await api.get(`/api/search?q=${encodeURIComponent(q)}${scopeParam}`);
      const items = (data.items || []).filter((i) => i.url && !i.url.includes("list=") && i.resultType !== "artist" && i.resultType !== "album" && !i.url.includes("/browse/"));
        if (!items.length) { resultsEl.innerHTML = `<p class="meta" style="padding:8px 0">Sin resultados.</p>`; return; }
        const itemMap = new Map(items.map((i) => [i.url, i]));
        resultsEl.innerHTML = `<div class="add-song-list">${items.map((item) => `
          <div style="display:flex;align-items:center;gap:10px;padding:6px 4px;border-radius:6px;border-bottom:1px solid var(--br)">
            ${item.thumbnail
              ? `<img src="${escapeHtml(thumbSrc(item.thumbnail))}" alt="" width="54" height="38" style="width:54px;height:38px;object-fit:cover;border-radius:4px;flex-shrink:0">`
              : `<div style="width:54px;height:38px;background:var(--bg3);border-radius:4px;flex-shrink:0"></div>`}
            <div style="flex:1;min-width:0;overflow:hidden">
              <div style="font-size:13px;font-weight:600;white-space:nowrap;overflow:hidden;text-overflow:ellipsis">${escapeHtml(item.title || "")}</div>
              <div style="font-size:11px;color:var(--tx2)">${escapeHtml(item.channel || "")}${item.durationText ? ` · ${escapeHtml(item.durationText)}` : ""}</div>
            </div>
            <button class="btn ${added.has(item.url) ? "btn-primary" : "btn-ghost"}" type="button" style="flex-shrink:0;min-width:36px" data-add-url="${escapeHtml(item.url)}">${added.has(item.url) ? "✓" : "＋"}</button>
          </div>
        `).join("")}</div>`;
        resultsEl.querySelectorAll("[data-add-url]").forEach((btn) => {
          btn.addEventListener("click", async () => {
            const url = btn.dataset.addUrl;
            if (added.has(url)) return;
            const item = itemMap.get(url);
            if (!item) return;
            try {
              await api.post(`/api/playlists/${plId}/items`, {
                url: item.url, title: item.title,
                thumbnail: item.thumbnail || "", channel: item.channel || "", durationText: item.durationText || "",
              });
              added.add(url);
              btn.textContent = "✓";
              btn.classList.replace("btn-ghost", "btn-primary");
              const pl = state.playlists.find((p) => p.id === plId);
              if (pl && !pl.items.find((i) => i.url === url)) pl.items.push(item);
              toast(`"${item.title}" agregado`);
            } catch (err) {
              toast(err.message);
            }
          });
        });
      } catch (err) {
        resultsEl.innerHTML = `<p class="meta" style="padding:8px 0">${escapeHtml(err.message)}</p>`;
      }
  }

  document.getElementById("addSongSearch").addEventListener("input", (ev) => {
    const q = ev.target.value.trim();
    if (!q) { clearTimeout(searchTimer); resultsEl.innerHTML = ""; return; }
    triggerSearch(q);
  });
  setTimeout(() => document.getElementById("addSongSearch")?.focus(), 50);
}

// ── Events ────────────────────────────────────────────────────────────────────

function bindEvents() {
  $$(".nav-item[data-view]").forEach((btn) => btn.addEventListener("click", () => setView(btn.dataset.view)));
  $("#sidebarToggle").onclick = () => toggleSidebar();
  $("#themeBtn").onclick = () => setTheme(state.theme === "dark" ? "light" : "dark");
  $("#searchInput").addEventListener("input", (ev) => {
    state.query = ev.target.value;
    clearTimeout(bindEvents.searchTimer);
    bindEvents.searchTimer = setTimeout(doSearch, 450);
  });
  $("#searchInput").addEventListener("keydown", (ev) => {
    if (ev.key === "Enter") {
      clearTimeout(bindEvents.searchTimer);
      doSearch();
    }
  });
  $("#clearSearch").onclick = () => {
    $("#searchInput").value = "";
    state.query = "";
    state.results = [];
    renderSearch();
  };
  $("#directPlay").onclick = () => directPlay();

  document.body.addEventListener("submit", async (ev) => {
    ev.preventDefault();
    if (ev.target.id === "channelForm") {
      const form = new FormData(ev.target);
      try {
        toast("Agregando canal...");
        await api.post("/api/channels", { url: form.get("url"), name: form.get("name") });
        await refreshLists();
        setView("channels");
        toast("Canal agregado. Ve a Para ti o Canales para ver videos recientes.");
      } catch (err) {
        toast(err.message);
      }
    }
    if (ev.target.id === "playlistForm") {
      ev.preventDefault();
      const form = new FormData(ev.target);
      try {
        await api.post("/api/playlists", { name: form.get("name") });
        ev.target.reset();
        const data = await api.get("/api/playlists");
        state.playlists = data.items || [];
        renderCollections();
      } catch (err) {
        toast(err.message);
      }
    }
    if (ev.target.id === "collectionForm") {
      const form = new FormData(ev.target);
      try {
        await api.post("/api/collections", { name: form.get("name") });
        ev.target.reset();
        await refreshSection("collections", "/api/collections");
      } catch (err) {
        toast(err.message);
      }
    }
    if (ev.target.id === "configForm") {
      const form = new FormData(ev.target);
      const submittedConfig = {
        mediaRoot: form.get("mediaRoot"),
        defaultPlayer: form.get("defaultPlayer"),
        mpvPath: form.get("mpvPath"),
        vlcPath: form.get("vlcPath"),
        ffmpegPath: form.get("ffmpegPath"),
        audioFormat: form.get("audioFormat"),
        playbackQuality: form.get("playbackQuality"),
        searchLimit: Number(form.get("searchLimit") || 12),
        cookiesBrowser: form.get("cookiesBrowser") || "",
        cookiesFile: form.get("cookiesFile") || "",
        showShorts: form.get("showShorts") === "on",
        discordEnabled: form.get("discordEnabled") === "on",
        discordShowMedia: form.get("discordShowMedia") === "on",
        discordShowParty: form.get("discordShowParty") === "on",
      };
      try {
        const savedConfig = await api.post("/api/config", submittedConfig);
        state.config = { ...submittedConfig, ...savedConfig, playbackQuality: savedConfig.playbackQuality || submittedConfig.playbackQuality };
        applyShortsVisibility();
        if (IS_TAURI) {
          await refreshNativePlayerStatus();
          _refreshDiscordStatus().catch(() => {});
          _syncDiscordMediaPresence(true);
        }
        toast("Configuración guardada");
        renderConfig();
      } catch (err) {
        toast(err.message);
      }
    }
  });

  _setupHoverPrefetch();
  document.body.addEventListener("click", async (ev) => {
    const musicFilter = ev.target.closest("[data-music-filter]");
    if (musicFilter) {
      state.musicSearchFilter = musicFilter.dataset.musicFilter;
      await doSearch();
      return;
    }
    if (ev.target.closest("#authStatus")) {
      if (state.musicAuthState === "ok") {
        openConnectedAccountModal();
        return;
      }
      startLoginFlow();
      return;
    }
    if (ev.target.closest("[data-music-login]")) {
      if (state.musicAuthState === "ok") {
        setView("music");
        return;
      }
      startLoginFlow();
      return;
    }
    const searchScope = ev.target.closest("[data-search-scope]");
    if (searchScope) {
      state.searchScope = searchScope.dataset.searchScope;
      state.musicSearchFilter = "";
      await doSearch();
      return;
    }
    const openArtist = ev.target.closest("[data-open-artist]");
    if (openArtist) {
      await openArtistView(openArtist.dataset.openArtist, openArtist.dataset.openArtistName);
      return;
    }
    const artistPlay = ev.target.closest("[data-artist-play]");
    if (artistPlay) {
      const songs = state.artistView?.data?.songs || [];
      const idx = Number(artistPlay.dataset.artistPlay) || 0;
      if (songs.length) await openWatchQueue(songs.map(_itemToQueueEntry), idx, "music");
      return;
    }
    const albumPlay = ev.target.closest("[data-album-play]");
    if (albumPlay) {
      toast("Cargando álbum...");
      try {
        const data = await api.post("/api/music/playlist-songs", { url: albumPlay.dataset.albumPlay });
        if (!data.items?.length) {
          toast("No se encontraron canciones en este álbum");
          return;
        }
        await openWatchQueue(data.items.map(_itemToQueueEntry), 0, "music");
      } catch (err) {
        toast(err.message);
      }
      return;
    }
    if (ev.target.closest("[data-open-import-playlist]")) {
      openImportPlaylistModal();
      return;
    }
    const importResult = ev.target.closest("[data-import-result-playlist]");
    if (importResult) {
      openImportPlaylistModal(importResult.dataset.importResultPlaylist);
      return;
    }
    if (ev.target.closest("#refreshMusicLibrary")) {
      try {
        await startBackgroundTask(await api.post("/api/music/library/refresh"), "Sincronizando biblioteca de YouTube Music...");
      } catch (err) { toast(err.message); }
      return;
    }
    const dl = ev.target.closest("[data-download]");
    if (dl) {
      const item = findVideo(dl.dataset.id);
      if (item) openDownloadModal(item, dl.dataset.download);
    }
    const play = ev.target.closest("[data-play-default]");
    if (play) {
      const item = findVideo(play.dataset.id);
      if (item) await openWatchQueue([_itemToQueueEntry(item)], 0, state.view);
    }
    const addToQueueBtn = ev.target.closest("[data-add-to-queue]");
    if (addToQueueBtn) {
      const item = findVideo(addToQueueBtn.dataset.id);
      if (item) addToQueue(item);
    }
    const local = ev.target.closest("[data-local-play-default]");
    if (local) await playTarget(local.dataset.path, null, local.dataset.path);

    const urlPlay = ev.target.closest("[data-url-play]");
    if (urlPlay && state.query.trim()) {
      await directPlay();
    }

    const urlDownload = ev.target.closest("[data-url-download]");
    if (urlDownload && state.query.trim()) {
      openDownloadModal({ url: state.query.trim(), title: state.query.trim(), channel: "YouTube" }, urlDownload.dataset.urlDownload);
    }
    const refreshChannel = ev.target.closest("[data-refresh-channel]");
    if (refreshChannel) {
      try {
        toast("Actualizando canal...");
        await startBackgroundTask(await api.post(`/api/channels/${refreshChannel.dataset.refreshChannel}/refresh`), "Actualizando canal en segundo plano...");
      } catch (err) {
        toast(err.message);
      }
    }
    if (ev.target.closest("#refreshAllChannels")) {
      try {
        toast("Actualizando canales...");
        await startBackgroundTask(await api.post("/api/channels/refresh"), "Actualizando canales en segundo plano...");
      } catch (err) {
        toast(err.message);
      }
    }
    const deleteChannel = ev.target.closest("[data-delete-channel]");
    if (deleteChannel) {
      if (!confirm("¿Eliminar este canal?")) return;
      try {
        await api.delete(`/api/channels/${deleteChannel.dataset.deleteChannel}`);
        await refreshSection("channels", "/api/channels");
        renderChannels();
      } catch (err) {
        toast(err.message);
      }
    }
    const viewGo = ev.target.closest("[data-view-go]");
    if (viewGo) setView(viewGo.dataset.viewGo);

    if (ev.target.closest("#refreshMusicRecom")) {
      try {
        toast("Obteniendo recomendaciones de YT Music...");
        await startBackgroundTask(await api.post("/api/music/recommendations/refresh"), "Actualizando YT Music en segundo plano...");
      } catch (err) {
        toast(err.message);
      }
    }
    if (ev.target.closest("#refreshMusicPlaylists")) {
      try {
        toast("Obteniendo playlists de YT Music...");
        await startBackgroundTask(await api.post("/api/music/playlists/refresh"), "Actualizando playlists en segundo plano...");
      } catch (err) {
        toast(err.message);
      }
    }
    const musicPlaylist = ev.target.closest("[data-music-playlist]");
    if (musicPlaylist) {
      const url = musicPlaylist.dataset.musicPlaylist;
      toast("Cargando canciones...");
      try {
        const data = await api.post("/api/player/playlist", { url, quality: state.config?.playbackQuality || "best" });
        if (data.requiresLogin || !(data.items?.length)) {
          toast(data.error || "No se encontraron canciones en la playlist");
          return;
        }
        await openWatchQueue(data.items, 0, "music");
      } catch (err) {
        toast(err.message);
      }
    }
    if (ev.target.closest("#refreshRecom")) {
      try {
        toast("Actualizando recomendaciones de YouTube y YT Music...");
        const [ytRes, ytmRes] = await Promise.allSettled([
          api.post("/api/recommendations/refresh"),
          api.post("/api/music/recommendations/refresh"),
        ]);
        const taskRes = ytRes.status === "fulfilled"
          ? ytRes.value
          : (ytmRes.status === "fulfilled" ? ytmRes.value : null);
        if (taskRes) {
          await startBackgroundTask(taskRes, "Actualizando recomendaciones en segundo plano...");
        } else if (ytRes.status === "rejected") {
          throw ytRes.reason;
        }
      } catch (err) {
        toast(err.message);
      }
    }
    const addChannel = ev.target.closest("[data-add-channel]");
    if (addChannel) {
      try {
        toast("Agregando canal...");
        await api.post("/api/channels", {
          url: addChannel.dataset.addChannel,
          name: addChannel.dataset.addChannelName,
        });
        await refreshLists();
        toast(`Canal agregado: ${addChannel.dataset.addChannelName}`);
        _refreshAfterFollowChange();
      } catch (err) {
        toast(err.message);
      }
      return;
    }
    const unfollowChannel = ev.target.closest("[data-unfollow-channel]");
    if (unfollowChannel) {
      try {
        await api.delete(`/api/channels/${unfollowChannel.dataset.unfollowChannel}`);
        await refreshLists();
        toast("Dejaste de seguir el canal");
        _refreshAfterFollowChange();
      } catch (err) {
        toast(err.message);
      }
      return;
    }
    const homeExpand = ev.target.closest("[data-home-expand]");
    if (homeExpand) {
      const key = homeExpand.dataset.homeExpand;
      state.homeExpanded[key] = !state.homeExpanded[key];
      renderHome();
      return;
    }
    const recomExpand = ev.target.closest("[data-recom-expand]");
    if (recomExpand) {
      _expandRecomSection(recomExpand.dataset.recomExpand);
      return;
    }
    const watchChannel = ev.target.closest("[data-watch-channel]");
    if (watchChannel) {
      openChannelView(watchChannel.dataset.watchChannel, watchChannel.dataset.watchChannelName);
      return;
    }
    const viewChannel = ev.target.closest("[data-view-channel]");
    if (viewChannel) {
      const channel = state.channels.find((c) => c.id === viewChannel.dataset.viewChannel);
      if (channel) openChannelView(channel.url, channel.name);
      return;
    }
    const cancelDownload = ev.target.closest("[data-cancel-download]");
    if (cancelDownload) {
      try {
        await api.post(`/api/downloads/${cancelDownload.dataset.cancelDownload}/cancel`);
        await refreshSummary();
      } catch (err) {
        toast(err.message);
      }
    }
    const deleteDownload = ev.target.closest("[data-delete-download]");
    if (deleteDownload) {
      try {
        await api.delete(`/api/downloads/${deleteDownload.dataset.deleteDownload}`);
        await refreshSummary();
      } catch (err) {
        toast(err.message);
      }
    }
    const deleteCollection = ev.target.closest("[data-delete-collection]");
    if (deleteCollection) {
      ev.stopPropagation();
      if (!confirm("¿Eliminar esta colección?")) return;
      try {
        await api.delete(`/api/collections/${deleteCollection.dataset.deleteCollection}`);
        await refreshSection("collections", "/api/collections");
      } catch (err) {
        toast(err.message);
      }
    }
    const collection = ev.target.closest("[data-collection]");
    if (collection) {
      const name = collection.dataset.collection;
      const items = state.library.filter((item) => item.collection === name);
      $("#view-collections").innerHTML = `
        <div class="section-hdr">
          <span class="section-title">Colección: ${escapeHtml(name)}</span>
          <button class="btn btn-ghost" type="button" onclick="renderCollections()">Volver</button>
        </div>
        ${items.length ? `<div class="grid">${items.map(libraryCard).join("")}</div>` : empty("Colección vacía", "Descarga archivos usando esta colección.")}
      `;
    }

    // Playlist handlers
    const addToPlaylist = ev.target.closest("[data-add-to-playlist]");
    if (addToPlaylist) {
      const d = addToPlaylist.dataset;
      openAddToPlaylistModal(d.addToPlaylist, {
        title: d.playlistTitle || "",
        thumbnail: d.playlistThumb || "",
        channel: d.playlistChannel || "",
        durationText: d.playlistDuration || "",
      });
    }
    const deletePlaylist = ev.target.closest("[data-delete-playlist]");
    if (deletePlaylist) {
      ev.stopPropagation();
      if (!confirm("¿Eliminar esta playlist?")) return;
      try {
        await api.delete(`/api/playlists/${deletePlaylist.dataset.deletePlaylist}`);
        const data = await api.get("/api/playlists");
        state.playlists = data.items || [];
        renderCollections();
      } catch (err) {
        toast(err.message);
      }
    }
    const renamePl = ev.target.closest("[data-rename-playlist]");
    if (renamePl) {
      const plId = renamePl.dataset.renamePlaylist;
      const pl = state.playlists.find((p) => p.id === plId);
      if (!pl) return;
      const newName = prompt("Nuevo nombre para la playlist:", pl.name);
      if (!newName?.trim() || newName.trim() === pl.name) return;
      try {
        await api.patch(`/api/playlists/${plId}`, { name: newName.trim() });
        pl.name = newName.trim();
        await refreshSection("playlists", "/api/playlists");
        _renderPlaylistDetail(plId);
        toast("Playlist renombrada");
      } catch (err) { toast(err.message); }
      return;
    }
    const addSongBtn = ev.target.closest("[data-add-song-to-playlist]");
    if (addSongBtn) {
      openAddSongToPlaylistModal(addSongBtn.dataset.addSongToPlaylist);
      return;
    }
    const syncPlaylist = ev.target.closest("[data-sync-playlist]");
    if (syncPlaylist) {
      const syncPlId = syncPlaylist.dataset.syncPlaylist;
      try {
        toast("Sincronizando playlist...");
        await api.post(`/api/playlists/${syncPlId}/sync`);
        await refreshSection("playlists", "/api/playlists");
        _renderPlaylistDetail(syncPlId);
        toast("Playlist sincronizada");
      } catch (err) {
        toast(err.message);
      }
      return;
    }
    const playlistCard = ev.target.closest("[data-playlist-id]");
    if (playlistCard && !ev.target.closest("[data-delete-playlist]")) {
      _renderPlaylistDetail(playlistCard.dataset.playlistId);
    }
    const playPlaylistBtn = ev.target.closest("[data-play-playlist-id]");
    if (playPlaylistBtn) {
      const plId = playPlaylistBtn.dataset.playPlaylistId;
      const pl = state.playlists.find((p) => p.id === plId);
      if (!pl?.items?.length) { toast("Playlist vacía"); return; }
      await openWatchQueue(pl.items.map(_itemToQueueEntry), 0, "search");
    }
    const removeFromPlaylist = ev.target.closest("[data-remove-from-playlist]");
    if (removeFromPlaylist) {
      const plId = removeFromPlaylist.dataset.removeFromPlaylist;
      const itemUrl = removeFromPlaylist.dataset.removeItemUrl;
      try {
        // optimistic update
        const pl = state.playlists.find((p) => p.id === plId);
        if (pl) pl.items = pl.items.filter((i) => i.url !== itemUrl);
        _renderPlaylistDetail(plId);
        await api.post(`/api/playlists/${plId}/items/remove`, { url: itemUrl });
        await refreshSection("playlists", "/api/playlists");
        _renderPlaylistDetail(plId);
      } catch (err) { toast(err.message); }
    }

    // Watch view controls
    const watchSecToggle = ev.target.closest("[data-watch-section-toggle]");
    if (watchSecToggle) {
      _toggleWatchSidebarSection(watchSecToggle.dataset.watchSectionToggle);
      return;
    }
    const partyRemoveBtn = ev.target.closest("[data-party-remove-idx]");
    if (partyRemoveBtn) {
      ev.stopPropagation();
      const idx = Number(partyRemoveBtn.dataset.partyRemoveIdx);
      try {
        const snap = await api.post("/api/party/queue/remove", { index: idx });
        _onPartyStateUpdate(snap);
      } catch (err) { toast(err.message); }
      return;
    }
    const queueItem = ev.target.closest("[data-queue-idx]");
    if (queueItem) {
      if (state.party?.active && state.party.role === "guest") {
        toast("Solo el host puede cambiar la pista actual");
        return;
      }
      const idx = Number(queueItem.dataset.queueIdx);
      await loadWebPlayerItem(idx);
    }
    const relatedQueue = ev.target.closest("[data-related-queue-url]");
    if (relatedQueue) {
      ev.stopPropagation();
      const url = relatedQueue.dataset.relatedQueueUrl;
      const ctx = state.player.sourceContext || "search";
      const allItems = ctx === "music"
        ? state.musicRecommendations
        : [...state.recommendations, ...state.feed, ...state.results];
      const found = allItems.find((i) => String(i.url || i.feedId || "") === url);
      addToQueue(found || { url, title: url });
      return;
    }
    const relatedItem = ev.target.closest("[data-related-url]");
    if (relatedItem) {
      const url = relatedItem.dataset.relatedUrl;
      const ctx = state.player.sourceContext || "search";
      const allItems = ctx === "music"
        ? state.musicRecommendations
        : [...state.recommendations, ...state.feed, ...state.results];
      const found = allItems.find((i) => String(i.url || i.feedId || "") === url);
      await openWatchQueue([_itemToQueueEntry(found || { url, title: url })], 0, ctx);
    }
    const watchDownload = ev.target.closest("[data-watch-download]");
    if (watchDownload && state.player.current) {
      openDownloadModal(state.player.current, watchDownload.dataset.watchDownload);
    }
    if (ev.target.closest("#watchBack")) {
      await closeWebPlayer();
    }
    if (ev.target.closest("#watchPrev") && !_partyIsGuest() && state.player.index > 0) {
      await loadWebPlayerItem(state.player.index - 1);
    }
    if (ev.target.closest("#watchNext") && !_partyIsGuest() && state.player.index < state.player.queue.length - 1) {
      await loadWebPlayerItem(state.player.index + 1);
    }
    if (ev.target.closest("#ctrlShuffle") && !_partyIsGuest()) { toggleShuffle(); }
    if (ev.target.closest("#ctrlRepeat") && !_partyIsGuest()) { cycleRepeat(); }
    if (ev.target.closest("#ctrlAutoplay") && !_partyIsGuest()) { toggleAutoplay(); }
    if (ev.target.closest("#playerExternal") && state.player.current?.url) {
      const cur = state.player.current;
      try { state.player.vjs?.pause(); } catch (_) {}
      try { state.player.audioVjs?.pause(); } catch (_) {}
      try { _getAudioEl()?.pause(); } catch (_) {}
      await playTarget(cur.url, state.config?.defaultPlayer || "mpv", cur.title, cur.thumbnail, cur.channel);
    }
    const nativeCtrlBtn = ev.target.closest("[data-native-ctrl]");
    if (nativeCtrlBtn) {
      const action = nativeCtrlBtn.dataset.nativeCtrl;
      if (action === "toggle") await sendNativePlayerControl({ action: "togglePause" });
      else if (action === "seek-back") await sendNativePlayerControl({ action: "seek", seconds: -10, relative: true });
      else if (action === "seek-fwd") await sendNativePlayerControl({ action: "seek", seconds: 10, relative: true });
      else if (action === "mute") await sendNativePlayerControl({ action: "setMute", muted: !state.player.native?.muted });
      else if (action === "stop") await sendNativePlayerControl({ action: "stop" });
    }
    if (ev.target.closest("#toggleAudioOnly")) {
      state.player.audioOnly = !state.player.audioOnly;
      await loadWebPlayerItem(state.player.index);
    }
    const historyPlay = ev.target.closest("[data-history-play]");
    if (historyPlay) {
      const hItem = state.history.find((h) => h.url === historyPlay.dataset.historyPlay || h.id === historyPlay.dataset.historyPlay);
      await playTarget(historyPlay.dataset.historyPlay, null, historyPlay.dataset.historyTitle || historyPlay.dataset.historyPlay, hItem?.thumbnail, hItem?.channel);
      return;
    }
    const historyDownload = ev.target.closest("[data-download-history]");
    if (historyDownload) {
      const item = state.history.find((h) => h.id === historyDownload.dataset.historyId);
      if (item) openDownloadModal(item, historyDownload.dataset.downloadHistory);
      return;
    }
    const historyPlaylist = ev.target.closest("[data-add-history-playlist]");
    if (historyPlaylist) {
      const item = state.history.find((h) => h.id === historyPlaylist.dataset.addHistoryPlaylist);
      if (item) openAddToPlaylistModal(item.url, item);
      return;
    }
    const deleteHistory = ev.target.closest("[data-delete-history]");
    if (deleteHistory) {
      try {
        await api.delete(`/api/history/${deleteHistory.dataset.deleteHistory}`);
        state.history = state.history.filter((h) => h.id !== deleteHistory.dataset.deleteHistory);
        renderHistory();
      } catch (err) { toast(err.message); }
    }
  });

  // Keyboard shortcuts for watch view
  document.addEventListener("keydown", async (ev) => {
    if (state.view !== "watch") return;
    if (ev.target.matches("input, textarea, select")) return;
    const audio = state.player.sourceContext === "music" || state.player.audioOnly ? _getAudioEl() : null;
    const vjs = state.player.vjs;
    if (ev.code === "Space") {
      ev.preventDefault();
      if (!_partyCanPlayPause()) return;
      if (audio) { audio.paused ? audio.play().catch(() => {}) : audio.pause(); }
      else if (vjs) { vjs.paused() ? vjs.play().catch(() => {}) : vjs.pause(); }
    } else if (ev.code === "ArrowLeft") {
      ev.preventDefault();
      if (!_partyCanSeek()) return;
      if (audio) audio.currentTime = Math.max(0, audio.currentTime - 10);
      else if (vjs) vjs.currentTime(Math.max(0, vjs.currentTime() - 10));
    } else if (ev.code === "ArrowRight") {
      ev.preventDefault();
      if (!_partyCanSeek()) return;
      if (audio) audio.currentTime = Math.min(audio.duration || Infinity, audio.currentTime + 10);
      else if (vjs) vjs.currentTime(Math.min(vjs.duration() || Infinity, vjs.currentTime() + 10));
    } else if (ev.code === "KeyN") {
      if (_partyIsGuest()) return;
      if (state.player.index < state.player.queue.length - 1) await loadWebPlayerItem(state.player.index + 1);
      else _onTrackEnded();
    } else if (ev.code === "KeyP") {
      if (_partyIsGuest()) return;
      if (state.player.index > 0) await loadWebPlayerItem(state.player.index - 1);
    } else if (ev.code === "KeyS") {
      if (_partyIsGuest()) return;
      toggleShuffle();
    } else if (ev.code === "KeyR") {
      if (_partyIsGuest()) return;
      cycleRepeat();
    }
  });
}

// ── Poll ──────────────────────────────────────────────────────────────────────

let _pollTimer = null;

function scheduleNextPoll() {
  if (IS_TAURI) return;
  clearTimeout(_pollTimer);
  if (document.hidden) return;
  const hasActive = state.downloads.some((d) => !["done", "error", "cancelled"].includes(d.status));
  _hadActiveDownloads = _hadActiveDownloads || hasActive;
  _pollTimer = setTimeout(async () => {
    if (document.hidden) return;
    try {
      await refreshSummary();
      const stillActive = state.downloads.some((d) => !["done", "error", "cancelled"].includes(d.status));
      const syncRunning = Boolean(state.syncStatus?.running);
      const syncSuccess = state.syncStatus?.lastSuccess || "";
      const syncFinished = !syncRunning && syncSuccess && syncSuccess !== _lastSeenSyncSuccess;
      if ((_hadActiveDownloads && !stillActive) || syncFinished) {
        _hadActiveDownloads = stillActive;
        _lastSeenSyncSuccess = syncSuccess;
        await refreshLists();
      }
    } catch (_) { /* ignore */ }
    scheduleNextPoll();
  }, hasActive ? 2500 : 12000);
}

document.addEventListener("visibilitychange", () => {
  if (document.hidden) {
    clearTimeout(_pollTimer);
    _pollTimer = null;
  } else {
    refreshSummary().catch(() => {});
    scheduleNextPoll();
  }
});

async function bindTauriEvents() {
  if (!IS_TAURI || !window.__TAURI__?.event?.listen) return;
  const { listen } = window.__TAURI__.event;
  await listen("download-progress", (event) => {
    const ev = event.payload || {};
    const job = state.downloads.find((d) => d.id === ev.jobId);
    if (job) {
      job.status = ev.status ?? job.status;
      job.progress = ev.progress ?? job.progress;
      job.speed = ev.speed ?? job.speed;
      job.eta = ev.eta ?? job.eta;
      job.error = ev.error ?? job.error;
      if (ev.path) job.path = ev.path;
      job.updatedAt = ev.updatedAt ?? job.updatedAt;
    }
    if (state.view === "downloads") renderDownloads();
    $("#downloadBadge").textContent = state.downloads.filter((d) => !["done", "error", "cancelled"].includes(d.status)).length || "";
    if (ev.status === "done") {
      refreshLists().catch(() => {});
    }
  });
  await listen("sync-status", (event) => {
    const ev = event.payload || {};
    state.syncStatus = { ...(state.syncStatus || {}), ...ev };
    if (!ev.running && ev.lastSuccess && ev.lastSuccess !== _lastSeenSyncSuccess) {
      _lastSeenSyncSuccess = ev.lastSuccess;
      refreshLists().catch(() => {});
    }
  });
  await listen("auth-status", (event) => {
    const ev = event.payload || {};
    const wasOk = state.musicAuthState === "ok";
    if (ev.authState) state.musicAuthState = ev.authState;
    if (ev.accountName !== undefined) state.musicAccountName = ev.accountName;
    renderAuthStatus();
    if (ev.authState === "ok" && !wasOk) {
      toast(ev.accountName ? `Conectado como ${ev.accountName}` : "Sesión iniciada.");
    }
    refreshLists().catch(() => {});
  });
  await listen("update-status", (event) => {
    const prevStatus = state.updateStatus?.status || "";
    const ev = event.payload || {};
    state.updateStatus = ev;
    if (state.view === "config") renderConfig();
    if (ev.status === "available" && prevStatus !== "available") {
      toast(`Nueva versión v${ev.latestVersion} encontrada. Descargando...`);
    } else if (ev.status === "installing" && prevStatus !== "installing") {
      toast(`Instalando Playzi v${ev.latestVersion}... La app se reiniciará.`);
    } else if (ev.status === "up-to-date" && ev.manual && prevStatus === "checking") {
      toast(`Playzi ya está actualizado (v${ev.currentVersion}).`);
    } else if (ev.status === "error" && ev.manual && prevStatus !== "error") {
      toast(`Error al actualizar: ${ev.error || "desconocido"}`);
    }
  });
  await listen("player-state", (event) => {
    const ev = event.payload || {};
    if (ev.state) {
      state.player.native = ev.state;
      if (state.view === "watch") _updateWatchMeta();
    }
  });
  await listen("party-state", (event) => {
    if (event.payload) _onPartyStateUpdate(event.payload);
  });
  await listen("party-notice", (event) => {
    const msg = event.payload?.message;
    if (msg) toast(msg);
  });
  await listen("social-state", (event) => {
    if (event.payload) _onSocialStateUpdate(event.payload);
  });
  await listen("social-notice", (event) => {
    const msg = event.payload?.message;
    if (msg) toast(msg);
  });
  await listen("social-join-approved", (event) => {
    const ev = event.payload || {};
    if (ev.roomCode && ev.hostUserId) {
      _socialSentJoinReqs.delete(`${String(ev.roomCode).toUpperCase()}:${String(ev.hostUserId).toUpperCase()}`);
      if (state.view === "party") renderParty();
    }
  });
  await listen("discord-state", (event) => {
    if (event.payload) _onDiscordStateUpdate(event.payload);
  });
  await listen("discord-notice", (event) => {
    const msg = event.payload?.message;
    if (msg) toast(msg);
  });
}

let _discordPresenceTimer = null;
let _discordLastSentKey = "";

function _onDiscordStateUpdate(snap) {
  if (!snap) return;
  state.discord = {
    ...state.discord,
    ...snap,
    pendingJoinRequests: Array.isArray(snap.pendingJoinRequests) ? snap.pendingJoinRequests : [],
  };
  if (state.view === "config") renderConfig();
  if (state.view === "party") renderParty();
  if (state.view === "watch") _updateWatchPartyBar();
}

async function _refreshDiscordStatus() {
  if (!IS_TAURI) return;
  try {
    const snap = await api.get("/api/discord/status");
    _onDiscordStateUpdate(snap);
  } catch (_) {}
}

function _getCurrentMediaDuration() {
  const isAudio = state.player.sourceContext === "music" || state.player.audioOnly;
  if (isAudio) {
    if (state.player.audioVjs && !state.player.audioVjs.isDisposed()) {
      return Number(state.player.audioVjs.duration() || 0);
    }
    const audio = _getAudioEl();
    if (audio) return Number(audio.duration || 0);
  } else if (state.player.vjs && !state.player.vjs.isDisposed()) {
    return Number(state.player.vjs.duration() || 0);
  }
  return 0;
}

function _syncDiscordMediaPresence(immediate = false) {
  if (!IS_TAURI) return;
  const send = () => {
    const item = state.player.current;
    if (!item || !item.title) {
      if (_discordLastSentKey === "inactive" && !immediate) return;
      _discordLastSentKey = "inactive";
      api.post("/api/discord/presence", { active: false }).catch(() => {});
      return;
    }
    const media = _getCurrentMediaState();
    const duration = _getCurrentMediaDuration();
    const isAudio = state.player.sourceContext === "music" || state.player.audioOnly;
    const thumb = normalizeThumbUrl(item.thumbnail || "");
    const payload = {
      active: true,
      title: String(item.title || ""),
      channel: String(item.channel || ""),
      thumbnail: /^https?:\/\//i.test(thumb) ? thumb : "",
      url: String(item.url || ""),
      isPlaying: Boolean(media.isPlaying),
      isAudio: Boolean(isAudio),
      positionSecs: Number.isFinite(media.positionSecs) ? Math.max(0, media.positionSecs) : 0,
      durationSecs: Number.isFinite(duration) ? Math.max(0, duration) : 0,
    };
    const key = `${payload.url}|${payload.title}|${payload.isPlaying}|${payload.isAudio}|${Math.round(payload.positionSecs / 5)}|${Math.round(payload.durationSecs)}`;
    if (!immediate && key === _discordLastSentKey) return;
    _discordLastSentKey = key;
    api.post("/api/discord/presence", payload).catch(() => {});
  };
  clearTimeout(_discordPresenceTimer);
  if (immediate) {
    send();
  } else {
    _discordPresenceTimer = setTimeout(send, 250);
  }
}

// ── Watch Party / Sesión Compartida ──────────────────────────────────────────

let _applyingPartySync = false;
let _applyingPartySyncTimer = null;
let _partyMediaLoadingLock = false;
let _partyHeartbeatTimer = null;
let _partyViewPollTimer = null;
let _partyRenderDeferred = false;
let _partySyncDebounce = null;
let _partyLastSyncedUrl = "";
let _partyLastAppliedVersion = -1;
let _partyGuestControlHoldUntil = 0;
let _partyUsersCardOpen = false;
const _socialSentInvites = new Set();
const _socialSentJoinReqs = new Set();

function _lockPartySync(ms = 1000) {
  _applyingPartySync = true;
  clearTimeout(_applyingPartySyncTimer);
  _applyingPartySyncTimer = setTimeout(() => {
    _applyingPartySync = false;
    _applyingPartySyncTimer = null;
  }, ms);
}

function _partyIsGuest() {
  return Boolean(state.party?.active && state.party.role === "guest");
}

function _partyCanPlayPause() {
  if (!_partyIsGuest()) return true;
  return Boolean(state.party.canSelfPlayPause);
}

function _partyCanSeek() {
  if (!_partyIsGuest()) return true;
  return Boolean(state.party.canSelfSeek);
}

function _partyCanRemoveQueue() {
  if (!state.party?.active) return false;
  if (state.party.role === "host") return true;
  return Boolean(state.party.canSelfRemoveQueue);
}

function _partyExpectedPlaybackPos() {
  const pb = state.party?.playback || {};
  const elapsedSecs = pb.isPlaying && pb.updatedAtMs
    ? Math.max(0, (Date.now() - Number(pb.updatedAtMs)) / 1000) * Number(pb.playbackRate || 1)
    : 0;
  return Math.max(0, Number(pb.positionSecs || 0) + elapsedSecs);
}

function _applyPartyPlayerRestrictions() {
  const canPlayPause = _partyCanPlayPause();
  const canSeek = _partyCanSeek();
  const isGuest = _partyIsGuest();

  if (state.player.vjs && !state.player.vjs.isDisposed()) {
    const vjsEl = state.player.vjs.el();
    if (vjsEl) {
      vjsEl.classList.toggle("vjs-party-no-playpause", !canPlayPause);
      vjsEl.classList.toggle("vjs-party-no-seek", !canSeek);
    }
  }

  const musicPlayBtn = $("#musicPlayPause");
  if (musicPlayBtn) {
    musicPlayBtn.disabled = !canPlayPause;
    musicPlayBtn.classList.toggle("music-btn--locked", !canPlayPause);
  }
  const musicProg = $("#musicProgressWrap");
  if (musicProg) {
    musicProg.classList.toggle("music-track--locked", !canSeek);
  }

  const prevBtn = $("#watchPrev");
  if (prevBtn) prevBtn.disabled = isGuest || state.player.index <= 0;
  const nextBtn = $("#watchNext");
  if (nextBtn) nextBtn.disabled = isGuest || state.player.index >= state.player.queue.length - 1;
  const shuffleBtn = $("#ctrlShuffle");
  if (shuffleBtn) shuffleBtn.disabled = isGuest;
  const repeatBtn = $("#ctrlRepeat");
  if (repeatBtn) repeatBtn.disabled = isGuest;
  const autoplayBtn = $("#ctrlAutoplay");
  if (autoplayBtn) autoplayBtn.disabled = isGuest;
}

function _socialPendingCount() {
  const s = state.social || {};
  return (
    (s.incomingFriendRequests?.length || 0) +
    (s.incomingRoomInvites?.length || 0) +
    (s.incomingJoinRequests?.length || 0)
  );
}

function _partyViewHasFocusedInput() {
  const root = $("#view-party");
  const active = document.activeElement;
  if (!root || !active || !root.contains(active)) return false;
  const tag = String(active.tagName || "").toUpperCase();
  return tag === "INPUT" || tag === "TEXTAREA" || tag === "SELECT";
}

function _requestPartyRender() {
  if (state.view !== "party") return;
  if (_partyViewHasFocusedInput()) {
    _partyRenderDeferred = true;
    return;
  }
  _partyRenderDeferred = false;
  renderParty();
}

function _onSocialStateUpdate(snapshot) {
  if (!snapshot) return;
  state.social = snapshot;
  if (Array.isArray(snapshot.friends)) {
    state.friends = snapshot.friends;
  }
  _updatePartyBadge();
  _requestPartyRender();
}

async function _refreshSocialStatus() {
  if (!IS_TAURI) return;
  try {
    const snap = await api.get("/api/social/status");
    _onSocialStateUpdate(snap);
  } catch (_) {}
}

async function _syncPartyViewNow() {
  if (!IS_TAURI) return;
  try {
    const [socialSnap, partySnap] = await Promise.all([
      api.post("/api/social/sync", {}).catch(() => null),
      api.get("/api/party/status").catch(() => null),
    ]);
    if (socialSnap) _onSocialStateUpdate(socialSnap);
    if (partySnap) _onPartyStateUpdate(partySnap, { skipAutoLoad: true });
  } catch (_) {}
}

function _ensurePartyViewPolling() {
  if (state.view === "party") {
    if (!_partyViewPollTimer) {
      _partyViewPollTimer = setInterval(() => {
        if (state.view !== "party") {
          clearInterval(_partyViewPollTimer);
          _partyViewPollTimer = null;
          return;
        }
        _syncPartyViewNow();
      }, 5000);
    }
  } else if (_partyViewPollTimer) {
    clearInterval(_partyViewPollTimer);
    _partyViewPollTimer = null;
  }
}

document.addEventListener("focusout", (ev) => {
  if (!_partyRenderDeferred || state.view !== "party") return;
  const root = $("#view-party");
  if (!root || !root.contains(ev.target)) return;
  setTimeout(() => {
    if (_partyRenderDeferred && state.view === "party" && !_partyViewHasFocusedInput()) {
      _partyRenderDeferred = false;
      renderParty();
    }
  }, 50);
});

window.addEventListener("beforeunload", () => {
  if (state.party?.active) {
    try { api.post("/api/party/leave", {}); } catch (_) {}
  }
});

function _updatePartyBadge() {
  const badge = $("#partyBadge");
  if (!badge) return;
  const pending = _socialPendingCount();
  if (state.party?.active) {
    badge.textContent = pending > 0
      ? `${state.party.participants?.length || 1}·${pending}`
      : String(state.party.participants?.length || 1);
  } else if (pending > 0) {
    badge.textContent = String(pending);
  } else {
    badge.textContent = "";
  }
}

function _partyUsersPopoverHtml(p) {
  if (!p?.active || p.role !== "host" || !_partyUsersCardOpen) return "";
  const guests = (p.participants || []).filter((u) => !u.isHost);
  return `
    <div class="party-users-popover" id="partyUsersPopover">
      <div class="party-users-popover-hdr">
        <span class="party-users-popover-title">Gestionar usuarios (${guests.length})</span>
        <button class="icon-btn" type="button" data-party-action="close-users-card" title="Cerrar">×</button>
      </div>
      ${guests.length ? `
        <div class="party-users-popover-list">
          ${guests.map((u) => `
            <div class="party-user-perm-card">
              <div class="party-user-perm-name">
                <span>${escapeHtml(u.displayName || u.userId)}</span>
                <span class="party-pill">${escapeHtml(u.userId)}</span>
              </div>
              <div class="party-user-perm-toggles">
                <label class="party-perm-toggle">
                  <span>Adelantar / retroceder video</span>
                  <input type="checkbox" data-party-perm-user="${escapeHtml(u.userId)}" data-party-perm-key="canSeek" ${u.canSeek ? "checked" : ""}>
                </label>
                <label class="party-perm-toggle">
                  <span>Pausar / reproducir</span>
                  <input type="checkbox" data-party-perm-user="${escapeHtml(u.userId)}" data-party-perm-key="canPlayPause" ${u.canPlayPause ? "checked" : ""}>
                </label>
                <label class="party-perm-toggle">
                  <span>Quitar videos de la cola</span>
                  <input type="checkbox" data-party-perm-user="${escapeHtml(u.userId)}" data-party-perm-key="canRemoveQueue" ${u.canRemoveQueue ? "checked" : ""}>
                </label>
              </div>
            </div>
          `).join("")}
        </div>
      ` : `<div class="party-users-popover-empty">No hay usuarios conectados todavía.</div>`}
    </div>
  `;
}

function _updateWatchPartyBar() {
  const bar = $("#watch-party-bar");
  if (!bar) return;
  const p = state.party;
  if (!p?.active) {
    _partyUsersCardOpen = false;
    bar.innerHTML = "";
    return;
  }
  const roleLabel = p.role === "host" ? "Host" : `Invitado · Host: ${p.hostName || "Host"}`;
  let permLabel = "Solo lectura";
  if (p.role === "host") {
    permLabel = "Control total";
  } else {
    const granted = [];
    if (p.canSelfQueue) granted.push("agregar a cola");
    if (p.canSelfPlayPause) granted.push("pausar/reproducir");
    if (p.canSelfSeek) granted.push("adelantar/retroceder");
    if (p.canSelfRemoveQueue) granted.push("quitar de cola");
    permLabel = granted.length ? `Permisos: ${granted.join(", ")}` : "Solo ver";
  }
  const discordReqs = (state.discord?.pendingJoinRequests || []);
  const discordReqBarHtml = discordReqs.length ? `
    <div class="watch-party-bar" style="margin-top:8px;border-color:var(--accent-glow)">
      <div class="party-row">
        <span class="party-pill party-pill--warn">Discord</span>
        <span><strong>${escapeHtml(discordReqs[0].globalName || discordReqs[0].username)}</strong> pide unirse a tu sala${discordReqs.length > 1 ? ` (+${discordReqs.length - 1} más)` : ""}</span>
      </div>
      <div class="party-row">
        <button class="btn btn-primary" type="button" data-discord-join-respond="${escapeHtml(discordReqs[0].userId)}" data-accept="1">Aceptar</button>
        <button class="btn btn-ghost" type="button" data-discord-join-respond="${escapeHtml(discordReqs[0].userId)}" data-accept="0">Rechazar</button>
      </div>
    </div>
  ` : "";
  bar.innerHTML = `
    <div class="watch-party-bar">
      <div class="party-row">
        <span class="party-pill party-pill--live">En sesión · Sala ${escapeHtml(p.roomCode)}</span>
        <span>${escapeHtml(roleLabel)} · ${p.participants?.length || 1} conectados · ${escapeHtml(permLabel)}</span>
      </div>
      <div class="party-row">
        ${p.role === "guest" ? `<button class="btn btn-ghost" type="button" data-party-action="resync">Resincronizar</button>` : ""}
        ${p.role === "host" ? `<button class="btn btn-ghost ${_partyUsersCardOpen ? "active" : ""}" type="button" data-party-action="toggle-users-card">Gestionar usuarios</button>` : ""}
        <button class="btn btn-ghost" type="button" data-party-action="open-view">Gestionar sesión</button>
      </div>
    </div>
    ${discordReqBarHtml}
    ${_partyUsersPopoverHtml(p)}
  `;
}

function _getCurrentMediaState() {
  const isAudio = state.player.sourceContext === "music" || state.player.audioOnly;
  if (isAudio) {
    if (state.player.audioVjs && !state.player.audioVjs.isDisposed()) {
      return {
        isPlaying: !state.player.audioVjs.paused(),
        positionSecs: Number(state.player.audioVjs.currentTime() || 0),
        playbackRate: Number(state.player.audioVjs.playbackRate() || 1),
      };
    }
    const audio = _getAudioEl();
    if (audio) {
      return {
        isPlaying: !audio.paused,
        positionSecs: Number(audio.currentTime || 0),
        playbackRate: Number(audio.playbackRate || 1),
      };
    }
  } else if (state.player.vjs && !state.player.vjs.isDisposed()) {
    return {
      isPlaying: !state.player.vjs.paused(),
      positionSecs: Number(state.player.vjs.currentTime() || 0),
      playbackRate: Number(state.player.vjs.playbackRate() || 1),
    };
  }
  return { isPlaying: false, positionSecs: 0, playbackRate: 1 };
}

function _applyMediaSync(targetPlaying, targetPosSecs, targetRate = 1) {
  _lockPartySync(1000);
  try {
    const isAudio = state.player.sourceContext === "music" || state.player.audioOnly;
    if (isAudio) {
      const vjs = state.player.audioVjs && !state.player.audioVjs.isDisposed() ? state.player.audioVjs : null;
      const audio = !vjs ? _getAudioEl() : null;
      if (vjs) {
        const cur = Number(vjs.currentTime() || 0);
        if (Math.abs(cur - targetPosSecs) > 1.8) {
          _lockPartySync(1500);
          vjs.one("seeked", () => _lockPartySync(450));
          vjs.currentTime(targetPosSecs);
        }
        if (targetPlaying && vjs.paused()) vjs.play().catch(() => {});
        else if (!targetPlaying && !vjs.paused()) vjs.pause();
      } else if (audio) {
        const cur = Number(audio.currentTime || 0);
        if (Math.abs(cur - targetPosSecs) > 1.8) {
          _lockPartySync(1500);
          audio.addEventListener("seeked", () => _lockPartySync(450), { once: true });
          audio.currentTime = targetPosSecs;
        }
        if (targetPlaying && audio.paused) audio.play().catch(() => {});
        else if (!targetPlaying && !audio.paused) audio.pause();
      }
    } else if (state.player.vjs && !state.player.vjs.isDisposed()) {
      const vjs = state.player.vjs;
      const cur = Number(vjs.currentTime() || 0);
      if (Math.abs(cur - targetPosSecs) > 1.8) {
        _lockPartySync(1500);
        vjs.one("seeked", () => _lockPartySync(450));
        vjs.currentTime(targetPosSecs);
      }
      if (targetRate && Math.abs((vjs.playbackRate() || 1) - targetRate) > 0.05) {
        vjs.playbackRate(targetRate);
      }
      if (targetPlaying && vjs.paused()) vjs.play().catch(() => {});
      else if (!targetPlaying && !vjs.paused()) vjs.pause();
    }
  } catch (_) {}
}

async function _pushGuestPlaybackSync(update) {
  if (!_partyIsGuest()) return;
  try {
    await api.post("/api/party/playback", update);
  } catch (err) {
    if (err?.message) toast(err.message);
  }
}

function _onLocalMediaEvent(kind = "sync") {
  _syncDiscordMediaPresence(kind === "play" || kind === "pause" || kind === "seeked");
  if (_applyingPartySync || _partyMediaLoadingLock || state.player.loading) return;
  if (!state.party?.active) return;

  if (state.party.role === "host") {
    clearTimeout(_partySyncDebounce);
    _partySyncDebounce = setTimeout(() => {
      if (_applyingPartySync || _partyMediaLoadingLock || state.player.loading) return;
      _pushHostPlaybackSync();
    }, 150);
    return;
  }

  if (state.party.role === "guest") {
    const pb = state.party.playback || {};
    const expectedPos = _partyExpectedPlaybackPos();
    const expectedRate = Number(pb.playbackRate || 1);

    if (kind === "play" || kind === "pause") {
      if (!_partyCanPlayPause()) {
        _applyMediaSync(Boolean(pb.isPlaying), expectedPos, expectedRate);
        return;
      }
      clearTimeout(_partySyncDebounce);
      _partySyncDebounce = setTimeout(() => {
        if (_applyingPartySync || _partyMediaLoadingLock || state.player.loading) return;
        const media = _getCurrentMediaState();
        const serverPlaying = Boolean(state.party?.playback?.isPlaying);
        if (media.isPlaying === serverPlaying) return;
        _partyGuestControlHoldUntil = Date.now() + 2400;
        _pushGuestPlaybackSync({ isPlaying: media.isPlaying });
      }, 120);
      return;
    }

    if (kind === "seeked") {
      if (!_partyCanSeek()) {
        _applyMediaSync(Boolean(pb.isPlaying), expectedPos, expectedRate);
        return;
      }
      clearTimeout(_partySyncDebounce);
      _partySyncDebounce = setTimeout(() => {
        if (_applyingPartySync || _partyMediaLoadingLock || state.player.loading) return;
        const media = _getCurrentMediaState();
        if (Math.abs(media.positionSecs - _partyExpectedPlaybackPos()) < 1.8) return;
        _partyGuestControlHoldUntil = Date.now() + 2400;
        _pushGuestPlaybackSync({ positionSecs: media.positionSecs });
      }, 160);
      return;
    }

    if (kind === "ratechange") {
      _applyMediaSync(Boolean(pb.isPlaying), expectedPos, expectedRate);
    }
  }
}

async function _pushHostPlaybackSync(override = {}) {
  if (!state.party?.active || state.party.role !== "host") return;
  const media = _getCurrentMediaState();
  try {
    await api.post("/api/party/playback", {
      currentIndex: override.currentIndex !== undefined ? override.currentIndex : state.player.index,
      isPlaying: override.isPlaying !== undefined ? override.isPlaying : media.isPlaying,
      positionSecs: override.positionSecs !== undefined ? override.positionSecs : media.positionSecs,
      playbackRate: override.playbackRate !== undefined ? override.playbackRate : media.playbackRate,
      repeat: state.player.repeat || "none",
      shuffle: Boolean(state.player.shuffle),
    });
  } catch (_) {}
}

function _ensurePartyHeartbeat() {
  if (state.party?.active && state.party.role === "host") {
    if (!_partyHeartbeatTimer) {
      _partyHeartbeatTimer = setInterval(() => {
        if (!state.party?.active || state.party.role !== "host") {
          clearInterval(_partyHeartbeatTimer);
          _partyHeartbeatTimer = null;
          return;
        }
        if (_applyingPartySync || _partyMediaLoadingLock || state.player.loading) return;
        const media = _getCurrentMediaState();
        if (media.isPlaying && state.player.current) {
          _pushHostPlaybackSync();
        }
      }, 4000);
    }
  } else if (_partyHeartbeatTimer) {
    clearInterval(_partyHeartbeatTimer);
    _partyHeartbeatTimer = null;
  }
}

function _onPartyStateUpdate(snapshot, opts = {}) {
  if (!snapshot) return;
  const wasActive = Boolean(state.party?.active);
  const prevRoomCode = state.party?.roomCode || "";
  state.party = snapshot;
  _updatePartyBadge();
  _ensurePartyHeartbeat();
  _updateWatchPartyBar();
  _updateWatchUsers();
  _applyPartyPlayerRestrictions();
  _requestPartyRender();

  if (!snapshot.active) {
    _partyLastSyncedUrl = "";
    _partyLastAppliedVersion = -1;
    _partyGuestControlHoldUntil = 0;
    _partyMediaLoadingLock = false;
    _partyUsersCardOpen = false;
    _socialSentInvites.clear();
    _applyPartyPlayerRestrictions();
    return;
  }
  if (prevRoomCode && prevRoomCode !== snapshot.roomCode) {
    _partyLastAppliedVersion = -1;
    _partyGuestControlHoldUntil = 0;
    _socialSentInvites.clear();
  }

  // Sync shared queue into player state
  const mappedQueue = (snapshot.queue || []).map((q) => ({
    id: q.id,
    url: q.url,
    title: q.title,
    thumbnail: q.thumbnail || "",
    channel: q.channel || "",
    durationText: q.durationText || "",
    sourceContext: q.sourceContext || "search",
    addedBy: q.addedBy || null,
  }));
  state.player.queue = mappedQueue;
  _updateWatchQueue();

  if (opts.skipAutoLoad) return;

  const pb = snapshot.playback || {};
  const expectedIdx = pb.currentIndex;
  if (expectedIdx === null || expectedIdx === undefined || !mappedQueue[expectedIdx]) return;
  const targetItem = mappedQueue[expectedIdx];
  const elapsedSecs = pb.isPlaying && pb.updatedAtMs
    ? Math.max(0, (Date.now() - Number(pb.updatedAtMs)) / 1000) * Number(pb.playbackRate || 1)
    : 0;
  const targetPos = Math.max(0, Number(pb.positionSecs || 0) + elapsedSecs);
  const pbVersion = Number(pb.version || 0);

  const isGuest = snapshot.role === "guest";
  const hostNeedsFirstLoad = snapshot.role === "host" && !state.player.current;
  const hostRemoteControl = snapshot.role === "host" && pb.updatedBy && pb.updatedBy !== snapshot.selfUserId;

  if (snapshot.role === "host" && state.player.current?.url === targetItem.url && state.player.index === expectedIdx) {
    _partyLastSyncedUrl = targetItem.url;
  }

  if (isGuest || hostNeedsFirstLoad || hostRemoteControl) {
    const currentUrl = state.player.current?.url || "";
    const needsTrackLoad = currentUrl !== targetItem.url || state.player.index !== expectedIdx || (isGuest && _partyLastSyncedUrl !== targetItem.url);
    if (needsTrackLoad) {
      _partyLastSyncedUrl = targetItem.url;
      _partyLastAppliedVersion = pbVersion;
      state.player.sourceContext = targetItem.sourceContext || "search";
      loadWebPlayerItem(expectedIdx, {
        fromPartySync: true,
        targetPos,
        targetPlaying: Boolean(pb.isPlaying),
        targetRate: Number(pb.playbackRate || 1),
      });
    } else if (pbVersion !== _partyLastAppliedVersion) {
      if (isGuest && Date.now() < _partyGuestControlHoldUntil && pb.updatedBy !== snapshot.selfUserId) {
        return;
      }
      _partyLastAppliedVersion = pbVersion;
      _applyMediaSync(Boolean(pb.isPlaying), targetPos, Number(pb.playbackRate || 1));
    }
  }
}

async function _refreshPartyStatus() {
  if (!IS_TAURI) return;
  try {
    const snap = await api.get("/api/party/status");
    _onPartyStateUpdate(snap, { skipAutoLoad: true });
  } catch (_) {}
}

function renderParty() {
  const root = $("#view-party");
  if (!root) return;
  const prof = state.userProfile || {};
  const social = state.social || {};
  const friends = (social.friends && social.friends.length) ? social.friends : (state.friends || []);
  const p = state.party || {};
  const isHost = p.active && p.role === "host";
  const isGuest = p.active && p.role === "guest";
  const friendIds = new Set(friends.map((f) => String(f.userId || "").toUpperCase()));
  const allowedSet = new Set(p.allowedQueueUserIds || []);
  const hubConnected = Boolean(social.connected);
  const hubConfigured = Boolean(social.configured || social.hubUrl);

  const policyLabels = {
    host_only: "Solo el host",
    all: "Todos",
    selected: "Solo seleccionados",
  };
  const accessLabels = {
    request: "Pedir permiso",
    open: "Abierta a amigos",
    invite_only: "Solo invitación",
  };

  const incomingFriendReqs = social.incomingFriendRequests || [];
  const outgoingFriendReqs = social.outgoingFriendRequests || [];
  const incomingInvites = social.incomingRoomInvites || [];
  const incomingJoinReqs = isHost ? (social.incomingJoinRequests || []) : [];
  const discordJoinReqs = p.active ? (state.discord?.pendingJoinRequests || []) : [];
  const totalPending = incomingFriendReqs.length + incomingInvites.length + incomingJoinReqs.length + discordJoinReqs.length;
  const roomParticipantIds = new Set((p.participants || []).map((u) => String(u.userId || "").toUpperCase()));
  const myRoomCodeUpper = String(p.roomCode || "").toUpperCase();
  const activeFriendRooms = friends.filter((f) => {
    if (!f.room || !f.room.roomCode) return false;
    const uidUpper = String(f.userId || "").toUpperCase();
    const fRoomUpper = String(f.room.roomCode || "").toUpperCase();
    if (p.active && roomParticipantIds.has(uidUpper) && fRoomUpper !== myRoomCodeUpper) {
      return false;
    }
    return true;
  });

  const activeFriendSessionsHtml = (activeFriendRooms.length > 0 || (hubConnected && friends.length > 0 && !p.active)) ? `
    <div class="party-card party-card--wide" style="margin-bottom:16px">
      <div class="party-card-hdr">
        <div class="party-row">
          <div class="party-card-title">Sesiones activas de amigos (${activeFriendRooms.length})</div>
          ${activeFriendRooms.length ? `<span class="party-pill party-pill--live">En vivo</span>` : ""}
        </div>
      </div>
      ${activeFriendRooms.length ? `
        <div class="party-list">
          ${activeFriendRooms.map((f) => {
            const room = f.room;
            const uidUpper = String(f.userId || "").toUpperCase();
            const rCodeUpper = String(room.roomCode || "").toUpperCase();
            const inSameRoom = Boolean(p.active && String(p.roomCode || "").toUpperCase() === rCodeUpper);
            const reqKey = `${rCodeUpper}:${uidUpper}`;
            const reqSent = _socialSentJoinReqs.has(reqKey);
            const accessLabel = accessLabels[room.roomAccess] || accessLabels.request;
            let actionHtml = "";
            if (inSameRoom) {
              actionHtml = `<span class="party-pill party-pill--ok">Ya estás aquí</span>`;
            } else if (p.active) {
              actionHtml = `<span class="party-session-status" title="Cierra tu sesión actual si quieres unirte a esta sala.">En una sesión</span>`;
            } else if (room.roomAccess === "invite_only") {
              actionHtml = `<span class="party-pill">Solo invitación</span>`;
            } else if (room.roomAccess === "open") {
              actionHtml = `<button class="btn btn-primary" type="button" data-social-join-friend="${escapeHtml(f.userId)}" data-social-join-room="${escapeHtml(room.roomCode)}" data-social-join-access="open">Unirse</button>`;
            } else if (reqSent) {
              actionHtml = `<button class="btn btn-ghost" type="button" disabled>Solicitud enviada</button>`;
            } else {
              actionHtml = `<button class="btn btn-primary" type="button" data-social-join-friend="${escapeHtml(f.userId)}" data-social-join-room="${escapeHtml(room.roomCode)}" data-social-join-access="request">Pedir unirse</button>`;
            }
            return `
              <div class="party-person">
                <div class="party-person-info">
                  <div class="party-person-name">
                    <span class="party-dot party-dot--on" title="En línea"></span>
                    <span>${escapeHtml(f.displayName)}</span>
                    <span class="party-pill party-pill--live">Sala ${escapeHtml(room.roomCode)} · ${room.participantsCount || 1}</span>
                    <span class="party-pill">${escapeHtml(accessLabel)}</span>
                  </div>
                  <div class="party-person-meta">
                    ${room.currentTitle ? `Reproduciendo: ${escapeHtml(room.currentTitle)}` : `ID: ${escapeHtml(f.userId)}`}
                  </div>
                </div>
                <div class="party-row">
                  ${actionHtml}
                </div>
              </div>
            `;
          }).join("")}
        </div>
      ` : `<div class="muted" style="font-size:.84rem">Ninguno de tus amigos tiene una sesión activa en este momento.</div>`}
    </div>
  ` : "";

  const notificationsHtml = totalPending > 0 ? `
    <div class="party-card party-card--wide" style="margin-bottom:16px;border-color:var(--accent-glow)">
      <div class="party-card-hdr">
        <div class="party-card-title">Notificaciones y solicitudes (${totalPending})</div>
      </div>
      <div class="party-list">
        ${incomingInvites.map((inv) => `
          <div class="party-person">
            <div class="party-person-info">
              <div class="party-person-name">
                <span class="party-pill party-pill--live">Invitación a sala</span>
                <span><strong>${escapeHtml(inv.fromDisplayName)}</strong> te invitó a la sala <strong>${escapeHtml(inv.roomCode)}</strong></span>
              </div>
              <div class="party-person-meta">${escapeHtml(inv.fromUserId)}</div>
            </div>
            <div class="party-row">
              <button class="btn btn-primary" type="button" data-social-accept-invite="${escapeHtml(inv.id)}" data-social-invite-url="${escapeHtml(inv.inviteUrl)}" data-social-invite-room="${escapeHtml(inv.roomCode)}">Unirse</button>
              <button class="btn btn-ghost" type="button" data-social-dismiss-invite="${escapeHtml(inv.id)}">Ignorar</button>
            </div>
          </div>
        `).join("")}
        ${incomingJoinReqs.map((jr) => `
          <div class="party-person">
            <div class="party-person-info">
              <div class="party-person-name">
                <span class="party-pill party-pill--warn">Solicitud de sala</span>
                <span><strong>${escapeHtml(jr.fromDisplayName)}</strong> pide unirse a tu sala <strong>${escapeHtml(jr.roomCode)}</strong></span>
              </div>
              <div class="party-person-meta">${escapeHtml(jr.fromUserId)}</div>
            </div>
            <div class="party-row">
              <button class="btn btn-primary" type="button" data-social-join-respond="${escapeHtml(jr.id)}" data-accept="1">Aceptar</button>
              <button class="btn btn-ghost" type="button" data-social-join-respond="${escapeHtml(jr.id)}" data-accept="0">Rechazar</button>
            </div>
          </div>
        `).join("")}
        ${discordJoinReqs.map((dr) => `
          <div class="party-person">
            <div class="party-person-info">
              <div class="party-person-name">
                <span class="party-pill party-pill--warn">Solicitud desde Discord</span>
                <span><strong>${escapeHtml(dr.globalName || dr.username)}</strong> pide unirse a tu sala</span>
              </div>
              <div class="party-person-meta">@${escapeHtml(dr.username)} · Discord</div>
            </div>
            <div class="party-row">
              <button class="btn btn-primary" type="button" data-discord-join-respond="${escapeHtml(dr.userId)}" data-accept="1">Aceptar</button>
              <button class="btn btn-ghost" type="button" data-discord-join-respond="${escapeHtml(dr.userId)}" data-accept="0">Rechazar</button>
            </div>
          </div>
        `).join("")}
        ${incomingFriendReqs.map((fr) => `
          <div class="party-person">
            <div class="party-person-info">
              <div class="party-person-name">
                <span class="party-pill party-pill--ok">Solicitud de amistad</span>
                <span><strong>${escapeHtml(fr.fromDisplayName)}</strong> quiere agregarte como amigo</span>
                ${fr.fromGoogleAccountName ? `<span class="party-pill">Google: ${escapeHtml(fr.fromGoogleAccountName)}</span>` : ""}
              </div>
              <div class="party-person-meta">${escapeHtml(fr.fromUserId)}</div>
            </div>
            <div class="party-row">
              <button class="btn btn-primary" type="button" data-social-friend-respond="${escapeHtml(fr.id)}" data-accept="1">Aceptar</button>
              <button class="btn btn-ghost" type="button" data-social-friend-respond="${escapeHtml(fr.id)}" data-accept="0">Rechazar</button>
            </div>
          </div>
        `).join("")}
      </div>
    </div>
  ` : "";

  let sessionCardHtml = "";
  if (!p.active) {
    sessionCardHtml = `
      <div class="party-grid">
        <div class="party-card">
          <div class="party-card-hdr">
            <div class="party-card-title">Crear una sala</div>
            <span class="party-pill ${hubConnected ? "party-pill--ok" : ""}">${hubConnected ? "Online listo" : "Automático"}</span>
          </div>
          <p class="muted" style="font-size:.84rem">Crea una sala en un clic para ver videos o escuchar música sincronizados con tus amigos.</p>
          <div class="party-form-grid">
            <label class="party-field">¿Quién puede entrar?
              <select class="form-input" id="partyRoomAccess">
                <option value="request" selected>Pedir permiso para unirse</option>
                <option value="open">Abierta para amigos</option>
                <option value="invite_only">Solo por invitación</option>
              </select>
            </label>
            <label class="party-field">¿Quién puede agregar a la cola?
              <select class="form-input" id="partyHostPolicy">
                <option value="all" selected>Todos en la sala</option>
                <option value="host_only">Solo yo</option>
                <option value="selected">Solo usuarios seleccionados</option>
              </select>
            </label>
          </div>
          <div class="party-row">
            <button class="btn btn-primary" type="button" data-party-action="start-host">Crear sala</button>
          </div>
        </div>

        <div class="party-card">
          <div class="party-card-hdr">
            <div class="party-card-title">Unirse a una sala</div>
            <span class="party-pill">Invitado</span>
          </div>
          <p class="muted" style="font-size:.84rem">Escribe el código de sala que te compartió tu amigo, o únete directo desde tu lista de amigos.</p>
          <label class="party-field">Código de sala
            <input class="form-input" id="partyJoinInvite" placeholder="ej. 4ENUAH">
          </label>
          <div class="party-row">
            <button class="btn btn-primary" type="button" data-party-action="join-session">Unirse</button>
          </div>
        </div>
      </div>
    `;
  } else if (isHost) {
    const selectableMap = new Map();
    for (const part of (p.participants || [])) {
      if (!part.isHost) {
        selectableMap.set(part.userId, {
          userId: part.userId,
          displayName: part.displayName,
          googleAccountName: part.googleAccountName,
          online: true,
        });
      }
    }
    for (const f of friends) {
      if (!selectableMap.has(f.userId)) {
        selectableMap.set(f.userId, {
          userId: f.userId,
          displayName: f.displayName,
          googleAccountName: f.googleAccountName,
          online: Boolean(f.online),
        });
      }
    }
    const selectableUsers = Array.from(selectableMap.values());
    const hasPublicUrl = Boolean(p.publicWsUrl);

    sessionCardHtml = `
      <div class="party-card party-card--wide">
        <div class="party-card-hdr">
          <div class="party-row">
            <div class="party-card-title">Tu sala está activa</div>
            <span class="party-pill party-pill--live">Sala ${escapeHtml(p.roomCode)}</span>
            <span class="party-pill party-pill--ok">${p.participants?.length || 1} conectados</span>
            ${p.relayConnected || p.tunnelRunning || hasPublicUrl
              ? `<span class="party-pill party-pill--ok">Online</span>`
              : `<span class="party-pill party-pill--warn">Preparando conexión online...</span>`}
            ${state.discord?.connected && state.discord?.inviteActive
              ? `<span class="party-pill party-pill--ok" title="Tus amigos pueden pedir unirse desde tu perfil de Discord o puedes invitarlos con el botón + en Discord.">Discord activo</span>`
              : ""}
          </div>
          <div class="party-row">
            <button class="btn btn-party-watch" type="button" data-party-action="go-watch">Ir al reproductor</button>
            <button class="btn btn-danger" type="button" data-party-action="leave">Cerrar sala</button>
          </div>
        </div>

        <div class="party-field">${hasPublicUrl || p.relayConnected ? "Código de sala para compartir (o invita directo a tus amigos abajo)" : "Preparando conexión online..."}
          <div class="party-code-box">
            <code id="partyInviteText">${escapeHtml(p.roomCode)}</code>
            <button class="btn btn-primary" type="button" data-party-copy="${escapeHtml(p.roomCode)}">Copiar código</button>
          </div>
        </div>
        ${state.discord?.connected && state.discord?.inviteActive
          ? `<div class="muted" style="font-size:.82rem">Discord conectado: usa el botón <strong>+</strong> en cualquier chat de Discord para invitar a esta sala, o acepta solicitudes de <strong>Pedir unirse</strong> aquí.</div>`
          : ""}
        ${p.tunnelError ? `<div class="muted" style="color:#fbbf24;font-size:.82rem">${escapeHtml(p.tunnelError)}</div>` : ""}

        <div style="border-top:1px solid var(--br);padding-top:12px">
          <div class="party-card-title" style="font-size:.92rem;margin-bottom:8px">¿Quién puede entrar a tu sala?</div>
          <div class="party-policy-tabs">
            <button type="button" class="party-policy-btn ${(p.roomAccess || "request") === "request" ? "active" : ""}" data-party-access="request">${accessLabels.request}</button>
            <button type="button" class="party-policy-btn ${p.roomAccess === "open" ? "active" : ""}" data-party-access="open">${accessLabels.open}</button>
            <button type="button" class="party-policy-btn ${p.roomAccess === "invite_only" ? "active" : ""}" data-party-access="invite_only">${accessLabels.invite_only}</button>
          </div>
        </div>

        <div style="border-top:1px solid var(--br);padding-top:12px">
          <div class="party-card-title" style="font-size:.92rem;margin-bottom:8px">¿Quién puede agregar videos/canciones a la cola?</div>
          <div class="party-policy-tabs">
            <button type="button" class="party-policy-btn ${p.queuePolicy === "host_only" ? "active" : ""}" data-party-policy="host_only">Solo yo</button>
            <button type="button" class="party-policy-btn ${p.queuePolicy === "all" ? "active" : ""}" data-party-policy="all">Todos en la sala</button>
            <button type="button" class="party-policy-btn ${p.queuePolicy === "selected" ? "active" : ""}" data-party-policy="selected">Solo usuarios seleccionados</button>
          </div>
          ${p.queuePolicy === "selected" ? `
            <div class="party-list" style="margin-top:10px">
              ${selectableUsers.length ? selectableUsers.map((u) => `
                <label class="party-person" style="cursor:pointer">
                  <div class="party-person-info">
                    <div class="party-person-name">
                      <input type="checkbox" data-party-allow-uid="${escapeHtml(u.userId)}" ${allowedSet.has(u.userId) ? "checked" : ""}>
                      <span>${escapeHtml(u.displayName)}</span>
                      ${u.online ? `<span class="party-pill party-pill--ok">En sala</span>` : `<span class="party-pill">Amigo</span>`}
                      ${u.googleAccountName ? `<span class="party-pill">Google: ${escapeHtml(u.googleAccountName)}</span>` : ""}
                    </div>
                    <div class="party-person-meta">${escapeHtml(u.userId)}</div>
                  </div>
                </label>
              `).join("") : `<div class="muted" style="font-size:.83rem">Aún no hay invitados conectados ni amigos para seleccionar.</div>`}
            </div>
          ` : ""}
        </div>
      </div>
    `;
  } else if (isGuest) {
    sessionCardHtml = `
      <div class="party-card party-card--wide">
        <div class="party-card-hdr">
          <div class="party-row">
            <div class="party-card-title">En la sala de ${escapeHtml(p.hostName || "Host")}</div>
            <span class="party-pill party-pill--live">Sala ${escapeHtml(p.roomCode)}</span>
            <span class="party-pill ${p.connected ? "party-pill--ok" : "party-pill--warn"}">${p.connected ? "Sincronizado" : "Desconectado"}</span>
            <span class="party-pill ${p.canSelfQueue ? "party-pill--ok" : ""}">${p.canSelfQueue ? "Puedes agregar a la cola" : "Solo ver"}</span>
          </div>
          <div class="party-row">
            <button class="btn btn-party-watch" type="button" data-party-action="go-watch">Ir al reproductor</button>
            <button class="btn btn-danger" type="button" data-party-action="leave">Salir</button>
          </div>
        </div>
        <div class="muted" style="font-size:.84rem">Cola: <strong>${p.queue?.length || 0}</strong> elementos · Permisos: <strong>${escapeHtml(policyLabels[p.queuePolicy] || p.queuePolicy)}</strong>.</div>
        ${p.lastError ? `<div class="muted" style="color:#fbbf24;font-size:.82rem">${escapeHtml(p.lastError)}</div>` : ""}
      </div>
    `;
  }

  const participantsHtml = p.active ? `
    <div class="party-card">
      <div class="party-card-hdr">
        <div class="party-card-title">En esta sala (${p.participants?.length || 0})</div>
      </div>
      <div class="party-list">
        ${(p.participants || []).map((part) => {
          const isSelf = part.userId === prof.userId || part.userId === p.selfUserId;
          const isFriend = friendIds.has(String(part.userId || "").toUpperCase());
          return `
            <div class="party-person">
              <div class="party-person-info">
                <div class="party-person-name">
                  <span>${escapeHtml(part.displayName)}</span>
                  ${part.isHost ? `<span class="party-pill party-pill--live">Host</span>` : ""}
                  ${isSelf ? `<span class="party-pill">Tú</span>` : ""}
                  ${part.googleAccountName ? `<span class="party-pill">Google: ${escapeHtml(part.googleAccountName)}</span>` : ""}
                  <span class="party-pill ${part.canQueue ? "party-pill--ok" : ""}">${part.canQueue ? "Cola OK" : "Solo ver"}</span>
                </div>
                <div class="party-person-meta">${escapeHtml(part.userId)}</div>
              </div>
              <div class="party-row">
                ${!isSelf && !isFriend ? `<button class="btn btn-ghost" type="button" data-party-save-friend="${escapeHtml(part.userId)}" data-party-save-name="${escapeHtml(part.displayName)}" data-party-save-google="${escapeHtml(part.googleAccountName || "")}">+ Agregar amigo</button>` : ""}
                ${isHost && !part.isHost ? `<button class="btn btn-ghost" type="button" data-party-kick="${escapeHtml(part.userId)}">Expulsar</button>` : ""}
              </div>
            </div>
          `;
        }).join("")}
      </div>
    </div>
  ` : "";

  root.innerHTML = `
    <div class="section-head">
      <div>
        <h2>Sesión compartida</h2>
        <p class="muted">Escucha música o mira videos con tus amigos en tiempo real.</p>
      </div>
    </div>

    <div class="party-card party-card--wide" style="margin-bottom:16px">
      <div class="party-card-hdr">
        <div class="party-card-title">Tu perfil</div>
        <div class="party-row">
          ${prof.googleAccountName ? `<span class="party-pill party-pill--ok">Google: ${escapeHtml(prof.googleAccountName)}</span>` : ""}
          <span class="party-pill ${hubConnected ? "party-pill--ok" : "party-pill--warn"}">
            ${hubConnected ? "Conectado online" : hubConfigured ? "Reconectando..." : "Servicio online pendiente"}
          </span>
        </div>
      </div>
      <div class="party-form-grid">
        <div class="party-field">Tu ID de Playzi (compártelo para que te agreguen)
          <div class="party-code-box">
            <code>${escapeHtml(prof.userId || "Generando...")}</code>
            <button class="btn btn-ghost" type="button" data-party-copy="${escapeHtml(prof.userId || "")}">Copiar ID</button>
          </div>
        </div>
        <div class="party-field">Tu nombre
          <div class="party-row">
            <input class="form-input" id="partyDisplayName" style="flex:1" value="${escapeHtml(prof.displayName || "")}" placeholder="Tu nombre">
            <button class="btn btn-primary" type="button" data-party-action="save-profile">Guardar</button>
          </div>
        </div>
      </div>
    </div>

    ${notificationsHtml}

    ${activeFriendSessionsHtml}

    ${sessionCardHtml}

    <div class="party-grid">
      ${participantsHtml}
      <div class="party-card">
        <div class="party-card-hdr">
          <div class="party-card-title">Amigos (${friends.length})</div>
          <span class="party-pill ${hubConnected ? "party-pill--ok" : ""}">${hubConnected ? "Online" : "Sin conexión"}</span>
        </div>
        <div class="party-row">
          <input class="form-input" id="friendIdInput" style="flex:1" placeholder="ID de Playzi de tu amigo (ej. PZ-AB12-CD34)">
          <button class="btn btn-primary" type="button" data-party-action="send-friend-request" ${hubConnected ? "" : "disabled"}>Enviar solicitud</button>
        </div>
        ${!hubConnected ? `<div class="muted" style="font-size:.8rem">Los amigos funcionan online. ${hubConfigured ? "Reconectando con el servidor..." : "Configura la URL del servidor en Opciones avanzadas."}</div>` : ""}
        ${!hubConnected && hubConfigured && social.lastError ? `<div class="muted" style="font-size:.78rem;color:#fbbf24">${escapeHtml(social.lastError)}</div>` : ""}

        ${outgoingFriendReqs.length ? `
          <div class="party-list" style="margin-top:4px">
            <div class="muted" style="font-size:.78rem;font-weight:600">Solicitudes enviadas</div>
            ${outgoingFriendReqs.map((or) => `
              <div class="party-person">
                <div class="party-person-info">
                  <div class="party-person-name">
                    <span>${escapeHtml(or.toDisplayName || or.toUserId)}</span>
                    <span class="party-pill party-pill--warn">Pendiente</span>
                  </div>
                  <div class="party-person-meta">${escapeHtml(or.toUserId)}</div>
                </div>
                <button class="btn btn-ghost" type="button" data-social-friend-cancel="${escapeHtml(or.id)}">Cancelar</button>
              </div>
            `).join("")}
          </div>
        ` : ""}

        <div class="party-list">
          ${friends.length ? friends.map((f) => {
            const uidUpper = String(f.userId || "").toUpperCase();
            const hasRoom = Boolean(f.room && f.room.roomCode);
            const rCodeUpper = hasRoom ? String(f.room.roomCode || "").toUpperCase() : "";
            const myActive = Boolean(p.active);
            const inSameRoom = hasRoom && myActive && String(p.roomCode || "").toUpperCase() === rCodeUpper;
            const inMyRoom = myActive && (roomParticipantIds.has(uidUpper) || inSameRoom);
            const friendInOtherSession = hasRoom && !inMyRoom;
            const bothActiveSessions = myActive && friendInOtherSession;
            const canRequestJoin = !myActive && hasRoom && f.room.roomAccess !== "invite_only" && !inMyRoom;
            const reqSent = hasRoom && _socialSentJoinReqs.has(`${rCodeUpper}:${uidUpper}`);
            const canInvite = hubConnected && isHost && f.online && !inMyRoom;
            const invited = isHost && _socialSentInvites.has(`${String(p.roomCode || "").toUpperCase()}:${uidUpper}`);
            return `
            <div class="party-person">
              <span class="party-dot ${f.online ? "party-dot--on" : "party-dot--off"}" title="${f.online ? "En línea" : "Desconectado"}"></span>
              <div class="party-person-info">
                <div class="party-person-name">
                  <span>${escapeHtml(f.displayName)}</span>
                  ${inMyRoom ? `<span class="party-pill party-pill--ok">${isHost ? "En tu sala" : "En esta sala"}</span>` : ""}
                  ${hasRoom && !bothActiveSessions && !inMyRoom ? `<span class="party-pill party-pill--live">Sala ${escapeHtml(f.room.roomCode)} · ${f.room.participantsCount || 1}</span>` : ""}
                  ${f.googleAccountName ? `<span class="party-pill">Google: ${escapeHtml(f.googleAccountName)}</span>` : ""}
                </div>
                <div class="party-person-meta">
                  ${escapeHtml(f.userId)}${hasRoom && f.room.currentTitle ? ` · ${escapeHtml(f.room.currentTitle)}` : ""}
                </div>
              </div>
              <div class="party-row">
                ${bothActiveSessions
                  ? `<span class="party-session-status" title="Este amigo está en una sesión activa. Cierra tu sesión actual primero si quieres unirte.">En una sesión</span>`
                  : ""}
                ${canRequestJoin
                  ? (reqSent
                    ? `<button class="btn btn-ghost" type="button" disabled>Solicitud enviada</button>`
                    : `<button class="btn btn-primary" type="button" data-social-join-friend="${escapeHtml(f.userId)}" data-social-join-room="${escapeHtml(f.room.roomCode)}" data-social-join-access="${escapeHtml(f.room.roomAccess || "request")}">${f.room.roomAccess === "open" ? "Unirse" : "Pedir unirse"}</button>`)
                  : (!myActive && friendInOtherSession && f.room.roomAccess === "invite_only"
                    ? `<span class="party-session-status" title="Este amigo está en una sesión solo por invitación.">En una sesión</span>`
                    : "")}
                ${canInvite
                  ? `<button class="btn ${invited && !bothActiveSessions ? "btn-ghost" : "btn-primary"}" type="button" data-social-invite-friend="${escapeHtml(f.userId)}" ${bothActiveSessions ? `data-friend-in-session="1" data-friend-name="${escapeHtml(f.displayName)}"` : ""}>${invited && !bothActiveSessions ? "Reenviar" : "Invitar"}</button>`
                  : ""}
                <button class="btn btn-ghost" type="button" data-friend-toggle="${escapeHtml(f.userId)}">${f.defaultAllowQueue ? "Quitar cola" : "Permitir cola"}</button>
                <button class="btn btn-ghost" type="button" data-friend-remove="${escapeHtml(f.userId)}">Eliminar</button>
              </div>
            </div>
          `;
          }).join("") : `<div class="muted" style="font-size:.84rem">Aún no tienes amigos agregados. Pide el ID de Playzi de tu amigo y envíale una solicitud.</div>`}
        </div>
      </div>
    </div>
  `;
}

document.addEventListener("click", async (ev) => {
  const copyBtn = ev.target.closest("[data-party-copy]");
  if (copyBtn) {
    const text = copyBtn.dataset.partyCopy || "";
    if (text) {
      try {
        await navigator.clipboard.writeText(text);
        toast("Copiado al portapapeles");
      } catch (_) {
        toast("No se pudo copiar automáticamente");
      }
    }
    return;
  }

  const accessBtn = ev.target.closest("[data-party-access]");
  if (accessBtn) {
    try {
      const snap = await api.post("/api/party/room-access", {
        roomAccess: accessBtn.dataset.partyAccess,
      });
      _onPartyStateUpdate(snap, { skipAutoLoad: true });
      toast("Acceso de sala actualizado");
    } catch (err) { toast(err.message); }
    return;
  }

  const policyBtn = ev.target.closest("[data-party-policy]");
  if (policyBtn) {
    const policy = policyBtn.dataset.partyPolicy;
    try {
      const snap = await api.post("/api/party/policy", {
        queuePolicy: policy,
        allowedQueueUserIds: state.party.allowedQueueUserIds || [],
      });
      _onPartyStateUpdate(snap, { skipAutoLoad: true });
      toast("Permisos de cola actualizados");
    } catch (err) { toast(err.message); }
    return;
  }

  const acceptInviteBtn = ev.target.closest("[data-social-accept-invite]");
  if (acceptInviteBtn) {
    const inviteId = acceptInviteBtn.dataset.socialAcceptInvite;
    const inviteUrl = acceptInviteBtn.dataset.socialInviteUrl || "";
    const roomCode = acceptInviteBtn.dataset.socialInviteRoom || "";
    try {
      const socialSnap = await api.post("/api/social/room/invite/dismiss", { inviteId });
      _onSocialStateUpdate(socialSnap);
      toast(`Conectando a sala ${roomCode}...`);
      const snap = await api.post("/api/party/join", { invite: inviteUrl, roomCode: roomCode || null });
      _onPartyStateUpdate(snap);
    } catch (err) { toast(err.message); }
    return;
  }

  const dismissInviteBtn = ev.target.closest("[data-social-dismiss-invite]");
  if (dismissInviteBtn) {
    try {
      const socialSnap = await api.post("/api/social/room/invite/dismiss", {
        inviteId: dismissInviteBtn.dataset.socialDismissInvite,
      });
      _onSocialStateUpdate(socialSnap);
    } catch (err) { toast(err.message); }
    return;
  }

  const friendRespondBtn = ev.target.closest("[data-social-friend-respond]");
  if (friendRespondBtn) {
    try {
      const socialSnap = await api.post("/api/social/friends/respond", {
        requestId: friendRespondBtn.dataset.socialFriendRespond,
        accept: friendRespondBtn.dataset.accept === "1",
      });
      _onSocialStateUpdate(socialSnap);
    } catch (err) { toast(err.message); }
    return;
  }

  const friendCancelBtn = ev.target.closest("[data-social-friend-cancel]");
  if (friendCancelBtn) {
    try {
      const socialSnap = await api.post("/api/social/friends/cancel", {
        requestId: friendCancelBtn.dataset.socialFriendCancel,
      });
      _onSocialStateUpdate(socialSnap);
    } catch (err) { toast(err.message); }
    return;
  }

  const joinRespondBtn = ev.target.closest("[data-social-join-respond]");
  if (joinRespondBtn) {
    try {
      const socialSnap = await api.post("/api/social/room/join-respond", {
        requestId: joinRespondBtn.dataset.socialJoinRespond,
        accept: joinRespondBtn.dataset.accept === "1",
      });
      _onSocialStateUpdate(socialSnap);
    } catch (err) { toast(err.message); }
    return;
  }

  const discordJoinBtn = ev.target.closest("[data-discord-join-respond]");
  if (discordJoinBtn) {
    try {
      const accept = discordJoinBtn.dataset.accept === "1";
      const discordSnap = await api.post("/api/discord/join-respond", {
        userId: discordJoinBtn.dataset.discordJoinRespond,
        accept,
      });
      _onDiscordStateUpdate(discordSnap);
      toast(accept ? "Invitación enviada por Discord." : "Solicitud de Discord rechazada.");
    } catch (err) { toast(err.message); }
    return;
  }

  const discordReconnectBtn = ev.target.closest("[data-discord-reconnect]");
  if (discordReconnectBtn) {
    try {
      const discordSnap = await api.post("/api/discord/reconnect", {});
      _onDiscordStateUpdate(discordSnap);
      toast("Intentando reconectar con Discord...");
    } catch (err) { toast(err.message); }
    return;
  }

  const joinFriendBtn = ev.target.closest("[data-social-join-friend]");
  if (joinFriendBtn) {
    const uid = joinFriendBtn.dataset.socialJoinFriend || "";
    const roomCode = joinFriendBtn.dataset.socialJoinRoom || "";
    const access = joinFriendBtn.dataset.socialJoinAccess || "request";
    const reqKey = `${roomCode.toUpperCase()}:${uid.toUpperCase()}`;
    if (access !== "open") {
      _socialSentJoinReqs.add(reqKey);
      renderParty();
    }
    try {
      await api.post("/api/social/room/join-request", {
        userId: uid,
        roomCode,
      });
      toast(access === "open" ? `Uniéndote a la sala ${roomCode}...` : `Solicitud enviada para la sala ${roomCode}`);
    } catch (err) {
      _socialSentJoinReqs.delete(reqKey);
      renderParty();
      toast(err.message);
    }
    return;
  }

  const inviteFriendBtn = ev.target.closest("[data-social-invite-friend]");
  if (inviteFriendBtn) {
    const uid = inviteFriendBtn.dataset.socialInviteFriend || "";
    const roomCode = state.party?.roomCode || "";
    const targetFriend = (state.social?.friends || []).find(
      (x) => String(x.userId || "").toUpperCase() === uid.toUpperCase()
    );
    const targetRoomCode = String(targetFriend?.room?.roomCode || "").toUpperCase();
    const inOtherSession =
      inviteFriendBtn.dataset.friendInSession === "1" ||
      (Boolean(targetRoomCode) && targetRoomCode !== String(roomCode || "").toUpperCase());
    if (inOtherSession) {
      const friendName = inviteFriendBtn.dataset.friendName || targetFriend?.displayName || "Este amigo";
      toast(`${friendName} está en una sesión activa`);
      return;
    }
    const invKey = `${roomCode.toUpperCase()}:${uid.toUpperCase()}`;
    _socialSentInvites.add(invKey);
    renderParty();
    try {
      await api.post("/api/social/room/invite", {
        userId: uid,
      });
      toast("Invitación enviada");
    } catch (err) {
      _socialSentInvites.delete(invKey);
      renderParty();
      toast(err.message);
    }
    return;
  }

  const saveFriendBtn = ev.target.closest("[data-party-save-friend]");
  if (saveFriendBtn) {
    try {
      if (state.social?.connected) {
        const snap = await api.post("/api/social/friends/request", {
          userId: saveFriendBtn.dataset.partySaveFriend,
        });
        _onSocialStateUpdate(snap);
      } else {
        const resp = await api.post("/api/friends", {
          userId: saveFriendBtn.dataset.partySaveFriend,
          displayName: saveFriendBtn.dataset.partySaveName || "",
          googleAccountName: saveFriendBtn.dataset.partySaveGoogle || "",
          defaultAllowQueue: true,
        });
        state.friends = resp.items || [];
        renderParty();
        toast("Amigo guardado");
      }
    } catch (err) { toast(err.message); }
    return;
  }

  const kickBtn = ev.target.closest("[data-party-kick]");
  if (kickBtn) {
    try {
      const snap = await api.post("/api/party/kick", { userId: kickBtn.dataset.partyKick });
      _onPartyStateUpdate(snap, { skipAutoLoad: true });
      toast("Participante removido");
    } catch (err) { toast(err.message); }
    return;
  }

  const friendToggle = ev.target.closest("[data-friend-toggle]");
  if (friendToggle) {
    const uid = friendToggle.dataset.friendToggle;
    const list = (state.social?.friends?.length ? state.social.friends : state.friends) || [];
    const f = list.find((x) => x.userId === uid);
    if (!f) return;
    try {
      const resp = await api.post("/api/friends", {
        userId: f.userId,
        displayName: f.displayName,
        googleAccountName: f.googleAccountName || "",
        defaultAllowQueue: !f.defaultAllowQueue,
      });
      state.friends = resp.items || [];
      await _refreshSocialStatus();
      renderParty();
    } catch (err) { toast(err.message); }
    return;
  }

  const friendRemove = ev.target.closest("[data-friend-remove]");
  if (friendRemove) {
    try {
      const uid = friendRemove.dataset.friendRemove;
      if (state.social?.connected) {
        const snap = await api.post("/api/social/friends/remove", { userId: uid });
        _onSocialStateUpdate(snap);
      } else {
        const resp = await api.del(`/api/friends/${encodeURIComponent(uid)}`);
        state.friends = resp.items || [];
        renderParty();
        toast("Amigo eliminado");
      }
    } catch (err) { toast(err.message); }
    return;
  }

  const actionBtn = ev.target.closest("[data-party-action]");
  if (!actionBtn) {
    if (_partyUsersCardOpen && !ev.target.closest("#watch-party-bar")) {
      _partyUsersCardOpen = false;
      _updateWatchPartyBar();
    }
    return;
  }
  const action = actionBtn.dataset.partyAction;

  if (action === "toggle-users-card") {
    _partyUsersCardOpen = !_partyUsersCardOpen;
    _updateWatchPartyBar();
    return;
  }
  if (action === "close-users-card") {
    _partyUsersCardOpen = false;
    _updateWatchPartyBar();
    return;
  }
  if (action === "open-view") { _partyUsersCardOpen = false; setView("party"); return; }
  if (action === "go-watch") { setView("watch"); return; }
  if (action === "resync") {
    _partyLastSyncedUrl = "";
    _partyLastAppliedVersion = -1;
    _partyGuestControlHoldUntil = 0;
    _onPartyStateUpdate(state.party);
    toast("Sincronizando con el host...");
    return;
  }
  if (action === "save-profile") {
    const name = ($("#partyDisplayName")?.value || "").trim();
    try {
      state.userProfile = await api.post("/api/profile", { displayName: name });
      renderParty();
      toast("Perfil actualizado");
    } catch (err) { toast(err.message); }
    return;
  }
  if (action === "send-friend-request") {
    const userId = ($("#friendIdInput")?.value || "").trim();
    try {
      const snap = await api.post("/api/social/friends/request", { userId });
      _onSocialStateUpdate(snap);
      const inp = $("#friendIdInput");
      if (inp) inp.value = "";
    } catch (err) { toast(err.message); }
    return;
  }
  if (action === "add-friend") {
    const userId = ($("#friendIdInput")?.value || "").trim();
    const displayName = ($("#friendNameInput")?.value || "").trim();
    const defaultAllowQueue = Boolean($("#friendDefaultQueue")?.checked);
    try {
      const resp = await api.post("/api/friends", { userId, displayName, defaultAllowQueue });
      state.friends = resp.items || [];
      renderParty();
      toast("Amigo agregado localmente");
    } catch (err) { toast(err.message); }
    return;
  }
  if (action === "start-host") {
    const queuePolicy = $("#partyHostPolicy")?.value || "all";
    const roomAccess = $("#partyRoomAccess")?.value || "request";
    const media = _getCurrentMediaState();
    const initialQueue = (state.player.queue || []).map((q) => ({
      url: q.url,
      title: q.title || q.url,
      thumbnail: q.thumbnail || "",
      channel: q.channel || "",
      durationText: q.durationText || "",
      sourceContext: state.player.sourceContext || "search",
    }));
    try {
      const snap = await api.post("/api/party/host", {
        queuePolicy,
        roomAccess,
        useRelay: true,
        initialQueue,
        initialIndex: state.player.index || 0,
        initialPositionSecs: media.positionSecs || 0,
        initialIsPlaying: media.isPlaying || false,
      });
      _onPartyStateUpdate(snap, { skipAutoLoad: true });
      toast(`Sala creada · ${snap.roomCode}`);
    } catch (err) { toast(err.message); }
    return;
  }
  if (action === "join-session") {
    const invite = ($("#partyJoinInvite")?.value || "").trim();
    const roomCode = ($("#partyJoinRoom")?.value || "").trim();
    try {
      toast("Conectando a la sesión...");
      const snap = await api.post("/api/party/join", { invite, roomCode: roomCode || null });
      _onPartyStateUpdate(snap);
    } catch (err) { toast(err.message); }
    return;
  }
  if (action === "leave") {
    try {
      const snap = await api.post("/api/party/leave", {});
      _onPartyStateUpdate(snap, { skipAutoLoad: true });
      toast("Sesión finalizada");
    } catch (err) { toast(err.message); }
    return;
  }
});

document.addEventListener("change", async (ev) => {
  const permChk = ev.target.closest("[data-party-perm-user]");
  if (permChk) {
    if (!state.party?.active || state.party.role !== "host") return;
    const userId = permChk.dataset.partyPermUser;
    const permKey = permChk.dataset.partyPermKey;
    if (!userId || !permKey) return;
    try {
      const snap = await api.post("/api/party/permissions", {
        userId,
        [permKey]: Boolean(permChk.checked),
      });
      _onPartyStateUpdate(snap, { skipAutoLoad: true });
    } catch (err) {
      permChk.checked = !permChk.checked;
      toast(err.message);
    }
    return;
  }

  const allowChk = ev.target.closest("[data-party-allow-uid]");
  if (!allowChk || !state.party?.active || state.party.role !== "host") return;
  const uid = allowChk.dataset.partyAllowUid;
  const set = new Set(state.party.allowedQueueUserIds || []);
  if (allowChk.checked) set.add(uid);
  else set.delete(uid);
  try {
    const snap = await api.post("/api/party/policy", {
      queuePolicy: "selected",
      allowedQueueUserIds: Array.from(set),
    });
    _onPartyStateUpdate(snap, { skipAutoLoad: true });
  } catch (err) { toast(err.message); }
});

async function main() {
  applySidebarState();
  bindEvents();
  await bindTauriEvents();
  if (IS_TAURI) {
    refreshNativePlayerStatus().catch(() => {});
    _refreshPartyStatus().catch(() => {});
    _refreshSocialStatus().catch(() => {});
    _refreshDiscordStatus().catch(() => {});
    api.get("/api/update/status").then((st) => {
      state.updateStatus = st;
      if (state.view === "config") renderConfig();
    }).catch(() => {});
  }
  await loadAll();
  scheduleNextPoll();
}

main();
