// socket-core SDK — un solo fichero, sin dependencias, navegador y Node >= 18.
//
//   import { connect } from "./socket-core.mjs";
//   const sc = connect({
//     url: "wss://hub.ejemplo.com",           // o http(s)://... -> se convierte
//     channel: "demo:chat",
//     device: "movil",                        // opcional
//     tokenProvider: async () => {            // LO IMPLEMENTA TU BACKEND
//       const r = await fetch("/socket-token"); // tu API key vive SOLO en el backend
//       const j = await r.json();
//       return { token: j.token, expires_in: j.expires_in }; // expires_in en segundos
//     },
//   });
//   sc.on("message", (ev) => ...); sc.on("status", (s) => ...);
//   await sc.publish({ type: "chat", text: "hola" }, { targets: ["user-1"] });
//   sc.close();
//
// Seguridad: el SDK NUNCA recibe la API key (solo JWT de corta vida). El token va
// por query porque el navegador no permite cabeceras en WebSocket; Nginx no loguea
// query strings y el JWT dura ~10 minutos (emite uno nuevo antes de expirar).

const MAX_PAYLOAD_BYTES = 64 * 1024;
const DEFAULT_RENEW_BEFORE_MS = 60_000;
const MIN_BACKOFF_MS = 500;
const MAX_BACKOFF_MS = 30_000;
const RATE_LIMITED_MIN_WAIT_MS = 10_000;

const FATAL_CODES = new Set([
  "forbidden",
  "credential_revoked",
  "token_revoked",
  "channel_deleted",
  "not_found",
  "invalid_payload",
  "message_too_large",
]);
const RECONNECT_CODES = new Set([
  "token_expired",
  "server_shutdown",
  "idle_timeout",
  "handshake_timeout",
]);

/** Estrategia recomendada para un código de error del servidor. */
export function _classifyClose(code) {
  if (!code) return "reconnect"; // cierre de red / desconocido sin código
  if (FATAL_CODES.has(code)) return "fatal";
  if (code === "rate_limited") return "rate_limited";
  if (RECONNECT_CODES.has(code)) return "reconnect";
  return "fatal"; // código nuevo no contemplado: no entrar en bucle
}

function normalizeWsUrl(url) {
  if (!url) throw new Error("url es obligatoria (ws://host/ws o https://host)");
  const u = new URL(url);
  if (u.protocol === "http:") u.protocol = "ws:";
  else if (u.protocol === "https:") u.protocol = "wss:";
  if (u.protocol !== "ws:" && u.protocol !== "wss:") {
    throw new Error("url debe ser ws(s):// o http(s)://");
  }
  if (!u.pathname || u.pathname === "/") u.pathname = "/ws";
  u.search = "";
  u.hash = "";
  return u;
}

/** Abre el WebSocket nativo (navegador o Node >= 22). Sin cliente casero. */
function openWebSocket(url) {
  if (typeof globalThis.WebSocket !== "function") {
    throw new Error(
      "WebSocket no disponible: usa un navegador o Node.js >= 22 (o pasa un polyfill global)",
    );
  }
  return new globalThis.WebSocket(url);
}

/** Intervalo mínimo entre llamadas a tokenProvider (Nginx limita /auth/token a 5 r/s). */
export const _MIN_TOKEN_INTERVAL_MS = 220;

/** Deduplicador acotado por id (últimos N). */
export class _RecentIds {
  constructor(cap = 1000) {
    this.cap = cap;
    this.set = new Set();
    this.order = [];
  }
  /** true si es la primera vez que se ve el id. */
  first(id) {
    if (!id) return true;
    if (this.set.has(id)) return false;
    this.set.add(id);
    this.order.push(id);
    if (this.order.length > this.cap) {
      this.set.delete(this.order.shift());
    }
    return true;
  }
  get size() {
    return this.set.size;
  }
}

/** Estrategia ante errores de tokenProvider: 401/403 => fatal; red/5xx => reintentar. */
export function _classifyTokenError(err) {
  const status = Number(err?.status ?? err?.statusCode ?? 0);
  if (status === 401 || status === 403) return "fatal";
  const msg = String(err?.message ?? err ?? "");
  if (/\b(401|403)\b/.test(msg)) return "fatal";
  return "retry";
}

/**
 * Abre una conexión gestionada al hub.
 * @param {object} options
 * @param {(ctx:{reason:string})=>Promise<{token:string,expires_in:number,role?:string}>} options.tokenProvider
 * @param {string} options.channel
 * @param {string} [options.device]
 * @param {string} options.url
 * @param {number} [options.renewBeforeMs=60000]
 * @param {string} [options.role] si se conoce, permite rechazar publish localmente
 */
export function connect(options = {}) {
  const tokenProvider = options.tokenProvider;
  if (typeof tokenProvider !== "function") throw new Error("tokenProvider es obligatorio");
  if (!options.channel) throw new Error("channel es obligatorio");
  const wsUrl = normalizeWsUrl(options.url);

  const listeners = new Map();
  const state = {
    closed: false,
    fatal: false,
    status: "idle",
    active: null,
    candidate: null,
    reconnectTimer: null,
    renewTimer: null,
    backoff: 0,
    role: options.role || null,
    seen: new _RecentIds(1000),
    lastTokenAt: 0,
  };

  const emit = (name, data) => {
    for (const fn of listeners.get(name) || []) {
      try {
        fn(data);
      } catch {
        /* un listener no debe romper el SDK */
      }
    }
  };
  const setStatus = (s) => {
    state.status = s;
    emit("status", s);
  };
  const clearTimers = () => {
    if (state.reconnectTimer) clearTimeout(state.reconnectTimer);
    if (state.renewTimer) clearTimeout(state.renewTimer);
    state.reconnectTimer = null;
    state.renewTimer = null;
  };
  const safeClose = (sock) => {
    if (!sock) return;
    try {
      sock.ws.close();
    } catch {}
  };
  const dedupe = (id) => state.seen.first(id);

  // Serializa y espacia las llamadas a tokenProvider (límite de /auth/token).
  let tokenChain = Promise.resolve();
  function getToken() {
    const run = async () => {
      const wait = Math.max(0, state.lastTokenAt + _MIN_TOKEN_INTERVAL_MS - Date.now());
      if (wait > 0) await new Promise((r) => setTimeout(r, wait));
      state.lastTokenAt = Date.now();
      const t = await tokenProvider();
      if (!t || typeof t.token !== "string" || !t.token) {
        throw new Error("tokenProvider debe devolver { token, expires_in }");
      }
      if (t.role) state.role = t.role;
      return t;
    };
    const next = tokenChain.then(run, run);
    // La cadena no debe romperse si un intento falla.
    tokenChain = next.catch(() => {});
    return next;
  }

  function handleFatal(code, detail) {
    if (state.fatal || state.closed) return;
    state.fatal = true;
    clearTimers();
    safeClose(state.active);
    safeClose(state.candidate);
    setStatus("fatal");
    emit("fatal", { code, detail });
  }

  function startSocket(tokenInfo, mode) {
    const url = new URL(wsUrl.toString());
    url.searchParams.set("token", tokenInfo.token);
    const ws = openWebSocket(url.toString());
    const sock = { ws, mode, ready: false, tokenInfo };
    ws.onopen = () => {
      try {
        ws.send(JSON.stringify({ channel: options.channel, ...(options.device ? { device: options.device } : {}) }));
      } catch {
        try {
          ws.close();
        } catch {}
      }
    };
    ws.onmessage = (ev) => onData(sock, ev.data);
    ws.onclose = (ev) => onClose(sock, ev);
    ws.onerror = () => {};
    return sock;
  }

  function handleMessage(msg) {
    if (!msg || typeof msg !== "object") return;
    // Mensajes de control del servidor (top-level type).
    if (msg.type === "missed_events") {
      emit("missed_events", msg);
      return;
    }
    if (msg.id && !dedupe(msg.id)) return;
    // Eventos de sistema: llegan como Event con payload.type de presencia.
    const ptype = msg.payload && msg.payload.type;
    if (ptype === "client_joined" || ptype === "client_left") {
      emit("presence", {
        type: ptype,
        user: msg.payload.user,
        connection_id: msg.payload.connection_id,
        total_clients: msg.payload.total_clients,
        channel: msg.channel,
        event_id: msg.id,
        source: msg.source,
        timestamp: msg.timestamp,
      });
      return;
    }
    emit("message", msg);
  }

  function handleServerError(code, detail) {
    const strategy = _classifyClose(code);
    if (strategy === "fatal") {
      handleFatal(code, detail);
      return;
    }
    safeClose(state.active);
    if (strategy === "rate_limited") {
      scheduleReconnect(RATE_LIMITED_MIN_WAIT_MS);
    } else {
      scheduleReconnect();
    }
  }

  function onData(sock, raw) {
    if (state.closed || state.fatal) return;
    let msg;
    try {
      msg = JSON.parse(raw);
    } catch {
      return;
    }
    if (msg && msg.type === "error") {
      const code = msg.code || "invalid_payload";
      // Durante una renovación, un error transitorio no debe tumbar la conexión
      // buena; pero un código fatal (key revocada, forbidden, ...) sí escala.
      if (sock.mode === "renewal" && _classifyClose(code) !== "fatal") {
        if (state.candidate === sock) state.candidate = null;
        safeClose(sock);
        scheduleRenewalRetry();
        return;
      }
      handleServerError(code, msg.detail);
      return;
    }

    if (!sock.ready) {
      sock.ready = true;
      if (sock.mode === "renewal") {
        const old = state.active;
        state.active = sock;
        state.candidate = null;
        state.backoff = 0;
        setStatus("connected");
        scheduleRenewal(sock);
        const overlap = Math.max(0, options._renewOverlapMs ?? 0);
        const closeOld = () => safeClose(old);
        if (overlap > 0) setTimeout(closeOld, overlap);
        else closeOld();
      } else {
        state.active = sock;
        state.backoff = 0;
        setStatus("connected");
        scheduleRenewal(sock);
      }
    }
    handleMessage(msg);
  }

  function onClose(sock, ev) {
    if (state.closed || state.fatal) return;
    if (sock.mode === "renewal") {
      if (state.candidate === sock) state.candidate = null;
      scheduleRenewalRetry();
      return;
    }
    if (sock !== state.active) return;
    // Cierre sin código de error => red/servidor caído: reconectar con backoff.
    scheduleReconnect();
  }

  function scheduleReconnect(minWaitMs = 0) {
    if (state.closed || state.fatal) return;
    clearTimers();
    const base = Math.min(MAX_BACKOFF_MS, MIN_BACKOFF_MS * 2 ** state.backoff);
    const jitter = Math.random() * base * 0.5;
    const wait = Math.max(minWaitMs, base + jitter);
    state.backoff = Math.min(state.backoff + 1, 6);
    setStatus("reconnecting");
    state.reconnectTimer = setTimeout(connectOnce, wait);
  }

  async function connectOnce() {
    if (state.closed || state.fatal) return;
    let tokenInfo;
    try {
      tokenInfo = await getToken();
    } catch (e) {
      if (state.closed || state.fatal) return;
      if (_classifyTokenError(e) === "fatal") {
        handleFatal(e?.code || "auth_failed", String(e?.message || e));
      } else {
        scheduleReconnect();
      }
      return;
    }
    if (state.closed || state.fatal) return;
    try {
      state.active = startSocket(tokenInfo, "primary");
    } catch (e) {
      handleFatal("ws_unavailable", String(e?.message || e));
    }
  }

  function scheduleRenewal(sock) {
    if (state.closed || state.fatal) return;
    if (state.renewTimer) clearTimeout(state.renewTimer);
    const ttlMs = Math.max(1000, Number(sock.tokenInfo.expires_in || 600) * 1000);
    const before = Math.max(0, options.renewBeforeMs ?? DEFAULT_RENEW_BEFORE_MS);
    const delay = Math.max(0, ttlMs - before);
    state.renewTimer = setTimeout(() => renew(sock), delay);
  }

  function scheduleRenewalRetry() {
    if (state.closed || state.fatal) return;
    if (state.renewTimer) clearTimeout(state.renewTimer);
    state.renewTimer = setTimeout(() => {
      if (state.active) renew(state.active);
    }, 2000);
  }

  async function renew(oldSock) {
    if (state.closed || state.fatal || oldSock !== state.active || state.candidate) return;
    setStatus("renewing");
    let tokenInfo;
    try {
      tokenInfo = await getToken();
    } catch (e) {
      if (state.closed || state.fatal) return;
      if (_classifyTokenError(e) === "fatal") {
        handleFatal(e?.code || "auth_failed", String(e?.message || e));
      } else {
        scheduleRenewalRetry();
      }
      return;
    }
    if (state.closed || state.fatal) return;
    try {
      state.candidate = startSocket(tokenInfo, "renewal");
    } catch (e) {
      handleFatal("ws_unavailable", String(e?.message || e));
    }
  }

  const api = {
    on(event, fn) {
      if (!listeners.has(event)) listeners.set(event, new Set());
      listeners.get(event).add(fn);
      return api;
    },
    off(event, fn) {
      listeners.get(event)?.delete(fn);
      return api;
    },
    async publish(payload, opts = {}) {
      if (state.closed) throw new Error("socket cerrado");
      if (state.fatal) throw new Error("conexión en estado fatal");
      if (!state.active?.ready) throw new Error("no conectado");
      const body =
        payload && typeof payload === "object" && !Array.isArray(payload)
          ? { ...payload }
          : { data: payload };
      if (opts.targets) body.targets = opts.targets;
      const size = new TextEncoder().encode(JSON.stringify(body)).length;
      if (size > MAX_PAYLOAD_BYTES) {
        const e = new Error("message_too_large: el payload supera 64 KB");
        e.code = "message_too_large";
        throw e;
      }
      if (state.role && state.role !== "both" && state.role !== "publish") {
        const e = new Error("forbidden: tu rol no permite publicar");
        e.code = "forbidden";
        throw e;
      }
      try {
        state.active.ws.send(JSON.stringify(body));
      } catch (err) {
        const e = new Error(`no se pudo publicar: ${err?.message || err}`);
        e.code = "not_connected";
        throw e;
      }
      return { ok: true };
    },
    close() {
      if (state.closed) return;
      state.closed = true;
      clearTimers();
      safeClose(state.active);
      safeClose(state.candidate);
      state.active = null;
      state.candidate = null;
      setStatus("closed");
    },
    get status() {
      return state.status;
    },
  };

  setStatus("connecting");
  connectOnce();
  return api;
}

export const SDK_VERSION = "1.0.0";
export const MAX_MESSAGE_BYTES = MAX_PAYLOAD_BYTES;
