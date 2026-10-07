"""Small synthetic checks for the bounded FreePin3 topology decoder."""

import struct
import os
import unittest
from pathlib import Path

import py_aep
from aep_ir_export import Exporter


def state(indices, *, version=2, count=None):
    vertices = [(0, 0), (0, 1), (1, 0), (1, 1)]
    header = b"\x0a\xd0\xbe\xef" + struct.pack(">I", version) + b"\x01\0\0\0\0"
    header += struct.pack(">4I", len(vertices), len(indices) if count is None else count, 0, 0)
    return header + b"".join(struct.pack(">2d", *point) for point in vertices) + struct.pack(f">{len(indices)}I", *indices)


class PuppetMeshTest(unittest.TestCase):
    def test_edge_sextuples_decode_to_two_faces(self):
        data = state([0, 1, 0, 2, 1, 2, 1, 3, 1, 2, 3, 2], count=6)
        mesh = Exporter.puppet_mesh(data)
        self.assertEqual(mesh["triangles"], [(0, 2, 1), (1, 2, 3)])

    def test_direct_triples(self):
        mesh = Exporter.puppet_mesh(state([0, 2, 1, 1, 2, 3, 0, 2, 1, 1, 2, 3]))
        self.assertEqual(len(mesh["triangles"]), 4)

    def test_bad_version_and_bounds(self):
        self.assertIsNone(Exporter.puppet_mesh(state([0, 2, 1] * 4, version=3)))
        self.assertIsNone(Exporter.puppet_mesh(state([0, 2, 1] * 4, count=120_000)))

    @unittest.skipUnless(os.environ.get("EFFECTCRAFT_PUPPET_TREE_BRANCH_AEP"), "local corpus path not set")
    def test_tree_branch_five_pin_mesh(self):
        app = py_aep.parse(Path(os.environ["EFFECTCRAFT_PUPPET_TREE_BRANCH_AEP"]))
        for item in app.project:
            if not hasattr(item, "layers"):
                continue
            for layer in item.layers:
                if layer.name != "4 fingers":
                    continue
                stack = list(layer.properties)
                while stack:
                    group = stack.pop()
                    if group.match_name == "ADBE FreePin3 Mesh Group":
                        node = Exporter().property(group, [1], "test")
                        mesh = next(child for child in node["children"] if child["match_name"] == "ADBE FreePin3 Mesh Atom")
                        pins = next(child for child in mesh["children"] if child["match_name"] == "ADBE FreePin3 PosPins")
                        self.assertEqual(len(pins["children"]), 5)
                        self.assertEqual(len(mesh["mesh_geometry"]["vertices"]), 231)
                        self.assertEqual(len(mesh["mesh_geometry"]["triangles"]), 380)
                        return
                    stack.extend(getattr(group, "properties", []) or [])
        self.fail("4 fingers Puppet mesh not found")


if __name__ == "__main__":
    unittest.main()
