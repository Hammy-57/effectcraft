//! Shape-preserving subdivision of an animated motion path.

use super::{Ease, Interp, Keyframe, SpatialSeg, Value, effective_ease, secs, solve_u, spatial_tangents, v3};
use effectcraft_time::Tick;

fn split<const N: usize>(p: [[f64; N]; 4], u: f64) -> ([[f64; N]; 4], [[f64; N]; 4]) {
    let mix = |a: [f64; N], b: [f64; N]| std::array::from_fn(|d| a[d] + (b[d] - a[d]) * u);
    let a = mix(p[0], p[1]);
    let b = mix(p[1], p[2]);
    let c = mix(p[2], p[3]);
    let d = mix(a, b);
    let e = mix(b, c);
    let f = mix(d, e);
    ([p[0], a, d, f], [f, e, c, p[3]])
}

fn ease(p: [[f64; 2]; 4], out: bool, length: f64) -> Result<Ease, &'static str> {
    let duration = p[3][0] - p[0][0];
    let distance = p[3][1] - p[0][1];
    let (a, b) = if out { (p[0], p[1]) } else { (p[2], p[3]) };
    let dt = b[0] - a[0];
    if dt <= 1e-14 || duration <= 0.0 || distance <= 1e-12 {
        return Err("Cannot insert at a stationary or degenerate ease; choose another point on the moving segment");
    }
    let result = Ease { speed: (b[1] - a[1]) / dt * length / distance, influence: dt / duration };
    if !result.speed.is_finite() || !result.influence.is_finite() || !(0.0..=1.0).contains(&result.influence) {
        return Err("Cannot represent this temporal split without changing the motion; adjust the temporal ease first");
    }
    Ok(result)
}

/// Insert a key at exact layer time, subdividing spatial and temporal Beziers. The curve is
/// unchanged; distance evaluation retains the evaluator's arc-table approximation. Returns a
/// replacement only on success, so unsupported input never partially edits the caller's keys.
pub fn insert_spatial(keys: &[Keyframe], time: Tick) -> Result<Vec<Keyframe>, &'static str> {
    if keys.windows(2).any(|pair| pair.first().zip(pair.get(1)).is_some_and(|(a, b)| a.time >= b.time)) {
        return Err("Motion-path key times must be strictly increasing");
    }
    if keys.iter().any(|k| k.time == time) {
        return Err("A keyframe already exists at this layer time");
    }
    let next = keys.partition_point(|k| k.time < time);
    let i = next.checked_sub(1).ok_or("Choose a time strictly between two motion-path keys")?;
    let a = keys.get(i).ok_or("Choose a time strictly between two motion-path keys")?;
    let b = keys.get(next).ok_or("Choose a time strictly between two motion-path keys")?;
    if !matches!((&a.value, &b.value), (Value::Vec2(_), Value::Vec2(_)) | (Value::Vec3(_), Value::Vec3(_))) {
        return Err("Insert Spatial requires a 2D or 3D motion path");
    }
    if a.out_interp == Interp::Hold || a.roving || b.roving {
        return Err("Cannot split Hold or roving motion-path segments; disable Hold/Rove Across Time first");
    }
    let (tin, tout) = spatial_tangents(keys, i);
    let (bin, bout) = spatial_tangents(keys, next);
    let p0 = v3(&a.value);
    let p3 = v3(&b.value);
    let points = [p0, std::array::from_fn(|d| p0[d] + tout[d]), std::array::from_fn(|d| p3[d] + bin[d]), p3];
    if !points.iter().flatten().all(|v| v.is_finite()) {
        return Err("Motion-path coordinates must be finite");
    }
    let seg = SpatialSeg::new(points);
    let len = seg.length();
    let duration = secs(b.time) - secs(a.time);
    if !len.is_finite() || len <= 1e-12 || duration <= 0.0 {
        return Err("Choose a nonzero motion-path segment");
    }
    let linear = Ease { speed: len / duration, influence: 1.0 / 3.0 };
    let eo = if a.out_interp == Interp::Linear { linear } else { effective_ease(keys, i, 0, true, true) };
    let ei = if b.in_interp != Interp::Bezier { linear } else { effective_ease(keys, next, 0, true, false) };
    let temporal = [
        [0.0, 0.0],
        [eo.influence * duration, eo.speed * eo.influence * duration],
        [(1.0 - ei.influence) * duration, len - ei.speed * ei.influence * duration],
        [duration, len],
    ];
    if !temporal.iter().flatten().all(|v| v.is_finite())
        || !(0.0..=1.0).contains(&eo.influence)
        || !(0.0..=1.0).contains(&ei.influence)
        || temporal[1][1] < 0.0
        || temporal[2][1] < temporal[1][1]
        || temporal[2][1] > len
    {
        return Err("Cannot split a reversing or overshooting temporal ease; use monotone motion first");
    }
    let u = solve_u(0.0, temporal[1][0], temporal[2][0], duration, secs(time) - secs(a.time));
    let (tl, tr) = split(temporal, u);
    let distance = tl[3][1];
    let path_u = seg.parameter_at_length(distance);
    let (left, right) = split(points, path_u);
    let left_len = SpatialSeg::new(left).length();
    let right_len = SpatialSeg::new(right).length();
    let (lo, li, ro, ri) = (ease(tl, true, left_len)?, ease(tl, false, left_len)?, ease(tr, true, right_len)?, ease(tr, false, right_len)?);
    let mut first = a.clone();
    let mut last = b.clone();
    // Freeze both sides before changing neighbours, including their unrelated temporal sides.
    if first.auto_bezier {
        first.in_ease = vec![effective_ease(keys, i, 0, true, false)].into();
    }
    if last.auto_bezier {
        last.out_ease = vec![effective_ease(keys, next, 0, true, true)].into();
    }
    first.auto_bezier = false;
    last.auto_bezier = false;
    first.spatial_auto = false;
    last.spatial_auto = false;
    first.spatial_in = tin;
    first.spatial_out = std::array::from_fn(|d| left[1][d] - left[0][d]);
    last.spatial_in = std::array::from_fn(|d| right[2][d] - right[3][d]);
    last.spatial_out = bout;
    first.out_interp = Interp::Bezier;
    last.in_interp = Interp::Bezier;
    first.out_ease = vec![lo].into();
    last.in_ease = vec![ri].into();
    let mut inserted = Keyframe::new(time, a.value.with_components(&left[3]));
    inserted.spatial_auto = false;
    inserted.spatial_continuous = true;
    inserted.spatial_in = std::array::from_fn(|d| left[2][d] - left[3][d]);
    inserted.spatial_out = std::array::from_fn(|d| right[1][d] - right[0][d]);
    inserted.in_interp = Interp::Bezier;
    inserted.out_interp = Interp::Bezier;
    inserted.in_ease = vec![li].into();
    inserted.out_ease = vec![ro].into();
    let mut result = keys.to_vec();
    *result.get_mut(i).ok_or("Missing segment start")? = first;
    *result.get_mut(next).ok_or("Missing segment end")? = last;
    result.insert(next, inserted);
    Ok(result)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::evaluate;

    #[test]
    fn insert_spatial_preserves_curved_motion_and_neighbours() {
        for eased in [false, true] {
            let mut keys: Vec<_> = [[0., 0., 0.], [200., 400., 80.], [800., 50., 160.], [1100., 300., 20.]]
                .into_iter()
                .enumerate()
                .map(|(i, value)| Keyframe::new(Tick::from_seconds_f64(i as f64), Value::Vec3(value)))
                .collect();
            if eased {
                for k in &mut keys {
                    k.auto_bezier = true;
                    k.in_interp = Interp::Bezier;
                    k.out_interp = Interp::Bezier;
                    k.in_ease = vec![Ease::EASY].into();
                    k.out_ease = vec![Ease::EASY].into();
                }
            }
            let t = Tick::from_seconds_f64(1.3729);
            let split = insert_spatial(&keys, t).unwrap();
            assert_eq!(split.len(), 5);
            assert_eq!(split[2].time, t);
            assert_eq!(split[0], keys[0]);
            assert_eq!(split[4], keys[3]);
            for step in 0..=3000 {
                let t = Tick::from_seconds_f64(step as f64 / 1000.0);
                let a = evaluate(&keys, t, true).unwrap().as_vec3();
                let b = evaluate(&split, t, true).unwrap().as_vec3();
                let error = a.iter().zip(b).map(|(a, b)| (a - b).powi(2)).sum::<f64>().sqrt();
                // Both paths use 96-piece arc tables; subdivision must stay subpixel.
                assert!(error < 0.08, "eased={eased} time={} error={error}", t.seconds());
                if !(1.0..=2.0).contains(&t.seconds()) {
                    assert_eq!(a, b, "the neighbouring segments must be exactly unchanged");
                }
            }
        }
    }

    #[test]
    fn insert_spatial_rejects_unsupported_segments_without_mutation() {
        let mut keys = vec![Keyframe::new(Tick::ZERO, Value::Vec2([0., 0.])), Keyframe::new(Tick::from_seconds_f64(1.), Value::Vec2([100., 50.]))];
        let t = Tick::from_seconds_f64(0.4);
        assert!(insert_spatial(&keys, Tick::ZERO).is_err());
        assert!(insert_spatial(&keys, Tick::from_seconds_f64(2.)).is_err());
        keys[0].out_interp = Interp::Hold;
        let before = keys.clone();
        assert!(insert_spatial(&keys, t).is_err());
        assert_eq!(keys, before);
        keys[0].out_interp = Interp::Linear;
        keys[1].roving = true;
        assert!(insert_spatial(&keys, t).is_err());
        keys[1].roving = false;
        keys[0].out_interp = Interp::Bezier;
        keys[0].out_ease = vec![Ease { speed: -100., influence: 0.3 }].into();
        assert!(insert_spatial(&keys, t).is_err());
    }
}
