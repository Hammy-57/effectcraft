"""Build/audit the SceneYard AE effect match-name to EffectCraft ID map.

Scan: python ae_effect_map.py --root /path/to/SceneYard [--cli /path/to/effectcraft-cli]
Audit: append --check or --check-losses.
Emit reviewed static importer IDs: append --emit-rust /path/to/crates/aep/src/effects.rs.
The JSON is evidence from the 292 primary AEPs, the scripting match-name table,
and EffectCraft's live effect.list registry. Display-name fallback is recorded.
"""

import argparse
from collections import Counter, defaultdict
from concurrent.futures import ProcessPoolExecutor
from hashlib import sha256
import json
import os
from pathlib import Path
import re
import subprocess
import sys

ROOT = None
REPO = Path(os.environ.get('EFFECTCRAFT_REPO', Path(__file__).resolve().parents[2])).resolve()
OUTPUT = Path(__file__).with_suffix('.json')
CLI = Path(os.environ.get('EFFECTCRAFT_CLI', REPO / 'target/debug/effectcraft-cli'))
MATCHNAMES = REPO / 'crates/script/src/matchnames.rs'
SKIP_CHILD = {'ADBE Effect Built In Params'}  # AE compositing metadata, not EffectSpec parameters.
# Independently checked across all corpus instances: these hidden/section controls are 0.0 and unkeyed.
FIXED_HIDDEN_ZERO = {('ADBE Tint', 'ADBE Tint-0004'), ('ADBE Ramp', 'ADBE Ramp-0008'),
                     ('CC Cylinder', 'CC Cylinder-0003'), ('CC Cylinder', 'CC Cylinder-0008'),
                     ('ADBE Lumetri', 'ADBE Lumetri-0084')}
# These AE nodes are section headers despite matching native aggregate control labels.
FORCED_UNMAPPED = {('CC Cylinder', 'CC Cylinder-0003'), ('CC Cylinder', 'CC Cylinder-0008'),
                   ('ADBE Lumetri', 'ADBE Lumetri-0084')}
# Full raw-value scans of all 163 PEDG, 17 PESS2, and 26 PESS3 instances:
# these are the only still-unmapped vendor child IDs with differing saved values.
VARIABLE_VENDOR_UNMAPPED = {
    'PEDG': set(),
    'PESS2': {'PESS2-0017'},
    'PESS3': {'PESS3-0005'},
}
# Version/vendor/renderer diagnostics under the vendors' About & Support sections.
# All observed values are unkeyed scalar 1; this excludes PESS3-0068's actual renderer control.
DIAGNOSTIC_UI_DEFAULTS = {
    **{('PEDG', f'PEDG-{n:04d}'): 1 for n in (72, 73, 74, 75)},
    **{('PESS2', f'PESS2-{n:04d}'): 1 for n in (59, 60, 61, 62)},
    **{('PESS3', f'PESS3-{n:04d}'): 1 for n in (52, 53, 54, 55)},
}
CONTROL_ONLY = {
    'Pseudo/@@OMludPqsS0ifGA70qIaLCA', 'Pseudo/GridBuilderRadial_106',
    'Pseudo/MDS Elastic Controller', 'Pseudo/MHAC PrCtrl J95 4',
    'ADBE CM CropEdges', 'ADBE CM LightLeaksRandom', 'ADBE CM WigglePosition',
}


def primary_projects(root):
    return sorted((p for p in root.rglob('*') if p.is_file() and p.suffix.lower() == '.aep'
                   and not any('auto-save' in part.lower() or 'autosave' in part.lower()
                               for part in p.relative_to(root).parts)),
                  key=lambda p: str(p.relative_to(root)).casefold())
EXPLICIT_EFFECTS = {'PEDG': 'ec.stylize.deepglow',
                    'PESS2': 'ec.perspective.shadowstudio2',
                    'PESS3': 'ec.perspective.shadowstudio3',
                    'Keylight 906': 'ec.keying.keylight'}
SAPPHIRE_PARAMS = {
    'S_Monochrome': (1, 235, 540, 542, *range(50, 55)),
    'S_Gradient': range(50, 61),
    'S_DropShadow': range(50, 56),
    'S_Glow': (*range(50, 60), 61, 62, 65),
    'S_Grid': (*range(50, 64), 66),
    'S_HalfTone': range(50, 61),
    'S_CartoonPaint': (1, 235, 540, 542, *range(50, 61), *range(62, 71)),
}
EXPLICIT_EFFECTS.update({name: name for name in SAPPHIRE_PARAMS})
# The Deep Glow spec keeps these AE IDs as its own parameter IDs (misc.rs).
EXPLICIT_CHILDREN = {'PEDG': {f'PEDG-{number:04d}': f'PEDG-{number:04d}'
                              for number in (1, 2, 4, 5, 8, 9, 10, 11, 13, 15, 19, 23, 24, 25, 31, 32, 33, 37, 38, 39, 41, 43, 55, 56, 57, 59, 84)},
                     'ADBE Exposure2': dict(zip(
                         (f'ADBE Exposure2-{n:04d}' for n in (1, 3, 4, 5, 8, 9, 10, 13, 14, 15, 18, 19, 20)),
                         ('channels', 'master/exposure', 'master/offset', 'master/gamma',
                          'red/redExposure', 'red/redOffset', 'red/redGamma',
                          'green/greenExposure', 'green/greenOffset', 'green/greenGamma',
                          'blue/blueExposure', 'blue/blueOffset', 'blue/blueGamma'))),
                     'ADBE CurvesCustom': {'ADBE CurvesCustom-0002': 'channel'},
                     'ADBE Brightness & Contrast 2': {'ADBE Brightness & Contrast 2-0003': 'useLegacy'},
                     'ADBE Grid': {
                         'ADBE Grid-0004': 'width', 'ADBE Grid-0005': 'height',
                         'ADBE Grid-0008': 'feather/featherWidth',
                         'ADBE Grid-0009': 'feather/featherHeight',
                     },
                     'ADBE 4ColorGradient': {
                         'ADBE 4ColorGradient-0008': 'positionsColors/color4',
                     },
                     'ADBE Roughen Edges': {
                         'ADBE Roughen Edges-0014': 'evolutionOptions/randomSeed',
                     },
                     'ADBE Turbulent Displace': {
                         'ADBE Turbulent Displace-0010': 'evolutionOptions/randomSeed',
                     },
                     'CC Cylinder': {
                         'CC Cylinder-0002': 'radius',
                         'CC Cylinder-0004': 'positionX',
                         'CC Cylinder-0005': 'positionY',
                         'CC Cylinder-0006': 'positionZ',
                         'CC Cylinder-0009': 'rotationX',
                         'CC Cylinder-0010': 'rotationY',
                         'CC Cylinder-0011': 'rotationZ',
                     },
                     'CC Ball Action': {
                         'CC Ball Action-0008': 'instabilityState',
                     },
                     'CC Sphere': {'CC Sphere-0020': 'metal'},
                     'CC Page Turn': {
                         'CC Page Turn-0008': 'controls',
                     },
                     # Adobe Noise docs define these binary controls as Use Color Noise and Clipping.
                     # Both AE defaults are 1; native Bool defaults are true.
                     'ADBE Noise2': {
                         'ADBE Noise2-0002': 'color',
                         'ADBE Noise2-0003': 'clip',
                     },
                     'VISINF Grain Implant': {
                         'VISINF Grain Implant-0044': 'application/channelBalance/redShadows',
                         'VISINF Grain Implant-0045': 'application/channelBalance/redMidtones',
                         'VISINF Grain Implant-0046': 'application/channelBalance/redHighlights',
                         'VISINF Grain Implant-0047': 'application/channelBalance/greenShadows',
                         'VISINF Grain Implant-0048': 'application/channelBalance/greenMidtones',
                         'VISINF Grain Implant-0049': 'application/channelBalance/greenHighlights',
                         'VISINF Grain Implant-0050': 'application/channelBalance/blueShadows',
                         'VISINF Grain Implant-0051': 'application/channelBalance/blueMidtones',
                         'VISINF Grain Implant-0052': 'application/channelBalance/blueHighlights',
                         'VISINF Grain Implant-0053': 'VISINF Grain Implant-0053',
                         'VISINF Grain Implant-0054': 'VISINF Grain Implant-0054',
                         'VISINF Grain Implant-0055': 'VISINF Grain Implant-0055',
                         'VISINF Grain Implant-0056': 'VISINF Grain Implant-0056',
                         'VISINF Grain Implant-0058': 'VISINF Grain Implant-0058',
                     },
                     # Lumetri's numbered sections disambiguate repeated labels such as Saturation.
                     'ADBE Lumetri': {
                         'ADBE Lumetri-0003': 'basicCorrection/active',
                         'ADBE Lumetri-0005': 'basicCorrection/inputLut',
                         'ADBE Lumetri-0007': 'basicCorrection/whiteBalance/temperature',
                         'ADBE Lumetri-0008': 'basicCorrection/whiteBalance/tint',
                         'ADBE Lumetri-0012': 'basicCorrection/tone/contrast',
                         'ADBE Lumetri-0013': 'basicCorrection/tone/highlights',
                         'ADBE Lumetri-0014': 'basicCorrection/tone/shadows',
                         'ADBE Lumetri-0020': 'basicCorrection/saturation',
                         'ADBE Lumetri-0023': 'creative/active',
                         'ADBE Lumetri-0025': 'creative/look',
                         'ADBE Lumetri-0026': 'creative/lookIntensity',
                         'ADBE Lumetri-0029': 'creative/adjustments/sharpen',
                         'ADBE Lumetri-0031': 'creative/adjustments/creativeSaturation',
                         'ADBE Lumetri-0037': 'curves/active',
                         'ADBE Lumetri-0046': 'colorWheels/active',
                         'ADBE Lumetri-0050': 'vignette/active',
                         'ADBE Lumetri-0063': 'basicCorrection/tone/hdrWhite',
                         'ADBE Lumetri-0068': 'hslSecondary/active',
                         'ADBE Lumetri-0082': 'hslSecondary/refine/blur',
                         'ADBE Lumetri-0086': 'hslSecondary/correction/temperature',
                         'ADBE Lumetri-0087': 'hslSecondary/correction/tint',
                         'ADBE Lumetri-0088': 'hslSecondary/correction/contrast',
                         'ADBE Lumetri-0089': 'hslSecondary/correction/sharpen',
                         'ADBE Lumetri-0090': 'hslSecondary/correction/saturation',
                         'ADBE Lumetri-0104': 'curves/hueSaturationCurves/hueVsSatActive',
                         'ADBE Lumetri-0105': 'curves/hueSaturationCurves/hueVsHueActive',
                         'ADBE Lumetri-0107': 'curves/hueSaturationCurves/hueVsLumaActive',
                         'ADBE Lumetri-0109': 'curves/hueSaturationCurves/lumaVsSatActive',
                         'ADBE Lumetri-0111': 'curves/hueSaturationCurves/satVsSatActive',
                         'ADBE Lumetri-0118': 'curves/rgbCurves/active',
                     },
                     # Keylight's numbered Screen Matte, Inside Mask, Foreground, and Edge
                     # sections disambiguate repeated labels. Other duplicate headings remain unresolved.
                     'Keylight 906': {
                         'Keylight 906-0019': 'screenMatte/replaceMethod',
                         'Keylight 906-0020': 'screenMatte/replaceColour',
                         'Keylight 906-0023': 'insideMask/insideMask',
                         'Keylight 906-0025': 'insideMask/invertInsideMask',
                         'Keylight 906-0026': 'insideMask/insideReplaceMethod',
                         'Keylight 906-0027': 'insideMask/insideReplaceColour',
                         'Keylight 906-0031': 'outsideMask/outsideMask',
                         'Keylight 906-0033': 'outsideMask/invertOutsideMask',
                         'Keylight 906-0037': 'foregroundColourCorrection/saturation',
                         'Keylight 906-0038': 'foregroundColourCorrection/contrast',
                         'Keylight 906-0039': 'foregroundColourCorrection/brightness',
                         'Keylight 906-0041': 'foregroundColourCorrection/colourSuppression',
                         'Keylight 906-0042': 'foregroundColourCorrection/suppressionBalance',
                         'Keylight 906-0043': 'foregroundColourCorrection/suppressionAmount',
                         'Keylight 906-0046': 'foregroundColourCorrection/colourBalanceHue',
                         'Keylight 906-0047': 'foregroundColourCorrection/colourBalanceSaturation',
                         'Keylight 906-0056': 'edgeColourCorrection/edgeSaturation',
                         'Keylight 906-0057': 'edgeColourCorrection/edgeContrast',
                         'Keylight 906-0058': 'edgeColourCorrection/edgeBrightness',
                         'Keylight 906-0060': 'edgeColourCorrection/edgeColourSuppression',
                         'Keylight 906-0061': 'edgeColourCorrection/edgeSuppressionBalance',
                         'Keylight 906-0062': 'edgeColourCorrection/edgeSuppressionAmount',
                         'Keylight 906-0065': 'edgeColourCorrection/edgeColourBalanceHue',
                         'Keylight 906-0066': 'edgeColourCorrection/edgeColourBalanceSaturation',
                     },
                     # AE repeats labels (Feather, Contrast, Shift Edge, Reduce Chatter) in
                     # separate matte groups; their raw IDs identify the intended native control.
                     'ADBE Samurai': {
                         '/spatialsmoothing2': 'rotoBrushMatte/feather',
                         '/spatialsoftness2': 'rotoBrushMatte/contrast',
                         '/rmshiftedges': 'rotoBrushMatte/shiftEdge',
                         '/rmreducechatter': 'rotoBrushMatte/reduceChatter',
                         '/HBspatialsoftness2': 'refineEdgeMatte/feather',
                         '/HBspatialcontrast': 'refineEdgeMatte/contrast',
                         '/HBspatialchoke': 'refineEdgeMatte/shiftEdge',
                         '/rm2reducechatter': 'refineEdgeMatte/reduceChatter',
                         '/rmsamplesperframe': 'refineEdgeMatte/motionBlur/motionBlurSamples',
                         '/rmcleaningamount': 'refineEdgeMatte/decontamination/decontaminationAmount',
                         '/rmadditionalcleaningradius': 'refineEdgeMatte/decontamination/increaseDecontaminationRadius',
                         '/rmviewedgecleaningmap': 'refineEdgeMatte/decontamination/viewDecontaminationMap',
                     },
                     'PESS2': {f'PESS2-{number:04d}': f'PESS2-{number:04d}'
                               for number in (2, 4, 5, 6, 8, 9, 22, 27, 28, 44)},
                     'PESS3': {f'PESS3-{number:04d}': f'PESS3-{number:04d}'
                               for number in (2, 4, 6, 22, 26, 27, 38, 75, 76, 78, 84, 86, 87)}}
EXPLICIT_CHILDREN.update({name: {f'{name}-{n:04d}': f'{name}-{n:04d}' for n in numbers}
                          for name, numbers in SAPPHIRE_PARAMS.items()})


def family(match):
    if match.startswith(('PEDG', 'PESS', 'S_', 'MB ')):
        return 'external_plugin'
    if match.startswith('Pseudo/'):
        return 'pseudo_effect'
    if match.startswith(('CC ', 'CS ', 'CSRainfall', 'Mettle SkyBox', 'Keylight')):
        return 'ae_bundled_plugin'
    return 'ae_native_or_builtin'


def child_status(effect_match, child_match, param_id):
    if param_id:
        return 'native_parameter'
    if (effect_match, child_match) in FIXED_HIDDEN_ZERO:
        return 'fixed_hidden_zero'
    if (effect_match, child_match) in DIAGNOSTIC_UI_DEFAULTS:
        return 'diagnostic_ui_default'
    if effect_match == 'ADBE FreePin3' and child_match.startswith('ADBE FreePin3 PosPin'):
        return 'dedicated_indexed_import'
    if effect_match == 'ADBE Samurai' and child_match.startswith('ADBE Paint '):
        return 'requires_indexed_native_model'
    if effect_match in VARIABLE_VENDOR_UNMAPPED:
        return ('varying_vendor_property' if child_match in VARIABLE_VENDOR_UNMAPPED[effect_match]
                else 'constant_vendor_property')
    return 'unresolved_raw_property'


def scan(path):
    import py_aep
    effects = Counter()
    children = Counter()
    names = defaultdict(Counter)
    errors = []
    try:
        project = py_aep.parse(path).project
        for comp in project.compositions:
            for layer in comp.layers:
                for effect in layer.effects or ():
                    match = effect.match_name
                    effects[match] += 1
                    names[match][effect.name] += 1

                    def walk(props):
                        for prop in props:
                            if prop.match_name in SKIP_CHILD:
                                continue
                            nested = getattr(prop, 'properties', None)
                            if nested is not None:
                                walk(nested)
                            else:
                                children[(match, prop.match_name, prop.name)] += 1

                    walk(effect.properties)
    except Exception as exc:
        errors.append(f'{path}: {type(exc).__name__}: {exc}')
    return effects, children, names, errors


def catalog():
    if not CLI.is_file():
        raise FileNotFoundError(f'EffectCraft CLI not found: {CLI}; pass --cli or EFFECTCRAFT_CLI')
    result = subprocess.run([str(CLI), 'exec', 'effect.list', '--empty', '--params', '{}', '--json'],
                            check=True, capture_output=True, text=True)
    return json.loads(result.stdout)


def ae_matchnames():
    source = MATCHNAMES.read_text()
    table = source.split('pub const EFFECTS:', 1)[1].split('];', 1)[0]
    return dict((match, effect_id) for effect_id, match in
                re.findall(r'\("([^"]+)",\s*"([^"]+)"\)', table))


def unique_by_name(items):
    indexed = defaultdict(list)
    for item in items:
        indexed[item['name'].casefold()].append(item)
    return indexed


def build():
    if ROOT is None or not ROOT.is_dir():
        raise ValueError('A corpus scan needs --root or SCENEYARD_AEP_ROOT')
    files = primary_projects(ROOT)
    assert len(files) == 292, f'Expected 292 primary AEPs, found {len(files)}'
    total_effects, total_children = Counter(), Counter()
    names = defaultdict(Counter)
    errors = []
    with ProcessPoolExecutor(max_workers=8) as pool:
        for effects, children, found_names, failures in pool.map(scan, files):
            total_effects.update(effects)
            total_children.update(children)
            errors.extend(failures)
            for match, counted in found_names.items():
                names[match].update(counted)

    registry = catalog()
    by_id = {item['id']: item for item in registry}
    by_name = unique_by_name(registry)
    official = ae_matchnames()
    mapped_effects = 0
    mapped_children = 0
    out = {}
    for match, count in sorted(total_effects.items()):
        display = names[match].most_common()
        spec_id = official.get(match)
        evidence = 'matchnames.rs' if spec_id in by_id else None
        if not evidence:
            # A unique exact display name is a lead; keep the weaker provenance explicit.
            candidates = {entry['id'] for name, _ in display for entry in by_name[name.casefold()]}
            if len(candidates) == 1:
                spec_id = candidates.pop()
                evidence = 'unique_exact_display_name'
            else:
                spec_id = None
        if match in EXPLICIT_EFFECTS:
            spec_id = EXPLICIT_EFFECTS[match]
            evidence = 'explicit_source_alias'
        spec = by_id.get(spec_id)
        params = unique_by_name(spec['params']) if spec else {}
        child_name_ids = defaultdict(set)
        for (parent, child_match, child_name) in total_children:
            if parent == match:
                child_name_ids[child_name.casefold()].add(child_match)
        child_map = {}
        for (_, child_match, child_name), instances in sorted(total_children.items()):
            if _ != match:
                continue
            options = params.get(child_name.casefold(), [])
            param_id = EXPLICIT_CHILDREN.get(match, {}).get(child_match)
            if (param_id is None and (match, child_match) not in FORCED_UNMAPPED
                    and len(options) == 1 and len(child_name_ids[child_name.casefold()]) == 1):
                param_id = options[0]['id']
            key = child_match
            entry = child_map.setdefault(key, {'names': Counter(), 'instances': 0, 'param_ids': set()})
            entry['names'][child_name] += instances
            entry['instances'] += instances
            if param_id:
                entry['param_ids'].add(param_id)
        for key, entry in child_map.items():
            ids = entry.pop('param_ids')
            entry['param_id'] = next(iter(ids)) if len(ids) == 1 and len(entry['names']) == 1 else None
            entry['status'] = child_status(match, key, entry['param_id'])
            if entry['param_id']:
                mapped_children += entry['instances']
            entry['names'] = dict(entry['names'])
        if spec:
            mapped_effects += count
        role = 'expression_controller' if match in CONTROL_ONLY else 'visual_effect'
        out[match] = {'instances': count, 'family': family(match), 'role': role,
                      'ae_names': dict(display), 'effect_id': spec_id,
                      'registry_present': bool(spec), 'evidence': evidence,
                      'children': child_map}
    families = defaultdict(lambda: {'effect_types': 0, 'instances': 0, 'unmapped_instances': 0})
    for effect in out.values():
        summary = families[effect['family']]
        summary['effect_types'] += 1
        summary['instances'] += effect['instances']
        if not effect['registry_present']:
            summary['unmapped_instances'] += effect['instances']
    statuses = Counter()
    for effect in out.values():
        for child in effect['children'].values():
            statuses[child['status']] += child['instances']
    data = {'schema_version': 1, 'corpus': str(ROOT),
            'sources': {'matchnames_rs_sha256': sha256(MATCHNAMES.read_bytes()).hexdigest(),
                        'effectcraft_cli_sha256': sha256(CLI.read_bytes()).hexdigest()},
            'primary_aep_count': len(files),
            'sources_parsed': len(files) - len(errors), 'parse_errors': errors,
            'effect_instances': sum(total_effects.values()), 'mapped_effect_instances': mapped_effects,
            'targeted_effect_instances': sum(e['instances'] for e in out.values() if e['effect_id']),
            'effect_role_instances': {role: sum(e['instances'] for e in out.values() if e['role'] == role)
                                      for role in ('expression_controller', 'visual_effect')},
            'parameter_instances': sum(total_children.values()),
            'mapped_parameter_instances': mapped_children,
            'child_status_instances': dict(sorted(statuses.items())),
            'families': dict(sorted(families.items())),
            'effects': out}
    return data


def check(data, validate_registry=False):
    assert data['primary_aep_count'] == 292 and data['sources_parsed'] == 292
    assert sum(v['instances'] for v in data['effects'].values()) == data['effect_instances']
    assert sum(v['instances'] for v in data['effects'].values() if v['effect_id']) == data['targeted_effect_instances']
    assert data['effect_role_instances'] == {role: sum(e['instances'] for e in data['effects'].values()
                                                        if e['role'] == role)
                                             for role in ('expression_controller', 'visual_effect')}
    assert sum(c['instances'] for e in data['effects'].values() for c in e['children'].values()) == data['parameter_instances']
    assert sum(c['instances'] for e in data['effects'].values() for c in e['children'].values()
               if c['param_id']) == data['mapped_parameter_instances']
    statuses = Counter()
    for match, effect in data['effects'].items():
        assert effect['role'] == ('expression_controller' if match in CONTROL_ONLY else 'visual_effect')
        for child_id, child in effect['children'].items():
            assert child['status'] == child_status(match, child_id, child['param_id'])
            statuses[child['status']] += child['instances']
    assert dict(sorted(statuses.items())) == data['child_status_instances']
    assert statuses['fixed_hidden_zero'] == 688
    assert statuses['diagnostic_ui_default'] == 820
    assert data['effects'].get('PEDG', {}).get('effect_id') == 'ec.stylize.deepglow'
    assert data['effects'].get('PEDG', {}).get('children', {}).get('PEDG-0001', {}).get('param_id') == 'PEDG-0001'
    for match, count in (('ADBE Tint', 462), ('ADBE Ramp', 206),
                         ('ADBE FreePin3', 54), ('PEDG', 163), ('PESS2', 17), ('PESS3', 26)):
        assert data['effects'][match]['instances'] == count, match
    for match, child in (('ADBE Tint', 'ADBE Tint-0004'),
                         ('ADBE Ramp', 'ADBE Ramp-0008'),
                         ('ADBE FreePin3', 'ADBE FreePin3 PosPin Position')):
        assert data['effects'][match]['children'][child]['param_id'] is None
    registry = {spec['id']: {param['id'] for param in spec['params']} for spec in catalog()} if validate_registry else {}
    for match, effect in data['effects'].items():
        assert effect['effect_id'] or not effect['registry_present'], match
        if validate_registry and effect['registry_present']:
            assert effect['effect_id'] in registry, match
        name_ids = defaultdict(set)
        for child_id, child in effect['children'].items():
            for name in child['names']:
                name_ids[name.casefold()].add(child_id)
        for child_id, child in effect['children'].items():
            assert child['instances'] >= 1
            param = child['param_id']
            if param is None:
                continue
            if child_id not in EXPLICIT_CHILDREN.get(match, {}):
                assert all(len(name_ids[name.casefold()]) == 1 for name in child['names']), (match, child_id)
            if validate_registry and effect['registry_present'] and child_id not in EXPLICIT_CHILDREN.get(match, {}):
                assert param in registry[effect['effect_id']], (match, child_id, param)


def check_loss_audit(path):
    data = json.loads(path.read_text())
    rows = data['pairs']
    assert not data['parse_errors']
    assert len({(r['effect_id'], r['raw_match']) for r in rows}) == len(rows)
    assert sum(r['losses'] for r in rows) == data['loss_instances']
    assert sum(r['seen'] for r in rows) == data['loss_instances']
    for row in rows:
        assert row['seen'] == row['default_known'] + row['default_unknown']
        assert row['seen'] == row['zero_unkeyed'] + row['nonzero_unkeyed'] + row['keyed']
        assert row['confirmed_native_param_id'] or row['map_status'] != 'native_parameter'
    print(json.dumps(data['value_state_instances']))


def emit_rust(data, path):
    """Refresh match arms while preserving hand-authored importer policy/tests."""
    effects = data['effects']
    q = lambda value: json.dumps(value, ensure_ascii=False)
    lines = [
        '//! Corpus-observed AE effect/child match names → stable EffectCraft IDs.',
        '//! Source: integrations/effectcraft/ae_effect_map.json.',
        '//! Generated match arms; hand-authored policy and tests below are preserved.',
        '//! Unresolved IDs return None; no display-name lookup occurs during import.',
        '',
        "pub(crate) fn effect_id(ae_match: &str) -> Option<&'static str> {",
        '    Some(match ae_match {',
    ]
    for ae, effect in sorted(effects.items()):
        if effect['effect_id']:
            lines.append(f"        {q(ae)} => {q(effect['effect_id'])},")
    lines += ['        _ => return None,', '    })', '}', '',
              "pub(crate) fn param_id(effect_id: &str, ae_param_match: &str) -> Option<&'static str> {",
              '    Some(match (effect_id, ae_param_match) {']
    for ae, effect in sorted(effects.items()):
        if effect['effect_id']:
            for raw, child in sorted(effect['children'].items()):
                if child['param_id']:
                    lines.append(f"        ({q(effect['effect_id'])}, {q(raw)}) => {q(child['param_id'])},")
    lines += [
        '        _ => return None,', '    })', '}', '',
        '/// Vendor About/System Profile diagnostics only; altered or animated values remain losses.',
        'pub(crate) fn inert_diagnostic(effect_id: &str, raw: &str, value: &serde_json::Value, keyed: bool, expressed: bool) -> bool {',
        '    if keyed || expressed || value.as_f64() != Some(1.0) { return false; }',
        '    matches!((effect_id, raw),',
    ]
    diagnostic = sorted((effects[ae]['effect_id'], raw) for ae, raw in DIAGNOSTIC_UI_DEFAULTS)
    lines += [f'        ({q(effect_id)}, {q(raw)}) |' for effect_id, raw in diagnostic]
    lines[-1] = lines[-1][:-2]
    lines += [
        '    )', '}', '',
        '/// These exact AE effects expose expression controls and do not render pixels.',
        'pub(crate) fn control_only(ae_match: &str) -> bool {',
        '    matches!(ae_match,',
    ]
    lines += [f'        {q(match)} |' for match in sorted(CONTROL_ONLY)]
    lines[-1] = lines[-1][:-2]
    lines += [
        '    )', '}', '',
        '#[cfg(test)]', 'mod tests {', '    use super::*;', '',
        '    #[test]', '    fn reviewed_match_ids_and_diagnostics() {',
        '        assert_eq!(effect_id("PEDG"), Some("ec.stylize.deepglow"));',
        '        assert_eq!(param_id("ec.stylize.deepglow", "PEDG-0001"), Some("PEDG-0001"));',
        '        assert_eq!(param_id("ec.stylize.deepglow", "PEDG-0019"), Some("PEDG-0019"));',
        '        assert_eq!(param_id("ec.stylize.deepglow", "PEDG-0055"), Some("PEDG-0055"));',
        '        assert_eq!(param_id("ec.stylize.deepglow", "PEDG-0084"), Some("PEDG-0084"));',
        '        assert_eq!(param_id("ec.stylize.deepglow", "PEDG-0039"), Some("PEDG-0039"));',
        '        assert_eq!(param_id("ec.stylize.deepglow", "PEDG-0057"), Some("PEDG-0057"));',
        '        assert_eq!(param_id("ec.stylize.deepglow", "PEDG-0059"), Some("PEDG-0059"));',
        '        assert_eq!(param_id("ec.stylize.deepglow", "PEDG-0056"), Some("PEDG-0056"));',
        '        for raw in ["PEDG-0009", "PEDG-0025", "PEDG-0031", "PEDG-0032", "PEDG-0033", "PEDG-0043"] { assert_eq!(param_id("ec.stylize.deepglow", raw), Some(raw)); }',
        '        assert_eq!(param_id("ec.perspective.shadowstudio2", "PESS2-0022"), Some("PESS2-0022"));',
        '        assert_eq!(param_id("ec.perspective.shadowstudio2", "PESS2-0028"), Some("PESS2-0028"));',
        '        assert_eq!(param_id("ec.perspective.shadowstudio3", "PESS3-0027"), Some("PESS3-0027"));',
        '        assert_eq!(param_id("ec.perspective.shadowstudio3", "PESS3-0078"), Some("PESS3-0078"));',
        '        assert_eq!(param_id("ec.perspective.cccylinder", "CC Cylinder-0010"), Some("rotationY"));',
        '        assert_eq!(param_id("ec.sim.ccballaction", "CC Ball Action-0008"), Some("instabilityState"));',
        '        assert_eq!(param_id("ec.distort.ccpageturn", "CC Page Turn-0008"), Some("controls"));',
        '        assert_eq!(param_id("ec.noise.noise", "ADBE Noise2-0002"), Some("color"));',
        '        assert_eq!(param_id("ec.color.lumetri", "ADBE Lumetri-0005"), Some("basicCorrection/inputLut"));',
        '        assert_eq!(param_id("ec.color.lumetri", "ADBE Lumetri-0025"), Some("creative/look"));',
        '        assert_eq!(param_id("ec.color.lumetri", "ADBE Lumetri-0026"), Some("creative/lookIntensity"));',
        '        assert_eq!(param_id("ec.color.lumetri", "ADBE Lumetri-0089"), Some("hslSecondary/correction/sharpen"));',
        '        assert_eq!(param_id("ec.distort.turbulentdisplace", "ADBE Turbulent Displace-0010"), Some("evolutionOptions/randomSeed"));',
        '        assert_eq!(param_id("ec.keying.keylight", "Keylight 906-0043"), Some("foregroundColourCorrection/suppressionAmount"));',
        '        assert_eq!(param_id("ec.keying.keylight", "Keylight 906-0062"), Some("edgeColourCorrection/edgeSuppressionAmount"));',
        '        assert_eq!(param_id("ec.keying.keylight", "Keylight 906-0023"), Some("insideMask/insideMask"));',
        '        assert_eq!(param_id("ec.keying.keylight", "Keylight 906-0031"), Some("outsideMask/outsideMask"));',
        '        assert_eq!(param_id("ec.noise.addgrain", "VISINF Grain Implant-0044"), Some("application/channelBalance/redShadows"));',
        '        assert_eq!(param_id("ec.noise.addgrain", "VISINF Grain Implant-0052"), Some("application/channelBalance/blueHighlights"));',
        '        assert_eq!(param_id("ec.noise.addgrain", "VISINF Grain Implant-0053"), Some("VISINF Grain Implant-0053"));',
        '        assert_eq!(param_id("ec.noise.addgrain", "VISINF Grain Implant-0054"), Some("VISINF Grain Implant-0054"));',
        '        assert_eq!(param_id("ec.noise.addgrain", "VISINF Grain Implant-0055"), Some("VISINF Grain Implant-0055"));',
        '        assert_eq!(param_id("ec.noise.addgrain", "VISINF Grain Implant-0056"), Some("VISINF Grain Implant-0056"));',
        '        assert_eq!(param_id("ec.noise.addgrain", "VISINF Grain Implant-0058"), Some("VISINF Grain Implant-0058"));',
        '        assert_eq!(param_id("ec.noise.addgrain", "VISINF Grain Implant-0057"), None);',
        '        assert_eq!(param_id("ec.noise.addgrain", "VISINF Grain Implant-0059"), None);',
        '        assert_eq!(param_id("ec.noise.addgrain", "VISINF Grain Implant-0060"), None);',
        '        assert_eq!(param_id("S_Monochrome", "S_Monochrome-0001"), Some("S_Monochrome-0001"));',
        '        assert_eq!(param_id("S_Monochrome", "S_Monochrome-0235"), Some("S_Monochrome-0235"));',
        '        assert_eq!(param_id("S_CartoonPaint", "S_CartoonPaint-0540"), Some("S_CartoonPaint-0540"));',
        '        assert_eq!(param_id("S_CartoonPaint", "S_CartoonPaint-0542"), Some("S_CartoonPaint-0542"));',
        '        assert_eq!(param_id("S_CartoonPaint", "S_CartoonPaint-0541"), None);',
        '        assert_eq!(param_id("S_CartoonPaint", "S_CartoonPaint-0520"), None);',
        '        assert_eq!(param_id("ec.perspective.cccylinder", "CC Cylinder-0003"), None);',
        '        assert_eq!(param_id("ec.color.lumetri", "ADBE Lumetri-0084"), None);',
        '        assert_eq!(effect_id("MB LookSuite3"), None);',
        '        assert!(control_only("ADBE CM WigglePosition"));',
        '        assert!(!control_only("MB LookSuite3"));',
        '        assert!(inert_diagnostic("ec.stylize.deepglow", "PEDG-0072", &serde_json::json!(1), false, false));',
        '        assert!(!inert_diagnostic("ec.stylize.deepglow", "PEDG-0072", &serde_json::json!(2), false, false));',
        '        assert!(!inert_diagnostic("ec.stylize.deepglow", "PEDG-0072", &serde_json::json!(1), true, false));',
        '        assert!(!inert_diagnostic("ec.stylize.deepglow", "PEDG-0072", &serde_json::json!(1), false, true));',
        '        assert!(!inert_diagnostic("ec.perspective.shadowstudio3", "PESS3-0068", &serde_json::json!(1), false, false));',
        '    }', '}', '',
    ]
    rendered = '\n'.join(lines)
    marker = '/// Vendor About/System Profile diagnostics only; altered or animated values remain losses.'
    if not path.is_file():
        raise FileNotFoundError(f'Restore the existing Rust policy/tests before generating: {path}')
    existing = path.read_text()
    if marker not in existing or marker not in rendered:
        raise ValueError(f'Refusing to overwrite hand-authored Rust in {path}')
    rendered = rendered[:rendered.index(marker)] + existing[existing.index(marker):]
    path.write_text(rendered)
    subprocess.run(['rustfmt', str(path)], check=True)


if __name__ == '__main__':
    parser = argparse.ArgumentParser(description=__doc__)
    mode = parser.add_mutually_exclusive_group()
    mode.add_argument('--check', action='store_true')
    mode.add_argument('--check-losses', nargs='?', const=str(Path(__file__).with_name('ae_effect_map_loss_audit.json')))
    mode.add_argument('--emit-rust', type=Path, metavar='PATH')
    parser.add_argument('--repo', type=Path, default=REPO)
    parser.add_argument('--root', type=Path, default=os.environ.get('SCENEYARD_AEP_ROOT'))
    parser.add_argument('--cli', type=Path, default=os.environ.get('EFFECTCRAFT_CLI'))
    args = parser.parse_args()
    REPO = args.repo.expanduser().resolve()
    MATCHNAMES = REPO / 'crates/script/src/matchnames.rs'
    CLI = Path(args.cli or (REPO / 'target/debug/effectcraft-cli' if (REPO / 'target/debug/effectcraft-cli').is_file()
                            else REPO / 'target/release/effectcraft-cli')).expanduser().resolve()
    ROOT = Path(args.root).expanduser().resolve() if args.root else None
    if args.check_losses:
        check_loss_audit(Path(args.check_losses))
        raise SystemExit(0)
    if args.emit_rust:
        result = json.loads(OUTPUT.read_text())
        check(result)
        emit_rust(result, args.emit_rust)
        raise SystemExit(0)
    result = json.loads(OUTPUT.read_text()) if args.check else build()
    if not args.check:
        OUTPUT.write_text(json.dumps(result, indent=2, ensure_ascii=False) + '\n')
    check(result, validate_registry=CLI.is_file())
    print(json.dumps({k: result[k] for k in ('primary_aep_count', 'sources_parsed',
          'effect_instances', 'mapped_effect_instances', 'targeted_effect_instances', 'parameter_instances',
          'mapped_parameter_instances')}))
