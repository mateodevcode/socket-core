#!/usr/bin/env python3
"""Tests de scripts/alta_cliente.py y estado.py (U1/U2/U3).

Ejecutar:
    python3 -m unittest discover -s scripts -p "test_*.py" -v
"""

import argparse
import importlib.util
import os
import stat
import subprocess
import sys
import tempfile
import types
import unittest
from pathlib import Path
from unittest import mock

AQUI = Path(__file__).resolve().parent


def _load(nombre, archivo):
    spec = importlib.util.spec_from_file_location(nombre, AQUI / archivo)
    mod = importlib.util.module_from_spec(spec)
    sys.modules[nombre] = mod
    spec.loader.exec_module(mod)
    return mod


ac = _load("alta_cliente", "alta_cliente.py")
estado = _load("estado_mod", "estado.py")

SECRETO = "master-super-secreta-que-no-debe-ir-en-argv-123"


class TestPaquete(unittest.TestCase):
    def test_scopes_minimos_por_defecto(self):
        self.assertEqual(ac.default_scopes(), ["publish", "subscribe"])

    def test_paquete_para_proyecto_sin_admin_ni_container(self):
        texto = ac.render_package(
            project="demo",
            key_id="kid-1",
            api_key="sc_live_secreta",
            public_url="https://hub.example",
            domains=["https://app.example"],
            prefix="demo:*",
            scopes=["publish", "subscribe"],
        )
        for esperado in ["demo", "kid-1", "sc_live_secreta", "https://hub.example", "demo:*", "X-Api-Key", "/auth/token", "una sola vez"]:
            self.assertIn(esperado, texto)
        for prohibido in ["ADMIN_EXEC", "3015", "MASTER_KEY", "docker exec"]:
            self.assertNotIn(prohibido, texto)

    def test_ficha_operador_separada(self):
        texto = ac.render_operator_sheet("demo", "kid-1", "demo:*", "docker exec ... 127.0.0.1:3015")
        self.assertIn("NO ENTREGAR", texto)
        self.assertIn("kid-1", texto)
        self.assertIn("3015", texto)
        self.assertNotIn("sc_live_", texto)

    def test_escribe_paquete_con_0600(self):
        with tempfile.TemporaryDirectory() as d:
            ruta = Path(d) / "paquete.txt"
            ac.write_package(str(ruta), "contenido secreto")
            self.assertEqual(ruta.read_text(encoding="utf-8"), "contenido secreto")
            if os.name == "posix":
                self.assertEqual(stat.S_IMODE(ruta.stat().st_mode), 0o600)

    def test_hint_de_solape_legible(self):
        cuerpo = '{"success":false,"error":"channel_prefix solapa con el proyecto \'otra-key\' (incluye keys revocadas; bórrala o cámbiale el prefijo para liberarlo)"}'
        texto = ac.explain_create_error(400, cuerpo)
        self.assertIn("solapa", texto.lower())
        self.assertIn("otra-key", texto)
        self.assertIn("revocada", texto.lower())
        self.assertIn("--list-prefixes", texto)


class TestSalidaSegura(unittest.TestCase):
    def test_crea_dir_0700_y_fuera_del_repo(self):
        with tempfile.TemporaryDirectory() as d:
            out = Path(d) / "cred"
            ok, ruta = ac.ensure_safe_out(out, "credenciales-x.txt")
            self.assertTrue(ok, ruta)
            if os.name == "posix":
                self.assertEqual(stat.S_IMODE(out.stat().st_mode), 0o700)

    def test_rechaza_worktree_si_no_esta_ignorado(self):
        with tempfile.TemporaryDirectory() as d:
            repo = Path(d)
            subprocess.run(["git", "init", "-q", str(repo)], check=True)
            ok, msg = ac.ensure_safe_out(repo, "credenciales-x.txt")
            self.assertFalse(ok)
            self.assertIn("NO está ignorado", msg)
            (repo / ".gitignore").write_text("credenciales-*.txt\n", encoding="utf-8")
            ok, _ = ac.ensure_safe_out(repo, "credenciales-x.txt")
            self.assertTrue(ok)

    def test_default_out_fuera_del_repo(self):
        self.assertEqual(ac.default_out_dir(), Path.home() / "credenciales-socketcore")


class TestValidaciones(unittest.TestCase):
    def test_rate_y_exp(self):
        self.assertFalse(ac.parse_entero("0", "rate", 1, 100000)[0])
        self.assertFalse(ac.parse_entero("-1", "rate", 1, 100000)[0])
        self.assertFalse(ac.parse_entero("100001", "rate", 1, 100000)[0])
        self.assertFalse(ac.parse_entero("abc", "rate", 1, 100000)[0])
        self.assertTrue(ac.parse_entero("120", "rate", 1, 100000)[0])
        self.assertTrue(ac.parse_entero("", "exp", 1, 3650, permitir_vacio=True)[0])
        self.assertFalse(ac.parse_entero("3651", "exp", 1, 3650)[0])
        self.assertFalse(ac.parse_entero("-1", "exp", 1, 3650)[0])

    def test_core_sin_dominio_fijo(self):
        with mock.patch.dict(os.environ, {}, clear=False):
            os.environ.pop("SOCKET_PUBLIC_URL", None)
            self.assertEqual(ac.default_core(), "")
        with mock.patch.dict(os.environ, {"SOCKET_PUBLIC_URL": "https://x.example"}):
            self.assertEqual(ac.default_core(), "https://x.example")


class FakeRun:
    def __init__(self, stdout):
        self.stdout = stdout
        self.calls = []

    def __call__(self, argv, **kw):
        self.calls.append((argv, kw))
        return types.SimpleNamespace(returncode=0, stdout=self.stdout, stderr="")


class TestSecretosFueraDeArgv(unittest.TestCase):
    def test_alta_docker_exec_no_pone_master_en_argv(self):
        fake = FakeRun('{"ok":true}\n201')
        args = argparse.Namespace(admin="http://127.0.0.1:3015")
        with mock.patch.object(ac.subprocess, "run", fake):
            status, _body, _via = ac._docker_exec(args, "POST", "/admin/keys", {"name": "x"}, SECRETO)
        self.assertEqual(status, 201)
        self.assertTrue(fake.calls)
        for argv, kw in fake.calls:
            self.assertNotIn(SECRETO, " ".join(map(str, argv)))
            self.assertTrue(kw.get("shell") is False)
            self.assertIn(SECRETO, kw.get("input", ""))
            self.assertIn("-K", argv)

    def test_estado_docker_curl_no_pone_master_en_argv(self):
        fake = FakeRun('{"ok":true}\n200')
        with mock.patch.object(estado.subprocess, "run", fake):
            code, _texto = estado.docker_curl_json(SECRETO, "3015", "/admin/stats")
        self.assertEqual(code, 200)
        for argv, kw in fake.calls:
            self.assertNotIn(SECRETO, " ".join(map(str, argv)))
            self.assertTrue(kw.get("shell") is False)
            self.assertIn(SECRETO, kw.get("input", ""))
            self.assertIn("-K", argv)

    def test_estado_ignora_ficheros_master_key(self):
        with tempfile.TemporaryDirectory() as d:
            Path(d, ".master_key").write_text("fichero-no-permitido\n", encoding="utf-8")
            cwd = os.getcwd()
            os.chdir(d)
            try:
                with mock.patch.dict(os.environ, {}, clear=False):
                    os.environ.pop("SOCKET_MASTER_KEY", None)
                    args = argparse.Namespace(master_key="")
                    self.assertEqual(estado.leer_master(args), "")
            finally:
                os.chdir(cwd)


if __name__ == "__main__":
    unittest.main()
