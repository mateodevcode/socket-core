// Mini backend de ejemplo (Express).
// - /login: "login simulado" (sin contraseña) que asigna un ID OPACO por sesión.
// - /socket-token: usa la API key (SOLO aquí, en el servidor) para pedir el JWT
//   a socket-core y devuelve { token, expires_in, channel, url } al navegador.
//
// La API key NUNCA se envía al frontend.
import express from "express";
import { randomUUID } from "node:crypto";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";

const __dirname = dirname(fileURLToPath(import.meta.url));

import { canAccess } from "./policy.mjs";

function requireEnv(name) {
  const v = process.env[name];
  if (!v) {
    console.error(`Falta ${name} (obligatoria).`);
    process.exit(1);
  }
  return v;
}

// Backend -> hub (interno) y URL pública que recibe el navegador (puede diferir).
const HUB = requireEnv("SOCKET_CORE_URL");
const PUBLIC_URL = requireEnv("SOCKET_PUBLIC_URL");
const API_KEY = requireEnv("SOCKET_API_KEY");
const CHANNEL = process.env.SOCKET_CHANNEL || "demo:chat";
const PORT = Number(process.env.PORT || 8080);

const app = express();
app.use(express.json());
// Solo el fichero del SDK (no todo el directorio sdk/js).
app.get("/sdk/socket-core.mjs", (_req, res) => {
  res.sendFile(join(__dirname, "..", "..", "sdk", "js", "socket-core.mjs"));
});
app.use(express.static(join(__dirname, "..", "frontend")));

// Sesión de demo en memoria: cookie HttpOnly con un ID opaco (NO emails).
const users = new Map();
function sessionUser(req) {
  const m = /(?:^|;\s*)uid=([^;]+)/.exec(req.headers.cookie || "");
  if (!m) return null;
  const uid = decodeURIComponent(m[1]);
  return users.has(uid) ? { id: uid, name: users.get(uid) } : null;
}

app.post("/login", (req, res) => {
  const name = String(req.body?.name || "").trim().slice(0, 40);
  if (!name) return res.status(400).json({ error: "name requerido" });
  const uid = `u-${randomUUID()}`; // ID opaco; nunca un email
  users.set(uid, name);
  res.setHeader(
    "Set-Cookie",
    `uid=${encodeURIComponent(uid)}; Path=/; HttpOnly; SameSite=Lax`,
  );
  res.json({ user: { id: uid, name } });
});

app.get("/me", (req, res) => {
  const user = sessionUser(req);
  if (!user) return res.status(401).json({ error: "no has iniciado sesión" });
  res.json({ user });
});

app.get("/presence", async (req, res) => {
  const user = sessionUser(req);
  if (!user) return res.status(401).json({ error: "no has iniciado sesión" });
  try {
    const r = await fetch(`${HUB}/channels/${encodeURIComponent(CHANNEL)}/clients`, {
      headers: { "x-api-key": API_KEY },
    });
    if (!r.ok) return res.status(502).json({ error: "presence_unavailable" });
    const j = await r.json();
    // Solo lo mínimo: identidad opaca + conexión (sin device ni metadatos).
    const connections = (j.data || []).map((c) => ({
      user: c.id,
      connection_id: c.connection_id,
    }));
    res.json({ connections });
  } catch {
    res.status(502).json({ error: "presence_unavailable" });
  }
});

app.post("/socket-token", async (req, res) => {
  const user = sessionUser(req);
  if (!user) return res.status(401).json({ error: "no has iniciado sesión" });
  // Autorización explícita ANTES de emitir credenciales para el canal.
  // En una app real: comprobar membresía/rol/tenant, no solo el canal configurado.
  if (!canAccess(user, CHANNEL, CHANNEL)) {
    return res.status(403).json({ error: "forbidden" });
  }
  try {
    const r = await fetch(`${HUB}/auth/token`, {
      method: "POST",
      headers: { "content-type": "application/json", "x-api-key": API_KEY },
      body: JSON.stringify({ user_id: user.id, channel: CHANNEL, role: "both" }),
    });
    const j = await r.json().catch(() => ({}));
    if (!r.ok || !j.success) {
      // El detalle del hub se queda en el servidor; al navegador solo un código.
      console.error(`[hub] /auth/token ${r.status}: ${j.error || "sin detalle"}`);
      return res.status(502).json({ error: "token_unavailable" });
    }
    // El navegador recibe SOLO el JWT de corta vida y la URL PÚBLICA.
    res.json({
      token: j.data.token,
      expires_in: j.data.expires_in,
      channel: CHANNEL,
      url: PUBLIC_URL,
    });
  } catch (e) {
    console.error(`[hub] /auth/token error: ${e.message}`);
    res.status(502).json({ error: "token_unavailable" });
  }
});

app.listen(PORT, () => {
  console.log(`Ejemplo en http://127.0.0.1:${PORT}  (hub: ${HUB}, canal: ${CHANNEL})`);
});
