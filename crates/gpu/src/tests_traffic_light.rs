//! RT10e: the compositor's submissions on a device it shares with a window go through the
//! window's traffic light, so a surface configuration (which waits for the device to go idle and
//! fails if anything is submitted meanwhile) never overlaps one.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::time::{Duration, Instant};

use crate::{GpuContext, TrafficLight};

/// One small piece of GPU work, submitted the way the compositor submits.
fn submit_work(ctx: &GpuContext, buf: &wgpu::Buffer) {
    let mut enc = ctx.device.create_command_encoder(&wgpu::CommandEncoderDescriptor { label: Some("traffic light test") });
    enc.clear_buffer(buf, 0, None);
    ctx.submit(enc.finish());
}

/// `Surface::configure`'s idle check, without a window: while the light is red, waiting for the
/// device leaves its queue empty although frame workers keep submitting.
#[test]
fn a_red_light_keeps_the_queue_empty_for_a_configure() {
    let Some(ctx) = GpuContext::headless() else {
        assert!(!std::env::var("EFFECTCRAFT_REQUIRE_GPU").is_ok_and(|v| v == "1"));
        return;
    };
    let ctx = Arc::new(ctx);
    let light = TrafficLight::new();
    assert!(ctx.set_traffic_light(light.clone()));
    assert!(!ctx.set_traffic_light(TrafficLight::new()), "the light is set once");
    let stop = Arc::new(AtomicBool::new(false));
    let submitted = Arc::new(AtomicU64::new(0));
    let workers: Vec<_> = (0..4)
        .map(|_| {
            let (ctx, stop, submitted) = (ctx.clone(), stop.clone(), submitted.clone());
            std::thread::spawn(move || {
                let buf = ctx.device.create_buffer(&wgpu::BufferDescriptor {
                    label: None,
                    size: 4 << 20,
                    usage: wgpu::BufferUsages::COPY_DST,
                    mapped_at_creation: false,
                });
                while !stop.load(Ordering::Relaxed) {
                    submit_work(&ctx, &buf);
                    submitted.fetch_add(1, Ordering::Relaxed);
                }
            })
        })
        .collect();
    let t = Instant::now();
    let mut configures = 0;
    while t.elapsed() < Duration::from_secs(2) || submitted.load(Ordering::Relaxed) < 200 {
        let red = light.red();
        let before = submitted.load(Ordering::Relaxed);
        let status = ctx.device.poll(wgpu::PollType::wait_indefinitely());
        assert!(matches!(status, Ok(wgpu::PollStatus::QueueEmpty)), "a submission overlapped a configure: {status:?}");
        // The holder itself may composite (no deadlock).
        ctx.fence();
        drop(red);
        configures += 1;
        assert!(before <= submitted.load(Ordering::Relaxed));
        std::thread::sleep(Duration::from_millis(2));
        if t.elapsed() > Duration::from_secs(30) {
            break;
        }
    }
    stop.store(true, Ordering::Relaxed);
    for w in workers {
        w.join().unwrap();
    }
    let stats = light.stats();
    eprintln!("{configures} configures, {} submissions, {stats:?}", submitted.load(Ordering::Relaxed));
    assert!(configures > 50);
    assert!(stats.held > 0, "workers never met a red light");
    assert_eq!(stats.late, 0);
}
