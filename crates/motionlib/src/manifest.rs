//! The catalog documents: [`Head`] (version and manifest location) and [`Manifest`] (the cards).

use std::collections::BTreeMap;
use std::io::{Read, Write};

use serde::{Deserialize, Serialize};

use crate::hash::{is_hash, sha256_hex};

/// `format` of both documents.
pub const FORMAT: &str = "effectcraft-motion-library";
/// The key of the [`Head`] document in a remote and in the local store.
pub const HEAD_KEY: &str = "head.json";
/// Newest document version this build reads.
const SCHEMA: u32 = 1;
/// Largest manifest or project a client inflates (a bad blob can't exhaust memory).
const MAX_INFLATED: u64 = 512 * 1024 * 1024;

/// A file in the bucket, by content.
#[derive(Clone, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(default)]
pub struct BlobRef {
    /// SHA-256 of the file's bytes (before compression).
    pub hash: String,
    /// Size of the file's bytes (before compression).
    pub size: u64,
    /// Stored gzipped (text: projects, manifests).
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub gz: bool,
    /// The file's name (its extension tells the importer what it is).
    #[serde(skip_serializing_if = "String::is_empty")]
    pub name: String,
}

impl BlobRef {
    /// Where the blob is in the remote: `blobs/<hash>` (`.gz` when gzipped).
    pub fn remote_key(&self) -> String {
        format!("blobs/{}{}", self.hash, if self.gz { ".gz" } else { "" })
    }
    /// Where the blob is in a local store: `blobs/<hash>/<name>` (uncompressed).
    pub fn local_key(&self) -> String {
        format!("blobs/{}/{}", self.hash, safe_name(&self.name))
    }
    pub fn valid(&self) -> bool {
        is_hash(&self.hash)
    }
}

/// A file name that is safe as one path segment (keeps the extension readable).
pub(crate) fn safe_name(name: &str) -> String {
    let base = name.rsplit(['/', '\\']).next().unwrap_or_default();
    let cleaned: String = base.chars().map(|c| if c.is_alphanumeric() || matches!(c, '.' | '-' | '_' | ' ') { c } else { '_' }).collect();
    let trimmed = cleaned.trim().trim_start_matches('.');
    let mut out: String = trimmed.chars().take(120).collect();
    if out.is_empty() {
        out = "blob".into();
    }
    out
}

/// A file a card's project uses that is uploaded with it.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct CardFile {
    /// The path the project names (on the publishing Mac); replaced by the cached copy on apply.
    pub path: String,
    pub blob: BlobRef,
}

/// Footage a card's project uses that was not uploaded (video, or over the size limit): the
/// project keeps it as a missing item with its name, size, duration and rate, to relink.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Placeholder {
    /// The path the project names.
    pub path: String,
    pub name: String,
    /// File size in bytes (0 = unknown or missing on the publishing Mac).
    pub size: u64,
    /// `video`, `too large` or `missing` (not found when publishing).
    pub reason: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub width: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub height: Option<u32>,
    /// Seconds.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub duration: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub fps: Option<f64>,
}

/// One card of the catalog.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Card {
    pub id: String,
    pub name: String,
    /// Folder path, e.g. `Devin Jatho/UI`.
    pub category: String,
    /// `comp` (a project whose composition is nested) or `footage` (a file added as a layer).
    pub kind: String,
    /// The composition to nest (`comp` cards); empty = the project's primary composition.
    #[serde(skip_serializing_if = "String::is_empty")]
    pub comp: String,
    #[serde(skip_serializing_if = "String::is_empty")]
    pub description: String,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub tags: Vec<String>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub aliases: Vec<String>,
    #[serde(skip_serializing_if = "String::is_empty")]
    pub visual_caption: String,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub motion_events: Vec<String>,
    #[serde(skip_serializing_if = "String::is_empty")]
    pub summary: String,
    #[serde(skip_serializing_if = "String::is_empty")]
    pub generated_summary: String,
    #[serde(skip_serializing_if = "String::is_empty")]
    pub motion_terms: String,
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub priority: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub duration: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub thumbnail: Option<BlobRef>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub preview: Option<BlobRef>,
    /// The `.ecproj` (`comp`) or the file (`footage`).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub project: Option<BlobRef>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub files: Vec<CardFile>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub placeholders: Vec<Placeholder>,
    /// SHA-256 of everything above: a card whose revision changed is "changed".
    pub rev: String,
}

impl Card {
    /// Footage left out of the upload: applying the card imports it as missing footage.
    pub fn needs_footage(&self) -> bool {
        !self.placeholders.is_empty()
    }
    /// The revision of this card's contents ([`Card::rev`] excluded).
    pub fn compute_rev(&self) -> String {
        let mut c = self.clone();
        c.rev.clear();
        sha256_hex(&serde_json::to_vec(&c).unwrap_or_default())
    }
    /// Every blob applying needs (project and files), in order.
    pub fn apply_blobs(&self) -> Vec<&BlobRef> {
        self.project.iter().chain(self.files.iter().map(|f| &f.blob)).collect()
    }
    /// The top folder (`Devin Jatho` of `Devin Jatho/UI`).
    pub fn top_folder(&self) -> &str {
        self.category.split('/').next().unwrap_or_default()
    }
    /// Whether the card is in `folder` or one of its subfolders.
    pub fn in_folder(&self, folder: &str) -> bool {
        folder.is_empty() || self.category == folder || self.category.strip_prefix(folder).is_some_and(|rest| rest.starts_with('/'))
    }
}

/// The catalog.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Manifest {
    pub format: String,
    pub schema: u32,
    pub version: u64,
    pub name: String,
    /// The top folders published.
    pub folders: Vec<String>,
    pub cards: Vec<Card>,
}

/// The catalog's version and manifest location.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Head {
    pub format: String,
    pub schema: u32,
    pub version: u64,
    pub manifest: BlobRef,
    /// Seconds since 1970 (UTC) of the publish.
    pub published: u64,
    pub cards: usize,
}

impl Head {
    pub fn parse(bytes: &[u8]) -> Result<Head, String> {
        let head: Head = serde_json::from_slice(bytes).map_err(|e| format!("library head: {e}"))?;
        if head.format != FORMAT {
            return Err("library head: not an EffectCraft Motion Library".into());
        }
        if head.schema > SCHEMA {
            return Err(format!("library head: written for a newer EffectCraft (schema {})", head.schema));
        }
        if !head.manifest.valid() {
            return Err("library head: bad manifest reference".into());
        }
        Ok(head)
    }
    pub fn new(version: u64, manifest: BlobRef, published: u64, cards: usize) -> Head {
        Head { format: FORMAT.into(), schema: SCHEMA, version, manifest, published, cards }
    }
}

/// What a newer catalog changes.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize)]
pub struct Changes {
    pub new: usize,
    pub changed: usize,
    pub removed: usize,
}

impl Changes {
    pub fn is_empty(&self) -> bool {
        self.new + self.changed + self.removed == 0
    }
    /// "3 new · 2 changed" (only the non-zero parts).
    pub fn label(&self) -> String {
        let parts: Vec<String> =
            [(self.new, "new"), (self.changed, "changed"), (self.removed, "removed")].iter().filter(|(n, _)| *n > 0).map(|(n, w)| format!("{n} {w}")).collect();
        if parts.is_empty() { "no card changes".into() } else { parts.join(" · ") }
    }
}

impl Manifest {
    pub fn new(name: &str, folders: Vec<String>, cards: Vec<Card>, version: u64) -> Manifest {
        Manifest { format: FORMAT.into(), schema: SCHEMA, version, name: name.into(), folders, cards }
    }

    /// Parse a manifest's (uncompressed) bytes, dropping cards without an id.
    pub fn parse(bytes: &[u8]) -> Result<Manifest, String> {
        let mut m: Manifest = serde_json::from_slice(bytes).map_err(|e| format!("library manifest: {e}"))?;
        if m.format != FORMAT {
            return Err("library manifest: not an EffectCraft Motion Library".into());
        }
        if m.schema > SCHEMA {
            return Err(format!("library manifest: written for a newer EffectCraft (schema {})", m.schema));
        }
        m.cards.retain(|c| !c.id.is_empty());
        Ok(m)
    }

    pub fn card(&self, id: &str) -> Option<&Card> {
        self.cards.iter().find(|c| c.id == id)
    }

    /// What `newer` adds, changes and removes relative to this catalog.
    pub fn changes_to(&self, newer: &Manifest) -> Changes {
        let old: BTreeMap<&str, &str> = self.cards.iter().map(|c| (c.id.as_str(), c.rev.as_str())).collect();
        let new: BTreeMap<&str, &str> = newer.cards.iter().map(|c| (c.id.as_str(), c.rev.as_str())).collect();
        Changes {
            new: new.keys().filter(|id| !old.contains_key(*id)).count(),
            changed: new.iter().filter(|(id, rev)| old.get(*id).is_some_and(|o| o != *rev)).count(),
            removed: old.keys().filter(|id| !new.contains_key(*id)).count(),
        }
    }

    /// Every folder path with its card count (subfolders included in their parents), sorted.
    pub fn folder_counts(&self) -> Vec<(String, usize)> {
        let mut counts: BTreeMap<String, usize> = BTreeMap::new();
        for c in &self.cards {
            let mut path = String::new();
            for part in c.category.split('/').filter(|p| !p.is_empty()) {
                if !path.is_empty() {
                    path.push('/');
                }
                path.push_str(part);
                *counts.entry(path.clone()).or_default() += 1;
            }
        }
        counts.into_iter().collect()
    }
}

/// Bytes as stored: gzipped when `gz`.
pub fn encode_blob(data: &[u8], gz: bool) -> Result<Vec<u8>, String> {
    if !gz {
        return Ok(data.to_vec());
    }
    let mut enc = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::default());
    enc.write_all(data).map_err(|e| e.to_string())?;
    enc.finish().map_err(|e| e.to_string())
}

/// A stored blob back to its bytes, checked against its reference (hash and size).
pub fn decode_blob(blob: &BlobRef, stored: &[u8]) -> Result<Vec<u8>, String> {
    let data = if blob.gz {
        let mut out = Vec::new();
        flate2::read::GzDecoder::new(stored)
            .take(MAX_INFLATED.min(blob.size.saturating_add(1)))
            .read_to_end(&mut out)
            .map_err(|e| format!("{}: {e}", blob.hash))?;
        out
    } else {
        stored.to_vec()
    };
    if data.len() as u64 != blob.size || sha256_hex(&data) != blob.hash {
        return Err(format!("download of {} is damaged (hash or size differs)", if blob.name.is_empty() { &blob.hash } else { &blob.name }));
    }
    Ok(data)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn card(id: &str, rev: &str, cat: &str) -> Card {
        Card { id: id.into(), name: id.into(), category: cat.into(), rev: rev.into(), ..Default::default() }
    }

    #[test]
    fn changes_and_folders() {
        let a = Manifest::new("L", vec![], vec![card("a", "1", "Devin Jatho/UI"), card("b", "1", "SceneYard"), card("c", "1", "SceneYard")], 1);
        let b = Manifest::new("L", vec![], vec![card("a", "2", "Devin Jatho/UI"), card("b", "1", "SceneYard"), card("d", "1", "SceneYard")], 2);
        let ch = a.changes_to(&b);
        assert_eq!(ch, Changes { new: 1, changed: 1, removed: 1 });
        assert_eq!(ch.label(), "1 new · 1 changed · 1 removed");
        assert_eq!(a.folder_counts(), vec![("Devin Jatho".into(), 1), ("Devin Jatho/UI".into(), 1), ("SceneYard".into(), 2)]);
        assert!(a.cards[0].in_folder("Devin Jatho") && !a.cards[0].in_folder("Devin") && a.cards[0].in_folder(""));
    }

    #[test]
    fn blobs_round_trip_and_reject_damage() {
        let data = b"{\"hello\": \"world\"}".repeat(50);
        let blob = BlobRef { hash: sha256_hex(&data), size: data.len() as u64, gz: true, name: "x.ecproj".into() };
        let stored = encode_blob(&data, true).unwrap();
        assert!(stored.len() < data.len());
        assert_eq!(decode_blob(&blob, &stored).unwrap(), data);
        assert!(decode_blob(&blob, b"not gzip").is_err());
        let raw = BlobRef { gz: false, ..blob.clone() };
        assert!(decode_blob(&raw, &data[1..]).is_err());
        assert_eq!(blob.remote_key(), format!("blobs/{}.gz", blob.hash));
        assert_eq!(blob.local_key(), format!("blobs/{}/x.ecproj", blob.hash));
    }

    #[test]
    fn bad_documents_are_errors() {
        assert!(Manifest::parse(b"nope").is_err());
        assert!(Manifest::parse(br#"{"format":"other"}"#).is_err());
        assert!(Manifest::parse(br#"{"format":"effectcraft-motion-library","schema":99}"#).is_err());
        assert!(Head::parse(br#"{"format":"effectcraft-motion-library","manifest":{"hash":"../x"}}"#).is_err());
        let m = Manifest::parse(br#"{"format":"effectcraft-motion-library","cards":[{"id":""},{"id":"a","bogus":1}]}"#).unwrap();
        assert_eq!(m.cards.len(), 1);
    }

    #[test]
    fn safe_names() {
        assert_eq!(safe_name("/Users/x/My clip.mov"), "My clip.mov");
        assert_eq!(safe_name("../../etc/passwd"), "passwd");
        assert_eq!(safe_name(".."), "blob");
        assert_eq!(safe_name("a:b?.png"), "a_b_.png");
    }
}
