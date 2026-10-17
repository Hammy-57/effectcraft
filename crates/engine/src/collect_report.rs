//! Dependencies that media collection alone cannot make portable.
use effectcraft_keyframe::Value;
use effectcraft_project::{FootageKind, ItemKind, ParamUi, Project, Property};
use serde_json::{Value as Json, json};

/// This reports requirements, not installed-font availability or redistribution permission.
pub(super) fn dependency_warnings(project: &Project) -> Vec<Json> {
    let mut warnings = Vec::new();
    for item in project.items.values() {
        let footage = match &item.kind {
            ItemKind::Footage(f) => Some(f),
            _ => None,
        };
        for f in footage.into_iter().chain(item.proxy.as_ref().map(|p| &p.footage)) {
            if f.path.is_empty() && f.sequence.is_empty() && f.missing && !(f.kind == FootageKind::Data && f.data.is_some()) {
                warnings.push(json!({"kind": "placeholder", "item": item.id.0,
                    "message": "This placeholder has no source media and remains missing in the shared project."}));
            }
            if f.kind == FootageKind::Model {
                warnings.push(json!({
                    "kind": "model_dependencies", "item": item.id.0, "path": f.path,
                    "message": "External model buffers and textures are not collected; use a self-contained model or transfer its dependencies separately."
                }));
            }
        }
    }
    if !project.render_queue.is_empty() {
        warnings.push(json!({"kind": "render_destinations",
            "message": "Render Queue destinations are unchanged. Choose output locations on the recipient's computer before rendering."}));
    }
    for (comp, c) in project.comps() {
        for layer in &c.layers {
            layer.props.walk("", &mut |path, property| {
                for mut warning in property_warnings(property) {
                    warning["comp"] = json!(comp.0);
                    warning["layer"] = json!(layer.id.0);
                    warning["property"] = json!(path);
                    warnings.push(warning);
                }
            });
        }
    }
    warnings
}

pub(crate) fn property_warnings(property: &Property) -> Vec<Json> {
    let mut warnings = Vec::new();
    for value in std::iter::once(&property.value).chain(property.keys.iter().map(|key| &key.value)) {
        if let Value::Text(doc) = value {
            // Include the editable base style as well as explicitly styled character runs.
            for style in std::iter::once(doc.base_style()).chain(doc.runs.iter().map(|run| run.style.clone())) {
                let warning = json!({
                    "kind": "font_required", "font": style.font, "style": style.style,
                    "render_font": style.render_font, "font_file": style.font_file,
                    "message": "Fonts are not copied. The recipient needs these fonts or an explicit substitute."
                });
                if !warnings.contains(&warning) {
                    warnings.push(warning);
                }
            }
        }
        if let Value::Str(text) = value
            && (matches!(property.ui, ParamUi::File { .. }) || property.match_id == "shapeData")
            && external_file_value(&property.match_id, text)
        {
            let warning = json!({
                "kind": "external_effect_file", "parameter": property.match_id, "path": text.trim(),
                "message": "This external effect file is not collected; its own referenced files may also be required."
            });
            if !warnings.contains(&warning) {
                warnings.push(warning);
            }
        }
    }
    warnings
}

fn external_file_value(parameter: &str, value: &str) -> bool {
    let value = value.trim();
    if value.is_empty() {
        return false;
    }
    // Match the existing loaders' inline-content rules, without opening arbitrary files.
    match parameter {
        "lut" => !value.contains('\n') && !value.contains("LUT_"),
        "file" => !value.contains('\n'),
        "shapeData" => !value.starts_with('{') && !value.starts_with('['),
        _ => true,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use effectcraft_keyframe::{CharStyle, Keyframe, StyleRun, TextDoc};
    use effectcraft_time::Tick;

    #[test]
    fn fonts_include_keyframes_runs_and_linked_font_requirements() {
        let first = TextDoc {
            text: "first".into(),
            font: "Initial".into(),
            font_file: Some(effectcraft_keyframe::text_doc::FontFileLink { path: "/fonts/initial.otf".into(), sha256: "0".repeat(64) }),
            ..Default::default()
        };
        let next = TextDoc {
            text: "ab".into(),
            font: "Animated".into(),
            runs: vec![StyleRun { len: 1, style: CharStyle { font: "Run Font".into(), render_font: Some("Substitute".into()), ..Default::default() } }],
            ..Default::default()
        };
        let mut property = Property::new(1, "sourceText", "Text", Value::Text(Box::new(first)));
        property.keys.push(Keyframe::new(Tick::ZERO, Value::Text(Box::new(next))));
        let warnings = property_warnings(&property);
        assert!(warnings.iter().any(|w| w["font"] == "Initial" && w["font_file"]["path"] == "/fonts/initial.otf"));
        assert!(warnings.iter().any(|w| w["font"] == "Animated"));
        assert!(warnings.iter().any(|w| w["font"] == "Run Font" && w["render_font"] == "Substitute"));
    }

    #[test]
    fn file_properties_report_keyframes_but_not_inline_content() {
        let mut property = Property::new(1, "lut", "LUT", Value::Str("LUT_3D_SIZE 2\n0 0 0".into()));
        property.ui = ParamUi::File { filters: vec!["cube".into()] };
        property.keys.push(Keyframe::new(Tick::ZERO, Value::Str("/assets/grade.cube".into())));
        let warnings = property_warnings(&property);
        assert_eq!(warnings.len(), 1);
        assert_eq!(warnings[0]["path"], "/assets/grade.cube");
        assert!(!external_file_value("shapeData", "{\"shapes\": []}"));
        assert!(external_file_value("shapeData", "/assets/shapes.json"));
        assert!(!external_file_value("file", "LUT_1D_SIZE 2\n0 0 0\n1 1 1"));
        assert!(external_file_value("configFile", "/assets/config.ocio"));
    }

    #[test]
    fn models_report_sidecar_risk() {
        let mut project = Project::default();
        let id = project.add_item(
            "Model",
            Default::default(),
            None,
            ItemKind::Footage(effectcraft_project::Footage { kind: FootageKind::Model, path: "/models/scene.gltf".into(), ..Default::default() }),
        );
        let warnings = dependency_warnings(&project);
        assert_eq!(warnings.len(), 1);
        assert_eq!(warnings[0]["item"], id.0);
        assert_eq!(warnings[0]["kind"], "model_dependencies");
    }

    #[test]
    fn ordinary_strings_are_not_assumed_to_be_file_dependencies() {
        let property = Property::new(1, "description", "Description", Value::Str("/looks/like/a/path".into()));
        assert!(property_warnings(&property).is_empty());
        assert!(dependency_warnings(&Project::default()).is_empty());
    }
}
