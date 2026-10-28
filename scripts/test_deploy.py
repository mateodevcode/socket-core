#!/usr/bin/env python3
"""Tests de scripts/deploy.sh con binarios simulados (git/docker/curl/nginx/...).

Ejecuta el script real en un DEPLOY_ROOT temporal. No toca datos reales.
"""

import os
import shutil
import sqlite3
import subprocess
import tempfile
import unittest
from pathlib import Path

AQUI = Path(__file__).resolve().parent
DEPLOY = AQUI / "deploy.sh"
HELPER = AQUI / "sqlite_backup.py"

FAKES = r"""#!/usr/bin/env bash
# Funciones que simulan binarios externos (se cargan con BASH_ENV).
git() {
  case "${1:-}" in
    fetch) [ "${FAKE_FETCH_FAIL:-0}" = "1" ] && return 1; return 0 ;;
    rev-parse) cat "$FAKE_STATE/head" ;;
    reset)
      target="${3:-}"
      if [[ "$target" == origin/* ]]; then cat "$FAKE_STATE/origin" > "$FAKE_STATE/head"
      elif [ "$target" = "HEAD" ]; then :
      else echo "$target" > "$FAKE_STATE/head"; fi ;;
    show)
      case "$2" in
        *sqlite_backup.py) cat "$FAKE_HELPER" ;;
        *deploy.sh) printf '%s' "$FAKE_DEPLOY" ;;
      esac ;;
    *) : ;;
  esac
  return 0
}
docker() {
  case "$*" in
    *"compose build"*) return "${FAKE_BUILD_FAIL:-0}" ;;
    *"exec socket-core curl"*) return "${FAKE_ADMIN_HEALTH:-0}" ;;
    *"compose ps"*) echo "socket-core  Up"; return 0 ;;
    *"compose logs"*) echo "fake logs"; return 0 ;;
    *"rm -f socket-core"*) return 0 ;;
    *"compose up"*|*"compose down"*|*"compose stop"*|*"image prune"*) return 0 ;;
    *) return 0 ;;
  esac
}
curl() {
  local url="" a
  for a in "$@"; do case "$a" in http*) url="$a" ;; esac; done
  case "$url" in
    *"3005/health"*)
      local n
      n="$(cat "$FAKE_STATE/health_fail" 2>/dev/null || echo 0)"
      if [ "$n" -gt 0 ]; then echo $((n - 1)) > "$FAKE_STATE/health_fail"; return 1; fi
      return 0 ;;
    *) return "${FAKE_PUBLIC_HEALTH:-0}" ;;
  esac
}
nginx() { return "${FAKE_NGINX_FAIL:-0}"; }
systemctl() { return 0; }
certbot() { return 0; }
fail2ban-client() { return 0; }
python3() { "$REAL_PYTHON" "$@"; }
"""


def find_bash():
    for c in [
        os.environ.get("BASH"),
        r"C:\Program Files\Git\bin\bash.exe",
        r"C:\Program Files\Git\usr\bin\bash.exe",
    ]:
        if c and Path(c).is_file():
            return c
    return shutil.which("bash") or "bash"


class Scenario:
    def __init__(self):
        self.tmp = tempfile.TemporaryDirectory()
        self.root = Path(self.tmp.name)
        self.state = self.root / "state"
        self.state.mkdir()
        self.fakes = self.root / "fakes.sh"
        self.fakes.write_text(FAKES, encoding="utf-8", newline="")
        self.nginx_avail = self.root / "etc/nginx/sites-available"
        self.nginx_enabled = self.root / "etc/nginx/sites-enabled"
        self.nginx_confd = self.root / "etc/nginx/conf.d"
        for d in (self.nginx_avail, self.nginx_enabled, self.nginx_confd):
            d.mkdir(parents=True)
        (self.root / "scripts").mkdir()
        shutil.copy(HELPER, self.root / "scripts/sqlite_backup.py")
        (self.root / ".env").write_text("DUMMY=1\n", encoding="utf-8")
        (self.root / "data").mkdir()
        self.db = self.root / "data/socket-core.db"
        con = sqlite3.connect(self.db)
        con.execute("CREATE TABLE marker (v TEXT)")
        con.execute("INSERT INTO marker VALUES ('A')")
        con.commit()
        con.close()
        (self.root / "nginx/conf.d").mkdir(parents=True)
        (self.root / "nginx/ws.seventwo.tech.http.conf").write_text("NEW-HTTP\n")
        (self.root / "nginx/ws.seventwo.tech.https.conf").write_text("NEW-HTTPS\n")
        (self.root / "nginx/conf.d/socketcore-limits.conf").write_text("NEW-LIMITS\n")
        (self.root / "fail2ban/filter.d").mkdir(parents=True)
        (self.root / "fail2ban/jail.d").mkdir(parents=True)
        (self.root / "fail2ban/filter.d/socketcore-auth.conf").write_text("F\n")
        (self.root / "fail2ban/jail.d/socketcore-auth.conf").write_text("J\n")
        # nginx previos "OLD"
        for name in ("ws.seventwo.tech.http.conf", "ws.seventwo.tech.https.conf"):
            (self.nginx_avail / name).write_text("OLD\n")
        (self.nginx_confd / "socketcore-limits.conf").write_text("OLD\n")
        self.set_head("A")
        self.set_origin("A")

    def set_head(self, v):
        (self.state / "head").write_text(v + "\n")

    def set_origin(self, v):
        (self.state / "origin").write_text(v + "\n")

    def head(self):
        return (self.state / "head").read_text().strip()

    def last_good(self):
        p = self.root / ".last_good_commit"
        return p.read_text().strip() if p.is_file() else None

    def marker(self):
        con = sqlite3.connect(self.db)
        try:
            return con.execute("SELECT v FROM marker").fetchone()[0]
        finally:
            con.close()

    def nginx_content(self, name="ws.seventwo.tech.http.conf"):
        return (self.nginx_avail / name).read_text().strip()

    def run(self, extra_env=None):
        env = {
            **os.environ,
            "BASH_ENV": str(self.fakes),
            "DEPLOY_ROOT": str(self.root),
            "DEPLOY_BRANCH": "main",
            "SUDO": "",
            "BACKUP_DIR": str(self.root / "backups"),
            "DB_FILE": str(self.db),
            "PUBLIC_URL": "https://fake.example",
            "NGINX_AVAILABLE": str(self.nginx_avail),
            "NGINX_ENABLED": str(self.nginx_enabled),
            "NGINX_CONF_D": str(self.nginx_confd),
            "FAIL2BAN_FILTER": str(self.root / "etc/fail2ban/filter.d"),
            "FAIL2BAN_JAIL": str(self.root / "etc/fail2ban/jail.d"),
            "MAX_HEALTH_ATTEMPTS": "1",
            "HEALTH_SLEEP": "0",
            "SKIP_CERTBOT": "1",
            "FAKE_STATE": str(self.state),
            "FAKE_HELPER": str(HELPER),
            "FAKE_DEPLOY": DEPLOY.read_text(encoding="utf-8"),
            "REAL_PYTHON": shutil.which("python") or "python",
            "FAKE_BUILD_FAIL": "0",
            "FAKE_ADMIN_HEALTH": "0",
            "FAKE_NGINX_FAIL": "0",
            "FAKE_PUBLIC_HEALTH": "0",
        }
        for d in (self.root / "etc/fail2ban/filter.d", self.root / "etc/fail2ban/jail.d"):
            d.mkdir(parents=True, exist_ok=True)
        hf = str((extra_env or {}).get("FAKE_HEALTH_FAIL", "0"))
        (self.state / "health_fail").write_text(hf)
        if extra_env:
            env.update({k: str(v) for k, v in extra_env.items()})
        return subprocess.run(
            [find_bash(), str(DEPLOY)],
            env=env,
            capture_output=True,
            text=True,
            encoding="utf-8",
            errors="replace",
            timeout=120,
        )

    def close(self):
        self.tmp.cleanup()


class TestDeploy(unittest.TestCase):
    def setUp(self):
        self.s = Scenario()

    def tearDown(self):
        self.s.close()

    def test_deploy_ok_marca_last_good(self):
        r = self.s.run()
        self.assertEqual(r.returncode, 0, r.stdout + r.stderr)
        self.assertEqual(self.s.last_good(), "A")
        self.assertEqual(self.s.marker(), "A")
        backups = list((self.s.root / "backups").glob("data-*.db"))
        self.assertEqual(len(backups), 1)
        self.assertEqual(self.s.nginx_content(), "NEW-HTTP")

    def test_build_fallido_no_toca_last_good(self):
        # Primer deploy sano (A).
        r = self.s.run()
        self.assertEqual(r.returncode, 0, r.stdout + r.stderr)
        # Deploy B con build roto.
        self.s.set_origin("B")
        r = self.s.run({"FAKE_BUILD_FAIL": "1"})
        self.assertNotEqual(r.returncode, 0)
        self.assertEqual(self.s.last_good(), "A", "un build fallido NO actualiza last_good")

    def test_salud_fallida_vuelve_al_ultimo_bueno(self):
        # A sano.
        r = self.s.run()
        self.assertEqual(r.returncode, 0, r.stdout + r.stderr)
        # B: build falla (last_good sigue A).
        self.s.set_origin("B")
        r = self.s.run({"FAKE_BUILD_FAIL": "1"})
        self.assertNotEqual(r.returncode, 0)
        # C: build ok pero la salud falla la primera vez -> rollback a A.
        self.s.set_origin("C")
        r = self.s.run({"FAKE_BUILD_FAIL": "0", "FAKE_HEALTH_FAIL": "1"})
        self.assertNotEqual(r.returncode, 0, r.stdout + r.stderr)
        self.assertEqual(self.s.head(), "A", "rollback debe volver al commit bueno")
        self.assertEqual(self.s.last_good(), "A")
        self.assertEqual(self.s.marker(), "A", "DB restaurada desde el backup")
        self.assertIn("Rollback sano", r.stdout + r.stderr)

    def test_fetch_fallido_aborta_sin_tocar_nada(self):
        # R12: el fetch es lo primero; si falla no se crea .last_good ni backup.
        r = self.s.run({"FAKE_FETCH_FAIL": "1"})
        self.assertNotEqual(r.returncode, 0, r.stdout + r.stderr)
        self.assertIsNone(self.s.last_good(), "no debe inicializar .last_good si falla el fetch")
        self.assertEqual(self.s.head(), "A")
        self.assertEqual(self.s.marker(), "A")
        self.assertEqual(self.s.nginx_content(), "OLD")
        self.assertEqual(list((self.s.root / "backups").glob("data-*.db")), [])

    def test_primer_deploy_falla_salud_rollback_al_previo(self):
        # No existe .last_good_commit: debe inicializarse con el HEAD previo (A)
        # y, al fallar la salud del primer deploy (B), volver a A y restaurar datos.
        self.assertIsNone(self.s.last_good())
        self.s.set_origin("B")
        r = self.s.run({"FAKE_HEALTH_FAIL": "1"})
        self.assertNotEqual(r.returncode, 0, r.stdout + r.stderr)
        self.assertEqual(self.s.head(), "A", "rollback al HEAD previo al fetch")
        self.assertEqual(self.s.last_good(), "A", "inicializado con el commit previo")
        self.assertEqual(self.s.marker(), "A", "DB restaurada aunque el fichero no existiera")
        self.assertIn("inicializado con el commit previo", r.stdout)
        self.assertIn("Rollback sano", r.stdout + r.stderr)

    def test_deploy_ref_despliega_otra_rama_sin_main(self):
        r = self.s.run({"DEPLOY_REF": "security-hardening"})
        self.assertEqual(r.returncode, 0, r.stdout + r.stderr)
        self.assertIn("Ref a desplegar: security-hardening", r.stdout)
        self.assertEqual(self.s.last_good(), "A")

    def test_nginx_fallido_restaura_configuracion(self):
        r = self.s.run({"FAKE_NGINX_FAIL": "1"})
        self.assertNotEqual(r.returncode, 0, r.stdout + r.stderr)
        self.assertEqual(self.s.nginx_content(), "OLD", "debe restaurar el fichero previo")
        # Primer despliegue: se inicializó con el HEAD previo (A), pero NO se
        # marca como bueno el commit nuevo si nginx falla.
        self.assertEqual(self.s.last_good(), "A", "no se marca bueno si nginx falla")

    def test_sin_copia_de_env_a_tmp_y_umask(self):
        texto = DEPLOY.read_text(encoding="utf-8")
        self.assertNotIn(".env.bak", texto)
        self.assertNotIn("cp .env /tmp", texto)
        self.assertIn("umask 077", texto)

    def test_reejecuta_si_el_script_de_origin_difiere(self):
        nuevo = DEPLOY.read_text(encoding="utf-8") + "\n# origen-v2\n"
        r = self.s.run({"FAKE_DEPLOY": nuevo})
        self.assertEqual(r.returncode, 0, r.stdout + r.stderr)
        self.assertIn("re-ejecutando", r.stdout)
        self.assertEqual(self.s.last_good(), "A")


if __name__ == "__main__":
    unittest.main()
