//! Exact neutral Shadow Studio controls, kept outside generated effect-map code.
//!
//! Shadow Studio 3 values were read from a fresh AE 2026 plugin instance. Shadow
//! Studio 2 is unavailable on this host; its values were checked in all 17
//! corpus instances and against matching Shadow Studio 3 controls where possible.
//! Changed, keyed, or expressed controls must continue to produce import losses.

use serde_json::Value as Json;

pub(super) fn inert(effect: &str, raw: &str, value: &Json, keyed: bool, expressed: bool) -> bool {
    if keyed || expressed {
        return false;
    }
    let version = match effect {
        "ec.perspective.shadowstudio2" => 2,
        "ec.perspective.shadowstudio3" => 3,
        _ => return false,
    };
    if matches!((version, raw), (2, "PESS2-0003") | (3, "PESS3-0003")) {
        return value.as_array().is_some_and(|a| a.len() == 2 && a.iter().all(|v| v.as_f64() == Some(0.0)));
    }
    let neutral = match (version, raw) {
        (2, "PESS2-0030" | "PESS2-0031" | "PESS2-0032" | "PESS2-0033" | "PESS2-0040" | "PESS2-0045" | "PESS2-0050" | "PESS2-0052") => Some(0.0),
        (2, "PESS2-0007" | "PESS2-0011" | "PESS2-0024" | "PESS2-0035" | "PESS2-1011") => Some(1.0),
        (2, "PESS2-0023") => Some(3.0),
        (2, "PESS2-0041") => Some(15.0),
        (2, "PESS2-0016") => Some(80.0),
        (2, "PESS2-0012" | "PESS2-0017" | "PESS2-0018" | "PESS2-0042") => Some(100.0),
        (
            3,
            "PESS3-0029" | "PESS3-0030" | "PESS3-0031" | "PESS3-0032" | "PESS3-0034" | "PESS3-0043" | "PESS3-0045" | "PESS3-0059" | "PESS3-0064" | "PESS3-0069"
            | "PESS3-0079" | "PESS3-0081" | "PESS3-0088" | "PESS3-0102" | "PESS3-0104" | "PESS3-0105" | "PESS3-0109" | "PESS3-0115",
        ) => Some(0.0),
        (
            3,
            "PESS3-0001" | "PESS3-0007" | "PESS3-0011" | "PESS3-0060" | "PESS3-0062" | "PESS3-0074" | "PESS3-0077" | "PESS3-0080" | "PESS3-0082" | "PESS3-0083"
            | "PESS3-0103" | "PESS3-0106" | "PESS3-0108" | "PESS3-0110",
        ) => Some(1.0),
        (3, "PESS3-0061" | "PESS3-0068" | "PESS3-0073") => Some(2.0),
        (3, "PESS3-0023") => Some(3.0),
        (3, "PESS3-0093") => Some(5.0),
        (3, "PESS3-0072") => Some(9.0),
        (3, "PESS3-0008" | "PESS3-0009") => Some(20.0),
        (3, "PESS3-0071" | "PESS3-0094") => Some(25.0),
        (3, "PESS3-0035") => Some(41.0),
        (3, "PESS3-0085") => Some(50.0),
        (3, "PESS3-0016") => Some(80.0),
        (3, "PESS3-0012" | "PESS3-0017" | "PESS3-0018" | "PESS3-0036" | "PESS3-0063" | "PESS3-0070" | "PESS3-0090" | "PESS3-0101") => Some(100.0),
        (3, "PESS3-0005") => Some(250.0),
        (3, "PESS3-0089") => Some(-1.0),
        _ => None,
    };
    neutral.is_some_and(|n| value.as_f64() == Some(n))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn exact_defaults_leave_changed_and_animated_controls_visible() {
        let v2 = "ec.perspective.shadowstudio2";
        let v3 = "ec.perspective.shadowstudio3";
        assert!(inert(v2, "PESS2-0017", &json!(100), false, false));
        assert!(!inert(v2, "PESS2-0017", &json!(80), false, false));
        assert!(inert(v3, "PESS3-0005", &json!(250), false, false));
        assert!(!inert(v3, "PESS3-0005", &json!(140), false, false));
        assert!(!inert(v3, "PESS3-0005", &json!(250), true, false));
        assert!(!inert(v3, "PESS3-0068", &json!(1), false, false));
        assert!(inert(v3, "PESS3-0003", &json!([0, 0]), false, false));
    }
}
