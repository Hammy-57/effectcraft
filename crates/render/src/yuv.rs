//! Borrowed native video planes shared by media sources and accelerators.
//! No decoder or graphics API types cross this boundary.
use effectcraft_project::AlphaMode;

#[derive(Clone, Copy, Debug)]
pub enum Samples<'a> {
    U8(&'a [u8]),
    /// Numeric, right-aligned code values (not P010's left-aligned words).
    U16(&'a [u16]),
}
impl Samples<'_> {
    pub fn len(self) -> usize {
        match self {
            Self::U8(v) => v.len(),
            Self::U16(v) => v.len(),
        }
    }
    pub fn is_empty(self) -> bool {
        self.len() == 0
    }
    pub fn bytes_per_sample(self) -> usize {
        match self {
            Self::U8(_) => 1,
            Self::U16(_) => 2,
        }
    }
}

#[derive(Clone, Copy, Debug)]
pub struct Plane<'a> {
    pub samples: Samples<'a>,
    pub width: u32,
    pub height: u32,
    /// Row stride in samples, not bytes.
    pub stride: usize,
}
impl Plane<'_> {
    /// Bytes needed through the last sample, excluding unused trailing padding.
    pub fn required_bytes(self) -> Option<usize> {
        if self.width == 0 || self.height == 0 || self.stride < self.width as usize {
            return None;
        }
        let samples = (self.height as usize).checked_sub(1)?.checked_mul(self.stride)?.checked_add(self.width as usize)?;
        if samples > self.samples.len() {
            return None;
        }
        samples.checked_mul(self.samples.bytes_per_sample())
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ChromaSiting {
    /// 3/4 nearest + 1/4 adjacent on each subsampled axis, with edge clamp.
    Centered,
}

#[derive(Clone, Copy, Debug)]
pub struct Coefficients {
    /// Subtract offset then multiply scale, in source code-value space.
    pub luma: [f32; 2],
    pub chroma: [f32; 2],
    /// CrR, CrG, CbG, CbB; G subtracts both chroma terms.
    pub rgb: [f32; 4],
}

#[derive(Clone, Copy, Debug)]
pub struct View<'a> {
    pub width: u32,
    pub height: u32,
    /// Y, Cb, Cr. All planes use the same storage width.
    pub planes: [Plane<'a>; 3],
    pub alpha: Option<Plane<'a>>,
    pub bits: u32,
    /// 4:4:4=[0,0], 4:2:2=[1,0], 4:2:0=[1,1].
    pub subsampling: [u32; 2],
    pub siting: ChromaSiting,
    pub coefficients: Coefficients,
    pub alpha_mode: AlphaMode,
    pub matte: [f32; 3],
}
impl View<'_> {
    /// Validate untrusted metadata before any GPU allocation. Backends additionally
    /// enforce their adapter dimensions and memory limits.
    pub fn validate(self) -> Result<usize, &'static str> {
        if self.width == 0 || self.height == 0 {
            return Err("empty YUV frame");
        }
        if !matches!(self.subsampling, [0, 0] | [1, 0] | [1, 1]) {
            return Err("unsupported chroma subsampling");
        }
        let bytes = self.planes[0].samples.bytes_per_sample();
        if !(8..=16).contains(&self.bits) || (bytes == 1 && self.bits != 8) {
            return Err("unsupported YUV bit depth");
        }
        let coefficients = self.coefficients;
        if coefficients.luma.iter().chain(&coefficients.chroma).chain(&coefficients.rgb).any(|v| !v.is_finite())
            || coefficients.luma[1] <= 0.0
            || coefficients.chroma[1] <= 0.0
        {
            return Err("invalid YUV conversion coefficients");
        }
        if self.alpha.is_some() && self.matte.iter().any(|v| !v.is_finite()) {
            return Err("invalid YUV alpha matte");
        }
        let chroma = [self.width.div_ceil(1 << self.subsampling[0]), self.height.div_ceil(1 << self.subsampling[1])];
        let mut total = 0usize;
        for (i, plane) in self.planes.iter().chain(self.alpha.iter()).enumerate() {
            let size = if i == 1 || i == 2 { chroma } else { [self.width, self.height] };
            if [plane.width, plane.height] != size || plane.samples.bytes_per_sample() != bytes {
                return Err("inconsistent YUV plane layout");
            }
            let n = plane.required_bytes().ok_or("truncated YUV plane or invalid stride")?;
            total = total.checked_add(n).ok_or("YUV plane byte count overflow")?;
            if total > 1 << 30 {
                return Err("YUV planes exceed the transport limit");
            }
        }
        Ok(total)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn plane_lengths_and_strides_are_checked() {
        let samples = [0u8; 7];
        let plane = Plane { samples: Samples::U8(&samples), width: 3, height: 2, stride: 4 };
        assert_eq!(plane.required_bytes(), Some(7));
        assert_eq!(Plane { stride: 2, ..plane }.required_bytes(), None);
        assert_eq!(Plane { height: 3, ..plane }.required_bytes(), None);
        assert_eq!(Plane { stride: usize::MAX, ..plane }.required_bytes(), None);
        assert_eq!(Plane { width: 0, ..plane }.required_bytes(), None);
    }
    #[test]
    fn malformed_frame_metadata_is_rejected_before_upload() {
        let samples = [0u8; 4];
        let plane = Plane { samples: Samples::U8(&samples), width: 2, height: 2, stride: 2 };
        let view = View {
            width: 2,
            height: 2,
            planes: [plane; 3],
            alpha: None,
            bits: 8,
            subsampling: [0, 0],
            siting: ChromaSiting::Centered,
            coefficients: Coefficients { luma: [0.0, 1.0 / 255.0], chroma: [128.0, 1.0 / 255.0], rgb: [1.0; 4] },
            alpha_mode: effectcraft_project::AlphaMode::Straight,
            matte: [0.0; 3],
        };
        assert_eq!(view.validate(), Ok(12));
        assert!(View { bits: 7, ..view }.validate().is_err());
        assert!(View { bits: 10, ..view }.validate().is_err());
        assert!(View { subsampling: [32, 0], ..view }.validate().is_err());
        assert!(View { width: u32::MAX, ..view }.validate().is_err());
        assert!(View { planes: [Plane { stride: 1, ..plane }; 3], ..view }.validate().is_err());
        assert!(View { alpha: Some(Plane { width: 1, ..plane }), ..view }.validate().is_err());
        assert!(View { alpha: Some(plane), matte: [f32::NAN; 3], ..view }.validate().is_err());
        assert!(View { coefficients: Coefficients { chroma: [0.0, f32::INFINITY], ..view.coefficients }, ..view }.validate().is_err());
    }
}
