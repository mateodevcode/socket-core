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
const DEDUPE_CAP = 1000;

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

/** Transporte mínimo WebSocket para Node >= 18 (sin dependencias). */
class NodeWebSocket {
  constructor(url) {
    this.onopen = null;
    this.onmessage = null;
    this.onclose = null;
    this.onerror = null;
    this._buf = Buffer.alloc(0);
    this._handshakeDone = false;
    this._closed = false;
    this._closeEmitted = false;
    this._frag = "";
    this._init(url).catch((e) => this._fail(e));
  }

  async _init(url) {
    const u = new URL(url);
    const secure = u.protocol === "wss:";
    const host = u.hostname;
    const port = Number(u.port || (secure ? 443 : 80));
    const net = await import(secure ? "node:tls" : "node:net");
    const crypto = await import("node:crypto");
    this._crypto = crypto;
    const key = crypto.randomBytes(16).toString("base64");
    this._expectedAccept = crypto
      .createHash("sha1")
      .update(key + "258EAFA5-E914-47DA-95CA-C5AB0DC85B11")
      .digest("base64");

    const socket = secure
      ? net.connect({ host, port, servername: host })
      : net.connect({ host, port });
    this._socket = socket;
    socket.on("error", (e) => this._fail(e));
    socket.on("close", () => {
      if (!this._closeEmitted) this._emitClose(1006, "network");
    });
    socket.on("data", (chunk) => {
      this._buf = Buffer.concat([this._buf, chunk]);
      this._consume();
    });
    socket.once(secure ? "secureConnect" : "connect", () => {
      const path = u.pathname + (u.search || "");
      socket.write(
        `GET ${path} HTTP/1.1\r\nHost: ${u.host}\r\nUpgrade: websocket\r\n` +
          `Connection: Upgrade\r\nSec-WebSocket-Key: ${key}\r\n` +
          `Sec-WebSocket-Version: 13\r\n\r\n`,
      );
    });
  }

  _consume() {
    if (!this._handshakeDone) {
      const idx = this._buf.indexOf("\r\n\r\n");
      if (idx === -1) return;
      const head = this._buf.subarray(0, idx).toString("latin1");
      this._buf = this._buf.subarray(idx + 4);
      const status = Number((/^HTTP\/1\.1 (\d{3})/.exec(head.split("\r\n")[0]) || [])[1] || 0);
      const accept = (/sec-websocket-accept:\s*(\S+)/i.exec(head) || [])[1];
      if (status !== 101 || (accept && accept !== this._expectedAccept)) {
        this._fail(new Error(`handshake rechazado (HTTP ${status})`));
        return;
      }
      this._handshakeDone = true;
      if (!this._closed) this.onopen?.({});
    }
    for (;;) {
      const frame = this._parseFrame();
      if (!frame) return;
      this._handleFrame(frame);
    }
  }

  _parseFrame() {
    const b = this._buf;
    if (b.length < 2) return null;
    const fin = (b[0] & 0x80) !== 0;
    const opcode = b[0] & 0x0f;
    const masked = (b[1] & 0x80) !== 0;
    let len = b[1] & 0x7f;
    let off = 2;
    if (len === 126) {
      if (b.length < 4) return null;
      len = b.readUInt16BE(2);
      off = 4;
    } else if (len === 127) {
      if (b.length < 10) return null;
      const big = b.readBigUInt64BE(2);
      if (big > 16_777_216n) return null; // >16MB: no aplica a este hub
      len = Number(big);
      off = 10;
    }
    let mask = null;
    if (masked) {
      if (b.length < off + 4) return null;
      mask = b.subarray(off, off + 4);
      off += 4;
    }
    if (b.length < off + len) return null;
    const payload = Buffer.from(b.subarray(off, off + len));
    if (mask) for (let i = 0; i < payload.length; i++) payload[i] ^= mask[i % 4];
    this._buf = b.subarray(off + len);
    return { fin, opcode, payload };
  }

  _handleFrame({ fin, opcode, payload }) {
    if (opcode === 0x1 || opcode === 0x0) {
      this._frag += payload.toString("utf8");
      if (fin) {
        const data = this._frag;
        this._frag = "";
        this.onmessage?.({ data });
      }
      return;
    }
    if (opcode === 0x8) {
      this._emitClose(1000, payload.toString("utf8"));
      this._shutdown();
      return;
    }
    if (opcode === 0x9) {
      this._sendFrame(0xa, payload); // ping -> pong
      return;
    }
    if (opcode === 0xa) return; // pong
  }

  _sendFrame(opcode, data) {
    if (!this._socket || this._closed || !this._crypto) return;
    const payload = Buffer.isBuffer(data) ? data : Buffer.from(String(data), "utf8");
    let header;
    if (payload.length < 126) {
      header = Buffer.from([0x80 | opcode, 0x80 | payload.length]);
    } else if (payload.length < 65536) {
      header = Buffer.alloc(4);
      header[0] = 0x80 | opcode;
      header[1] = 0x80 | 126;
      header.writeUInt16BE(payload.length, 2);
    } else {
      header = Buffer.alloc(10);
      header[0] = 0x80 | opcode;
      header[1] = 0x80 | 127;
      header.writeBigUInt64BE(BigInt(payload.length), 2);
    }
    const mask = this._crypto.randomBytes(4);
    const masked = Buffer.alloc(payload.length);
    for (let i = 0; i < payload.length; i++) masked[i] = payload[i] ^ mask[i % 4];
    this._socket.write(Buffer.concat([header, mask, masked]));
  }

  send(data) {
    if (!this._handshakeDone) throw new Error("no conectado");
    this._sendFrame(0x1, data);
  }

  close() {
    if (this._closed) return;
    this._closed = true;
    try {
      this._sendFrame(0x8, Buffer.alloc(0));
    } catch {}
    this._shutdown();
  }

  _shutdown() {
    try {
      this._socket?.end();
      this._socket?.destroy();
    } catch {}
  }

  _fail(err) {
    this.onerror?.(err);
    this._emitClose(1006, String(err?.message || err));
    this._shutdown();
  }

  _emitClose(code, reason) {
    if (this._closeEmitted) return;
    this._closeEmitted = true;
    this.onclose?.({ code, reason });
  }
}

function createTransport(url, options) {
  if (!options._forceNodeTransport && typeof globalThis.WebSocket === "function") {
    return new globalThis.WebSocket(url);
  }
  return new NodeWebSocket(url);
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
    seen: new Set(),
    seenOrder: [],
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
  const dedupe = (id) => {
    if (!id) return true;
    if (state.seen.has(id)) return false;
    state.seen.add(id);
    state.seenOrder.push(id);
    if (state.seenOrder.length > DEDUPE_CAP) {
      const old = state.seenOrder.shift();
      state.seen.delete(old);
    }
    return true;
  };

  async function getToken() {
    const t = await tokenProvider();
    if (!t || typeof t.token !== "string" || !t.token) {
      throw new Error("tokenProvider debe devolver { token, expires_in }");
    }
    if (t.role) state.role = t.role;
    return t;
  }

  function startSocket(tokenInfo, mode) {
    const url = new URL(wsUrl.toString());
    url.searchParams.set("token", tokenInfo.token);
    const ws = createTransport(url.toString(), options);
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
      state.fatal = true;
      clearTimers();
      safeClose(state.active);
      safeClose(state.candidate);
      setStatus("fatal");
      emit("fatal", { code, detail });
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
    } catch {
      scheduleReconnect();
      return;
    }
    if (state.closed || state.fatal) return;
    state.active = startSocket(tokenInfo, "primary");
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
    } catch {
      scheduleRenewalRetry();
      return;
    }
    if (state.closed || state.fatal) return;
    state.candidate = startSocket(tokenInfo, "renewal");
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
