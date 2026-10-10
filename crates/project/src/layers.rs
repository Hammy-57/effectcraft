//! A composition's layer stack with per-layer structural sharing.
//!
//! [`Layers`] holds each layer behind an `Arc`, so cloning a comp (every undoable edit clones
//! the project, the history keeps snapshots, frontends hold frames of it) costs one reference
//! count per layer instead of a deep copy of every property tree. Mutable access is
//! copy-on-write per layer ([`Arc::make_mut`]): an edit copies only the layers it touches, and
//! the snapshots before and after it share every other layer by pointer.
//!
//! The API mirrors `Vec<Layer>` (reads see `&Layer`), and the serialized form is the plain list
//! of layers, byte for byte the same as before.

use std::fmt;
use std::ops::{Index, IndexMut, RangeBounds};
use std::sync::Arc;

use serde::{Deserialize, Serialize};

use crate::Layer;

/// Layer #1 first (top of the stack). See the module docs.
#[derive(Clone, Default, Serialize, Deserialize)]
#[serde(transparent)]
pub struct Layers(Vec<Arc<Layer>>);

fn shared(l: &Arc<Layer>) -> &Layer {
    l
}

fn unique(l: &mut Arc<Layer>) -> &mut Layer {
    Arc::make_mut(l)
}

/// Iterator over `&Layer`.
pub type Iter<'a> = std::iter::Map<std::slice::Iter<'a, Arc<Layer>>, fn(&Arc<Layer>) -> &Layer>;
/// Iterator over `&mut Layer` (each layer it yields is made unique: copy-on-write).
pub type IterMut<'a> = std::iter::Map<std::slice::IterMut<'a, Arc<Layer>>, fn(&mut Arc<Layer>) -> &mut Layer>;

impl Layers {
    pub const fn new() -> Layers {
        Layers(Vec::new())
    }
    pub fn with_capacity(n: usize) -> Layers {
        Layers(Vec::with_capacity(n))
    }
    pub fn len(&self) -> usize {
        self.0.len()
    }
    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }
    pub fn iter(&self) -> Iter<'_> {
        self.0.iter().map(shared as fn(&Arc<Layer>) -> &Layer)
    }
    /// Every layer it yields is copied unless no other snapshot shares it: to change one layer,
    /// prefer [`Layers::get_mut`] or [`crate::Comp::layer_mut`].
    pub fn iter_mut(&mut self) -> IterMut<'_> {
        self.0.iter_mut().map(unique as fn(&mut Arc<Layer>) -> &mut Layer)
    }
    /// Copy-on-write access to the layers matching `f`: the others are only read, so they stay
    /// shared (unlike `iter_mut().filter(..)`, which copies every layer it looks at).
    pub fn iter_mut_where<'a>(&'a mut self, mut f: impl FnMut(&Layer) -> bool + 'a) -> impl Iterator<Item = &'a mut Layer> + 'a {
        self.0.iter_mut().filter(move |l| f(l)).map(Arc::make_mut)
    }
    pub fn get(&self, i: usize) -> Option<&Layer> {
        self.0.get(i).map(|l| &**l)
    }
    /// Copy-on-write access to one layer.
    pub fn get_mut(&mut self, i: usize) -> Option<&mut Layer> {
        self.0.get_mut(i).map(Arc::make_mut)
    }
    pub fn first(&self) -> Option<&Layer> {
        self.0.first().map(|l| &**l)
    }
    pub fn last(&self) -> Option<&Layer> {
        self.0.last().map(|l| &**l)
    }
    pub fn first_mut(&mut self) -> Option<&mut Layer> {
        self.0.first_mut().map(Arc::make_mut)
    }
    pub fn last_mut(&mut self) -> Option<&mut Layer> {
        self.0.last_mut().map(Arc::make_mut)
    }
    /// Copy-on-write access to the first layer matching `f`: the others are only read, so they
    /// stay shared.
    pub fn find_mut(&mut self, mut f: impl FnMut(&Layer) -> bool) -> Option<&mut Layer> {
        let i = self.0.iter().position(|l| f(l))?;
        self.get_mut(i)
    }
    pub fn push(&mut self, l: Layer) {
        self.0.push(Arc::new(l));
    }
    pub fn insert(&mut self, i: usize, l: Layer) {
        self.0.insert(i, Arc::new(l));
    }
    /// Removes and returns the layer at `i` (panics when out of range, like `Vec::remove`).
    pub fn remove(&mut self, i: usize) -> Layer {
        Arc::unwrap_or_clone(self.0.remove(i))
    }
    pub fn pop(&mut self) -> Option<Layer> {
        self.0.pop().map(Arc::unwrap_or_clone)
    }
    pub fn retain(&mut self, mut f: impl FnMut(&Layer) -> bool) {
        self.0.retain(|l| f(l));
    }
    pub fn truncate(&mut self, n: usize) {
        self.0.truncate(n);
    }
    pub fn clear(&mut self) {
        self.0.clear();
    }
    pub fn swap(&mut self, a: usize, b: usize) {
        self.0.swap(a, b);
    }
    pub fn reverse(&mut self) {
        self.0.reverse();
    }
    pub fn rotate_left(&mut self, n: usize) {
        self.0.rotate_left(n);
    }
    pub fn rotate_right(&mut self, n: usize) {
        self.0.rotate_right(n);
    }
    pub fn sort_by_key<K: Ord>(&mut self, mut f: impl FnMut(&Layer) -> K) {
        self.0.sort_by_key(|l| f(l));
    }
    pub fn sort_by(&mut self, mut f: impl FnMut(&Layer, &Layer) -> std::cmp::Ordering) {
        self.0.sort_by(|a, b| f(a, b));
    }
    pub fn drain<R: RangeBounds<usize>>(&mut self, r: R) -> impl DoubleEndedIterator<Item = Layer> + '_ {
        self.0.drain(r).map(Arc::unwrap_or_clone)
    }
    pub fn split_off(&mut self, at: usize) -> Layers {
        Layers(self.0.split_off(at))
    }
    pub fn append(&mut self, other: &mut Layers) {
        self.0.append(&mut other.0);
    }
    /// The layers as a plain list (a deep copy).
    pub fn to_vec(&self) -> Vec<Layer> {
        self.iter().cloned().collect()
    }
    /// The shared layer handles: compare snapshots with `Arc::ptr_eq`, or clone one cheaply.
    pub fn arcs(&self) -> &[Arc<Layer>] {
        &self.0
    }
    /// The shared layer handles, for reordering or splicing without touching the layers.
    pub fn arcs_mut(&mut self) -> &mut Vec<Arc<Layer>> {
        &mut self.0
    }
    /// Whether layer `i` is the very same allocation in both stacks (unchanged by an edit).
    pub fn shares(&self, other: &Layers, i: usize) -> bool {
        matches!((self.0.get(i), other.0.get(i)), (Some(a), Some(b)) if Arc::ptr_eq(a, b))
    }
}

impl PartialEq for Layers {
    /// Shared layers compare by pointer first, so comparing two snapshots costs only the layers
    /// that differ.
    fn eq(&self, other: &Layers) -> bool {
        self.0.len() == other.0.len() && self.0.iter().zip(&other.0).all(|(a, b)| Arc::ptr_eq(a, b) || a == b)
    }
}

impl fmt::Debug for Layers {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_list().entries(self.iter()).finish()
    }
}

impl Index<usize> for Layers {
    type Output = Layer;
    fn index(&self, i: usize) -> &Layer {
        &self.0[i]
    }
}

impl IndexMut<usize> for Layers {
    fn index_mut(&mut self, i: usize) -> &mut Layer {
        Arc::make_mut(&mut self.0[i])
    }
}

impl<'a> IntoIterator for &'a Layers {
    type Item = &'a Layer;
    type IntoIter = Iter<'a>;
    fn into_iter(self) -> Iter<'a> {
        self.iter()
    }
}

impl<'a> IntoIterator for &'a mut Layers {
    type Item = &'a mut Layer;
    type IntoIter = IterMut<'a>;
    fn into_iter(self) -> IterMut<'a> {
        self.iter_mut()
    }
}

impl IntoIterator for Layers {
    type Item = Layer;
    type IntoIter = std::iter::Map<std::vec::IntoIter<Arc<Layer>>, fn(Arc<Layer>) -> Layer>;
    fn into_iter(self) -> Self::IntoIter {
        self.0.into_iter().map(Arc::unwrap_or_clone as fn(Arc<Layer>) -> Layer)
    }
}

impl FromIterator<Layer> for Layers {
    fn from_iter<I: IntoIterator<Item = Layer>>(it: I) -> Layers {
        Layers(it.into_iter().map(Arc::new).collect())
    }
}

impl FromIterator<Arc<Layer>> for Layers {
    fn from_iter<I: IntoIterator<Item = Arc<Layer>>>(it: I) -> Layers {
        Layers(it.into_iter().collect())
    }
}

impl Extend<Layer> for Layers {
    fn extend<I: IntoIterator<Item = Layer>>(&mut self, it: I) {
        self.0.extend(it.into_iter().map(Arc::new));
    }
}

impl From<Vec<Layer>> for Layers {
    fn from(v: Vec<Layer>) -> Layers {
        v.into_iter().collect()
    }
}

impl<const N: usize> From<[Layer; N]> for Layers {
    fn from(v: [Layer; N]) -> Layers {
        v.into_iter().collect()
    }
}

impl From<Layers> for Vec<Layer> {
    fn from(l: Layers) -> Vec<Layer> {
        l.into_iter().collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{Comp, ItemKind, LayerSource, Project, build};
    use effectcraft_color::Label;
    use effectcraft_time::{FrameRate, Tick};

    fn project() -> (Project, crate::ItemId) {
        let mut p = Project::default();
        let mut comp = Comp::new(64, 48, FrameRate::new(30, 1), Tick::from_seconds_f64(2.0));
        for (i, src) in [LayerSource::Null, LayerSource::Shape, LayerSource::Camera].into_iter().enumerate() {
            let l = build::layer(&mut p, &comp, &format!("L{i}"), src, (64, 48), None);
            comp.layers.push(l);
        }
        let id = p.add_item("C", Label::None, None, ItemKind::Comp(Arc::new(comp)));
        (p, id)
    }

    #[test]
    fn serializes_exactly_as_a_plain_list() {
        let (p, id) = project();
        let layers = &p.comp(id).unwrap().layers;
        let plain: Vec<Layer> = layers.to_vec();
        assert_eq!(serde_json::to_string(layers).unwrap(), serde_json::to_string(&plain).unwrap());
        assert_eq!(serde_json::to_string_pretty(layers).unwrap(), serde_json::to_string_pretty(&plain).unwrap());
        assert_eq!(format!("{layers:?}"), format!("{plain:?}"));
        // A list written by the old `Vec<Layer>` reads back the same, and the project file round-trips byte for byte.
        let back: Layers = serde_json::from_str(&serde_json::to_string(&plain).unwrap()).unwrap();
        assert_eq!(&back, layers);
        let text = p.to_json();
        let again = Project::from_json(&text).unwrap();
        assert_eq!(again, p);
        assert_eq!(again.to_json(), text);
    }

    #[test]
    fn clones_share_layers_until_one_is_changed() {
        let (p, id) = project();
        let mut q = p.clone();
        let c = q.comp_mut(id).unwrap();
        assert!((0..3).all(|i| c.layers.shares(&p.comp(id).unwrap().layers, i)));
        let lid = c.layers[1].id;
        c.layer_mut(lid).unwrap().name = "changed".into();
        let (a, b) = (&p.comp(id).unwrap().layers, &q.comp(id).unwrap().layers);
        assert!(a.shares(b, 0) && !a.shares(b, 1) && a.shares(b, 2));
        assert_eq!((a[1].name.as_str(), b[1].name.as_str()), ("L1", "changed"));
        assert_ne!(a, b);
        // Only the matching layers are copied.
        let mut r = p.clone();
        let rl = &mut r.comp_mut(id).unwrap().layers;
        for l in rl.iter_mut_where(|l| l.name == "L2") {
            l.name = "x".into();
        }
        assert!(rl.shares(a, 0) && rl.shares(a, 1) && !rl.shares(a, 2));
        let removed = rl.remove(0);
        assert_eq!(removed.name, "L0");
        assert_eq!(rl.len(), 2);
    }
}
