//! How much video memory a discrete GPU has (CR1). wgpu doesn't say, so it is asked of the
//! driver's own tools: `nvidia-smi` for NVIDIA (Linux and Windows), sysfs for AMD on Linux.
//! `EFFECTCRAFT_GPU_VRAM_MB` overrides it. Unknown elsewhere (Apple Silicon and other integrated
//! GPUs share the RAM; their budgets follow it instead).

/// Overrides the detected video memory, in MiB.
pub const VRAM_ENV: &str = "EFFECTCRAFT_GPU_VRAM_MB";

/// Memory budget thresholds for the instances this crate creates: on Vulkan (with
/// `VK_EXT_memory_budget`) and Direct3D 12, creating a texture or buffer past 90 % of the budget
/// the driver reports fails with an out-of-memory error the frame's error scope catches (the frame
/// then renders on the CPU and fewer GPU frames run at once), instead of overcommitting video
/// memory until the device is lost.
pub fn memory_budget_thresholds() -> wgpu::MemoryBudgetThresholds {
    wgpu::MemoryBudgetThresholds { for_resource_creation: Some(90), for_device_loss: None }
}

/// A discrete GPU's video memory, in bytes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct VideoMemory {
    pub total: u64,
    /// Free when asked (other programs, such as another EffectCraft, use the rest).
    pub free: u64,
}

/// The video memory of the adapter `info` describes, when it is a discrete GPU whose size can be
/// found out (or [`VRAM_ENV`] is set). Asks the driver's tool each time (when a device is made):
/// what is free changes.
pub fn dedicated_memory(info: &wgpu::AdapterInfo) -> Option<VideoMemory> {
    dedicated(info.device_type, info.vendor, info.device)
}

fn dedicated(device_type: wgpu::DeviceType, vendor: u32, device: u32) -> Option<VideoMemory> {
    if let Some(total) = std::env::var(VRAM_ENV).ok().and_then(|v| v.trim().parse::<u64>().ok()).filter(|mb| *mb > 0).and_then(|mb| mb.checked_mul(1 << 20)) {
        return Some(VideoMemory { total, free: total });
    }
    if device_type != wgpu::DeviceType::DiscreteGpu {
        return None;
    }
    let found = detect(vendor, device);
    match found {
        Some(m) => log::info!("gpu: {} MiB of video memory, {} MiB free", m.total >> 20, m.free >> 20),
        None => log::info!("gpu: video memory unknown (vendor {vendor:#06x}); GPU frames keep a 2 GiB working budget"),
    }
    found
}

#[cfg_attr(target_arch = "wasm32", allow(dead_code))]
const NVIDIA: u32 = 0x10de;
#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
const AMD: u32 = 0x1002;

#[cfg(not(target_arch = "wasm32"))]
fn detect(vendor: u32, device: u32) -> Option<VideoMemory> {
    match vendor {
        NVIDIA => nvidia_smi().and_then(|out| parse_nvidia_smi(&out, device)),
        #[cfg(target_os = "linux")]
        AMD => amd_sysfs(device),
        _ => None,
    }
}

#[cfg(target_arch = "wasm32")]
fn detect(_vendor: u32, _device: u32) -> Option<VideoMemory> {
    None
}

/// `nvidia-smi`'s answer: one line per GPU, "PCI device id, total MiB, used MiB". At most two
/// seconds (a wedged driver can make it hang).
#[cfg(not(target_arch = "wasm32"))]
fn nvidia_smi() -> Option<String> {
    use std::process::{Command, Stdio};
    use std::time::{Duration, Instant};
    let mut cmd = Command::new("nvidia-smi");
    cmd.args(["--query-gpu=pci.device_id,memory.total,memory.used", "--format=csv,noheader,nounits"])
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null());
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        // CREATE_NO_WINDOW: no console flashes up.
        cmd.creation_flags(0x0800_0000);
    }
    let mut child = cmd.spawn().ok()?;
    let t0 = Instant::now();
    loop {
        match child.try_wait() {
            Ok(Some(status)) if status.success() => break,
            Ok(Some(_)) | Err(_) => return None,
            Ok(None) if t0.elapsed() > Duration::from_secs(2) => {
                let _ = child.kill();
                let _ = child.wait();
                log::warn!("gpu: nvidia-smi did not answer within 2 s");
                return None;
            }
            Ok(None) => std::thread::sleep(Duration::from_millis(10)),
        }
    }
    let mut out = String::new();
    std::io::Read::read_to_string(&mut child.stdout.take()?, &mut out).ok()?;
    Some(out)
}

/// The memory `nvidia-smi` reports for PCI device `device` (lines like `0x220410DE, 24576,
/// 9782`: the device id in the high 16 bits, total and used MiB), or for its only GPU when the
/// ids don't match.
#[cfg_attr(target_arch = "wasm32", allow(dead_code))]
pub(crate) fn parse_nvidia_smi(out: &str, device: u32) -> Option<VideoMemory> {
    let gpus: Vec<(Option<u32>, u64, u64)> = out
        .lines()
        .filter_map(|line| {
            let mut cols = line.split(',').map(str::trim);
            let id = cols.next()?;
            let id = u32::from_str_radix(id.trim_start_matches("0x").trim_start_matches("0X"), 16).ok();
            let total = cols.next()?.parse::<u64>().ok()?;
            let used = cols.next().and_then(|u| u.parse::<u64>().ok()).unwrap_or(0);
            Some((id, total, used))
        })
        .collect();
    let (total, used) = match gpus.iter().find(|(id, _, _)| id.is_some_and(|id| id >> 16 == device)) {
        Some((_, total, used)) => (*total, *used),
        None if gpus.len() == 1 => gpus.first().map(|(_, t, u)| (*t, *u))?,
        None => return None,
    };
    let total = total.checked_mul(1 << 20).filter(|b| *b > 0)?;
    Some(VideoMemory { total, free: total.saturating_sub(used.saturating_mul(1 << 20)) })
}

/// `mem_info_vram_total` (and `_used`) of the DRM card whose PCI device is `device`.
#[cfg(target_os = "linux")]
fn amd_sysfs(device: u32) -> Option<VideoMemory> {
    let read = |p: &std::path::Path| std::fs::read_to_string(p).ok();
    let hex = |p: &std::path::Path| u32::from_str_radix(read(p)?.trim().trim_start_matches("0x"), 16).ok();
    std::fs::read_dir("/sys/class/drm").ok()?.flatten().find_map(|card| {
        let dev = card.path().join("device");
        if hex(&dev.join("device"))? != device || hex(&dev.join("vendor"))? != AMD {
            return None;
        }
        let total = read(&dev.join("mem_info_vram_total"))?.trim().parse::<u64>().ok().filter(|b| *b > 0)?;
        let used = read(&dev.join("mem_info_vram_used")).and_then(|u| u.trim().parse::<u64>().ok()).unwrap_or(0);
        Some(VideoMemory { total, free: total.saturating_sub(used) })
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn nvidia_smi_answers_by_device_id() {
        const MIB: u64 = 1 << 20;
        let mem = |total: u64, used: u64| Some(VideoMemory { total: total * MIB, free: (total - used) * MIB });
        // An RTX 3090 (0x2204) next to an RTX A4000 (0x24b0).
        let out = "0x220410DE, 24576, 9782\n0x24B010DE, 16376, 0\n";
        assert_eq!(parse_nvidia_smi(out, 0x2204), mem(24576, 9782));
        assert_eq!(parse_nvidia_smi(out, 0x24b0), mem(16376, 0));
        assert_eq!(parse_nvidia_smi(out, 0x1234), None, "two GPUs, neither this one");
        assert_eq!(parse_nvidia_smi("0x220410DE, 24576, 100\n", 0x1234), mem(24576, 100), "the only GPU");
        assert_eq!(parse_nvidia_smi("0x220410DE, 24576\n", 0x2204), mem(24576, 0), "no used column");
        assert_eq!(parse_nvidia_smi("0x220410DE, 24576, [N/A]\n", 0x2204), mem(24576, 0));
        assert_eq!(parse_nvidia_smi("", 0x2204), None);
        assert_eq!(parse_nvidia_smi("NVIDIA-SMI has failed\n", 0x2204), None);
        assert_eq!(parse_nvidia_smi("0x220410DE, [N/A], 3\n", 0x2204), None);
    }

    #[test]
    fn integrated_gpus_have_no_dedicated_memory() {
        if std::env::var_os(VRAM_ENV).is_some() {
            return;
        }
        assert_eq!(dedicated(wgpu::DeviceType::IntegratedGpu, 0, 0), None);
        assert_eq!(dedicated(wgpu::DeviceType::Cpu, NVIDIA, 0x2204), None);
    }
}
