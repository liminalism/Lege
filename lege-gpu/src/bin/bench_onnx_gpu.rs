//! Warm-latency probe for any NCHW float ONNX model on the wgpu executor.
//! Usage: bench_onnx_gpu <model.onnx> <N,C,H,W> [runs]
use anyhow::{Context, Result};
use lege_gpu::vision::{OnnxSession, Tensor};

fn main() -> Result<()> {
    let model = std::env::args().nth(1).context("model path")?;
    let dims: Vec<i64> = std::env::args()
        .nth(2)
        .context("input dims N,C,H,W")?
        .split(',')
        .map(|d| d.parse().context("bad dim"))
        .collect::<Result<_>>()?;
    let runs: usize = std::env::args()
        .nth(3)
        .map(|r| r.parse())
        .transpose()?
        .unwrap_or(5);
    let t = std::time::Instant::now();
    let session = OnnxSession::from_path_with_input_dims(&model, Some(&dims))?;
    println!("prepare {:?}", t.elapsed());
    let n: usize = dims.iter().map(|&d| d as usize).product();
    // LEGE_BENCH_EACH=<dir>: run every <dir>/<stem>.in.f32 through one compiled
    // graph and write <dir>/<stem>.<output>.f32 (evaluation over many pages).
    if let Some(dir) = std::env::var_os("LEGE_BENCH_EACH").map(std::path::PathBuf::from) {
        let mut stems: Vec<String> = std::fs::read_dir(&dir)?
            .filter_map(|e| e.ok()?.file_name().into_string().ok())
            .filter_map(|f| f.strip_suffix(".in.f32").map(str::to_owned))
            .collect();
        stems.sort();
        let shape: Vec<usize> = dims.iter().map(|&d| d as usize).collect();
        let inputs = stems.iter().map(|stem| -> Result<Tensor> {
            let data: Vec<f32> = std::fs::read(dir.join(format!("{stem}.in.f32")))?
                .chunks_exact(4)
                .map(|b| f32::from_le_bytes([b[0], b[1], b[2], b[3]]))
                .collect();
            Tensor::new(shape.clone(), data)
        });
        let t = std::time::Instant::now();
        session.run_gpu_each(inputs, |i, outputs| {
            for (name, tensor) in &outputs {
                let bytes: Vec<u8> = tensor.data.iter().flat_map(|v| v.to_le_bytes()).collect();
                std::fs::write(dir.join(format!("{}.{name}.f32", stems[i])), bytes)?;
            }
            Ok(())
        })?;
        println!("ran {} inputs in {:?}", stems.len(), t.elapsed());
        return Ok(());
    }
    // LEGE_BENCH_IO=<dir>: read <dir>/input.f32 (raw little-endian f32) if present,
    // and write every output as <dir>/<name>.f32 plus <dir>/shapes.txt.
    let io_dir = std::env::var_os("LEGE_BENCH_IO").map(std::path::PathBuf::from);
    let data: Vec<f32> = match io_dir
        .as_ref()
        .map(|d| d.join("input.f32"))
        .filter(|p| p.exists())
    {
        Some(path) => std::fs::read(&path)?
            .chunks_exact(4)
            .map(|b| f32::from_le_bytes([b[0], b[1], b[2], b[3]]))
            .collect(),
        None => (0..n)
            .map(|i| ((i * 2654435761) % 1000) as f32 / 1000.0 - 0.5)
            .collect(),
    };
    anyhow::ensure!(
        data.len() == n,
        "input.f32 has {} values, expected {n}",
        data.len()
    );
    let input = Tensor::new(dims.iter().map(|&d| d as usize).collect(), data)?;
    #[cfg(feature = "layout-detection")]
    if std::env::var_os("LEGE_BENCH_PROFILE").is_some() {
        println!("{}", session.profile_report_gpu(&input)?);
    }
    if let Some(dir) = &io_dir {
        let outputs = session.run_gpu(&input)?;
        let mut shapes = String::new();
        for (name, tensor) in &outputs {
            let safe: String = name
                .chars()
                .map(|c| if c.is_ascii_alphanumeric() { c } else { '_' })
                .collect();
            let bytes: Vec<u8> = tensor.data.iter().flat_map(|v| v.to_le_bytes()).collect();
            std::fs::write(dir.join(format!("{safe}.f32")), bytes)?;
            shapes.push_str(&format!(
                "{safe} {:?}
",
                tensor.shape
            ));
        }
        std::fs::write(dir.join("shapes.txt"), shapes)?;
    }
    let (compile, times) = session.bench_gpu(&input, runs)?;
    println!("compile {:?}", compile);
    for (i, d) in times.iter().enumerate() {
        println!("run {i}: {:?}", d);
    }
    let mut ms: Vec<f64> = times.iter().map(|d| d.as_secs_f64() * 1000.0).collect();
    ms.sort_by(|a, b| a.partial_cmp(b).unwrap());
    println!("median_ms {:.1}", ms[ms.len() / 2]);
    Ok(())
}
