//! Where the desktop compositor runs when the window's adapter can't run it well.
//!
//! A window on OpenGL (a remote or virtual desktop such as Xvfb with VirtualGL, where the
//! NVIDIA Vulkan driver can't present) has no GPU compositor at all, and one on a CPU
//! rasteriser (llvmpipe, lavapipe) composites in software, while a capable Vulkan, Metal or
//! DirectX 12 adapter may sit next to it. The compositor then gets a device of its own there;
//! its frames reach the window through the CPU (readback, upload), never a shared texture.

use wgpu::{Backend, DeviceType};

/// The window's adapter can't host the compositor, or only in software: OpenGL, or a CPU
/// device.
pub fn window_adapter_unfit(backend: Backend, device_type: DeviceType) -> bool {
    backend == Backend::Gl || device_type == DeviceType::Cpu
}

/// How well an adapter runs the compositor (higher is better); `None` for a backend it can't use.
/// DirectX 12 ranks below Vulkan and Metal on the same device: its shader compiler can't build
/// every kernel (#613).
fn rank(backend: Backend, device_type: DeviceType) -> Option<u8> {
    let api = match backend {
        Backend::Vulkan | Backend::Metal => 1,
        Backend::Dx12 => 0,
        _ => return None,
    };
    let device = match device_type {
        DeviceType::DiscreteGpu => 4,
        DeviceType::IntegratedGpu => 3,
        DeviceType::VirtualGpu => 2,
        DeviceType::Other => 1,
        DeviceType::Cpu => 0,
    };
    Some(device * 2 + api)
}

/// Which of `candidates` (backend, device type) the compositor should use instead of the
/// window's adapter; `None` to stay on the window's. Only when the window's is unfit, and only
/// for a candidate better than it: any usable one beats OpenGL (no compositor at all), only a
/// real GPU beats a CPU device.
pub fn pick_compositor_adapter(window: (Backend, DeviceType), candidates: &[(Backend, DeviceType)]) -> Option<usize> {
    if !window_adapter_unfit(window.0, window.1) {
        return None;
    }
    let software_ok = window.0 == Backend::Gl;
    candidates
        .iter()
        .enumerate()
        .filter(|(_, (_, t))| software_ok || *t != DeviceType::Cpu)
        .filter_map(|(i, &(b, t))| rank(b, t).map(|r| (r, i)))
        // The first of equals: the order the platform lists them in.
        .max_by_key(|&(r, i)| (r, std::cmp::Reverse(i)))
        .map(|(_, i)| i)
}

#[cfg(test)]
mod tests {
    use super::*;
    use Backend::*;
    use DeviceType::*;

    #[test]
    fn a_capable_window_adapter_keeps_the_compositor() {
        let all = [(Vulkan, DiscreteGpu), (Vulkan, Cpu)];
        assert_eq!(pick_compositor_adapter((Vulkan, IntegratedGpu), &all), None);
        assert_eq!(pick_compositor_adapter((Metal, IntegratedGpu), &all), None);
        assert_eq!(pick_compositor_adapter((Dx12, DiscreteGpu), &all), None);
    }

    #[test]
    fn opengl_window_moves_the_compositor_to_the_best_gpu() {
        // Xvfb + VirtualGL on an RTX 3090: llvmpipe's Vulkan listed first, the NVIDIA one second.
        let vast = [(Vulkan, Cpu), (Vulkan, DiscreteGpu)];
        assert_eq!(pick_compositor_adapter((Gl, DiscreteGpu), &vast), Some(1));
        assert_eq!(pick_compositor_adapter((Gl, Cpu), &vast), Some(1));
        // Without a GPU, software Vulkan still beats no compositor at all.
        assert_eq!(pick_compositor_adapter((Gl, Cpu), &[(Vulkan, Cpu)]), Some(0));
        // Vulkan over DirectX 12 on the same card (#613); a discrete card over an integrated one.
        assert_eq!(pick_compositor_adapter((Gl, IntegratedGpu), &[(Dx12, DiscreteGpu), (Vulkan, DiscreteGpu)]), Some(1));
        assert_eq!(pick_compositor_adapter((Gl, IntegratedGpu), &[(Vulkan, IntegratedGpu), (Vulkan, DiscreteGpu)]), Some(1));
        // OpenGL and browser adapters can't host it.
        assert_eq!(pick_compositor_adapter((Gl, DiscreteGpu), &[(Gl, DiscreteGpu), (BrowserWebGpu, DiscreteGpu)]), None);
        assert_eq!(pick_compositor_adapter((Gl, DiscreteGpu), &[]), None);
    }

    #[test]
    fn software_window_adapter_moves_only_to_a_real_gpu() {
        assert_eq!(pick_compositor_adapter((Vulkan, Cpu), &[(Vulkan, Cpu), (Vulkan, DiscreteGpu)]), Some(1));
        assert_eq!(pick_compositor_adapter((Vulkan, Cpu), &[(Vulkan, Cpu)]), None);
        assert_eq!(pick_compositor_adapter((Vulkan, Cpu), &[(Vulkan, VirtualGpu), (Vulkan, Cpu)]), Some(0));
    }
}
