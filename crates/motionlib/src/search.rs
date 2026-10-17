//! Card search, in parity with the Motion Library panel's `retrieval.py` (and `panel/search.js`):
//! every query word must match some field; closer matches in heavier fields rank higher.

use std::collections::BTreeSet;

use serde_json::Value;

use crate::manifest::Card;

const NUMBER_WORDS: [&str; 11] = ["zero", "one", "two", "three", "four", "five", "six", "seven", "eight", "nine", "ten"];

/// Field weights, as `retrieval.WEIGHTS`.
const WEIGHTS: [(&str, i64); 9] = [
    ("name", 10),
    ("aliases", 9),
    ("description", 8),
    ("visual_caption", 7),
    ("motion_events", 6),
    ("summary", 3),
    ("generated_summary", 1),
    ("category", 2),
    ("motion_terms", 2),
];

/// Words of `text`: camelCase split, lowercased, letters and digits only, number words as digits.
pub fn tokens(text: &str) -> Vec<String> {
    // "TextReveal" → "Text Reveal" (ASCII [a-z0-9] followed by [A-Z], as the Python regex).
    let mut spaced = String::with_capacity(text.len() + 8);
    let mut prev: Option<char> = None;
    for c in text.chars() {
        if c.is_ascii_uppercase() && prev.is_some_and(|p| p.is_ascii_lowercase() || p.is_ascii_digit()) {
            spaced.push(' ');
        }
        spaced.push(c);
        prev = Some(c);
    }
    spaced
        .to_lowercase()
        .split(|c: char| !c.is_alphanumeric())
        .filter(|w| !w.is_empty())
        .map(|w| NUMBER_WORDS.iter().position(|n| *n == w).map_or_else(|| w.to_string(), |i| i.to_string()))
        .collect()
}

/// A JSON field as text, as Python's `str()` of it (lists: items joined, objects by their
/// `description`).
fn value_text(v: &Value) -> String {
    match v {
        Value::Null => String::new(),
        Value::String(s) => s.clone(),
        Value::Bool(b) => if *b { "True" } else { "False" }.into(),
        Value::Number(n) => n.to_string(),
        Value::Array(items) => items
            .iter()
            .map(|i| match i {
                Value::Object(o) => o.get("description").map(value_text).unwrap_or_default(),
                other => value_text(other),
            })
            .collect::<Vec<_>>()
            .join(" "),
        Value::Object(_) => v.to_string(),
    }
}

/// The track and property names of a JSON entry's `segments` (`retrieval.motion_terms`).
fn segment_terms(entry: &Value) -> String {
    let mut out = Vec::new();
    for s in entry.get("segments").and_then(Value::as_array).into_iter().flatten() {
        for k in ["name", "comp_name"] {
            out.push(s.get(k).map(value_text).unwrap_or_default());
        }
        for p in s.get("properties").and_then(Value::as_array).into_iter().flatten() {
            out.push(value_text(p));
        }
        for t in s.get("tracks").and_then(Value::as_array).into_iter().flatten() {
            out.push(t.get("property").or_else(|| t.get("name")).map(value_text).unwrap_or_default());
        }
    }
    out.join(" ")
}

/// The word plus its likely singulars (heels → heel, boxes → box, bodies → body).
fn forms(word: &str) -> Vec<String> {
    let mut found = vec![word.to_string()];
    if word.chars().count() > 3 && word.ends_with('s') {
        found.push(word[..word.len() - 1].to_string());
        if word.ends_with("es") {
            found.push(word[..word.len() - 2].to_string());
        }
        if word.ends_with("ies") {
            found.push(format!("{}y", &word[..word.len() - 3]));
        }
    }
    found
}

/// 3 whole word, 2 plural/singular or start of a word (still typing), 1 inside a longer word.
fn closeness(word: &str, field: &BTreeSet<String>) -> i64 {
    if field.contains(word) {
        return 3;
    }
    if word.bytes().all(|b| b.is_ascii_digit()) {
        return 0; // numeric tokens are exact: 3 must not match 30
    }
    let mine = forms(word);
    if field.iter().any(|t| t.starts_with(word) || forms(t).iter().any(|f| mine.contains(f))) {
        return 2;
    }
    if word.chars().count() >= 3 && field.iter().any(|t| t.contains(word)) { 1 } else { 0 }
}

/// Searchable fields of a card or a JSON entry.
trait Fields {
    fn text(&self, key: &str) -> String;
    fn extra_motion_terms(&self) -> Option<String>;
}

impl Fields for Value {
    fn text(&self, key: &str) -> String {
        self.get(key).map(value_text).unwrap_or_default()
    }
    fn extra_motion_terms(&self) -> Option<String> {
        self.get("segments").map(|_| segment_terms(self))
    }
}

impl Fields for Card {
    fn text(&self, key: &str) -> String {
        match key {
            "name" => self.name.clone(),
            "aliases" => self.aliases.join(" "),
            "description" => self.description.clone(),
            "visual_caption" => self.visual_caption.clone(),
            "motion_events" => self.motion_events.join(" "),
            "summary" => self.summary.clone(),
            "generated_summary" => self.generated_summary.clone(),
            "category" => self.category.clone(),
            "motion_terms" => self.motion_terms.clone(),
            _ => String::new(),
        }
    }
    fn extra_motion_terms(&self) -> Option<String> {
        None
    }
}

fn score_fields(entry: &dyn Fields, query: &str) -> i64 {
    let words: BTreeSet<String> = tokens(query).into_iter().collect();
    if words.is_empty() {
        return 0;
    }
    let mut fields: Vec<(i64, BTreeSet<String>)> = WEIGHTS.iter().map(|(k, w)| (*w, tokens(&entry.text(k)).into_iter().collect())).collect();
    if let Some(extra) = entry.extra_motion_terms()
        && let Some((_, f)) = fields.last_mut()
    {
        f.extend(tokens(&extra));
    }
    let name = tokens(&entry.text("name"));
    if name.len() > 1
        && let Some((_, f)) = fields.first_mut()
    {
        f.insert(name.concat()); // "highheels" finds "High Heels"
    }
    let mut total = 0;
    for word in &words {
        let hit = fields.iter().map(|(w, f)| w * closeness(word, f)).max().unwrap_or(0);
        if hit == 0 {
            return -1;
        }
        total += hit;
    }
    total
}

/// A JSON entry's score for `query` (`retrieval.score`): 0 for an empty query, -1 when some word
/// matches nowhere.
pub fn score(entry: &Value, query: &str) -> i64 {
    score_fields(entry, query)
}

/// The cards matching `query` in `folder` (and its subfolders), best first; ties: priority
/// cards first, then by name (`retrieval.search`).
pub fn search<'a>(cards: &'a [Card], query: &str, folder: &str) -> Vec<&'a Card> {
    let mut found: Vec<(i64, &Card)> = cards.iter().filter(|c| c.in_folder(folder)).map(|c| (score_fields(c, query), c)).filter(|(s, _)| *s >= 0).collect();
    found.sort_by(|a, b| b.0.cmp(&a.0).then_with(|| b.1.priority.cmp(&a.1.priority)).then_with(|| a.1.name.to_lowercase().cmp(&b.1.name.to_lowercase())));
    found.into_iter().map(|(_, c)| c).collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    /// The fixtures and expectations of `test_retrieval.py`.
    #[test]
    fn retrieval_parity() {
        let entries = json!([
            {"id": "three", "name": "Three_Clicks", "category": "UI", "priority": false},
            {"id": "30", "name": "30_cards", "category": "UI", "priority": true},
            {"id": "caption", "name": "Example", "category": "UI", "visual_caption": "Three clicks checklist",
             "aliases": ["task list"], "motion_events": [{"description": "Soft overshoot", "preview_path": "/not_searchable.mov"}],
             "segments": [{"name": "Layer", "tracks": [{"name": "ADBE Opacity"}]}]},
            {"id": "camel", "name": "TextReveal", "category": "Type"},
            {"id": "heels", "name": "high heels", "category": "Comp", "description": "Paper box label"}
        ]);
        let entries = entries.as_array().unwrap();
        let cases: [(&str, &[&str]); 23] = [
            ("", &["30", "caption", "heels", "camel", "three"]),
            ("3", &["three", "caption"]),
            ("three clicks", &["three", "caption"]),
            ("Three_Clicks", &["three", "caption"]),
            ("3 cards", &[]),
            ("opacity", &["caption"]),
            ("soft overshoot", &["caption"]),
            ("soft missing", &[]),
            ("task list", &["caption"]),
            ("text reveal", &["camel"]),
            ("not_searchable", &[]),
            ("high heel", &["heels"]),
            ("hig", &["heels"]),
            ("high heels", &["heels"]),
            ("highheels", &["heels"]),
            ("heel high", &["heels"]),
            ("boxes", &["heels"]),
            ("labels", &["heels"]),
            ("eels", &["heels"]),
            ("high heelz", &[]),
            ("click", &["three", "caption"]),
            ("reveal", &["camel"]),
            ("cards", &["30"]),
        ];
        for (q, ids) in cases {
            let mut found: Vec<(i64, &Value)> = entries.iter().map(|e| (score(e, q), e)).filter(|(s, _)| *s >= 0).collect();
            found.sort_by(|a, b| {
                b.0.cmp(&a.0)
                    .then_with(|| b.1["priority"].as_bool().unwrap_or(false).cmp(&a.1["priority"].as_bool().unwrap_or(false)))
                    .then_with(|| a.1["name"].as_str().unwrap().to_lowercase().cmp(&b.1["name"].as_str().unwrap().to_lowercase()))
            });
            let got: Vec<&str> = found.iter().map(|(_, e)| e["id"].as_str().unwrap()).collect();
            assert_eq!(got, ids, "query {q:?}");
        }
    }

    /// Scores as `retrieval.score` computes them for the same fixtures (checked against Python).
    #[test]
    fn retrieval_scores() {
        let e = json!({"id": "caption", "name": "Example", "category": "UI", "visual_caption": "Three clicks checklist",
            "aliases": ["task list"], "motion_events": [{"description": "Soft overshoot"}],
            "segments": [{"name": "Layer", "tracks": [{"name": "ADBE Opacity"}]}]});
        assert_eq!(score(&e, "three clicks"), 21 + 21);
        assert_eq!(score(&e, "opacity"), 6);
        assert_eq!(score(&e, "task"), 27);
        assert_eq!(score(&e, "missing"), -1);
        assert_eq!(score(&e, ""), 0);
    }

    #[test]
    fn cards_search_by_folder() {
        let cards = vec![
            Card { id: "a".into(), name: "Reel card pop".into(), category: "Devin Jatho/UI".into(), ..Default::default() },
            Card { id: "b".into(), name: "Sunrise text reveal".into(), category: "SceneYard".into(), priority: true, ..Default::default() },
            Card { id: "c".into(), name: "Map".into(), category: "SceneYard".into(), description: "world reveal".into(), ..Default::default() },
        ];
        let ids = |v: Vec<&Card>| v.iter().map(|c| c.id.clone()).collect::<Vec<_>>();
        assert_eq!(ids(search(&cards, "reveal", "")), ["b", "c"]);
        assert_eq!(ids(search(&cards, "", "Devin Jatho")), ["a"]);
        assert_eq!(ids(search(&cards, "", "SceneYard")), ["b", "c"]);
        assert_eq!(ids(search(&cards, "cards", "")), ["a"]);
    }
}
