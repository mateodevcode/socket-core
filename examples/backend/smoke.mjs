// Prueba de humo real: login -> /socket-token -> WS -> mensaje ida y vuelta.
// Requiere el backend de ejemplo corriendo y ENV SMOKE_BACKEND (default 127.0.0.1:8080).
const { connect } = await import(new URL("../../sdk/js/socket-core.mjs", import.meta.url));

const BASE = process.env.SMOKE_BACKEND || "http://127.0.0.1:8080";
const texto = `smoke-${Math.random().toString(36).slice(2, 8)}`;

const login = await fetch(`${BASE}/login`, {
  method: "POST",
  headers: { "content-type": "application/json" },
  body: JSON.stringify({ name: "smoke" }),
});
if (!login.ok) throw new Error(`login ${login.status}`);
const cookie = (login.headers.get("set-cookie") || "").split(";")[0];
const user = (await login.json()).user;
console.log("login OK:", user.id.startsWith("u-") ? "u-***" : user.id);

const tokRes = await fetch(`${BASE}/socket-token`, {
  method: "POST",
  headers: { cookie },
});
if (!tokRes.ok) throw new Error(`socket-token ${tokRes.status}`);
const tok = await tokRes.json();
console.log("token OK: expires_in =", tok.expires_in, "url =", tok.url);

let usado = false;
const sc = connect({
  url: tok.url,
  channel: tok.channel,
  device: "smoke",
  tokenProvider: async () => {
    if (!usado) {
      usado = true;
      return { token: tok.token, expires_in: tok.expires_in };
    }
    const r = await fetch(`${BASE}/socket-token`, { method: "POST", headers: { cookie } });
    const j = await r.json();
    return { token: j.token, expires_in: j.expires_in };
  },
});

const echo = new Promise((resolve, reject) => {
  const t = setTimeout(() => reject(new Error("timeout esperando el mensaje")), 8000);
  sc.on("message", (m) => {
    if (m.payload?.text === texto) {
      clearTimeout(t);
      resolve(m);
    }
  });
  sc.on("fatal", (f) => reject(new Error(`fatal ${f.code}`)));
});

await new Promise((resolve, reject) => {
  const t = setTimeout(() => reject(new Error("timeout conexión")), 8000);
  sc.on("status", (s) => {
    if (s === "connected") {
      clearTimeout(t);
      resolve();
    }
  });
});
console.log("WS conectado");
await sc.publish({ type: "chat", text: texto, from: "smoke" });
const m = await echo;
console.log("mensaje ida y vuelta OK:", JSON.stringify(m.payload));
sc.close();
process.exit(0);
