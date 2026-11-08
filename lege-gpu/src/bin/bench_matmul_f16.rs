//! Micro-benchmark: does FP16 make the tiled transformer GEMM faster on this GPU?
//!
//! Times `matmul_tiled_vec4.wgsl` (f32) against three FP16 variants derived from
//! it by text substitution, on the RF-DETR-S MLP shapes:
//!   f16-io     f16 operands and output in global memory; f32 tiles and accumulators
//!   f16-tiles  f16 global memory and shared-memory tiles; f32 accumulators
//!   f16-all    f16 everywhere, accumulators included
//! Inputs are multiples of 1/256 in [-1, 1], exact in f16, so every variant reads
//! identical values; each result is checked against an f64 CPU product.
//! Usage: bench_matmul_f16 [iterations]
use anyhow::{Context, Result, bail};
use wgpu::util::DeviceExt;

const VEC4: &str = include_str!("../vision/ops/matmul_tiled_vec4.wgsl");

fn sub(src: &str, pairs: &[(&str, &str)]) -> Result<String> {
    let mut s = src.to_owned();
    for (from, to) in pairs {
        if !s.contains(from) {
            bail!("shader variant: `{from}` not found");
        }
        s = s.replace(from, to);
    }
    Ok(s)
}

fn variants() -> Result<Vec<(&'static str, String, bool)>> {
    let io: Vec<(&str, &str)> = vec![
        (
            "var<storage, read>       a      : array<vec4<f32>>;",
            "var<storage, read>       a      : array<vec4<f16>>;",
        ),
        (
            "var<storage, read>       b      : array<vec4<f32>>;",
            "var<storage, read>       b      : array<vec4<f16>>;",
        ),
        (
            "var<storage, read_write> out    : array<vec4<f32>>;",
            "var<storage, read_write> out    : array<vec4<f16>>;",
        ),
        (
            "out[out4 + gm * N4 + n4] = r;",
            "out[out4 + gm * N4 + n4] = vec4<f16>(r);",
        ),
    ];
    let tiles: Vec<(&str, &str)> = vec![
        (
            "var<workgroup> As: array<f32, 1024>;",
            "var<workgroup> As: array<f16, 1024>;",
        ),
        (
            "var<workgroup> Bs: array<vec4<f32>, 256>;",
            "var<workgroup> Bs: array<vec4<f16>, 256>;",
        ),
        (
            "var av = vec4<f32>(0.0, 0.0, 0.0, 0.0);",
            "var av = vec4<f16>(0.0, 0.0, 0.0, 0.0);",
        ),
        (
            "var bv = vec4<f32>(0.0, 0.0, 0.0, 0.0);",
            "var bv = vec4<f16>(0.0, 0.0, 0.0, 0.0);",
        ),
    ];
    let f16 = |s: String| format!("enable f16;\n{s}");

    let v1 = sub(VEC4, &io)?;
    let v1 = sub(
        &v1,
        &[
            (
                "av = a[lhs4 + gm_a * a_rs4 + gk4_a];",
                "av = vec4<f32>(a[lhs4 + gm_a * a_rs4 + gk4_a]);",
            ),
            (
                "bv = b[rhs4 + gk_b * b_ks4 + gn4_b];",
                "bv = vec4<f32>(b[rhs4 + gk_b * b_ks4 + gn4_b]);",
            ),
        ],
    )?;

    let mut v2 = sub(&sub(VEC4, &io)?, &tiles)?;
    for i in 0..4 {
        let from = format!("let a{i} = As[k * BM + thread_row * 4u + {i}u];");
        let to = format!("let a{i} = f32(As[k * BM + thread_row * 4u + {i}u]);");
        v2 = sub(&v2, &[(&from, &to)])?;
    }
    let v2 = sub(
        &v2,
        &[(
            "let bb = Bs[k * 16u + thread_col];",
            "let bb = vec4<f32>(Bs[k * 16u + thread_col]);",
        )],
    )?;

    let v3 = sub(&sub(VEC4, &io)?, &tiles)?;
    let v3 = sub(
        &v3,
        &[
            ("var acc: array<f32, 16>;", "var acc: array<f16, 16>;"),
            (
                "var r = alpha * vec4<f32>(acc[i * 4u + 0u], acc[i * 4u + 1u], acc[i * 4u + 2u], acc[i * 4u + 3u]);",
                "var r = alpha * vec4<f32>(vec4<f16>(acc[i * 4u + 0u], acc[i * 4u + 1u], acc[i * 4u + 2u], acc[i * 4u + 3u]));",
            ),
        ],
    )?;
    Ok(vec![
        ("f32 (current)", VEC4.to_owned(), false),
        ("f16-io", f16(v1), true),
        ("f16-tiles", f16(v2), true),
        ("f16-all", f16(v3), true),
    ])
}

/// f32 -> f16 bits for values exact in f16 (normal range or zero).
fn f16_bits(v: f32) -> u16 {
    if v == 0.0 {
        return 0;
    }
    let b = v.to_bits();
    let sign = ((b >> 16) & 0x8000) as u16;
    let exp = ((b >> 23) & 0xff) as i32 - 127 + 15;
    let man = ((b >> 13) & 0x3ff) as u16;
    sign | ((exp as u16) << 10) | man
}

fn f16_to_f32(h: u16) -> f32 {
    let sign = if h & 0x8000 != 0 { -1.0 } else { 1.0 };
    let exp = ((h >> 10) & 0x1f) as i32;
    let man = (h & 0x3ff) as f32;
    match exp {
        0 => sign * man * 2f32.powi(-24),
        31 => sign * f32::INFINITY,
        e => sign * (1.0 + man / 1024.0) * 2f32.powi(e - 15),
    }
}

fn values(n: usize, seed: u64) -> Vec<f32> {
    let mut s = seed
        .wrapping_mul(6364136223846793005)
        .wrapping_add(1442695040888963407);
    (0..n)
        .map(|_| {
            s = s
                .wrapping_mul(6364136223846793005)
                .wrapping_add(1442695040888963407);
            ((s >> 33) % 513) as f32 / 256.0 - 1.0
        })
        .collect()
}

fn main() -> Result<()> {
    let iters: u32 = std::env::args()
        .nth(1)
        .map(|s| s.parse())
        .transpose()?
        .unwrap_or(30);
    let instance = lege_gpu::wgpu_setup::create_instance();
    let adapter = pollster::block_on(instance.request_adapter(&wgpu::RequestAdapterOptions {
        power_preference: wgpu::PowerPreference::HighPerformance,
        ..Default::default()
    }))
    .context("no adapter")?;
    let info = adapter.get_info();
    let has_f16 = adapter.features().contains(wgpu::Features::SHADER_F16);
    println!(
        "adapter {} ({:?}); SHADER_F16 {has_f16}",
        info.name, info.backend
    );
    let coop = adapter
        .features()
        .contains(wgpu::Features::EXPERIMENTAL_COOPERATIVE_MATRIX);
    println!("EXPERIMENTAL_COOPERATIVE_MATRIX {coop}");
    for p in adapter.cooperative_matrix_properties() {
        println!("  coop matrix {p:?}");
    }
    if std::env::var_os("PROBE_ONLY").is_some() {
        return Ok(());
    }
    let features = if has_f16 {
        wgpu::Features::SHADER_F16
    } else {
        wgpu::Features::empty()
    };
    let (device, queue) = pollster::block_on(adapter.request_device(&wgpu::DeviceDescriptor {
        label: None,
        required_features: features,
        required_limits: adapter.limits(),
        ..Default::default()
    }))?;

    let shapes = [
        (1604usize, 384usize, 1536usize),
        (1604, 1536, 384),
        (6400, 256, 256),
    ];
    let storage = |data: &[u8], usage: wgpu::BufferUsages| {
        device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
            label: None,
            contents: data,
            usage: wgpu::BufferUsages::STORAGE | usage,
        })
    };
    for &(m, k, n) in &shapes {
        let a = values(m * k, 1);
        let b = values(k * n, 2);
        let flops = 2.0 * (m * k * n) as f64;
        println!("\nM={m} K={k} N={n}  ({:.2} GFLOP)", flops / 1e9);
        for (label, wgsl, half) in variants()? {
            if half && !has_f16 {
                println!("  {label:14} skipped (no SHADER_F16)");
                continue;
            }
            let (a_bytes, b_bytes): (Vec<u8>, Vec<u8>) = if half {
                (
                    a.iter().flat_map(|&v| f16_bits(v).to_le_bytes()).collect(),
                    b.iter().flat_map(|&v| f16_bits(v).to_le_bytes()).collect(),
                )
            } else {
                (
                    bytemuck::cast_slice(&a).to_vec(),
                    bytemuck::cast_slice(&b).to_vec(),
                )
            };
            let elem = if half { 2 } else { 4 };
            let ab = storage(&a_bytes, wgpu::BufferUsages::empty());
            let bb = storage(&b_bytes, wgpu::BufferUsages::empty());
            let zero = storage(&[0u8; 16], wgpu::BufferUsages::empty());
            let out = device.create_buffer(&wgpu::BufferDescriptor {
                label: None,
                size: (m * n * elem) as u64,
                usage: wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_SRC,
                mapped_at_creation: false,
            });
            let mut p = [0u32; 25];
            p[0] = m as u32;
            p[1] = n as u32;
            p[2] = k as u32;
            p[3] = k as u32;
            p[4] = 1;
            p[5] = n as u32;
            p[6] = 1;
            p[21] = 1f32.to_bits();
            let params = storage(bytemuck::cast_slice(&p), wgpu::BufferUsages::empty());
            let module = device.create_shader_module(wgpu::ShaderModuleDescriptor {
                label: Some(label),
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
            let bgl = pipeline.get_bind_group_layout(0);
            // `layout: None` drops bindings the shader never reads (res when
            // the residual flag is off is still referenced, so all six stay).
            let entries: Vec<wgpu::BindGroupEntry> = [&ab, &bb, &zero, &zero, &out, &params]
                .iter()
                .enumerate()
                .map(|(i, buf)| wgpu::BindGroupEntry {
                    binding: i as u32,
                    resource: buf.as_entire_binding(),
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
                        pass.dispatch_workgroups(m.div_ceil(64) as u32, n.div_ceil(64) as u32, 1);
                    }
                }
                let t = std::time::Instant::now();
                queue.submit([enc.finish()]);
                device.poll(wgpu::PollType::wait_indefinitely())?;
                Ok(t.elapsed().as_secs_f64())
            };
            run(3)?;
            let secs = (0..3)
                .map(|_| run(iters))
                .collect::<Result<Vec<_>>>()?
                .into_iter()
                .fold(f64::INFINITY, f64::min)
                / iters as f64;

            // Correctness on the first 32 rows.
            let rows = 32.min(m);
            let read = device.create_buffer(&wgpu::BufferDescriptor {
                label: None,
                size: (rows * n * elem) as u64,
                usage: wgpu::BufferUsages::MAP_READ | wgpu::BufferUsages::COPY_DST,
                mapped_at_creation: false,
            });
            let mut enc = device.create_command_encoder(&Default::default());
            enc.copy_buffer_to_buffer(&out, 0, &read, 0, (rows * n * elem) as u64);
            queue.submit([enc.finish()]);
            read.slice(..).map_async(wgpu::MapMode::Read, |_| {});
            device.poll(wgpu::PollType::wait_indefinitely())?;
            let got: Vec<f32> = {
                let view = read.slice(..).get_mapped_range()?;
                if half {
                    view.chunks_exact(2)
                        .map(|c| f16_to_f32(u16::from_le_bytes([c[0], c[1]])))
                        .collect()
                } else {
                    bytemuck::cast_slice::<u8, f32>(&view).to_vec()
                }
            };
            let mut worst = 0f64;
            for r in 0..rows {
                for c in 0..n {
                    let want: f64 = (0..k)
                        .map(|i| a[r * k + i] as f64 * b[i * n + c] as f64)
                        .sum();
                    let err = (got[r * n + c] as f64 - want).abs() / (1.0 + want.abs());
                    worst = worst.max(err);
                }
            }
            println!(
                "  {label:14} {:7.3} ms  {:6.2} TFLOPS  max rel err {worst:.1e}",
                secs * 1e3,
                flops / secs / 1e12
            );
        }
    }
    Ok(())
}
