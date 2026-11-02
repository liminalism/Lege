#!/usr/bin/env python3
"""Prepare a temporary historical-source crate for a matched baseline build.

This copies Git source verbatim, preserving notices, outside the distributed
crate. The historical connected-component implementation has GPL provenance;
the reference is measurement material, not part of a non-GPL release.
Build it separately, then use compare_quality_workers.py with both binaries.
"""
import argparse
from pathlib import Path
import shutil
import subprocess

CRATE = Path(__file__).resolve().parents[1]
ROOT = CRATE.parents[1]
PREFIX = str(CRATE.relative_to(ROOT))


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--revision", default="HEAD")
    parser.add_argument("--output", type=Path, required=True)
    args = parser.parse_args()
    output = args.output.resolve()
    if output.is_relative_to(CRATE):
        parser.error("historical reference must stay outside the distributed crate")
    output.mkdir(parents=True, exist_ok=False)
    commit = subprocess.check_output(["git", "rev-parse", args.revision], cwd=ROOT, text=True).strip()
    names = subprocess.check_output(["git", "ls-tree", "-r", "--name-only", commit, "--", PREFIX + "/src"],cwd=ROOT,text=True).splitlines()
    for name in names:
        target = output / Path(name).relative_to(PREFIX)
        target.parent.mkdir(parents=True,exist_ok=True)
        target.write_bytes(subprocess.check_output(["git","show",f"{commit}:{name}"],cwd=ROOT))
    for name in ["Cargo.toml","Cargo.lock","build.rs","LICENSE-MIT","LICENSE-APACHE","NOTICE","README.md"]:
        shutil.copyfile(CRATE/name,output/name)
    shutil.copytree(CRATE/"tests",output/"tests")
    shutil.copytree(CRATE/"benches",output/"benches")
    (output/"tools").mkdir()
    shutil.copyfile(CRATE/"tools/pdfium_oracle.py",output/"tools/pdfium_oracle.py")
    # Same loading, timed encode, decode and oracle loops; only new APIs and
    # post-encode reporting are removed from this compatibility harness.
    worker = (CRATE/"tests/quality_benchmark.rs").read_text()
    worker = worker.replace('#[path = "quality_benchmark/grouped.rs"]\nmod grouped;\n','')
    worker = worker.replace('#[path = "quality_benchmark/memory.rs"]\nmod memory;\n','')
    start = worker.index('// ENCODE_MEMORY_WORKER_BEGIN')
    end = worker.index('// ENCODE_MEMORY_WORKER_END',start) + len('// ENCODE_MEMORY_WORKER_END')
    worker = worker[:start] + worker[end:]

    start = worker.index('fn config(')
    end = worker.index('\nfn differences',start)
    worker = worker[:start] + '''fn config(mode: &str) -> Jbig2Config {
    let mut cfg = match mode {
        "generic" => Jbig2Config::lossless(),
        "symbol_shipping" | "symbol_fixed" => Jbig2Config::text(),
        "unify_shipping" | "unify_fixed" => Jbig2Config::text_symbol_unify(),
        _ => panic!("unsupported historical mode {mode}"),
    };
    if mode.ends_with("_fixed") { cfg.auto_thresh = false; }
    cfg.want_full_headers = false;
    cfg
}
''' + worker[end:]
    worker = worker.replace('Jbig2Config, PdfDictionaryPolicy, encode_single_image_lossless','Jbig2Config, encode_single_image_lossless')
    worker = worker.replace('let canonical_termination = cfg.canonical_arithmetic_termination;',
                            'let canonical_termination = false;')
    start = worker.index('    if let Some((base, size)) = mode.rsplit_once("_group")')
    end = worker.index('    let pages: Vec<_>',start)
    worker = worker[:start] + worker[end:]
    start = worker.index('    grouped::print_resources')
    end = worker.index('    println!(\n        "QUALITY_STAGES',start)
    worker = worker[:start] + worker[end:]
    start = worker.index('    assert_eq!(\n        dictionary_memory,')
    end = worker.index('    let mut context',start)
    worker = worker[:start] + worker[end:]
    (output/"tests/quality_benchmark.rs").write_text(worker)
    (output/"REFERENCE-NOTICE").write_text(
        "Benchmark-only historical source snapshot at " + commit + "\n"
        "Original source notices are retained. The old cc.rs identifies a GPLv2-or-later DjVuLibre port.\n"
        "This temporary reference is not included in the proposed non-GPL package.\n"
        "Manifest/lock/build script and compatibility harness are taken from the current working tree.\n")
    print(output)


if __name__ == "__main__":
    main()
