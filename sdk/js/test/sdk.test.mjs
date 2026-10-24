// Tests del SDK contra el hub REAL (target/debug/socket-core).
// Ejecutar:  cargo build && node --test sdk/js/test/sdk.test.mjs
import test from "node:test";
import assert from "node:assert/strict";
import { spawn } from "node:child_process";
import { mkdtemp, rm } from "node:fs/promises";
import { tmpdir } from "node:os";
import { join, dirname } from "node:path";
import { fileURLToPath } from "node:url";
import {
  connect,
  _classifyClose,
  _classifyTokenError,
  _RecentIds,
  _MIN_TOKEN_INTERVAL_MS,
} from "../socket-core.mjs";

const HERE = dirname(fileURLToPath(import.meta.url));
const ROOT = join(HERE, "..", "..", "..");
const BIN =
  process.env.SOCKET_CORE_BIN ||
  join(ROOT, "target", "debug", process.platform === "win32" ? "socket-core.exe" : "socket-core");

const JWT_SECRET = "test-jwt-secret-32-chars-minimum!!!!";
const MASTER = "test-master-key-32-chars-minimum!!!!!!";
const CHANNEL = "t:chat";

const sleep = (ms) => new Promise((r) => setTimeout(r, ms));

async function freePort() {
  const net = await import("node:net");
  return new Promise((res) => {
    const s = net.createServer();
    s.listen(0, "127.0.0.1", () => {
      const p = s.address().port;
      s.close(() => res(p));
    });
  });
}

async function waitHttp(url, ms = 10000) {
  const end = Date.now() + ms;
  while (Date.now() < end) {
    try {
      const r = await fetch(url);
      if (r.ok) return true;
    } catch {}
    await sleep(100);
  }
  return false;
}

async function startHub({ port, adminPort, dbPath, jwtTtl = 600 }) {
  const env = {
    ...process.env,
    APP_ENV: "dev",
    JWT_SECRET,
    MASTER_KEY: MASTER,
    JWT_TTL_SECS: String(jwtTtl),
    PORT: String(port),
    ADMIN_PORT: String(adminPort),
    DATABASE_URL: `sqlite:${dbPath}?mode=rwc`,
    ALLOW_BOOTSTRAP: "false",
    RUST_LOG: "warn",
  };
  const child = spawn(BIN, [], { env, stdio: ["ignore", "pipe", "pipe"] });
  let stderr = "";
  child.stderr.on("data", (d) => (stderr += d.toString()));
  const ok = await waitHttp(`http://127.0.0.1:${port}/health`);
  if (!ok) {
    child.kill();
    throw new Error(`el hub no arrancó. stderr: ${stderr.slice(-800)}`);
  }
  return child;
}

async function adminJson(adminPort, path, method, body, master = MASTER) {
  const r = await fetch(`http://127.0.0.1:${adminPort}${path}`, {
    method,
    headers: { "content-type": "application/json", "x-master-key": master },
    body: body === undefined ? undefined : JSON.stringify(body),
  });
  const j = await r.json().catch(() => ({}));
  return { status: r.status, body: j };
}

async function setupHub(jwtTtl = 600) {
  const dir = await mkdtemp(join(tmpdir(), "sc-sdk-"));
  const dbPath = join(dir, "hub.db");
  const port = await freePort();
  const adminPort = await freePort();
  const child = await startHub({ port, adminPort, dbPath, jwtTtl });

  const key = await adminJson(adminPort, "/admin/keys", "POST", {
    name: "sdk-proj",
    channel_prefix: "t:*",
    scopes: ["publish", "subscribe", "manage"],
    domains: ["*"],
  });
  assert.equal(key.status, 201, JSON.stringify(key.body));
  const raw = key.body.data.api_key;
  const keyId = key.body.data.id;

  const ch = await fetch(`http://127.0.0.1:${port}/channels`, {
    method: "POST",
    headers: { "content-type": "application/json", "x-api-key": raw },
    body: JSON.stringify({ name: CHANNEL }),
  });
  if (ch.status !== 201) throw new Error(`crear canal ${ch.status}: ${await ch.text()}`);

  const mint = async (userId, role = "both") => {
    const r = await fetch(`http://127.0.0.1:${port}/auth/token`, {
      method: "POST",
      headers: { "content-type": "application/json", "x-api-key": raw },
      body: JSON.stringify({ user_id: userId, channel: CHANNEL, role }),
    });
    if (r.status !== 200) throw new Error(`mint ${r.status}: ${await r.text()}`);
    return r.json();
  };

  const emit = async (payload, targets = ["*"]) => {
    const r = await fetch(`http://127.0.0.1:${port}/channels/${CHANNEL}/events`, {
      method: "POST",
      headers: { "content-type": "application/json", "x-api-key": raw },
      body: JSON.stringify({ source: "backend", targets, payload }),
    });
    if (r.status !== 200) throw new Error(`emit ${r.status}: ${await r.text()}`);
  };

  const cleanup = async () => {
    try {
      child.kill();
    } catch {}
    await sleep(800);
    for (let i = 0; i < 10; i++) {
      try {
        await rm(dir, { recursive: true, force: true });
        return;
      } catch {
        await sleep(300);
      }
    }
  };

  return {
    port,
    adminPort,
    dbPath,
    rawKey: raw,
    keyId,
    mint,
    emit,
    cleanup,
    restart: async () => {
      try {
        child.kill();
      } catch {}
      await sleep(500);
      return startHub({ port, adminPort, dbPath, jwtTtl });
    },
    child,
  };
}

function clientSdk(hub, userId, opts = {}) {
  return connect({
    url: `ws://127.0.0.1:${hub.port}/ws`,
    channel: CHANNEL,
    device: opts.device || "sdk-test",
    tokenProvider: async () => {
      const j = await hub.mint(userId);
      return { token: j.data.token, expires_in: j.data.expires_in };
    },
    ...opts,
  });
}

function once(emitter, event, timeoutMs = 10000, predicate = null) {
  return new Promise((resolve, reject) => {
    const t = setTimeout(() => {
      emitter.off(event, handler);
      reject(new Error(`timeout esperando '${event}'`));
    }, timeoutMs);
    const handler = (v) => {
      if (predicate && !predicate(v)) return;
      clearTimeout(t);
      emitter.off(event, handler);
      resolve(v);
    };
    emitter.on(event, handler);
  });
}

async function waitStatus(sdk, target, timeoutMs = 10000) {
  if (sdk.status === target) return;
  await once(sdk, "status", timeoutMs, (s) => s === target);
}

async function waitFor(fn, timeoutMs = 10000) {
  const end = Date.now() + timeoutMs;
  while (Date.now() < end) {
    if (fn()) return true;
    await sleep(50);
  }
  return false;
}

// ---------- Estrategia por código (pura) ----------

test("clasificación de códigos de cierre", () => {
  for (const c of ["token_expired", "server_shutdown", "idle_timeout", "handshake_timeout"]) {
    assert.equal(_classifyClose(c), "reconnect", c);
  }
  for (const c of [
    "forbidden",
    "credential_revoked",
    "token_revoked",
    "channel_deleted",
    "not_found",
    "invalid_payload",
    "message_too_large",
  ]) {
    assert.equal(_classifyClose(c), "fatal", c);
  }
  assert.equal(_classifyClose("rate_limited"), "rate_limited");
});

// ---------- Connect, presence, publish, receive (transporte Node) ----------

test("conecta, presencia, publica y recibe", { timeout: 30000 }, async () => {
  const hub = await setupHub();
  const a = clientSdk(hub, "user-a");
  const b = clientSdk(hub, "user-b");
  const presence = [];
  const messages = [];
  a.on("presence", (p) => presence.push(p));
  b.on("message", (m) => messages.push(m));
  try {
    await waitStatus(a, "connected");
    await waitStatus(b, "connected");

    // Al conectar b, a debe recibir presencia de user-b.
    const okPresence = await waitFor(() => presence.some((p) => p.type === "client_joined"));
    assert.ok(okPresence, "debe llegar presence client_joined");

    await a.publish({ type: "chat", text: "hola" });
    const okMsg = await waitFor(() => messages.some((m) => m.payload?.text === "hola"));
    assert.ok(okMsg, "b debe recibir el mensaje publicado por a");

    // publish valida localmente el tamaño.
    await assert.rejects(
      () => a.publish({ type: "chat", text: "x".repeat(70 * 1024) }),
      /64 ?KB|message_too_large/
    );
  } finally {
    a.close();
    b.close();
    await hub.cleanup();
  }
});

// ---------- No reconectar en credential_revoked ----------

test("credential_revoked => fatal y NO reconecta", { timeout: 30000 }, async () => {
  const hub = await setupHub();
  const sdk = clientSdk(hub, "user-revoked");
  const statuses = [];
  sdk.on("status", (s) => statuses.push(s));
  try {
    await waitStatus(sdk, "connected");
    const rev = await adminJson(hub.adminPort, `/admin/keys/${hub.keyId}/revoke`, "POST");
    assert.equal(rev.status, 200);

    const fatal = await once(sdk, "fatal", 8000);
    assert.equal(fatal.code, "credential_revoked");

    await sleep(1500);
    const idx = statuses.lastIndexOf("fatal");
    const despues = idx >= 0 ? statuses.slice(idx + 1) : statuses;
    assert.ok(
      !despues.includes("reconnecting") && !despues.includes("connected"),
      `no debe reconectar tras fatal: ${JSON.stringify(statuses)}`
    );
  } finally {
    sdk.close();
    await hub.cleanup();
  }
});

// ---------- Reconexión tras caída del servidor ----------

test("reconecta tras cerrarse el servidor y vuelve a recibir", { timeout: 45000 }, async () => {
  const hub = await setupHub();
  const sdk = clientSdk(hub, "user-reconnect");
  const statuses = [];
  sdk.on("status", (s) => statuses.push(s));
  try {
    await waitStatus(sdk, "connected");
    // Cierre abrupto del servidor (éxito de red/close inesperado).
    hub.child.kill();
    const okReconnecting = await waitFor(() => statuses.includes("reconnecting"), 8000);
    assert.ok(okReconnecting, `debe pasar a reconnecting: ${JSON.stringify(statuses)}`);

    hub.child = await hub.restart();
    const okReconnected = await waitFor(() => statuses.filter((s) => s === "connected").length >= 2, 20000);
    assert.ok(okReconnected, `debe reconectar: ${JSON.stringify(statuses)}`);

    const got = [];
    sdk.on("message", (m) => got.push(m));
    await hub.emit({ type: "chat", text: "tras-reinicio" });
    const recibido = await waitFor(() => got.some((m) => m.payload?.text === "tras-reinicio"), 8000);
    assert.ok(recibido, "debe recibir mensajes tras reconectar");
  } finally {
    sdk.close();
    await hub.cleanup();
  }
});

// ---------- Dedupe de eventos en la renovación solapada ----------

test("renueva con solape y deduplica por id", { timeout: 40000 }, async () => {
  const hub = await setupHub(60); // JWT de 60s (mínimo)
  const statuses = [];
  const sdk = clientSdk(hub, "user-renew", {
    _renewBeforeMs: 58_000, // renueva ~2s después de conectar
    _renewOverlapMs: 1500, // mantiene la vieja 1.5s tras el handshake de la nueva
  });
  sdk.on("status", (s) => statuses.push(s));
  const ids = [];
  sdk.on("message", (m) => {
    if (m.payload?.type === "chat") ids.push(m.id);
  });
  try {
    await waitStatus(sdk, "connected");
    const okRenewing = await waitFor(() => statuses.includes("renewing"), 10000);
    assert.ok(okRenewing, `debe renovar: ${JSON.stringify(statuses)}`);

    // Emite durante el solape: los mismos eventos llegan por ambas conexiones.
    for (let i = 0; i < 40; i++) {
      await hub.emit({ type: "chat", seq: i });
      await sleep(60);
    }
    await sleep(800);
    assert.ok(ids.length > 0, "debe recibir eventos durante la renovación");
    const unicos = new Set(ids);
    assert.equal(unicos.size, ids.length, `hubo ids duplicados: ${ids.length} vs ${unicos.size}`);
  } finally {
    sdk.close();
    await hub.cleanup();
  }
});

// ---------- Fatal durante la renovación (regresión) ----------

test("credential_revoked durante renovación => fatal (no bucle)", { timeout: 30000 }, async () => {
  const hub = await setupHub(60);
  const statuses = [];
  const sdk = clientSdk(hub, "user-renew-revoked", {
    _renewBeforeMs: 58_000, // renueva ~2 s después de conectar
  });
  sdk.on("status", (s) => statuses.push(s));
  try {
    await waitStatus(sdk, "connected");
    const okRenewing = await waitFor(() => statuses.includes("renewing"), 8000);
    assert.ok(okRenewing, `debe renovar: ${JSON.stringify(statuses)}`);
    const rev = await adminJson(hub.adminPort, `/admin/keys/${hub.keyId}/revoke`, "POST");
    assert.equal(rev.status, 200);
    const fatal = await once(sdk, "fatal", 8000);
    assert.equal(fatal.code, "credential_revoked");
  } finally {
    sdk.close();
    await hub.cleanup();
  }
});

// ---------- B6: clasificación de errores de tokenProvider ----------

test("tokenProvider 401/403 => fatal sin reconectar", { timeout: 20000 }, async () => {
  for (const status of [401, 403]) {
    const hub = await setupHub();
    const statuses = [];
    const sdk = clientSdk(hub, "user-authfail", {
      tokenProvider: async () => {
        throw Object.assign(new Error(`no autorizado (${status})`), { status });
      },
    });
    sdk.on("status", (s) => statuses.push(s));
    try {
      const fatal = await once(sdk, "fatal", 8000);
      assert.equal(fatal.code, "auth_failed");
      await sleep(800);
      assert.ok(!statuses.includes("reconnecting"), `no debe reintentar: ${JSON.stringify(statuses)}`);
    } finally {
      sdk.close();
      await hub.cleanup();
    }
  }
});

test("tokenProvider 500/red => backoff y reintenta", { timeout: 20000 }, async () => {
  const hub = await setupHub();
  let intentos = 0;
  const sdk = clientSdk(hub, "user-500", {
    tokenProvider: async () => {
      intentos += 1;
      if (intentos === 1) {
        throw Object.assign(new Error("boom 500"), { status: 500 });
      }
      const j = await hub.mint("user-500");
      return { token: j.data.token, expires_in: j.data.expires_in };
    },
  });
  const statuses = [];
  sdk.on("status", (s) => statuses.push(s));
  try {
    await waitStatus(sdk, "connected", 12000);
    assert.ok(statuses.includes("reconnecting"), `debe pasar por reconnecting: ${JSON.stringify(statuses)}`);
    assert.ok(intentos >= 2, "debe reintentar el tokenProvider");
  } finally {
    sdk.close();
    await hub.cleanup();
  }
});

test("throttle de tokenProvider respeta 5 req/s", () => {
  assert.ok(_MIN_TOKEN_INTERVAL_MS >= 200, ">=200 ms para no superar 5 req/s");
  assert.equal(_classifyTokenError({ status: 401 }), "fatal");
  assert.equal(_classifyTokenError({ status: 403 }), "fatal");
  assert.equal(_classifyTokenError({ status: 500 }), "retry");
  assert.equal(_classifyTokenError(new Error("network down")), "retry");
});

test("dedupe acotado a los últimos 1000 ids", () => {
  const d = new _RecentIds(1000);
  for (let i = 0; i < 1200; i++) assert.equal(d.first(`id-${i}`), true);
  assert.equal(d.size, 1000);
  assert.equal(d.first("id-0"), true, "el más viejo fue expulsado");
  assert.equal(d.first("id-1199"), false, "los recientes se recuerdan");
});

test("close() limpia timers y no deja reconexiones", { timeout: 20000 }, async () => {
  const hub = await setupHub();
  let llamadas = 0;
  const sdk = clientSdk(hub, "user-close", {
    tokenProvider: async () => {
      llamadas += 1;
      const j = await hub.mint("user-close");
      return { token: j.data.token, expires_in: j.data.expires_in };
    },
  });
  const statuses = [];
  sdk.on("status", (s) => statuses.push(s));
  try {
    await waitStatus(sdk, "connected");
    const antes = llamadas;
    sdk.close();
    await sleep(1500);
    assert.equal(sdk.status, "closed");
    assert.equal(llamadas, antes, "no debe pedir más tokens tras close()");
    assert.ok(!statuses.slice(statuses.indexOf("closed")).includes("reconnecting"));
    assert.ok(statuses.includes("closed"));
  } finally {
    sdk.close();
    await hub.cleanup();
  }
});
