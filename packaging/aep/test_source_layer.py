"""Saved source bindings remain available without opening/relinking the artwork."""
import struct
import unittest
from types import SimpleNamespace
from aep_ir_export import Exporter


class SourceLayerTest(unittest.TestCase):
    def source(self, index=1, bounds=(20.25, 20.5, 40.5, 60.75)):
        data = bytearray(596)
        struct.pack_into(">4i", data, 16, *(round(v * 65536) for v in bounds))
        return SimpleNamespace(_opti=SimpleNamespace(text_layer_name="Art", text_document_layers=2,
                               tobytes=lambda: bytes(data)), _sspc=SimpleNamespace(layer_index=index))

    def test_fractional_offline_binding(self):
        self.assertEqual(Exporter.vector_source_layer(self.source()),
                         {"index":1, "name":"Art", "layer_size":True,
                          "crop_bounds":[20.25,20.5,40.5,60.75]})

    def test_whole_document_is_not_a_layer(self):
        self.assertIsNone(Exporter.vector_source_layer(SimpleNamespace()))

    def test_invalid_saved_binding_is_not_silently_dropped(self):
        for source in [self.source(index=4294967295), self.source(bounds=(20,0,10,10))]:
            with self.assertRaises(ValueError):
                Exporter.vector_source_layer(source)


if __name__ == "__main__":
    unittest.main()
