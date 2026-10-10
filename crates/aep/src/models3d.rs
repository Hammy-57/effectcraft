//! AE 3D model layers remain editable model references, including offline footage.

use super::*;
use effectcraft_project::Layer;

pub(super) fn layer_source(ir: &Json, project: &Project, item_ids: &BTreeMap<i64, ItemId>, comp_id: i64, losses: &mut Vec<String>) -> Option<LayerSource> {
    let source_id = integer(ir, &["source_id"]);
    let item = source_id.and_then(|id| item_ids.get(&id)).copied();
    match item.and_then(|id| project.item(id)) {
        Some(source) if matches!(&source.kind, ItemKind::Footage(_)) => Some(LayerSource::Model { item: source.id }),
        _ => {
            losses.push(format!("comp {comp_id}: model layer {} has missing model source {:?}", string(ir, &["name"]).unwrap_or("?"), source_id));
            None
        }
    }
}

/// The common importer already handles 3D transforms, timing, keyframes, and AE-only
/// properties. Normalize AE's model material flags onto the native render controls.
/// `model_scale` is the footage's object scale from AE's model settings (1 = as authored).
pub(super) fn import(_project: &mut Project, layer: &mut Layer, ir: &Json, model_scale: Option<f64>, losses: &mut Vec<String>, context: &str) {
    let layer_time = LayerTime::of(layer);
    if let Some(scale) = layer.props.prop_mut("geometryOptions/unitScale") {
        scale.value = Value::Scalar(512.0);
    } else {
        losses.push(format!("{context}: native model scale missing"));
    }
    if let Some(k) = model_scale.filter(|k| k.is_finite() && *k > 0.0 && (*k - 1.0).abs() > 1e-12) {
        if let Some(scale) = layer.props.prop_mut("geometryOptions/objectScale") {
            scale.value = Value::Scalar(k * 100.0);
        } else {
            losses.push(format!("{context}: native model object scale missing"));
        }
    }
    if let Some(axes) = layer.props.prop_mut("geometryOptions/aeSourceSpace") {
        axes.value = Value::Bool(true);
    } else {
        losses.push(format!("{context}: native AE model axes control missing"));
    }
    let Some(material) = array(ir, "properties").iter().find(|p| string(p, &["match_name"]) == Some("ADBE Material Options Group")) else {
        return;
    };
    for source in array(material, "children") {
        let target = match string(source, &["match_name"]) {
            Some("ADBE Casts Shadows") => "castsShadows",
            Some("ADBE Accepts Shadows") => "acceptsShadows",
            Some("ADBE Accepts Lights") => "acceptsLights",
            Some("ADBE Appears in Reflections") => "appearsInReflections",
            _ => continue,
        };
        let Some(prop) = layer.props.prop_mut(&format!("materialOptions/{target}")) else {
            losses.push(format!("{context}: native model material {target} missing"));
            continue;
        };
        // AE model tri-state values are 0=Off, 1=On, 2=Only. The common
        // property adapter expects AE's usual 1-based enum, so offset here.
        let remap: Option<&dyn Fn(f64) -> f64> = if matches!(target, "castsShadows" | "acceptsShadows") { Some(&|v| v + 1.0) } else { None };
        let raw = remap_json(source.get("value").unwrap_or(&Json::Null), remap);
        let value_type = string(source, &["value_type"]).unwrap_or("");
        if let Some(value) = convert_value(&raw, value_type) {
            fill_property_mapped(prop, source, value, value_type, 1.0, remap, layer_time, losses, context);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn model_reference_and_material_keys_stay_editable() {
        let mut project = Project::default();
        let item = project.add_item(
            "offline.glb",
            Label::None,
            None,
            ItemKind::Footage(Footage { path: "/offline/offline.glb".into(), kind: FootageKind::Model, missing: true, ..Footage::default() }),
        );
        let ir = serde_json::json!({"name":"Offline model","source_id":42,"properties":[
            {"match_name":"ADBE Material Options Group","children":[
                {"match_name":"ADBE Casts Shadows","value_type":6417,"value":2.0,"keyframes":[{"time":0.0,"value":0.0},{"time":1.0,"value":2.0}]},
                {"match_name":"ADBE Accepts Shadows","value_type":6417,"value":1.0,"keyframes":[]},
                {"match_name":"ADBE Accepts Lights","value_type":6417,"value":0.0,"keyframes":[]},
                {"match_name":"ADBE Appears in Reflections","value_type":6417,"value":0.0,"keyframes":[]}
            ]}
        ]});
        let ids = BTreeMap::from([(42, item)]);
        let mut losses = Vec::new();
        let source = layer_source(&ir, &project, &ids, 1, &mut losses);
        assert_eq!(source, Some(LayerSource::Model { item }));
        let comp = Comp::new(1920, 1080, FrameRate::from_f64(30.0), tick(2.0));
        let mut layer = build::layer(&mut project, &comp, "Offline model", source.unwrap_or(LayerSource::Null), (1920, 1080), None);
        import(&mut project, &mut layer, &ir, Some(0.125), &mut losses, "test");
        assert_eq!(layer.props.prop("materialOptions/castsShadows").map(|p| &p.value), Some(&Value::Enum(2)));
        assert_eq!(layer.props.prop("materialOptions/castsShadows").map(|p| p.keys.len()), Some(2));
        assert_eq!(layer.props.prop("materialOptions/acceptsShadows").map(|p| &p.value), Some(&Value::Enum(1)));
        assert_eq!(layer.props.prop("materialOptions/acceptsLights").map(|p| &p.value), Some(&Value::Bool(false)));
        assert_eq!(layer.props.prop("materialOptions/appearsInReflections").map(|p| &p.value), Some(&Value::Bool(false)));
        assert_eq!(layer.props.prop("geometryOptions/unitScale").map(|p| &p.value), Some(&Value::Scalar(512.0)));
        assert_eq!(layer.props.prop("geometryOptions/aeSourceSpace").map(|p| &p.value), Some(&Value::Bool(true)));
        assert_eq!(layer.props.prop("geometryOptions/objectScale").map(|p| &p.value), Some(&Value::Scalar(12.5)));
        assert!(losses.is_empty(), "{losses:?}");
    }
}
