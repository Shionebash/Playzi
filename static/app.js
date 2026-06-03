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
  musicPlaylists: [],
  musicPlaylistsLastSync: "",
  channelsLastSync: "",
  musicAuthState: "",
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
  },
  config: null,
  theme: localStorage.getItem("playzi-theme") || "dark",
  sidebarCollapsed: localStorage.getItem("playzi-sidebar-collapsed") === "1",
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

const api = {
  async get(path) {
    const res = await fetch(path);
    if (!res.ok) throw new Error(await errorText(res));
    return res.json();
  },
  async post(path, body = {}) {
    const res = await fetch(path, {
      method: "POST",
      headers: { "Content-Type": "application/json" },
      body: JSON.stringify(body),
    });
    if (!res.ok) throw new Error(await errorText(res));
    return res.json();
  },
  async patch(path, body = {}) {
    const res = await fetch(path, {
      method: "PATCH",
      headers: { "Content-Type": "application/json" },
      body: JSON.stringify(body),
    });
    if (!res.ok) throw new Error(await errorText(res));
    return res.json();
  },
  async delete(path) {
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
  return `/api/thumbnail?url=${encodeURIComponent(normalized)}`;
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
  state.player.current = null;
  state.player.queue = [];
  state.player._originalQueue = [];
  state.player.loading = false;
  state.player.audioOnly = false;
  state.player.sourceContext = "search";
}

function setView(view) {
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
  if (view === "music") _autoTriggerMusicSync();
}

function applySidebarState() {
  document.body.classList.toggle("sidebar-collapsed", state.sidebarCollapsed);
}

function toggleSidebar() {
  state.sidebarCollapsed = !state.sidebarCollapsed;
  localStorage.setItem("playzi-sidebar-collapsed", state.sidebarCollapsed ? "1" : "0");
  applySidebarState();
}

const MOON_SVG = `<svg width="15" height="15" viewBox="0 0 24 24" fill="currentColor"><path d="M21 12.79A9 9 0 1 1 11.21 3 7 7 0 0 0 21 12.79z"/></svg>`;
const SUN_SVG  = `<svg width="15" height="15" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round"><circle cx="12" cy="12" r="5"/><path d="M12 1v2M12 21v2M4.22 4.22l1.42 1.42M18.36 18.36l1.42 1.42M1 12h2M21 12h2M4.22 19.78l1.42-1.42M18.36 5.64l1.42-1.42"/></svg>`;

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
  } catch (err) {
    toast(err.message);
  }
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
  state.syncStatus = data.syncStatus || {};
  _lastSeenSyncSuccess = state.syncStatus.lastSuccess || _lastSeenSyncSuccess;
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
    const data = await api.get(`/api/search?q=${encodeURIComponent(q)}${scope}${filter}`);
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
  if (state.view === "home") renderHome();
  if (state.view === "search") renderSearch();
  if (state.view === "recom") renderRecom();
  if (state.view === "channels") renderChannels();
  if (state.view === "library") renderLibrary();
  if (state.view === "music") renderMusic();
  if (state.view === "collections") renderCollections();
  if (state.view === "downloads") renderDownloads();
  if (state.view === "history") renderHistory();
  if (state.view === "config") renderConfig();
  if (state.view === "watch") renderWatch();
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
  const music = [...(state.musicLibrary?.liked || []), ...state.musicRecommendations].slice(0, 8);
  const videos = [...state.recommendations, ...state.feed].slice(0, 8);
  const imported = state.playlists.filter((pl) => pl.externalUrl || pl.importMode !== "local").slice(0, 6);
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
    ${music.length ? homeSection("Música para ti", `<div class="grid">${music.map(videoCard).join("")}</div>`) : ""}
    ${videos.length ? homeSection("Videos y canales", `<div class="grid">${videos.map(videoCard).join("")}</div>`) : ""}
    ${imported.length ? homeSection("Playlists importadas", playlistGrid(imported)) : ""}
    ${state.library.length ? homeSection("Biblioteca local", `<div class="grid">${state.library.slice(0, 8).map(libraryCard).join("")}</div>`) : ""}
  `;
}

function homeSection(title, body) {
  return `
    <section class="home-section">
      <div class="section-hdr"><span class="section-title">${escapeHtml(title)}</span></div>
      ${body}
    </section>
  `;
}

function renderSearch(loading = false) {
  const root = $("#view-search");
  const q = state.query.trim();
  const direct = isUrl(q);
  root.innerHTML = `
    ${direct ? `
      <div class="url-banner">
        <strong>Link</strong>
        <div style="min-width:0;flex:1">
          <div class="url-banner-title">URL de YouTube detectada</div>
          <div class="url-banner-sub">${escapeHtml(q)}</div>
        </div>
        <button class="btn btn-primary" type="button" data-url-download="video">Descargar</button>
        <button class="btn btn-ghost" type="button" data-url-download="audio">Audio</button>
      </div>` : ""}
    <div class="section-hdr">
      <span class="section-title">${q ? `Resultados para "${escapeHtml(q)}"` : "Buscar"}</span>
      <span class="meta">${loading ? "Buscando..." : `${state.results.length} resultados`}</span>
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
    ${q && !loading && state.results.length === 0 ? empty("Sin resultados", "Prueba otra búsqueda o revisa la URL.") : ""}
    ${state.results.length ? `<div class="grid">${state.results.map(videoCard).join("")}</div>` : ""}
  `;
}

function renderRecom() {
  const hasCookies = state.config?.cookiesBrowser || state.config?.cookiesFile;
  const mixedFeed = _uniqueByUrl([...state.recommendations, ...state.feed]);
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
    ${mixedFeed.length
      ? `<div class="grid">${mixedFeed.map(videoCard).join("")}</div>`
      : hasCookies
        ? empty("Sin feed cargado", "Actualiza recomendaciones o canales para llenar Para ti.")
        : empty("Configura cookies primero", "Necesitas seleccionar el navegador con tu sesión de YouTube.")}
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

async function openChannelPreview(url, name) {
  if (!url) return;
  state._lastChannelPreview = { url, name };
  setView("channels");
  $("#view-channels").innerHTML = `
    <div class="section-hdr"><span class="section-title">${escapeHtml(name || "Canal")}</span></div>
    ${empty("Cargando…", "Obteniendo videos del canal.")}`;
  try {
    const data = await api.get(`/api/channels/preview?url=${encodeURIComponent(url)}`);
    _renderChannelPreview(data);
  } catch (err) {
    $("#view-channels").innerHTML = `
      <div class="section-hdr">
        <span class="section-title">${escapeHtml(name || "Canal")}</span>
        <button class="btn btn-ghost" type="button" id="backToChannels">← Canales</button>
      </div>
      ${empty("Error", err.message)}`;
    const b = $("#backToChannels");
    if (b) b.onclick = () => renderChannels();
  }
}

function _refreshAfterFollowChange() {
  if (state.view === "watch") {
    _updateWatchMeta();
  } else if (state.view === "channels" && state._lastChannelPreview) {
    openChannelPreview(state._lastChannelPreview.url, state._lastChannelPreview.name);
  }
}

function _renderChannelPreview(data) {
  const followBtn = data.alreadyFollowed
    ? `<button class="btn btn-ghost" type="button" data-unfollow-channel="${escapeHtml(data.channelId)}">Siguiendo ✓</button>`
    : `<button class="btn btn-primary" type="button" data-add-channel="${escapeHtml(data.url)}" data-add-channel-name="${escapeHtml(data.name || "")}">＋ Seguir canal</button>`;
  $("#view-channels").innerHTML = `
    <div class="section-hdr">
      <span class="section-title">${escapeHtml(data.name || "Canal")}</span>
      <div style="display:flex;gap:8px">
        ${followBtn}
        <button class="btn btn-ghost" type="button" id="backToChannels">← Canales</button>
      </div>
    </div>
    ${data.items.length
      ? `<div class="grid">${data.items.map(videoCard).join("")}</div>`
      : empty("Sin videos", "Este canal no tiene videos recientes.")}`;
  const b = $("#backToChannels");
  if (b) b.onclick = () => renderChannels();
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

async function _awaitLoginTask(taskId) {
  if (!taskId) return;
  // Poll the login task until the user closes the browser window.
  for (let i = 0; i < 600; i++) {
    await new Promise((r) => setTimeout(r, 2000));
    let task;
    try {
      task = await api.get(`/api/tasks/${taskId}`);
    } catch (_) { continue; }
    if (task.status === "done") {
      await refreshLists();
      _musicSyncTriggered = false;
      if (state.musicAuthState === "ok") {
        toast("Sesión iniciada. Cargando tu contenido...");
        try {
          await api.post("/api/music/recommendations/refresh");
          await api.post("/api/music/playlists/refresh");
        } catch (_) {}
      } else {
        toast("La sesión sigue sin validar. Asegúrate de iniciar sesión completamente.");
      }
      if (state.view === "music") renderMusic();
      return;
    }
    if (task.status === "error") {
      toast(task.error || "Error al iniciar sesión");
      return;
    }
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
  $("#view-music").innerHTML = `
    <div class="section-hdr">
      <span class="section-title">YT Music — Recomendadas ${state.musicRecomLastSync ? `<span class="meta" style="font-size:10px;margin-left:6px">· ${escapeHtml(state.musicRecomLastSync)}</span>` : ""}</span>
      <button class="btn btn-primary" type="button" id="refreshMusicRecom">Actualizar</button>
    </div>
    ${!hasCookies ? `<div class="url-banner" style="margin-bottom:16px"><strong>⚙</strong><div style="min-width:0;flex:1"><div class="url-banner-title">Cookies requeridas</div><div class="url-banner-sub">Configura el navegador en Configuración.</div></div><button class="btn btn-ghost" type="button" data-view-go="config">Configurar</button></div>` : ""}
    ${_musicAuthBanner()}
    ${hasCookies && !expired ? syncNotice("musicRecommendations") : ""}
    ${expired
      ? `<div style="margin-bottom:32px">${empty("Contenido no personalizado", "Inicia sesión arriba para cargar tus recomendaciones reales.")}</div>`
      : state.musicRecommendations.length
      ? `<div class="grid" style="margin-bottom:32px">${state.musicRecommendations.map(videoCard).join("")}</div>`
      : hasCookies ? `<div style="margin-bottom:32px">${
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
        ${canPlay ? `<button class="btn btn-primary" type="button" data-history-play="${escapeHtml(url)}" data-history-title="${escapeHtml(item.title || "")}">Reproducir</button>` : ""}
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

function renderConfig() {
  const c = state.config || {};
  const playbackQuality = c.playbackQuality || "best";
  const curColor = document.documentElement.dataset.color || "pink";
  $("#view-config").innerHTML = `
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
          ${["", "playzi", "chrome", "firefox", "edge", "brave", "chromium", "opera"].map((b) =>
            `<option value="${b}" ${(c.cookiesBrowser || "") === b ? "selected" : ""}>${b === "playzi" ? "Playzi automatico" : (b || "Ninguno")}</option>`
          ).join("")}
        </select>
      </label>
      <label>Archivo cookies.txt (alternativo; también puedes dejarlo en D:\\ytlocal\\cookies)
        <input class="form-input" name="cookiesFile" value="${escapeHtml(c.cookiesFile || "")}" placeholder="C:\\ruta\\cookies.txt">
      </label>
      <button class="btn btn-primary" type="submit">Guardar configuración</button>
    </form>
  `;
}

// ── Watch view ────────────────────────────────────────────────────────────────

function _videoLayoutHtml() {
  return `
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
      <div class="watch-sidebar">
        <div class="watch-sidebar-section">
          <div id="watch-queue-hdr" class="watch-queue-hdr">Cola</div>
          <div id="watch-queue" class="watch-queue-list"></div>
        </div>
        <div class="watch-sidebar-section">
          <div class="watch-queue-hdr" id="watch-related-hdr">Relacionados</div>
          <div id="watch-related" class="watch-queue-list"></div>
        </div>
      </div>
    </div>
  `;
}

function _musicLayoutHtml() {
  return `
    <div class="watch-layout watch-layout--music">
      <div class="watch-main watch-main--music">
        <div class="music-player-card">
          <div class="music-art-wrap">
            <img id="musicArt" class="music-art" src="" alt="" style="display:none"
              onerror="this.style.display='none';var f=document.getElementById('musicArtFallback');if(f)f.style.display=''">
            <div class="music-art-fallback" id="musicArtFallback">♪</div>
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
            <button class="music-ctrl-sm" id="ctrlShuffle" type="button" title="Aleatorio">⇄</button>
            <button class="music-btn" id="watchPrev" type="button" title="Anterior">⏮</button>
            <button class="music-btn music-btn--play" id="musicPlayPause" type="button" title="Play/Pausa">▶</button>
            <button class="music-btn" id="watchNext" type="button" title="Siguiente">⏭</button>
            <button class="music-ctrl-sm" id="ctrlRepeat" type="button" title="Repetir">↻</button>
          </div>
          <div class="music-extra-ctrl">
            <button class="btn btn-ghost btn-player-ctrl" type="button" id="ctrlAutoplay" title="Autoplay">∞</button>
          </div>
          <div class="music-volume">
            <span class="music-vol-icon" id="musicVolIcon">🔊</span>
            <input type="range" id="musicVolume" class="volume-slider" min="0" max="1" step="0.05" value="1">
          </div>
          <div class="music-actions">
            <span id="musicFollow"></span>
            <button class="btn btn-ghost" type="button" data-watch-download="audio">Descargar</button>
            <button class="btn btn-ghost" type="button" id="toggleAudioOnly">Solo audio</button>
            <button class="btn btn-ghost" type="button" id="playerExternal">Abrir externo</button>
            <button class="btn btn-ghost" type="button" id="watchBack">← Volver</button>
          </div>
          <div id="musicStatus" class="player-status" style="display:none"></div>
          <audio id="webPlayerAudio" autoplay></audio>
        </div>
      </div>
      <div class="watch-sidebar">
        <div class="watch-sidebar-section">
          <div id="watch-queue-hdr" class="watch-queue-hdr">Cola</div>
          <div id="watch-queue" class="watch-queue-list"></div>
        </div>
        <div class="watch-sidebar-section">
          <div class="watch-queue-hdr" id="watch-related-hdr">Relacionados</div>
          <div id="watch-related" class="watch-queue-list"></div>
        </div>
      </div>
    </div>
  `;
}

function renderWatch() {
  const root = $("#view-watch");
  const isMusicCtx = state.player.sourceContext === "music";
  const useMusicLayout = isMusicCtx || state.player.audioOnly;
  const hasCorrectLayout = useMusicLayout
    ? !!root.querySelector(".watch-layout--music")
    : !!root.querySelector(".watch-layout--video");

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
  _updateWatchQueue();
  _updateWatchRelated();
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
  const duration = item?.durationText || "";
  const channelHtml = channelUrl
    ? `<span class="channel-link" data-watch-channel="${escapeHtml(channelUrl)}" data-watch-channel-name="${escapeHtml(channel)}">${escapeHtml(channel)}</span>`
    : escapeHtml(channel);
  const hasPrev = state.player.index > 0;
  const hasNext = state.player.index < state.player.queue.length - 1;
  el.innerHTML = `
    <div class="watch-title">${escapeHtml(title)}</div>
    <div class="watch-channel">${channelHtml}${duration ? ` · ${escapeHtml(duration)}` : ""}</div>
    <div class="watch-actions">
      <button class="btn btn-ghost" type="button" id="watchPrev" ${hasPrev ? "" : "disabled"}>⏮ Anterior</button>
      <button class="btn btn-ghost" type="button" id="watchNext" ${hasNext ? "" : "disabled"}>Siguiente ⏭</button>
      ${_followButtonHtml(channelUrl, channel)}
      ${_playerCtrlsHtml()}
      <button class="btn btn-primary" type="button" data-watch-download="video">Descargar</button>
      <button class="btn btn-ghost" type="button" data-watch-download="audio">Audio</button>
      <button class="btn btn-ghost" type="button" id="toggleAudioOnly">Solo audio</button>
      <button class="btn btn-ghost" type="button" id="playerExternal">Abrir externo</button>
      <button class="btn btn-ghost" type="button" id="watchBack">← Volver</button>
    </div>
    ${state.player.loading ? `<div class="player-status">Cargando...</div>` : ""}
    ${error ? `<div class="player-error-card"><span>⚠ ${escapeHtml(error)}</span><button class="btn btn-ghost" type="button" onclick="loadWebPlayerItem(state.player.index)">Reintentar</button></div>` : ""}
  `;
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
  if (titleEl) titleEl.textContent = item?.title || "...";
  if (artistEl) {
    artistEl.innerHTML = channelUrl
      ? `<span class="channel-link" data-watch-channel="${escapeHtml(channelUrl)}" data-watch-channel-name="${escapeHtml(channel)}">${escapeHtml(channel)}</span>`
      : escapeHtml(channel);
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
      statusEl.innerHTML = `⚠ ${escapeHtml(error)} <button class="btn btn-ghost" type="button" style="padding:2px 8px;font-size:11px" onclick="loadWebPlayerItem(state.player.index)">Reintentar</button>`;
      statusEl.style.display = "";
      statusEl.className = "player-status error-text";
    } else {
      statusEl.style.display = "none";
    }
  }
}

function _setupMusicListeners(sig) {
  const audio = $("#webPlayerAudio");
  if (!audio) return;

  // Restore saved volume
  const savedVol = localStorage.getItem("playzi-volume");
  if (savedVol !== null) audio.volume = parseFloat(savedVol);

  const volSlider = $("#musicVolume");
  if (volSlider) {
    volSlider.value = audio.volume;
    volSlider.addEventListener("input", () => {
      audio.volume = parseFloat(volSlider.value);
      localStorage.setItem("playzi-volume", volSlider.value);
      const icon = $("#musicVolIcon");
      if (icon) icon.textContent = parseFloat(volSlider.value) === 0 ? "🔇" : "🔊";
    }, sig);
  }

  audio.addEventListener("timeupdate", () => {
    const fill = $("#musicProgressFill");
    const cur = $("#musicCurrent");
    if (fill && audio.duration) fill.style.width = `${(audio.currentTime / audio.duration) * 100}%`;
    if (cur) cur.textContent = _formatTime(audio.currentTime);
  }, sig);

  audio.addEventListener("loadedmetadata", () => {
    const dur = $("#musicDuration");
    if (dur) dur.textContent = _formatTime(audio.duration);
  }, sig);

  audio.addEventListener("play", () => {
    const btn = $("#musicPlayPause");
    if (btn) btn.textContent = "⏸";
    if (navigator.mediaSession) navigator.mediaSession.playbackState = "playing";
  }, sig);

  audio.addEventListener("pause", () => {
    const btn = $("#musicPlayPause");
    if (btn) btn.textContent = "▶";
    if (navigator.mediaSession) navigator.mediaSession.playbackState = "paused";
  }, sig);

  const wrap = $("#musicProgressWrap");
  if (wrap) {
    wrap.addEventListener("click", (ev) => {
      if (!audio.duration) return;
      const rect = wrap.getBoundingClientRect();
      audio.currentTime = Math.max(0, Math.min(1, (ev.clientX - rect.left) / rect.width)) * audio.duration;
    }, sig);
  }

  const playBtn = $("#musicPlayPause");
  if (playBtn) {
    playBtn.addEventListener("click", () => {
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
  const items = ctx === "music"
    ? state.musicRecommendations
    : (state.recommendations.length ? state.recommendations : state.feed);
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
  if (hdr) hdr.textContent = `Cola · ${idx + 1} / ${q.length}`;
  el.innerHTML = q.map((item, i) => `
    <div class="queue-item ${i === idx ? "active" : ""}" data-queue-idx="${i}">
      <span class="queue-idx">${i === idx ? "▶" : i + 1}</span>
      ${item.thumbnail ? `<img class="queue-thumb" ${thumbAttrs(item.thumbnail)}>` : `<div class="queue-thumb"></div>`}
      <div class="queue-info">
        <div class="queue-title">${escapeHtml(item.title || item.url)}</div>
        <div class="queue-meta">${escapeHtml(item.channel || "")}${item.durationText ? ` · ${escapeHtml(item.durationText)}` : ""}</div>
      </div>
    </div>
  `).join("");
  const activeEl = el.querySelector(".queue-item.active");
  if (activeEl) activeEl.scrollIntoView({ block: "nearest" });
}

// ── Player logic ──────────────────────────────────────────────────────────────

function _itemToQueueEntry(item) {
  return {
    url: item.url,
    title: item.title || item.url,
    thumbnail: item.thumbnail || "",
    channel: item.channel || item.sourceChannelName || "",
    durationText: item.durationText || "--:--",
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
    repeatBtn.classList.toggle("btn-player-ctrl-on", repeat !== "none");
    repeatBtn.textContent = repeat === "one" ? "↻¹" : "↻";
    repeatBtn.title = repeat === "none" ? "Repetir" : repeat === "one" ? "Repetir: una" : "Repetir: todas";
  }
  if (autoplayBtn) {
    autoplayBtn.classList.toggle("btn-player-ctrl-on", state.player.autoplay);
    autoplayBtn.title = state.player.autoplay ? "Autoplay: activo" : "Autoplay";
  }
}

function _playerCtrlsHtml() {
  const sh = state.player.shuffle;
  const rep = state.player.repeat;
  const ap = state.player.autoplay;
  return `
    <button class="btn btn-ghost btn-player-ctrl ${sh ? "btn-player-ctrl-on" : ""}" type="button" id="ctrlShuffle" title="${sh ? "Aleatorio: activo" : "Aleatorio"}">⇄</button>
    <button class="btn btn-ghost btn-player-ctrl ${rep !== "none" ? "btn-player-ctrl-on" : ""}" type="button" id="ctrlRepeat" title="${rep === "none" ? "Repetir" : rep === "one" ? "Repetir: una" : "Repetir: todas"}">${rep === "one" ? "↻¹" : "↻"}</button>
    <button class="btn btn-ghost btn-player-ctrl ${ap ? "btn-player-ctrl-on" : ""}" type="button" id="ctrlAutoplay" title="${ap ? "Autoplay: activo" : "Autoplay"}">∞</button>
  `;
}

async function _onTrackEnded() {
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

function addToQueue(item) {
  const entry = _itemToQueueEntry(item);
  if (!state.player.current) {
    const ctx = String(item.url || "").includes("music.youtube.com") ? "music" : state.view;
    openWatchQueue([entry], 0, ctx);
    return;
  }
  state.player.queue.push(entry);
  if (state.player.shuffle) state.player._originalQueue.push(entry);
  _updateWatchQueue();
  toast("Añadido a la cola");
}

function playNext(item) {
  const entry = _itemToQueueEntry(item);
  if (!state.player.current) {
    const ctx = String(item.url || "").includes("music.youtube.com") ? "music" : state.view;
    openWatchQueue([entry], 0, ctx);
    return;
  }
  state.player.queue.splice(state.player.index + 1, 0, entry);
  if (state.player.shuffle) state.player._originalQueue.push(entry);
  _updateWatchQueue();
  toast("Se reproducirá a continuación");
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
  const audioEl = isAudio ? $("#webPlayerAudio") : null;
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
}

async function openWatchQueue(queue, index = 0, sourceContext = "search") {
  state.player.audioOnly = false;
  state.player.sourceContext = sourceContext;
  state.player.shuffle = false;
  state.player._originalQueue = [];
  if (queue.length === 1 && isUrl(queue[0].url) && queue[0].url.includes("list=") && !queue[0].url.includes("watch?v=")) {
    toast("Cargando cola...");
    try {
      const data = await api.post("/api/player/playlist", { url: queue[0].url, quality: state.config?.playbackQuality || "best" });
      if (data.requiresLogin || (data.items?.length === 0 && data.error)) {
        toast(data.error || "No se pudo cargar la playlist");
        return;
      }
      if (data.items?.length) queue = data.items;
    } catch (err) {
      toast(err.message);
      return;
    }
  }
  state.player.queue = queue;
  state.player.index = index;
  state.player.current = null;
  state.player.loading = true;
  setView("watch");
  await loadWebPlayerItem(index);
}

async function loadWebPlayerItem(index) {
  const item = state.player.queue[index];
  if (!item) return;
  const loadId = ++state.player._loadingId;
  state.player.index = index;
  state.player.current = item;
  state.player.loading = true;
  renderWatch();
  _updateWatchMeta();
  _updateWatchQueue();
  _updateWatchRelated();

  try {
    if (state.player.sourceContext === "music" || state.player.audioOnly) {
      await _loadMusicItem(item, loadId);
    } else {
      await _loadVideoItem(item, loadId);
    }
  } catch (err) {
    if (state.player._loadingId !== loadId) return;
    state.player.loading = false;
    _updateWatchMeta(err.message);
  }
}

async function _loadMusicItem(item, loadId) {
  const info = await api.get(`/api/player/audio?url=${encodeURIComponent(item.url)}`);
  if (state.player._loadingId !== loadId) return;
  const audio = $("#webPlayerAudio");
  if (!audio) return;
  state.player.current = { ...item, ...(info.item || {}) };
  state.player.loading = false;
  audio.src = info.proxyUrl;
  audio.load();
  audio.play().catch(() => {});
  _updateWatchMeta();
  _updateWatchQueue();
  _updateWatchRelated();
  _updateMediaSession();
  _prefetchNextItem();
}

// Warm the backend cache for the next queue item so advancing has no gap.
const _prefetchedUrls = new Set();
function _prefetchNextItem() {
  const next = state.player.queue[state.player.index + 1];
  if (!next || !next.url || _prefetchedUrls.has(next.url)) return;
  _prefetchedUrls.add(next.url);
  const isAudio = state.player.sourceContext === "music" || state.player.audioOnly;
  const quality = encodeURIComponent(state.config?.playbackQuality || "best");
  const endpoint = isAudio
    ? `/api/player/audio?url=${encodeURIComponent(next.url)}`
    : next.manifestUrl
      ? next.manifestUrl
      : `/api/player/manifest.mpd?url=${encodeURIComponent(next.url)}&quality=${quality}`;
  fetch(endpoint).catch(() => {});
}

async function _loadVideoItem(item, loadId) {
  const info = item.manifestUrl
    ? { manifestUrl: item.manifestUrl, item }
    : await api.get(`/api/player/info?url=${encodeURIComponent(item.url)}&quality=${encodeURIComponent(state.config?.playbackQuality || "best")}`);
  if (state.player._loadingId !== loadId) return;
  await ensureVideoPlayer();
  if (state.player._loadingId !== loadId) return;
  const player = state.player.vjs;
  player.src({ type: "application/dash+xml", src: info.manifestUrl });
  player.play().catch(() => {});
  state.player.current = { ...item, ...(info.item || {}) };
  state.player.loading = false;
  _updateWatchMeta();
  _updateWatchQueue();
  _updateWatchRelated();
  _updateMediaSession();
  _prefetchNextItem();
}

async function ensureVideoPlayer() {
  if (!window.videojs) throw new Error("Video.js no está cargado.");
  if (state.player.vjs && !state.player.vjs.isDisposed()) return;
  const videoEl = document.getElementById("webPlayerVideo");
  if (!videoEl) {
    renderWatch();
  }
  const player = videojs("webPlayerVideo", {
    controls: true,
    autoplay: true,
    preload: "auto",
    fill: true,
    playbackRates: [0.25, 0.5, 0.75, 1, 1.25, 1.5, 1.75, 2],
    html5: { vhs: { overrideNative: true } },
  });
  state.player.vjs = player;
  player.on("error", () => {
    const err = player.error();
    const code = err?.code;
    const msg = code === 4
      ? "Formato no compatible con el reproductor web. Prueba con un reproductor externo (MPV/VLC) o baja la calidad."
      : (err?.message || "Error de reproducción");
    toast(msg);
    _updateWatchMeta(msg);
  });
  player.on("ended", () => { _onTrackEnded(); });
  // Restore / persist volume
  player.ready(() => {
    const savedVol = localStorage.getItem("playzi-volume");
    if (savedVol !== null) player.volume(parseFloat(savedVol));
  });
  player.on("volumechange", () => {
    localStorage.setItem("playzi-volume", player.volume());
  });
  _registerQualityMenu(player);
  _registerTheaterBtn(player);
}

function _registerQualityMenu(player) {
  const qualities = ["best", "1080p", "720p", "480p", "360p", "240p"];
  let activeQ = state.config?.playbackQuality || "best";

  const MenuButton = videojs.getComponent("MenuButton");
  const MenuItem = videojs.getComponent("MenuItem");

  class QualityMenuItem extends MenuItem {
    constructor(p, opts) {
      super(p, { ...opts, selectable: true, multiSelectable: false });
      this.selected(opts.label === activeQ);
    }
    handleClick() {
      activeQ = this.options_.label;
      const curTime = player.currentTime();
      const curSrc = player.currentSrc();
      const newSrc = curSrc.replace(/quality=[^&]*/, `quality=${encodeURIComponent(activeQ)}`);
      player.src({ type: "application/dash+xml", src: newSrc });
      player.one("canplay", () => {
        player.currentTime(curTime);
        player.play().catch(() => {});
      });
      const btn = player.controlBar.getChild("QualityButton");
      if (btn) btn.items.forEach((it) => it.selected(it.options_.label === activeQ));
    }
  }

  class QualityButton extends MenuButton {
    buildCSSClass() { return `vjs-quality-button ${super.buildCSSClass()}`; }
    createItems() { return qualities.map((q) => new QualityMenuItem(player, { label: q })); }
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
        <button class="mini-btn" id="miniPrev" title="Anterior" ${hasPrev ? "" : "disabled"}>⏮</button>
        <button class="mini-btn mini-btn--play" id="miniPlayPause" title="Play/Pausa">⏸</button>
        <button class="mini-btn" id="miniNext" title="Siguiente" ${hasNext ? "" : "disabled"}>⏭</button>
        <button class="mini-btn" id="miniExpand" title="Abrir reproductor">⤢</button>
        <button class="mini-btn" id="miniClose" title="Cerrar">✕</button>
      </div>
    </div>
  `;
}

function _updateMiniPlayer() {
  const item = state.player.current;
  if (!item) return;

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
  if (playBtn) playBtn.textContent = paused ? "▶" : "⏸";
  const prevBtn = document.getElementById("miniPrev");
  if (prevBtn) prevBtn.disabled = state.player.index <= 0;
  const nextBtn = document.getElementById("miniNext");
  if (nextBtn) nextBtn.disabled = state.player.index >= state.player.queue.length - 1;
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
  if (ev.target.closest("#miniPrev") && state.player.index > 0) {
    loadWebPlayerItem(state.player.index - 1);
    return;
  }
  if (ev.target.closest("#miniNext") && state.player.index < state.player.queue.length - 1) {
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
  const isPlaylist = item.resultType === "playlist" || /[?&]list=/.test(item.url || "");
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

  const channelNameHtml = (item.channelUrl && channelName)
    ? `<span class="channel-link" data-watch-channel="${escapeHtml(item.channelUrl)}" data-watch-channel-name="${escapeHtml(channelName)}">${escapeHtml(channelName)}</span>`
    : escapeHtml(channelName);

  return `
    <article class="card">
      <div class="thumb">
        ${item.thumbnail ? `<img ${thumbAttrs(item.thumbnail)}>` : ""}
        <span class="duration">${escapeHtml(item.durationText || "--:--")}</span>
      </div>
      <div class="card-body">
        <div class="card-title">${escapeHtml(item.title)}</div>
        <div class="meta"><span class="tag tag-muted">${escapeHtml(sourceLabel(item.source || (String(item.url || "").includes("music.youtube.com") ? "music" : "youtube")))}</span> ${channelNameHtml}${item.viewCount ? ` · ${Number(item.viewCount).toLocaleString()} vistas` : ""}</div>
      </div>
      <div class="card-actions">
        ${isPlaylist ? `<button class="btn btn-primary" type="button" data-import-result-playlist="${escapeHtml(item.url || "")}">Importar</button>` : `<button class="btn btn-primary" type="button" data-download="video" data-id="${id}">Descargar</button>`}
        ${isPlaylist ? "" : `<button class="btn btn-ghost" type="button" data-download="audio" data-id="${id}">Audio</button>`}
        <button class="btn btn-ghost" type="button" data-play-default data-id="${id}">▶ Play</button>
        ${!isPlaylist ? `<button class="btn btn-ghost" type="button" data-add-to-queue data-id="${id}" title="Añadir al final de la cola">+ Cola</button>` : ""}
        <button class="btn btn-ghost" type="button"
          data-add-to-playlist="${escapeHtml(item.url || item.feedId || '')}"
          data-playlist-title="${escapeHtml(item.title || '')}"
          data-playlist-thumb="${escapeHtml(item.thumbnail || '')}"
          data-playlist-channel="${escapeHtml(typeof channelName === 'string' ? channelName : '')}"
          data-playlist-duration="${escapeHtml(item.durationText || '')}">+ Lista</button>
        ${channelBtn}
      </div>
    </article>
  `;
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
        <button class="btn btn-primary" type="button" data-local-play-default data-path="${escapeHtml(item.path)}">Reproducir</button>
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
      ${failed ? `<div class="error-text">${escapeHtml(item.error)}</div>` : ""}
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
  return [...state.results, ...state.feed, ...state.recommendations, ...state.musicRecommendations, ...musicLibraryItems, ...state.library, ...playlistItems, ...state.history];
}

function findVideo(id) {
  return allVideoItems().find((item) => String(item.feedId || item.id || item.url) === String(id));
}

// Warm the backend cache when the user hovers a result, so the click feels
// instant. Debounced; one prefetch in flight; never re-warms the same URL.
function _setupHoverPrefetch() {
  let hoverTimer = null;
  let inFlight = null;
  document.body.addEventListener("mouseover", (ev) => {
    // Trigger anywhere on the card (thumbnail/title), not just the Play button.
    const card = ev.target.closest(".card");
    const playBtn = card?.querySelector("[data-play-default]");
    if (!playBtn) return;
    const item = findVideo(playBtn.dataset.id);
    if (!item || !item.url || _prefetchedUrls.has(item.url)) return;
    clearTimeout(hoverTimer);
    hoverTimer = setTimeout(() => {
      if (_prefetchedUrls.has(item.url)) return;
      _prefetchedUrls.add(item.url);
      if (inFlight) inFlight.abort();
      inFlight = new AbortController();
      const isAudio = String(item.url).includes("music.youtube.com");
      const quality = encodeURIComponent(state.config?.playbackQuality || "best");
      const endpoint = isAudio
        ? `/api/player/audio?url=${encodeURIComponent(item.url)}`
        : `/api/player/manifest.mpd?url=${encodeURIComponent(item.url)}&quality=${quality}`;
      fetch(endpoint, { signal: inFlight.signal }).catch(() => {});
    }, 200);
  });
  document.body.addEventListener("mouseout", (ev) => {
    if (ev.target.closest(".card")) clearTimeout(hoverTimer);
  });
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
        scopeParam += "&limit=25";
      }
      if (addScope === "youtube") scopeParam += "&limit=20";
      const data = await api.get(`/api/search?q=${encodeURIComponent(q)}${scopeParam}`);
      const items = (data.items || []).filter((i) => i.url && !i.url.includes("list="));
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
      };
      try {
        const savedConfig = await api.post("/api/config", submittedConfig);
        state.config = { ...submittedConfig, ...savedConfig, playbackQuality: savedConfig.playbackQuality || submittedConfig.playbackQuality };
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
    if (ev.target.closest("[data-music-login]")) {
      try {
        const resp = await api.post("/api/music/login");
        toast("Abriendo navegador... inicia sesión en YouTube y cierra la ventana al terminar.");
        _awaitLoginTask(resp?.task?.id);
      } catch (err) { toast(err.message); }
      return;
    }
    const searchScope = ev.target.closest("[data-search-scope]");
    if (searchScope) {
      state.searchScope = searchScope.dataset.searchScope;
      state.musicSearchFilter = "";
      await doSearch();
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
        toast("Obteniendo recomendaciones de YouTube...");
        await startBackgroundTask(await api.post("/api/recommendations/refresh"), "Actualizando recomendaciones en segundo plano...");
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
    const watchChannel = ev.target.closest("[data-watch-channel]");
    if (watchChannel) {
      openChannelPreview(watchChannel.dataset.watchChannel, watchChannel.dataset.watchChannelName);
      return;
    }
    const viewChannel = ev.target.closest("[data-view-channel]");
    if (viewChannel) {
      const channelId = viewChannel.dataset.viewChannel;
      setView("channels");
      const channel = state.channels.find((c) => c.id === channelId);
      const videos = state.feed.filter((f) => f.sourceChannelId === channelId);
      $("#view-channels").innerHTML = `
        <div class="section-hdr">
          <span class="section-title">${escapeHtml(channel?.name || "Canal")}</span>
          <div style="display:flex;gap:8px">
            <button class="btn btn-ghost" type="button" data-refresh-channel="${escapeHtml(channelId)}">Actualizar</button>
            <button class="btn btn-ghost" type="button" id="backToChannels">← Canales</button>
          </div>
        </div>
        ${videos.length
          ? `<div class="grid">${videos.map(videoCard).join("")}</div>`
          : empty("Sin videos recientes", "Pulsa Actualizar para cargar los videos recientes de este canal.")}
      `;
      $("#backToChannels").onclick = () => renderChannels();
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
    const queueItem = ev.target.closest("[data-queue-idx]");
    if (queueItem) {
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
    if (ev.target.closest("#watchPrev") && state.player.index > 0) {
      await loadWebPlayerItem(state.player.index - 1);
    }
    if (ev.target.closest("#watchNext") && state.player.index < state.player.queue.length - 1) {
      await loadWebPlayerItem(state.player.index + 1);
    }
    if (ev.target.closest("#ctrlShuffle")) { toggleShuffle(); }
    if (ev.target.closest("#ctrlRepeat")) { cycleRepeat(); }
    if (ev.target.closest("#ctrlAutoplay")) { toggleAutoplay(); }
    if (ev.target.closest("#playerExternal") && state.player.current?.url) {
      const cur = state.player.current;
      await playTarget(cur.url, state.config?.defaultPlayer || "mpv", cur.title, cur.thumbnail, cur.channel);
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
    const audio = state.player.sourceContext === "music" || state.player.audioOnly ? $("#webPlayerAudio") : null;
    const vjs = state.player.vjs;
    if (ev.code === "Space") {
      ev.preventDefault();
      if (audio) { audio.paused ? audio.play().catch(() => {}) : audio.pause(); }
      else if (vjs) { vjs.paused() ? vjs.play() : vjs.pause(); }
    } else if (ev.code === "ArrowLeft") {
      ev.preventDefault();
      if (audio) audio.currentTime = Math.max(0, audio.currentTime - 10);
      else if (vjs) vjs.currentTime(Math.max(0, vjs.currentTime() - 10));
    } else if (ev.code === "ArrowRight") {
      ev.preventDefault();
      if (audio) audio.currentTime = Math.min(audio.duration || Infinity, audio.currentTime + 10);
      else if (vjs) vjs.currentTime(Math.min(vjs.duration() || Infinity, vjs.currentTime() + 10));
    } else if (ev.code === "KeyN") {
      if (state.player.index < state.player.queue.length - 1) await loadWebPlayerItem(state.player.index + 1);
      else _onTrackEnded();
    } else if (ev.code === "KeyP") {
      if (state.player.index > 0) await loadWebPlayerItem(state.player.index - 1);
    } else if (ev.code === "KeyS") {
      toggleShuffle();
    } else if (ev.code === "KeyR") {
      cycleRepeat();
    }
  });
}

// ── Poll ──────────────────────────────────────────────────────────────────────

let _pollTimer = null;

function scheduleNextPoll() {
  clearTimeout(_pollTimer);
  const hasActive = state.downloads.some((d) => !["done", "error", "cancelled"].includes(d.status));
  _hadActiveDownloads = _hadActiveDownloads || hasActive;
  _pollTimer = setTimeout(async () => {
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

async function main() {
  applySidebarState();
  bindEvents();
  await loadAll();
  scheduleNextPoll();
}

main();
