#!/usr/bin/env python3
"""Measure encoder-worker RSS before external verification.

The child loads one PBM at a time, retains only the current codec group, encodes
and writes streams, then exits. Linux wait4 measures that child alone. The parent
subsequently verifies every page with external jbig2dec/Pillow; those allocations
are excluded from encoder RSS. Encoded streams are temporary; SHA-256 manifests
and logs persist. This measures a process, including its input loader and output
sink, not allocations attributable only to the library.
"""
import argparse
import csv
import hashlib
import json
import os
from pathlib import Path
import platform
import shutil
import subprocess
import tempfile

from PIL import Image, ImageChops, __version__ as pillow_version

CRATE = Path(__file__).resolve().parents[1]


def digest(path):
    value = hashlib.sha256()
    with path.open("rb") as stream:
        for block in iter(lambda:stream.read(1024*1024),b""):
            value.update(block)
    return value.hexdigest()


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--corpus",type=Path,required=True)
    parser.add_argument("--output",type=Path,required=True)
    parser.add_argument("--modes",nargs="+",default=["exact_group32","exact_group64","exact_group128","exact_groupwhole"])
    parser.add_argument("--require-exact",action="store_true",help="fail on any source pixel difference")
    args = parser.parse_args()
    oracle = os.environ.get("JBIG2DEC") or shutil.which("jbig2dec")
    if not oracle:
        parser.error("JBIG2DEC or jbig2dec on PATH is required")
    if platform.system() != "Linux" or not hasattr(os,"wait4"):
        parser.error("encoder RSS measurement requires Linux wait4")
    if any("_group" not in mode for mode in args.modes):
        parser.error("each mode must explicitly identify its group size")
    output = args.output.resolve(); output.mkdir(parents=True,exist_ok=False)
    corpus = args.corpus.resolve(); pages = sorted(corpus.glob("*.pbm"))
    if not pages:
        parser.error("corpus has no PBM pages")
    manifest = [{"file":path.name,"sha256":digest(path)} for path in pages]
    (output/"corpus.json").write_text(json.dumps(manifest,indent=2)+"\n")
    build = subprocess.check_output(["cargo","test","--release","--test","quality_benchmark","--no-run","--message-format=json"],cwd=CRATE,text=True)
    executables = [message["executable"] for line in build.splitlines()
                   if (message:=json.loads(line)).get("executable") and message.get("target",{}).get("name")=="quality_benchmark"]
    if len(executables)!=1:
        raise RuntimeError("expected one memory worker")
    metadata = {"scope":"encoding-only child: incremental PBM loading, codec grouping and stream writes; excludes compilation, parent and decoder verification; not library-only allocations",
        "platform":platform.platform(),"pillow_version":pillow_version,"pages":len(pages),
        "cpu":next((line.split(":",1)[1].strip() for line in Path("/proc/cpuinfo").read_text().splitlines() if line.startswith("model name")),"unknown"),"modes":args.modes,"require_exact":args.require_exact,
        "rustc":subprocess.check_output(["rustc","--version"],text=True).strip(),
        "git_head":subprocess.check_output(["git","rev-parse","HEAD"],cwd=CRATE,text=True).strip(),
        "worker_binary_sha256":digest(Path(executables[0])),"driver_sha256":digest(Path(__file__)),
        "source_files":{str(path.relative_to(CRATE)):digest(path) for path in sorted([*CRATE.joinpath("src").rglob("*.rs"),*CRATE.joinpath("tests/quality_benchmark").glob("*.rs"),CRATE/"tests/quality_benchmark.rs",CRATE/"Cargo.toml",CRATE/"Cargo.lock"])},
        "jbig2dec":subprocess.check_output([oracle,"--version"],text=True).strip()}
    (output/"metadata.json").write_text(json.dumps(metadata,indent=2)+"\n")
    with (output/"results.csv").open("w",newline="") as results:
        writer=csv.writer(results);writer.writerow(["mode","pages","encoded_bytes","encoder_worker_peak_rss_kib","jbig2dec_source_differences"])
        for mode in args.modes:
            with tempfile.TemporaryDirectory(prefix="encoded-streams-",dir=output) as temporary:
                directory=Path(temporary)
                env=dict(os.environ,QUALITY_CORPUS=str(corpus),QUALITY_MODE=mode,QUALITY_ARTIFACT_DIR=str(directory))
                env.pop("QUALITY_GLOBAL_BITMAP_LIMIT",None)
                with (output/(mode+".log")).open("w") as log:
                    with subprocess.Popen([executables[0],"quality_encode_only_worker","--exact","--ignored","--nocapture"],cwd=CRATE,env=env,stdout=log,stderr=subprocess.STDOUT) as child:
                        _,status,usage=os.wait4(child.pid,0);child.returncode=os.waitstatus_to_exitcode(status)
                        if child.returncode:
                            raise RuntimeError(f"encoder child failed; see {mode}.log")
                records=[line.split("\t")[1:] for line in (output/(mode+".log")).read_text().splitlines() if line.startswith("QUALITY_MEMORY_RESULT\t")]
                if len(records)!=1 or records[0][:2]!=[mode,str(len(pages))]:
                    raise RuntimeError("invalid encoder result")
                streams=sorted(directory.glob("group-*/*.jb2"))
                stream_manifest=[{"file":str(path.relative_to(directory)),"bytes":path.stat().st_size,"sha256":digest(path)} for path in streams]
                total=sum(entry["bytes"] for entry in stream_manifest)
                if total!=int(records[0][2]):
                    raise RuntimeError("written streams differ from encoded byte count")
                differences=0;seen=set()
                for group in sorted(directory.glob("group-*")):
                    globals_file=group/"globals.jb2"
                    for page in sorted(group.glob("page-*.jb2")):
                        index=int(page.stem.removeprefix("page-"))
                        if index in seen or not 0<=index<len(pages):
                            raise RuntimeError("invalid page association")
                        seen.add(index);decoded=directory/"decoded.pbm"
                        command=[oracle,"-e","-o",str(decoded)]
                        if globals_file.exists():command.append(str(globals_file))
                        command.append(str(page))
                        check=subprocess.run(command,text=True,capture_output=True)
                        if check.returncode:raise RuntimeError(check.stderr)
                        with Image.open(pages[index]) as source,Image.open(decoded) as bitmap:
                            if source.size!=bitmap.size:raise RuntimeError("decoded dimensions differ")
                            difference=ImageChops.difference(source.convert("1"),bitmap.convert("1")).convert("L")
                            differences+=sum(difference.histogram()[1:])
                if len(seen)!=len(pages):raise RuntimeError("missing encoded pages")
                if (args.require_exact or mode.startswith(("exact_","generic_"))) and differences:
                    raise RuntimeError(f"exact source contract failed: {differences} differing pixels")
                (output/(mode+"-streams.json")).write_text(json.dumps(stream_manifest,indent=2)+"\n")
                writer.writerow([mode,len(pages),total,usage.ru_maxrss,differences]);results.flush()
                print(f"{mode}: encoder RSS {usage.ru_maxrss} KiB; source differences {differences}",flush=True)
            if manifest!=[{"file":path.name,"sha256":digest(path)} for path in sorted(corpus.glob("*.pbm"))]:
                raise RuntimeError("corpus changed during memory measurement")
    print(output/"results.csv")


if __name__=="__main__":
    main()
