//! A save-time check that a project serialises to JSON that reads back: JSON has no NaN or
//! infinity (serde_json writes them as `null`, which a number field then refuses on open). It walks
//! the value through serde without writing anything, which is much cheaper than parsing the saved
//! text again, and names where the bad number is.

use serde::ser::{self, Serialize};
use std::fmt;

/// Check that `value` holds no number JSON can't store (where reading it back would fail).
/// The error names the path to the first one (`items/3/kind/Comp/layers/0/…`).
pub fn check<T: Serialize + ?Sized>(value: &T) -> Result<(), NonFinite> {
    value.serialize(&mut Checker { in_some: false, index: Vec::new() })
}

/// A number that isn't finite where a project file needs one.
#[derive(Debug)]
pub struct NonFinite {
    /// Path segments, innermost first (filled in as the error propagates out).
    path: Vec<String>,
    msg: String,
}

impl NonFinite {
    fn at(mut self, seg: impl fmt::Display) -> NonFinite {
        self.path.push(seg.to_string());
        self
    }
}

impl fmt::Display for NonFinite {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let path: Vec<&str> = self.path.iter().rev().map(String::as_str).collect();
        if path.is_empty() { write!(f, "{}", self.msg) } else { write!(f, "{} at {}", self.msg, path.join("/")) }
    }
}

impl std::error::Error for NonFinite {}

impl ser::Error for NonFinite {
    fn custom<M: fmt::Display>(msg: M) -> Self {
        NonFinite { path: vec![], msg: msg.to_string() }
    }
}

/// Serialises nothing; fails on a non-finite float unless it is directly inside `Some` (written as
/// `null`, it reads back as `None`, as it always has).
struct Checker {
    in_some: bool,
    /// The next element index of each open sequence (for error paths).
    index: Vec<usize>,
}

impl Checker {
    fn float(&mut self, finite: bool) -> Result<(), NonFinite> {
        let in_some = std::mem::take(&mut self.in_some);
        if finite || in_some { Ok(()) } else { Err(ser::Error::custom("a number that isn't finite (NaN or infinity)")) }
    }
    fn leaf(&mut self) -> Result<(), NonFinite> {
        self.in_some = false;
        Ok(())
    }
    fn compound(&mut self) -> Result<&mut Self, NonFinite> {
        self.in_some = false;
        Ok(self)
    }
    fn sequence(&mut self) -> Result<&mut Self, NonFinite> {
        self.index.push(0);
        self.compound()
    }
    /// Check the next element of the innermost sequence.
    fn item<T: Serialize + ?Sized>(&mut self, value: &T) -> Result<(), NonFinite> {
        let i = self.index.last().copied().unwrap_or(0);
        if let Some(n) = self.index.last_mut() {
            *n = n.saturating_add(1);
        }
        element(self, value).map_err(|e| e.at(i))
    }
    fn end_sequence(&mut self) -> Result<(), NonFinite> {
        self.index.pop();
        Ok(())
    }
}

/// The text of a map key for an error path (only built when a value under it fails).
fn key_text<K: Serialize + ?Sized>(key: &K) -> String {
    serde_json::to_string(key).map(|s| s.trim_matches('"').to_string()).unwrap_or_else(|_| "?".into())
}

impl ser::Serializer for &mut Checker {
    type Ok = ();
    type Error = NonFinite;
    type SerializeSeq = Self;
    type SerializeTuple = Self;
    type SerializeTupleStruct = Self;
    type SerializeTupleVariant = Self;
    type SerializeMap = Self;
    type SerializeStruct = Self;
    type SerializeStructVariant = Self;

    fn serialize_bool(self, _: bool) -> Result<(), NonFinite> {
        self.leaf()
    }
    fn serialize_i8(self, _: i8) -> Result<(), NonFinite> {
        self.leaf()
    }
    fn serialize_i16(self, _: i16) -> Result<(), NonFinite> {
        self.leaf()
    }
    fn serialize_i32(self, _: i32) -> Result<(), NonFinite> {
        self.leaf()
    }
    fn serialize_i64(self, _: i64) -> Result<(), NonFinite> {
        self.leaf()
    }
    fn serialize_i128(self, _: i128) -> Result<(), NonFinite> {
        self.leaf()
    }
    fn serialize_u8(self, _: u8) -> Result<(), NonFinite> {
        self.leaf()
    }
    fn serialize_u16(self, _: u16) -> Result<(), NonFinite> {
        self.leaf()
    }
    fn serialize_u32(self, _: u32) -> Result<(), NonFinite> {
        self.leaf()
    }
    fn serialize_u64(self, _: u64) -> Result<(), NonFinite> {
        self.leaf()
    }
    fn serialize_u128(self, _: u128) -> Result<(), NonFinite> {
        self.leaf()
    }
    fn serialize_f32(self, v: f32) -> Result<(), NonFinite> {
        self.float(v.is_finite())
    }
    fn serialize_f64(self, v: f64) -> Result<(), NonFinite> {
        self.float(v.is_finite())
    }
    fn serialize_char(self, _: char) -> Result<(), NonFinite> {
        self.leaf()
    }
    fn serialize_str(self, _: &str) -> Result<(), NonFinite> {
        self.leaf()
    }
    fn serialize_bytes(self, _: &[u8]) -> Result<(), NonFinite> {
        self.leaf()
    }
    fn serialize_none(self) -> Result<(), NonFinite> {
        self.leaf()
    }
    fn serialize_some<T: Serialize + ?Sized>(self, value: &T) -> Result<(), NonFinite> {
        self.in_some = true;
        let r = value.serialize(&mut *self);
        self.in_some = false;
        r
    }
    fn serialize_unit(self) -> Result<(), NonFinite> {
        self.leaf()
    }
    fn serialize_unit_struct(self, _: &'static str) -> Result<(), NonFinite> {
        self.leaf()
    }
    fn serialize_unit_variant(self, _: &'static str, _: u32, _: &'static str) -> Result<(), NonFinite> {
        self.leaf()
    }
    // Transparent in JSON: `Some(Newtype(NaN))` is `null` too, so `in_some` carries through.
    fn serialize_newtype_struct<T: Serialize + ?Sized>(self, _: &'static str, value: &T) -> Result<(), NonFinite> {
        value.serialize(self)
    }
    fn serialize_newtype_variant<T: Serialize + ?Sized>(self, _: &'static str, _: u32, variant: &'static str, value: &T) -> Result<(), NonFinite> {
        self.in_some = false;
        value.serialize(self).map_err(|e| e.at(variant))
    }
    fn serialize_seq(self, _: Option<usize>) -> Result<Self, NonFinite> {
        self.sequence()
    }
    fn serialize_tuple(self, _: usize) -> Result<Self, NonFinite> {
        self.sequence()
    }
    fn serialize_tuple_struct(self, _: &'static str, _: usize) -> Result<Self, NonFinite> {
        self.sequence()
    }
    fn serialize_tuple_variant(self, _: &'static str, _: u32, _: &'static str, _: usize) -> Result<Self, NonFinite> {
        self.sequence()
    }
    fn serialize_map(self, _: Option<usize>) -> Result<Self, NonFinite> {
        self.compound()
    }
    fn serialize_struct(self, _: &'static str, _: usize) -> Result<Self, NonFinite> {
        self.compound()
    }
    fn serialize_struct_variant(self, _: &'static str, _: u32, _: &'static str, _: usize) -> Result<Self, NonFinite> {
        self.compound()
    }
}

/// Check one element, struct field or map entry.
fn element<T: Serialize + ?Sized>(c: &mut Checker, value: &T) -> Result<(), NonFinite> {
    c.in_some = false;
    value.serialize(&mut *c)
}

impl ser::SerializeSeq for &mut Checker {
    type Ok = ();
    type Error = NonFinite;
    fn serialize_element<T: Serialize + ?Sized>(&mut self, value: &T) -> Result<(), NonFinite> {
        self.item(value)
    }
    fn end(self) -> Result<(), NonFinite> {
        self.end_sequence()
    }
}

impl ser::SerializeTuple for &mut Checker {
    type Ok = ();
    type Error = NonFinite;
    fn serialize_element<T: Serialize + ?Sized>(&mut self, value: &T) -> Result<(), NonFinite> {
        self.item(value)
    }
    fn end(self) -> Result<(), NonFinite> {
        self.end_sequence()
    }
}

impl ser::SerializeTupleStruct for &mut Checker {
    type Ok = ();
    type Error = NonFinite;
    fn serialize_field<T: Serialize + ?Sized>(&mut self, value: &T) -> Result<(), NonFinite> {
        self.item(value)
    }
    fn end(self) -> Result<(), NonFinite> {
        self.end_sequence()
    }
}

impl ser::SerializeTupleVariant for &mut Checker {
    type Ok = ();
    type Error = NonFinite;
    fn serialize_field<T: Serialize + ?Sized>(&mut self, value: &T) -> Result<(), NonFinite> {
        self.item(value)
    }
    fn end(self) -> Result<(), NonFinite> {
        self.end_sequence()
    }
}

impl ser::SerializeMap for &mut Checker {
    type Ok = ();
    type Error = NonFinite;
    fn serialize_key<T: Serialize + ?Sized>(&mut self, key: &T) -> Result<(), NonFinite> {
        element(self, key)
    }
    fn serialize_value<T: Serialize + ?Sized>(&mut self, value: &T) -> Result<(), NonFinite> {
        element(self, value)
    }
    fn serialize_entry<K: Serialize + ?Sized, V: Serialize + ?Sized>(&mut self, key: &K, value: &V) -> Result<(), NonFinite> {
        element(self, key)?;
        element(self, value).map_err(|e| e.at(key_text(key)))
    }
    fn end(self) -> Result<(), NonFinite> {
        Ok(())
    }
}

impl ser::SerializeStruct for &mut Checker {
    type Ok = ();
    type Error = NonFinite;
    fn serialize_field<T: Serialize + ?Sized>(&mut self, key: &'static str, value: &T) -> Result<(), NonFinite> {
        element(self, value).map_err(|e| e.at(key))
    }
    fn end(self) -> Result<(), NonFinite> {
        Ok(())
    }
}

impl ser::SerializeStructVariant for &mut Checker {
    type Ok = ();
    type Error = NonFinite;
    fn serialize_field<T: Serialize + ?Sized>(&mut self, key: &'static str, value: &T) -> Result<(), NonFinite> {
        element(self, value).map_err(|e| e.at(key))
    }
    fn end(self) -> Result<(), NonFinite> {
        Ok(())
    }
}
