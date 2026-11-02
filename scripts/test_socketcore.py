#!/usr/bin/env python3
"""Tests de scripts/socketcore.py con un hub simulado (sin Docker ni red)."""

import contextlib
import io
import json
import os
import re
import sys
import tempfile
import types
import unittest
from unittest import mock

AQUI = os.path.dirname(os.path.abspath(__file__))
sys.path.insert(0, AQUI)
import socketcore as sc  # noqa: E402

MASTER = "MASTER-DE-PRUEBA-NO-REAL"

KEYS = [
    {
        "id": "id-alfa",
        "name": "alfa",
        "domains": ["https://app.ejemplo.test"],
        "channel_prefixes": ["alfa:*"],
        "scopes": ["publish", "subscribe"],
        "rate_limit_per_min": 120,
        "expires_at": None,
        "revoked": False,
        "created_at": 1,
    },
    {
        "id": "id-beta",
        "name": "beta",
        "domains": ["*"],
        "channel_prefixes": ["beta:*"],
        "scopes": ["publish", "subscribe", "manage"],
        "rate_limit_per_min": 120,
        "expires_at": 1893456000,
        "revoked": True,
        "created_at": 2,
    },
]
STATS = {
    "projects": [
        {"project_name": "alfa", "channels": 3, "connections": 2, "clients": 2},
        {"project_name": "beta", "channels": 0, "connections": 0, "clients": 0},
    ]
}


class HubFalso:
    """Sustituye a subprocess.run: interpreta la configuración de curl recibida por stdin."""

    def __init__(self):
        self.llamadas = []  # (metodo, ruta, cuerpo, cabeceras)
        self.argvs = []

    def run(self, cmd, input=None, capture_output=False, **kw):
        self.argvs.append(list(cmd))
        cfg = (input or b"").decode()
        url = re.search(r'^url = "(.*)"$', cfg, re.M).group(1)
        metodo = re.search(r'^request = "(.*)"$', cfg, re.M).group(1)
        cab = dict(re.findall(r'^header = "([^:]+): (.*)"$', cfg, re.M))
        m = re.search(r'^data = "(.*)"$', cfg, re.M)
        cuerpo = (
            json.loads(m.group(1).replace('\\"', '"').replace("\\\\", "\\"))
            if m
            else None
        )
        ruta = url.replace(sc.ADM, "")
        self.llamadas.append((metodo, ruta, cuerpo, cab))
        if ruta == "/admin/keys" and metodo == "GET":
            data = {"success": True, "data": KEYS}
        elif ruta == "/admin/stats":
            data = {"success": True, "data": STATS}
        elif metodo == "DELETE":
            data = {"success": True, "data": {"deleted": True, "channels_deleted": 3}}
        else:
            data = {"success": True, "data": {}}
        salida = (json.dumps(data) + "\n200").encode()
        return types.SimpleNamespace(stdout=salida, stderr=b"", returncode=0)

    def mutaciones(self):
        return [c for c in self.llamadas if c[0] != "GET"]


class PruebasOperador(unittest.TestCase):
    def correr(self, *argv, respuestas=()):
        hub = HubFalso()
        out = io.StringIO()
        with tempfile.TemporaryDirectory() as d:
            with open(os.path.join(d, ".env"), "w", encoding="utf-8") as f:
                f.write(f"MASTER_KEY={MASTER}\n")
            with mock.patch.object(sc.subprocess, "run", hub.run), mock.patch.object(
                sc, "REPO", d
            ), mock.patch.object(sc, "HOME", d), mock.patch(
                "builtins.input", side_effect=list(respuestas)
            ), mock.patch.object(
                sys, "argv", ["socketcore.py", *argv]
            ), contextlib.redirect_stdout(
                out
            ):
                try:
                    sc.main()
                    codigo = 0
                except SystemExit as e:
                    codigo = e.code
        return out.getvalue(), hub, codigo

    def test_master_no_aparece_en_argv_y_viaja_por_stdin(self):
        _, hub, _ = self.correr("listar")
        for argv in hub.argvs:
            self.assertNotIn(MASTER, " ".join(argv))
            self.assertIn("-K", argv)
        self.assertTrue(all(c[3].get("X-Master-Key") == MASTER for c in hub.llamadas))

    def test_listar_muestra_proyectos_sin_secretos(self):
        out, hub, codigo = self.correr("listar")
        self.assertEqual(codigo, 0)
        self.assertIn("alfa", out)
        self.assertIn("REVOCADA", out)
        self.assertIn("canales: 3", out)
        self.assertNotIn("sc_live_", out)
        self.assertNotIn(MASTER, out)
        self.assertEqual(hub.mutaciones(), [])

    def test_revocar_cancelado_no_modifica_nada(self):
        _, hub, _ = self.correr("revocar", "alfa", respuestas=["n"])
        self.assertEqual(hub.mutaciones(), [])

    def test_revocar_confirmado_llama_al_endpoint(self):
        _, hub, _ = self.correr("revocar", "alfa", respuestas=["s"])
        self.assertEqual(
            [(m, r) for m, r, _, _ in hub.mutaciones()],
            [("POST", "/admin/keys/id-alfa/revoke")],
        )

    def test_revocar_ya_revocada_no_pregunta(self):
        _, hub, codigo = self.correr("revocar", "beta")
        self.assertIn("ya está revocada", str(codigo))
        self.assertEqual(hub.mutaciones(), [])

    def test_borrar_exige_escribir_el_nombre(self):
        _, hub, _ = self.correr("borrar", "alfa", respuestas=["s"])
        self.assertEqual(hub.mutaciones(), [])
        out, hub, _ = self.correr("borrar", "alfa", respuestas=["alfa"])
        self.assertEqual(
            [(m, r) for m, r, _, _ in hub.mutaciones()],
            [("DELETE", "/admin/keys/id-alfa")],
        )
        self.assertIn("canales eliminados: 3", out)

    def test_editar_solo_envia_lo_pedido(self):
        _, hub, _ = self.correr("editar", "alfa", "--rate", "300", respuestas=["s"])
        m = hub.mutaciones()
        self.assertEqual(len(m), 1)
        self.assertEqual(
            (m[0][0], m[0][1], m[0][2]),
            ("PATCH", "/admin/keys/id-alfa", {"rate_limit_per_min": 300}),
        )

    def test_editar_scopes_avisa_de_que_corta_conexiones(self):
        out, hub, _ = self.correr(
            "editar", "alfa", "--scopes", "publish,subscribe,manage", respuestas=["s"]
        )
        self.assertIn("CORTA las conexiones", out)
        self.assertEqual(
            hub.mutaciones()[0][2], {"scopes": ["publish", "subscribe", "manage"]}
        )

    def test_editar_sin_opciones_falla_sin_tocar_nada(self):
        _, hub, codigo = self.correr("editar", "alfa")
        self.assertIn("Indica qué cambiar", str(codigo))
        self.assertEqual(hub.mutaciones(), [])

    def test_proyecto_inexistente(self):
        _, hub, codigo = self.correr("borrar", "no-existe")
        self.assertIn("No existe el proyecto", str(codigo))
        self.assertEqual(hub.mutaciones(), [])

    def test_sin_dominio_ni_ruta_de_servidor_escritos_en_el_codigo(self):
        with open(os.path.join(AQUI, "socketcore.py"), encoding="utf-8") as f:
            fuente = f.read()
        self.assertIsNone(
            re.search(r"https?://[A-Za-z0-9-]+(\.[A-Za-z0-9-]+)*\.[A-Za-z]{2,}", fuente)
        )
        self.assertNotIn("/srv/", fuente)


if __name__ == "__main__":
    unittest.main()
