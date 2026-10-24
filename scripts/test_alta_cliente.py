#!/usr/bin/env python3
"""Tests de las piezas puras de alta_cliente.py (sin red ni interacción).

Ejecutar:
    python3 -m unittest discover -s scripts -p "test_*.py" -v
"""

import importlib.util
import os
import stat
import sys
import tempfile
import unittest
from pathlib import Path

AQUI = Path(__file__).resolve().parent
_spec = importlib.util.spec_from_file_location("alta_cliente", AQUI / "alta_cliente.py")
ac = importlib.util.module_from_spec(_spec)
sys.modules["alta_cliente"] = ac
_spec.loader.exec_module(ac)


class TestPaquete(unittest.TestCase):
    def test_scopes_minimos_por_defecto(self):
        scopes = ac.default_scopes()
        self.assertEqual(scopes, ["publish", "subscribe"])
        self.assertNotIn("manage", scopes)

    def test_paquete_contiene_datos_y_snippet(self):
        texto = ac.render_package(
            project="demo",
            key_id="kid-1",
            api_key="sc_live_secreta",
            public_url="https://hub.example",
            admin_url="http://127.0.0.1:3015",
            domains=["https://app.example"],
            prefix="demo:*",
            scopes=["publish", "subscribe"],
            examples=["https://app.example"],
        )
        for esperado in [
            "demo",
            "kid-1",
            "sc_live_secreta",
            "https://hub.example",
            "demo:*",
            "https://app.example",
            "X-Api-Key",
            "/auth/token",
            "una sola vez",
        ]:
            self.assertIn(esperado, texto)

    def test_escribe_paquete_con_0600(self):
        with tempfile.TemporaryDirectory() as d:
            ruta = Path(d) / "paquete.txt"
            ac.write_package(str(ruta), "contenido secreto")
            self.assertEqual(ruta.read_text(encoding="utf-8"), "contenido secreto")
            if os.name == "posix":
                modo = stat.S_IMODE(ruta.stat().st_mode)
                self.assertEqual(modo, 0o600)

    def test_hint_de_solape_legible(self):
        cuerpo = '{"success":false,"error":"channel_prefix solapa con el de otro proyecto"}'
        texto = ac.explain_create_error(400, cuerpo)
        self.assertIn("solapa", texto.lower())
        self.assertIn("--list-prefixes", texto)


if __name__ == "__main__":
    unittest.main()
