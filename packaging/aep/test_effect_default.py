"""Effect parameters AE left out of the file take the plugin default, not another instance's."""
import unittest
from types import SimpleNamespace
from aep_ir_export import Exporter


def param(value, default, live=False, parade=True):
    parade_group = SimpleNamespace(match_name="ADBE Effect Parade", parent_property=None)
    effect = SimpleNamespace(match_name="ADBE Tint", parent_property=parade_group if parade else None)
    return SimpleNamespace(value=value, default_value=default, last_value=value, parent_property=effect,
                           _is_live=lambda: live)


DARK_BLUE = [7 / 255, 7 / 255, 60 / 255, 1.0]
WHITE = [1.0, 1.0, 1.0, 0.0]


class EffectDefaultTest(unittest.TestCase):
    def test_untouched_parameter_takes_the_plugin_default(self):
        # SceneYard Student Computer Workflow: an untouched Tint's Map White To read as another
        # Tint's dark blue (the project's shared last value); aerender draws it white.
        self.assertEqual(Exporter.effect_default(param(DARK_BLUE, WHITE)), WHITE)

    def test_authored_and_non_effect_values_are_kept(self):
        self.assertIsNone(Exporter.effect_default(param(DARK_BLUE, WHITE, live=True)))
        self.assertIsNone(Exporter.effect_default(param(DARK_BLUE, WHITE, parade=False)))
        self.assertIsNone(Exporter.effect_default(param(34.0, None)))
        self.assertIsNone(Exporter.effect_default(param(34.0, 34.0)))


if __name__ == "__main__":
    unittest.main()
