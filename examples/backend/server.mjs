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

const HUB = process.env.SOCKET_CORE_URL || "http://127.0.0.1:3005";
const API_KEY = process.env.SOCKET_API_KEY;
const CHANNEL = process.env.SOCKET_CHANNEL || "demo:chat";
const PORT = Number(process.env.PORT || 8080);
const CREATE_CHANNEL = (process.env.SOCKET_CREATE_CHANNEL || "true") !== "false";

if (!API_KEY) {
  console.error("Falta SOCKET_API_KEY (la API key del proyecto; solo backend).");
  process.exit(1);
}

const app = express();
app.use(express.json());
app.use("/sdk", express.static(join(__dirname, "..", "..", "sdk", "js")));
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
  try {
    const r = await fetch(`${HUB}/auth/token`, {
      method: "POST",
      headers: { "content-type": "application/json", "x-api-key": API_KEY },
      body: JSON.stringify({ user_id: user.id, channel: CHANNEL, role: "both" }),
    });
    const j = await r.json().catch(() => ({}));
    if (!r.ok || !j.success) {
      return res.status(502).json({ error: j.error || `hub respondió ${r.status}` });
    }
    // El navegador recibe SOLO el JWT de corta vida.
    res.json({
      token: j.data.token,
      expires_in: j.data.expires_in,
      channel: CHANNEL,
      url: HUB,
    });
  } catch (e) {
    res.status(502).json({ error: `no se pudo contactar con el hub: ${e.message}` });
  }
});

async function ensureChannel() {
  if (!CREATE_CHANNEL) return;
  try {
    const r = await fetch(`${HUB}/channels`, {
      method: "POST",
      headers: { "content-type": "application/json", "x-api-key": API_KEY },
      body: JSON.stringify({ name: CHANNEL, description: "canal de ejemplo" }),
    });
    if (r.status === 201) console.log(`[hub] canal ${CHANNEL} creado`);
    else if (r.status === 400) console.log(`[hub] canal ${CHANNEL} ya existe`);
    else if (r.status === 403)
      console.warn(
        `[hub] la key no tiene scope 'manage': crea el canal ${CHANNEL} manualmente`,
      );
    else console.warn(`[hub] al crear canal: HTTP ${r.status}`);
  } catch (e) {
    console.warn(`[hub] no se pudo verificar el canal: ${e.message}`);
  }
}

app.listen(PORT, async () => {
  console.log(`Ejemplo en http://127.0.0.1:${PORT}  (hub: ${HUB}, canal: ${CHANNEL})`);
  await ensureChannel();
});
