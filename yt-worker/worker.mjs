import fs from 'node:fs';
import readline from 'node:readline';
import { JSDOM } from 'jsdom';
import { Innertube, Platform, UniversalCache, Utils } from 'youtubei.js';
import { BotGuardClient, getChallenge } from 'bgutils-js/botguard';
import { WebPoMinter, createColdStartToken } from 'bgutils-js/webpo';

// Provide a JS evaluator for youtubei.js n-challenge / signature deciphering
Platform.shim.eval = async (data, env) => {
  const properties = [];
  if (env.n) properties.push(`n: exportedVars.nFunction(${JSON.stringify(String(env.n))})`);
  if (env.sig) properties.push(`sig: exportedVars.sigFunction(${JSON.stringify(String(env.sig))})`);
  const code = `${data.output}\nreturn { ${properties.join(', ')} }`;
  return new Function(code)();
};

const PO_TOKEN_TTL_MS = 25 * 60 * 1000;
const REQUEST_KEY = 'O43z0dpjhgX20SCx4KAo';
const YT_LANG = (process.env.PLAYZI_YT_LANG || 'es').trim() || 'es';
const YT_REGION = (process.env.PLAYZI_YT_REGION || 'EC').trim() || 'EC';

let domInitialized = false;
function ensureJsdomGlobals() {
  if (domInitialized) return;
  const dom = new JSDOM('<!DOCTYPE html><html><head></head><body></body></html>', {
    url: 'https://www.youtube.com/',
    pretendToBeVisual: true,
  });
  Object.defineProperty(globalThis, 'window', { value: dom.window, configurable: true });
  Object.defineProperty(globalThis, 'document', { value: dom.window.document, configurable: true });
  Object.defineProperty(globalThis, 'navigator', { value: dom.window.navigator, configurable: true });
  domInitialized = true;
}

function parseNetscapeCookies(cookieFilePath) {
  if (!cookieFilePath || !fs.existsSync(cookieFilePath)) {
    return { cookieHeader: '', mtimeMs: 0 };
  }
  const stat = fs.statSync(cookieFilePath);
  const raw = fs.readFileSync(cookieFilePath, 'utf8');
  const nowSec = Math.floor(Date.now() / 1000);
  const seen = new Map();
  for (const line of raw.split(/\r?\n/)) {
    const trimmed = line.trim();
    if (!trimmed || (trimmed.startsWith('#') && !trimmed.startsWith('#HttpOnly_'))) continue;
    const clean = trimmed.startsWith('#HttpOnly_') ? trimmed.slice('#HttpOnly_'.length) : trimmed;
    const parts = clean.split('\t');
    if (parts.length < 7) continue;
    const [domain, , path, , expStr, name, value] = parts;
    const domLower = domain.toLowerCase();
    if (domLower !== '.youtube.com' && domLower !== 'youtube.com' && domLower !== 'www.youtube.com') {
      continue;
    }
    if (path && path !== '/') continue;
    const exp = Number.parseInt(expStr, 10);
    if (Number.isFinite(exp) && exp > 0 && exp < nowSec) continue;
    if (name && value !== undefined) {
      seen.set(name, value);
    }
  }
  const pairs = [];
  for (const [k, v] of seen.entries()) {
    pairs.push(`${k}=${v}`);
  }
  return { cookieHeader: pairs.join('; '), mtimeMs: stat.mtimeMs };
}

let cachedSession = null;
let cachedCookieKey = '';
let cachedMinter = null;
let cachedMinterAt = 0;
let cachedVisitorData = '';

async function ensureMinter(visitorData) {
  const now = Date.now();
  if (cachedMinter && now - cachedMinterAt < PO_TOKEN_TTL_MS && cachedVisitorData === visitorData) {
    return cachedMinter;
  }
  try {
    ensureJsdomGlobals();
    const bgChallenge = await getChallenge({
      requestKey: REQUEST_KEY,
      fetchFunction: globalThis.fetch,
      useYouTubeAPI: true,
    });
    if (!bgChallenge?.interpreterJavascript?.privateDoNotAccessOrElseSafeScriptWrappedValue) {
      return null;
    }
    const interpreterJs = bgChallenge.interpreterJavascript.privateDoNotAccessOrElseSafeScriptWrappedValue;
    new Function(interpreterJs)();
    const bgClient = await BotGuardClient.create({
      globalObject: globalThis,
      globalName: bgChallenge.globalName,
      program: bgChallenge.program,
    });
    const webPoSignalOutput = [];
    const botguardResponse = await bgClient.snapshot({ webPoSignalOutput });
    const integrityResp = await fetch('https://www.youtube.com/api/jnn/v1/GenerateIT', {
      method: 'POST',
      headers: {
        'Content-Type': 'application/json+protobuf',
        'x-user-agent': 'grpc-web-javascript/0.1',
      },
      body: JSON.stringify([REQUEST_KEY, botguardResponse]),
      signal: AbortSignal.timeout(5000),
    });
    if (!integrityResp.ok) return null;
    const integrityJson = await integrityResp.json();
    const integrityTokenData = {
      integrityToken: integrityJson[0],
      estimatedTtlSecs: integrityJson[1],
      mintRefreshThreshold: integrityJson[2],
      websafeFallbackToken: integrityJson[4],
    };
    const minter = await WebPoMinter.create(integrityTokenData, webPoSignalOutput);
    cachedMinter = minter;
    cachedMinterAt = now;
    cachedVisitorData = visitorData;
    return minter;
  } catch (err) {
    process.stderr.write(`[yt-worker] bgutils minter fallback: ${err?.message || err}\n`);
    return null;
  }
}

async function getInnertube(cookieFile) {
  const { cookieHeader, mtimeMs } = parseNetscapeCookies(cookieFile);
  const cookieKey = `${cookieFile || ''}:${mtimeMs}`;

  if (cachedSession && cachedCookieKey === cookieKey && Date.now() - cachedSession.createdAt < PO_TOKEN_TTL_MS) {
    return cachedSession;
  }

  // Lightweight bootstrap to obtain visitor_data without downloading player JS twice
  const baseYt = await Innertube.create({
    cookie: cookieHeader || undefined,
    lang: YT_LANG,
    location: YT_REGION,
    retrieve_player: false,
    generate_session_locally: true,
    cache: new UniversalCache(false),
  });

  const visitorData = baseYt.session.context.client.visitorData || '';
  let poToken = '';
  if (visitorData) {
    const minter = await ensureMinter(visitorData);
    if (minter) {
      try {
        poToken = await minter.mintAsWebsafeString(visitorData);
      } catch {
        poToken = createColdStartToken(visitorData);
      }
    } else {
      poToken = createColdStartToken(visitorData);
    }
  }

  const yt = await Innertube.create({
    cookie: cookieHeader || undefined,
    visitor_data: visitorData || undefined,
    po_token: poToken || undefined,
    lang: YT_LANG,
    location: YT_REGION,
    retrieve_player: true,
    generate_session_locally: true,
    cache: new UniversalCache(false),
  });

  cachedSession = { yt, visitorData, poToken, createdAt: Date.now() };
  cachedCookieKey = cookieKey;
  return cachedSession;
}

function parseMime(mimeType) {
  const raw = String(mimeType || '');
  const [basePart, ...rest] = raw.split(';');
  const mime = basePart.trim().toLowerCase();
  let codecs = '';
  for (const part of rest) {
    const m = part.match(/codecs\s*=\s*"([^"]+)"/i);
    if (m) {
      codecs = m[1].trim();
      break;
    }
  }
  const codecList = codecs ? codecs.split(',').map((s) => s.trim()).filter(Boolean) : [];
  let ext = 'mp4';
  if (mime.includes('webm')) ext = 'webm';
  else if (mime.includes('audio/mp4')) ext = 'm4a';
  else if (mime.includes('video/mp4')) ext = 'mp4';

  let vcodec = 'none';
  let acodec = 'none';
  if (mime.startsWith('audio/')) {
    acodec = codecList[0] || (ext === 'webm' ? 'opus' : 'mp4a.40.2');
  } else if (mime.startsWith('video/')) {
    vcodec = codecList[0] || (ext === 'webm' ? 'vp9' : 'avc1.640028');
    if (codecList.length > 1) {
      acodec = codecList[1];
    }
  }
  return { ext, vcodec, acodec };
}

function formatRange(r) {
  if (!r || r.start === undefined || r.end === undefined) return null;
  return `${r.start}-${r.end}`;
}

function extractUrlExpire(url) {
  try {
    const u = new URL(url);
    const exp = u.searchParams.get('expire');
    if (exp) {
      const n = Number.parseInt(exp, 10);
      if (Number.isFinite(n) && n > 0) return n;
    }
  } catch {
    // ignore
  }
  return null;
}

async function resolveVideo(videoId, originalUrl, cookieFile) {
  const session = await getInnertube(cookieFile);
  const { yt, poToken: sessionPoToken, visitorData } = session;

  // YouTube binds GVS PO tokens to videoId for web_music/web clients.
  let gvsPoToken = sessionPoToken;
  if (cachedMinter) {
    try {
      gvsPoToken = await cachedMinter.mintAsWebsafeString(videoId);
    } catch {
      // fall back to sessionPoToken
    }
  } else if (visitorData) {
    gvsPoToken = createColdStartToken(videoId);
  }

  let info = null;
  let usedClient = null;
  let lastErr = null;
  for (const client of ['YTMUSIC', 'WEB']) {
    try {
      const candidate = await yt.getBasicInfo(videoId, { client });
      const ps = candidate.playability_status;
      if (ps && ps.status === 'OK' && candidate.streaming_data) {
        const sd = candidate.streaming_data;
        const hasAdaptive = Array.isArray(sd.adaptive_formats) && sd.adaptive_formats.length > 0;
        const hasMuxed = Array.isArray(sd.formats) && sd.formats.length > 0;
        if (hasAdaptive || hasMuxed) {
          info = candidate;
          usedClient = client;
          break;
        }
      }
      lastErr = new Error(`client=${client} playabilityStatus=${ps?.status || 'UNKNOWN'}: ${ps?.reason || ''}`);
    } catch (e) {
      lastErr = e;
    }
  }
  if (!info) {
    throw lastErr || new Error('Failed to fetch video info');
  }
  if (info.basic_info?.is_live) {
    throw new Error('Live streams fall back to yt-dlp HLS');
  }

  const sd = info.streaming_data;

  const rawFormats = [
    ...(Array.isArray(sd.formats) ? sd.formats : []),
    ...(Array.isArray(sd.adaptive_formats) ? sd.adaptive_formats : []),
  ];

  const formats = [];
  let minExpire = null;

  for (const fmt of rawFormats) {
    if (!fmt || fmt.is_drm) continue;
    let url = '';
    try {
      url = await fmt.decipher(yt.session.player);
    } catch {
      url = fmt.url || '';
    }
    if (!url || !url.startsWith('http')) continue;
    if (gvsPoToken && !url.includes('pot=')) {
      url += (url.includes('?') ? '&' : '?') + `pot=${encodeURIComponent(gvsPoToken)}`;
    }

    const exp = extractUrlExpire(url);
    if (exp !== null && (minExpire === null || exp < minExpire)) {
      minExpire = exp;
    }

    const { ext, vcodec, acodec } = parseMime(fmt.mime_type);
    const bitrateKbps = Number.isFinite(fmt.bitrate) ? fmt.bitrate / 1000 : null;
    const isAudioOnly = vcodec === 'none' && acodec !== 'none';
    const contentLength = fmt.content_length ? Number(fmt.content_length) : null;

    formats.push({
      formatId: String(fmt.itag ?? ''),
      url,
      ext,
      vcodec,
      acodec,
      width: fmt.width ?? null,
      height: fmt.height ?? null,
      fps: fmt.fps ?? null,
      tbr: bitrateKbps,
      abr: isAudioOnly ? bitrateKbps : null,
      contentLength: Number.isFinite(contentLength) && contentLength > 0 ? contentLength : null,
      mimeType: typeof fmt.mime_type === 'string' ? fmt.mime_type.split(';')[0].trim() : null,
      initRange: formatRange(fmt.init_range),
      indexRange: formatRange(fmt.index_range),
      asr: Number.isFinite(Number(fmt.audio_sample_rate)) ? Number(fmt.audio_sample_rate) : null,
      audioChannels: Number.isFinite(Number(fmt.audio_channels)) ? Number(fmt.audio_channels) : null,
      language: fmt.language || null,
      languageName: fmt.audio_track?.display_name || null,
    });
  }

  if (formats.length === 0) {
    throw new Error('No playable formats deciphered');
  }

  const basic = info.basic_info || {};
  const duration = Number(basic.duration || 0);
  const thumbs = Array.isArray(basic.thumbnail) ? basic.thumbnail : [];
  const thumbnail = thumbs.length ? thumbs[thumbs.length - 1].url : `https://i.ytimg.com/vi/${videoId}/hqdefault.jpg`;
  // perceptual_loudness_db is absolute LUFS across all clients (WEB, YTMUSIC, IOS),
  // whereas YTMUSIC loudness_db is referenced to -7 LUFS instead of -14 LUFS.
  const audioConfig = info.player_config?.audio_config;
  const rawPerceptual = audioConfig?.perceptual_loudness_db;
  const rawLoudness = audioConfig?.loudness_db;
  let loudnessDb = null;
  if (Number.isFinite(rawPerceptual)) {
    loudnessDb = Math.round((rawPerceptual + 14) * 1000) / 1000;
  } else if (Number.isFinite(rawLoudness)) {
    const adjusted = usedClient === 'YTMUSIC' ? rawLoudness + 7 : rawLoudness;
    loudnessDb = Math.round(adjusted * 1000) / 1000;
  }

  return {
    item: {
      id: basic.id || videoId,
      title: basic.title || originalUrl,
      channel: basic.author || basic.channel?.name || '',
      thumbnail,
      url: originalUrl,
      duration: Math.max(0, Math.floor(duration)),
    },
    duration,
    loudnessDb,
    expiresAt: minExpire,
    formats,
  };
}

// NDJSON stdin/stdout server loop
const rl = readline.createInterface({ input: process.stdin, crlfDelay: Infinity });
rl.on('line', async (line) => {
  const trimmed = line.trim();
  if (!trimmed) return;
  let req;
  try {
    req = JSON.parse(trimmed);
  } catch (err) {
    process.stdout.write(`${JSON.stringify({ id: 0, ok: false, error: `Invalid JSON: ${err.message}` })}\n`);
    return;
  }
  const id = req.id ?? 0;
  try {
    const data = await resolveVideo(req.videoId, req.originalUrl || `https://www.youtube.com/watch?v=${req.videoId}`, req.cookieFile || '');
    process.stdout.write(`${JSON.stringify({ id, ok: true, data })}\n`);
  } catch (err) {
    process.stdout.write(`${JSON.stringify({ id, ok: false, error: String(err?.message || err) })}\n`);
  }
});
