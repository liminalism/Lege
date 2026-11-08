//! Times a sauvola-contract binarizer (`img01_inp` NHWC grey in, >0.5 = paper out)
//! on the wgpu executor. Usage: verify_sauvola_gpu <model.onnx> <image> [out.png]
use anyhow::Result;
use lege_gpu::vision::SauvolaProcessor;

fn main() -> Result<()> {
    let model = std::env::args().nth(1).expect("model path");
    let image_path = std::env::args().nth(2).expect("image path");
    let out = std::env::args()
        .nth(3)
        .unwrap_or_else(|| "sauvola_gpu_out.png".into());
    let proc = SauvolaProcessor::from_model_path(&model)?;
    let img = image::open(&image_path)?.to_rgb8();
    println!("input {}x{}", img.width(), img.height());
    for i in 0..4 {
        let t = std::time::Instant::now();
        let mask = proc.binarize_rgb(&img)?;
        println!("run {i}: {:?}", t.elapsed());
        if i == 0 {
            mask.save(&out)?;
        }
    }
    Ok(())
}
