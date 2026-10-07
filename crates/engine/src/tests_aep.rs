//! The AEP import command must leave the current project intact on bad input.

use serde_json::json;

use crate::Session;

#[test]
fn malformed_aep_ir_does_not_change_project() {
    let mut session = Session::default();
    session.execute("comp.new", json!({"name": "Keep Me"})).unwrap();
    let before = session.project.to_file_json().unwrap();
    let path = std::env::temp_dir().join(format!("effectcraft-bad-aep-ir-{}.json", std::process::id()));
    std::fs::write(&path, b"{broken").unwrap();

    let result = session.execute("file.importAeIr", json!({"path": path.to_string_lossy()}));
    assert!(result.is_err());
    assert_eq!(session.project.to_file_json().unwrap(), before);
}

#[test]
fn strict_import_rejects_declared_loss_without_mutating_project() {
    let mut session = Session::default();
    session.execute("comp.new", json!({"name": "Keep Me"})).unwrap();
    let before = session.project.to_file_json().unwrap();
    let path = std::env::temp_dir().join(format!("effectcraft-lossy-aep-ir-{}.json", std::process::id()));
    std::fs::write(
        &path,
        json!({"schema_version":1,"items":[
        {"id":1,"kind":"comp","name":"Rejected","width":320,"height":180,"duration":1,"frame_rate":24,"layers":[]}
    ],"losses":[{"reason":"opaque_value"}]})
        .to_string(),
    )
    .unwrap();

    assert!(session.execute("file.importAeIr", json!({"path":path.to_string_lossy()})).is_err());
    assert_eq!(session.project.to_file_json().unwrap(), before);
}

#[test]
fn imports_editable_comp_as_one_undo_step() {
    let mut session = Session::default();
    let path = std::env::temp_dir().join(format!("effectcraft-good-aep-ir-{}.json", std::process::id()));
    let ir = json!({"schema_version":1,"items":[
        {"id":10,"kind":"comp","name":"Imported","width":320,"height":180,"duration":2,"frame_rate":24,
         "layers":[{"id":11,"kind":"null","name":"Controller","timing":{"out_point":2}}]}
    ]});
    std::fs::write(&path, ir.to_string()).unwrap();

    let reply = session.execute("file.importAeIr", json!({"path":path.to_string_lossy()})).unwrap();
    assert_eq!(reply["comps"].as_array().unwrap().len(), 1);
    let imported = effectcraft_project::ItemId(reply["comps"][0].as_u64().unwrap());
    assert_eq!(session.project.comp(imported).unwrap().layers.len(), 1);
    assert!(reply["losses"].as_array().unwrap().is_empty());
    let original = session.project.comp(imported).unwrap().layers[0].transform().unwrap().get("position").unwrap().value.clone();
    session.execute("prop.set", json!({"comp":imported.0,"layer":"#1","path":"transform/position","value":[30,40,0]})).unwrap();
    assert_ne!(session.project.comp(imported).unwrap().layers[0].transform().unwrap().get("position").unwrap().value, original);
    session.execute("edit.undo", json!({})).unwrap();
    assert_eq!(session.project.comp(imported).unwrap().layers[0].transform().unwrap().get("position").unwrap().value, original);
    session.execute("edit.undo", json!({})).unwrap();
    assert!(session.project.comp(imported).is_none());
}
