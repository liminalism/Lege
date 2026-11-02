//! Minimal PDF wrapper and optional independent renderer oracles. Compare
//! against the same renderer's uncompressed-source control to account for its
//! pixel sampling/dithering, separately from the native source equality gate.
#![allow(dead_code)]
use super::pbm::Pbm;
use std::process::Command;

pub fn wrap_page(width: u32, height: u32, globals: Option<&[u8]>, page: &[u8]) -> Vec<u8> {
    let dictionary = format!(
        "/Filter /JBIG2Decode {}",
        if globals.is_some() {
            "/DecodeParms << /JBIG2Globals 6 0 R >>"
        } else {
            ""
        }
    );
    wrap_image(width, height, globals, page, &dictionary)
}

pub fn wrap_uncompressed(source: &Pbm) -> Vec<u8> {
    let stride = (source.width as usize).div_ceil(8);
    let mut packed = vec![0xff; stride * source.height as usize];
    for y in 0..source.height {
        for x in 0..source.width {
            if source.get(x, y) != 0 {
                packed[y as usize * stride + x as usize / 8] &= !(0x80 >> (x % 8));
            }
        }
    }
    wrap_image(source.width, source.height, None, &packed, "")
}

fn wrap_image(
    width: u32,
    height: u32,
    globals: Option<&[u8]>,
    page: &[u8],
    dictionary: &str,
) -> Vec<u8> {
    fn stream(dictionary: String, data: &[u8]) -> Vec<u8> {
        let mut bytes = format!("<< {dictionary} /Length {} >>\nstream\n", data.len()).into_bytes();
        bytes.extend_from_slice(data);
        bytes.extend_from_slice(b"\nendstream");
        bytes
    }
    let mut objects = vec![
        b"<< /Type /Catalog /Pages 2 0 R >>".to_vec(),
        b"<< /Type /Pages /Kids [3 0 R] /Count 1 >>".to_vec(),
        format!("<< /Type /Page /Parent 2 0 R /MediaBox [0 0 {width} {height}] /Resources << /XObject << /Im 4 0 R >> >> /Contents 5 0 R >>").into_bytes(),
        stream(format!("/Type /XObject /Subtype /Image /Width {width} /Height {height} /ColorSpace /DeviceGray /BitsPerComponent 1 /Interpolate false /Decode [0 1] {dictionary}"), page),
        stream(String::new(), format!("q {width} 0 0 {height} 0 0 cm /Im Do Q\n").as_bytes()),
    ];
    if let Some(globals) = globals {
        objects.push(stream(String::new(), globals));
    }
    let mut pdf = b"%PDF-1.4\n".to_vec();
    let mut offsets = Vec::new();
    for (index, object) in objects.iter().enumerate() {
        offsets.push(pdf.len());
        pdf.extend_from_slice(format!("{} 0 obj\n", index + 1).as_bytes());
        pdf.extend_from_slice(object);
        pdf.extend_from_slice(b"\nendobj\n");
    }
    let xref = pdf.len();
    pdf.extend_from_slice(
        format!("xref\n0 {}\n0000000000 65535 f \n", offsets.len() + 1).as_bytes(),
    );
    for offset in offsets {
        pdf.extend_from_slice(format!("{offset:010} 00000 n \n").as_bytes());
    }
    pdf.extend_from_slice(
        format!(
            "trailer\n<< /Size {} /Root 1 0 R >>\nstartxref\n{xref}\n%%EOF\n",
            objects.len() + 1
        )
        .as_bytes(),
    );
    pdf
}

/// Explicitly configured backends must run successfully. A release gate may
/// require both; regular developer tests can still run without external tools.
pub fn render(backend: &str, pdf: &[u8]) -> Option<Pbm> {
    render_with_diagnostics(backend, pdf).map(|(image, _)| image)
}

pub fn render_with_diagnostics(backend: &str, pdf: &[u8]) -> Option<(Pbm, String)> {
    let required = std::env::var("JBIG2_REQUIRE_PDF_ORACLES").is_ok_and(|v| v == "1");
    let executable = match std::env::var(backend) {
        Ok(path) => path,
        Err(_) => {
            assert!(!required, "required PDF oracle {backend} is not configured");
            eprintln!("PDF oracle {backend} unavailable");
            return None;
        }
    };
    let directory = tempfile::tempdir().unwrap();
    let input = directory.path().join("page.pdf");
    let output = directory.path().join("page.pbm");
    std::fs::write(&input, pdf).unwrap();
    let result = if backend == "PDFTOPPM" {
        Command::new(executable)
            .args([
                "-mono",
                "-r",
                "72",
                "-singlefile",
                "-aa",
                "no",
                "-aaVector",
                "no",
            ])
            .arg(&input)
            .arg(directory.path().join("page"))
            .output()
            .unwrap()
    } else {
        Command::new(executable)
            .arg(std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tools/pdfium_oracle.py"))
            .arg(&input)
            .arg(&output)
            .output()
            .unwrap()
    };
    assert!(
        result.status.success(),
        "{backend}: {}",
        String::from_utf8_lossy(&result.stderr)
    );
    if !result.stderr.is_empty() {
        eprintln!("{backend}: {}", String::from_utf8_lossy(&result.stderr));
    }
    if let Some(root) = std::env::var_os("JBIG2_PDF_ORACLE_ARTIFACTS") {
        let root = std::path::PathBuf::from(root).join(directory.path().file_name().unwrap());
        std::fs::create_dir_all(&root).unwrap();
        std::fs::copy(&input, root.join("page.pdf")).unwrap();
        std::fs::copy(&output, root.join("page.pbm")).unwrap();
        std::fs::write(root.join("stderr.txt"), &result.stderr).unwrap();
        eprintln!("{backend} artifacts: {}", root.display());
    }
    Some((
        Pbm::from_p4(&std::fs::read(output).unwrap()).unwrap(),
        String::from_utf8_lossy(&result.stderr).into_owned(),
    ))
}
