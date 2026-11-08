//! GEMM tuning harness: times the current tiled kernels and the generated SIMT
//! and tensor-core (Vulkan cooperative-matrix) kernels on RF-DETR-S's GEMM
//! shapes, checks each against an f64 CPU product, and totals the time weighted
//! by how often each shape occurs in one forward pass.
//! Usage: bench_gemm [iterations]   (WGPU_BACKEND=vulkan for the coop kernels)
use anyhow::{Context, Result};
use wgpu::util::DeviceExt;

#[allow(dead_code)]
#[path = "../vision/ops/gemm_gen.rs"]
mod gemm_gen;

/// The current kernels and their params packing (mirrors `ops::matmul_tiled`).
mod matmul_tiled {
    pub const MATMUL_TILED_WGSL: &str = include_str!("../vision/ops/matmul_tiled.wgsl");
    pub const MATMUL_TILED_VEC4_WGSL: &str = include_str!("../vision/ops/matmul_tiled_vec4.wgsl");

    pub fn vec4_eligible(k: usize, n: usize, lhs_batch: &[u32], rhs_batch: &[u32]) -> bool {
        k.is_multiple_of(4)
            && n.is_multiple_of(4)
            && lhs_batch
                .iter()
                .chain(rhs_batch)
                .all(|s| s.is_multiple_of(4))
    }

    /// Contiguous row-major operands: `[batch.., M, K] x [batch.., K, N]`.
    pub fn params(
        m: usize,
        n: usize,
        k: usize,
        dims: &[usize],
        lhs: &[u32],
        rhs: &[u32],
    ) -> [u32; 25] {
        let mut p = [0u32; 25];
        p[..8].copy_from_slice(&[
            m as u32,
            n as u32,
            k as u32,
            k as u32,
            1,
            n as u32,
            1,
            dims.len() as u32,
        ]);
        for (d, &size) in dims.iter().enumerate() {
            p[8 + d] = size as u32;
            p[12 + d] = lhs[d];
            p[16 + d] = rhs[d];
        }
        p[21] = 1f32.to_bits();
        p
    }
}

use gemm_gen::{CoopCfg, SimtCfg};

/// (label, batch, M, K, N, rhs batched, occurrences per forward pass)
const SHAPES: &[(&str, usize, usize, usize, usize, bool, usize)] = &[
    ("fc1", 1, 1604, 384, 1536, false, 12),
    ("fc2", 1, 1604, 1536, 384, false, 12),
    ("qkv-win", 4, 401, 384, 1152, false, 9),
    ("qkv-glob", 1, 1604, 384, 1152, false, 3),
    ("proj", 1, 1604, 384, 384, false, 12),
    ("qk-glob", 6, 1604, 64, 1604, true, 3),
    ("av-glob", 6, 1604, 1604, 64, true, 3),
    ("qk-win", 24, 401, 64, 401, true, 9),
    ("av-win", 24, 401, 401, 64, true, 9),
    ("dec-ffn1", 1, 300, 256, 2048, false, 3),
    ("dec-ffn2", 1, 300, 2048, 256, false, 3),
    ("dec-lin", 1, 300, 256, 256, false, 19),
];

enum Kernel {
    Baseline,
    Simt(SimtCfg),
    Coop(CoopCfg),
}

fn values(n: usize, seed: u64) -> Vec<f32> {
    let mut s = seed.wrapping_mul(6364136223846793005).wrapping_add(1);
    (0..n)
        .map(|_| {
            s = s
                .wrapping_mul(6364136223846793005)
                .wrapping_add(1442695040888963407);
            ((s >> 33) % 2001) as f32 / 1000.0 - 1.0
        })
        .collect()
}

fn main() -> Result<()> {
    let iters: u32 = std::env::args()
        .nth(1)
        .map(|s| s.parse())
        .transpose()?
        .unwrap_or(20);
    let only: Option<String> = std::env::var("GEMM_ONLY").ok();
    let instance = lege_gpu::wgpu_setup::create_instance();
    let adapter = pollster::block_on(instance.request_adapter(&wgpu::RequestAdapterOptions {
        power_preference: wgpu::PowerPreference::HighPerformance,
        ..Default::default()
    }))
    .context("no adapter")?;
    let info = adapter.get_info();
    let coop = adapter
        .features()
        .contains(wgpu::Features::EXPERIMENTAL_COOPERATIVE_MATRIX | wgpu::Features::SHADER_F16);
    let mut features = wgpu::Features::empty();
    if coop {
        features |= wgpu::Features::EXPERIMENTAL_COOPERATIVE_MATRIX | wgpu::Features::SHADER_F16;
    }
    println!(
        "adapter {} ({:?}); coop kernels: {coop}",
        info.name, info.backend
    );
    let (device, queue) = pollster::block_on(adapter.request_device(&wgpu::DeviceDescriptor {
        label: None,
        required_features: features,
        required_limits: adapter.limits(),
        experimental_features: unsafe { wgpu::ExperimentalFeatures::enabled() },
        ..Default::default()
    }))?;

    let mut kernels: Vec<(String, Kernel)> = vec![("tiled (current)".into(), Kernel::Baseline)];
    for &(bm, bn, bk, tr, tc) in &[
        (64, 64, 8, 16, 16),
        (64, 64, 16, 16, 16),
        (64, 64, 8, 8, 8),
        (128, 64, 8, 16, 16),
        (128, 64, 8, 16, 8),
        (64, 128, 8, 8, 16),
        (128, 128, 8, 16, 16),
        (128, 128, 16, 16, 16),
    ] {
        let cfg = SimtCfg { bm, bn, bk, tr, tc };
        if cfg.valid() {
            kernels.push((cfg.label(), Kernel::Simt(cfg)));
        }
    }
    if coop {
        for &(wm, wn, fm, fn_, bk) in &[
            (2, 2, 2, 2, 16),
            (2, 2, 2, 2, 32),
            (2, 2, 2, 2, 64),
            (4, 2, 2, 2, 32),
            (4, 2, 2, 2, 64),
            (2, 4, 2, 2, 32),
            (2, 2, 2, 4, 32),
            (4, 2, 2, 4, 32),
            (2, 2, 1, 2, 32),
        ] {
            let cfg = CoopCfg {
                wm,
                wn,
                fm,
                fn_,
                bk,
            };
            if cfg.valid() {
                kernels.push((cfg.label(), Kernel::Coop(cfg)));
            }
        }
    }
    if let Some(f) = &only {
        kernels.retain(|(l, _)| l.contains(f.as_str()) || l.starts_with("tiled"));
    }

    let buf = |data: &[f32], usage: wgpu::BufferUsages| {
        device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
            label: None,
            contents: bytemuck::cast_slice(data),
            usage: wgpu::BufferUsages::STORAGE | usage,
        })
    };
    let mut totals = vec![0f64; kernels.len()];
    let mut worst_err = vec![0f64; kernels.len()];
    let mut failed = vec![false; kernels.len()];
    print!("{:34}", "kernel \\ shape (ms)");
    for s in SHAPES {
        print!("{:>9}", s.0);
    }
    println!("{:>10}{:>9}", "model ms", "max err");
    let mut rows: Vec<Vec<String>> = vec![Vec::new(); kernels.len()];
    for &(_, batch, m, k, n, rhs_batched, _) in SHAPES {
        let a = values(batch * m * k, 1);
        let b = values(if rhs_batched { batch } else { 1 } * k * n, 2);
        let ab = buf(&a, wgpu::BufferUsages::empty());
        let bb = buf(&b, wgpu::BufferUsages::empty());
        let zero = buf(&[0.0; 4], wgpu::BufferUsages::empty());
        let out = device.create_buffer(&wgpu::BufferDescriptor {
            label: None,
            size: (batch * m * n * 4) as u64,
            usage: wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_SRC,
            mapped_at_creation: false,
        });
        let dims: Vec<usize> = if batch > 1 { vec![batch] } else { vec![] };
        let lhs_b: Vec<u32> = dims.iter().map(|_| (m * k) as u32).collect();
        let rhs_b: Vec<u32> = dims
            .iter()
            .map(|_| if rhs_batched { (k * n) as u32 } else { 0 })
            .collect();
        let p = matmul_tiled::params(m, n, k, &dims, &lhs_b, &rhs_b);
        let params = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
            label: None,
            contents: bytemuck::cast_slice(&p),
            usage: wgpu::BufferUsages::STORAGE,
        });
        // f64 reference for the first rows of batch 0.
        let check_rows = 8.min(m);
        let want: Vec<f64> = (0..check_rows * n)
            .map(|i| {
                let (r, c) = (i / n, i % n);
                (0..k)
                    .map(|t| a[r * k + t] as f64 * b[t * n + c] as f64)
                    .sum()
            })
            .collect();
        for (ki, (_, kernel)) in kernels.iter().enumerate() {
            let (wgsl, dispatch): (String, [u32; 3]) = match kernel {
                Kernel::Baseline => {
                    let vec4 = matmul_tiled::vec4_eligible(k, n, &lhs_b, &rhs_b);
                    let src = if vec4 {
                        matmul_tiled::MATMUL_TILED_VEC4_WGSL
                    } else {
                        matmul_tiled::MATMUL_TILED_WGSL
                    };
                    (
                        src.to_owned(),
                        [m.div_ceil(64) as u32, n.div_ceil(64) as u32, batch as u32],
                    )
                }
                Kernel::Simt(c) => (
                    gemm_gen::simt_wgsl(c),
                    [
                        m.div_ceil(c.bm as usize) as u32,
                        n.div_ceil(c.bn as usize) as u32,
                        batch as u32,
                    ],
                ),
                Kernel::Coop(c) => (
                    gemm_gen::coop_wgsl(c),
                    [
                        m.div_ceil(c.bm() as usize) as u32,
                        n.div_ceil(c.bn() as usize) as u32,
                        batch as u32,
                    ],
                ),
            };
            let scope = device.push_error_scope(wgpu::ErrorFilter::Validation);
            let module = device.create_shader_module(wgpu::ShaderModuleDescriptor {
                label: None,
                source: wgpu::ShaderSource::Wgsl(wgsl.into()),
            });
            let pipeline = device.create_compute_pipeline(&wgpu::ComputePipelineDescriptor {
                label: None,
                layout: None,
                module: &module,
                entry_point: Some("main"),
                compilation_options: Default::default(),
                cache: None,
            });
            if let Some(e) = pollster::block_on(scope.pop()) {
                if !failed[ki] {
                    eprintln!("{}: {e}", kernels[ki].0);
                }
                failed[ki] = true;
                rows[ki].push(format!("{:>9}", "ERR"));
                continue;
            }
            let bgl = pipeline.get_bind_group_layout(0);
            let entries: Vec<wgpu::BindGroupEntry> = [&ab, &bb, &zero, &zero, &out, &params]
                .iter()
                .enumerate()
                .map(|(i, b)| wgpu::BindGroupEntry {
                    binding: i as u32,
                    resource: b.as_entire_binding(),
                })
                .collect();
            let bind = device.create_bind_group(&wgpu::BindGroupDescriptor {
                label: None,
                layout: &bgl,
                entries: &entries,
            });
            let run = |count: u32| -> Result<f64> {
                let mut enc = device.create_command_encoder(&Default::default());
                {
                    let mut pass = enc.begin_compute_pass(&Default::default());
                    pass.set_pipeline(&pipeline);
                    pass.set_bind_group(0, &bind, &[]);
                    for _ in 0..count {
                        pass.dispatch_workgroups(dispatch[0], dispatch[1], dispatch[2]);
                    }
                }
                let t = std::time::Instant::now();
                queue.submit([enc.finish()]);
                device.poll(wgpu::PollType::wait_indefinitely())?;
                Ok(t.elapsed().as_secs_f64())
            };
            run(2)?;
            let secs = (0..3)
                .map(|_| run(iters))
                .collect::<Result<Vec<_>>>()?
                .into_iter()
                .fold(f64::INFINITY, f64::min)
                / iters as f64;
            let read = device.create_buffer(&wgpu::BufferDescriptor {
                label: None,
                size: (check_rows * n * 4) as u64,
                usage: wgpu::BufferUsages::MAP_READ | wgpu::BufferUsages::COPY_DST,
                mapped_at_creation: false,
            });
            let mut enc = device.create_command_encoder(&Default::default());
            enc.copy_buffer_to_buffer(&out, 0, &read, 0, (check_rows * n * 4) as u64);
            queue.submit([enc.finish()]);
            read.slice(..).map_async(wgpu::MapMode::Read, |_| {});
            device.poll(wgpu::PollType::wait_indefinitely())?;
            let got: Vec<f32> = bytemuck::cast_slice(&read.slice(..).get_mapped_range()?).to_vec();
            let err = want
                .iter()
                .zip(&got)
                .map(|(w, g)| (*g as f64 - w).abs() / (1.0 + w.abs()))
                .fold(0.0, f64::max);
            worst_err[ki] = worst_err[ki].max(err);
            let occurrences = SHAPES
                .iter()
                .find(|s| s.2 == m && s.3 == k && s.4 == n && s.1 == batch)
                .unwrap()
                .6;
            totals[ki] += secs * 1e3 * occurrences as f64;
            rows[ki].push(format!("{:>9.3}", secs * 1e3));
        }
    }
    for (ki, (label, _)) in kernels.iter().enumerate() {
        let total = if failed[ki] {
            "ERR".to_owned()
        } else {
            format!("{:.2}", totals[ki])
        };
        println!(
            "{label:34}{}{total:>10}{:>9.1e}",
            rows[ki].join(""),
            worst_err[ki]
        );
    }
    Ok(())
}
