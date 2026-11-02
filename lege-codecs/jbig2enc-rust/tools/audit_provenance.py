#!/usr/bin/env python3
"""Record source/history hashes and resolved dependency license evidence.

This inventories claims; it does not certify source authorship or SPDX license
compatibility. Supply Cargo metadata produced with --all-features and without
--filter-platform to include target-conditional dependencies.
"""
import argparse
import hashlib
import json
from pathlib import Path
import re
import subprocess
import tomllib

CRATE = Path(__file__).resolve().parents[1]
ROOT = CRATE.parents[1]


def digest(path):
    return hashlib.sha256(path.read_bytes()).hexdigest()


def git(*args):
    return subprocess.check_output(["git", *args], cwd=ROOT, text=True).strip()


def save(output, name, value):
    (output / name).write_text(json.dumps(value, indent=2, sort_keys=True) + "\n")


def license_markers(metadata):
    """Triage GNU markers in dependency text, including support assets.

    The classification is deliberately narrow. It does not turn a marker scan
    into a license grant or say that tests and target-only files are linked.
    """
    pattern = re.compile(r"\b(?:AGPL|LGPL|GPL)(?:v\d+)?\b|\bGNU (?:General|Lesser|Affero)", re.I)
    matches = []
    inventory = []
    for package in metadata["packages"]:
        directory = Path(package["manifest_path"]).parent
        if directory.resolve() == CRATE:
            continue
        stats = {"name": package["name"], "version": package["version"],
                 "text_files": 0, "text_bytes": 0, "binary_files": 0, "non_utf8_files": 0}
        for path in sorted(directory.rglob("*")):
            if not path.is_file():
                continue
            raw = path.read_bytes()
            if b"\0" in raw:
                stats["binary_files"] += 1
                continue
            try:
                content = raw.decode("utf-8")
            except UnicodeDecodeError:
                stats["non_utf8_files"] += 1
                continue
            stats["text_files"] += 1
            stats["text_bytes"] += len(raw)
            relative = str(path.relative_to(directory))
            for number, line in enumerate(content.splitlines(), 1):
                if not pattern.search(line):
                    continue
                category = "needs review"
                if package["name"] == "r-efi":
                    category = "alternative license: manifest offers MIT OR Apache-2.0 OR LGPL-2.1-or-later"
                elif "LLVM-exception" in path.name:
                    category = "GPL compatibility wording in Apache-2.0 LLVM exception"
                elif package["name"] == "nix" and relative == "test/test_kmod/hello_mod/hello.c":
                    category = "dual MIT/GPL kernel test asset; not library production source"
                elif package["name"] in ("winapi", "windows-sys") and "LOGPALETTE" in line:
                    category = "identifier named lgpl; not a license statement"
                matches.append({"name": package["name"], "version": package["version"],
                                "path": relative, "line": number, "text": line.strip(),
                                "sha256": hashlib.sha256(raw).hexdigest(), "classification": category})
        inventory.append(stats)
    return {"method": "GNU license marker regex in all UTF-8 text dependency files, including tests, documentation and license texts; no filename glob filter",
            "pattern": pattern.pattern, "inventory": inventory, "matches": matches,
            "limitations": ["Binary and non-UTF-8 files are counted but not interpreted.",
                            "A license marker is not an obligation analysis; absence of a marker does not establish source provenance.",
                            "Resolved dev/build/target packages need not be linked into a particular build."]}


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--metadata", type=Path, required=True)
    parser.add_argument("--output", type=Path, required=True)
    parser.add_argument("--scan-license-markers", action="store_true",
                        help="also scan dependency text, including support assets, for GNU license markers")
    args = parser.parse_args()
    args.output.mkdir(parents=True, exist_ok=True)
    metadata = json.loads(args.metadata.read_text())
    packages = []
    for package in metadata["packages"]:
        directory = Path(package["manifest_path"]).parent
        texts = []
        candidates = [path for path in directory.iterdir() if path.is_file()
                      and re.match(r"(?i)^(license|licence|copying|notice|copyright)([._-]|$)", path.name)]
        for child in directory.iterdir():
            if child.is_dir() and re.fullmatch(r"(?i)(licenses?|licences?|copying|notices?)", child.name):
                candidates.extend(path for path in child.rglob("*") if path.is_file())
        if package.get("license_file"):
            candidates.append(directory / package["license_file"])
        for path in sorted(set(candidates)):
            if path.is_file():
                texts.append({"path": str(path.relative_to(directory)), "sha256": digest(path), "bytes": path.stat().st_size})
        packages.append({"id": package["id"], "name": package["name"], "version": package["version"],
                         "license": package.get("license"), "license_file": package.get("license_file"),
                         "source": package.get("source"), "license_texts": texts,
                         "text_review": "inventoried; license texts require distribution review"})
    save(args.output, "dependencies.json", {"metadata_sha256": digest(args.metadata),
         "command": "cargo metadata --all-features --format-version 1 (no platform filter)",
         "scope": "resolved metadata, including dev/build/target-conditional dependencies; not a cross-compilation result",
         "packages": packages, "resolve": metadata["resolve"]})
    if args.scan_license_markers:
        report = license_markers(metadata)
        save(args.output, "dependency-license-markers.json", report)
        print(f"GNU marker scan: {len(report['matches'])} lines, "
              f"{sum(m['classification'] == 'needs review' for m in report['matches'])} unclassified")
    production = {*CRATE.joinpath("src").rglob("*.rs"), CRATE / "build.rs"}
    support = {*CRATE.joinpath("tests").rglob("*.rs"), *CRATE.joinpath("benches").rglob("*.rs"),
               *CRATE.joinpath("fuzz/fuzz_targets").rglob("*.rs"), *CRATE.joinpath("tools").glob("*.py"), *CRATE.joinpath("tools").glob("*.cc")}
    files = []
    for path in sorted(production | support):
        relative = str(path.relative_to(ROOT))
        history = git("log", "--follow", "--format=%H %ad %s", "--date=short", "--", relative).splitlines()
        explicit = [line.strip() for line in path.read_text().splitlines()
                    if re.search(r"(?i)(copyright|clean.room|\bport(?:ed)?\b|adapted|translated|derived from)", line)]
        files.append({"path": str(path.relative_to(CRATE)), "sha256": digest(path),
                      "category": "production/build" if path in production else "test/benchmark/fuzz/tool",
                      "history": history, "explicit_origin_comments": explicit,
                      "review": "history/origin inventory only; authorship and earlier upstream history not established"})
    save(args.output,"source-history.json", {"head": git("rev-parse","HEAD"), "production_file_count":len(production), "files": files,
         "limitation": "Local history begins with the monorepo import; absence of a notice or GPL keyword proves no origin claim."})
    manifests = []
    paths = git("ls-files", "--cached", "--others", "--exclude-standard", "--", "*Cargo.toml").splitlines()
    for name in sorted(set(paths)):
        path = ROOT / name
        if not path.is_file():
            continue
        manifest = tomllib.loads(path.read_text())
        package = manifest.get("package")
        if package is None:
            continue
        declared = package.get("license")
        resolved = declared
        inherited_from = None
        if isinstance(declared,dict) and declared.get("workspace"):
            resolved = None
            for parent in path.parent.parents:
                candidate = parent / "Cargo.toml"
                if candidate.is_file():
                    workspace = tomllib.loads(candidate.read_text()).get("workspace",{})
                    if workspace:
                        resolved = workspace.get("package",{}).get("license")
                        inherited_from = str(candidate.relative_to(ROOT))
                        break
        manifests.append({"path": name, "package": package["name"], "declared_license": declared,
                          "resolved_license": resolved, "license_file": package.get("license-file"),
                          "inherited_from": inherited_from, "manifest_sha256": digest(path)})
    save(args.output,"monorepo-manifests.json", {"packages": manifests,
         "scope": "Cargo package declarations only; not a file-level, models, npm, binary or transitive-license clearance"})
    listing = subprocess.check_output(["cargo","package","--list","--allow-dirty","--offline"],cwd=CRATE,text=True)
    package_files = listing.splitlines()
    expected_pbms = {"tests/fixtures/original/blank.pbm", "tests/fixtures/original/symbols.pbm",
                     "tests/fixtures/original/marks.pbm"}
    actual_pbms = {name for name in package_files if name.endswith(".pbm")}
    violations = [name for name in package_files if name.endswith((".pdf", ".pyc", ".exe"))
                  or "/__pycache__/" in name or name.startswith("papers/")]
    if actual_pbms != expected_pbms or violations:
        raise SystemExit(f"package fixture policy failed: PBMs={sorted(actual_pbms)}, forbidden={violations}")
    save(args.output,"package-files.json", {"command": "cargo package --list --allow-dirty --offline",
         "files": package_files,
         "fixture_policy": {"result": "pass", "pbms": sorted(actual_pbms),
                            "excluded_legacy_inputs": ["tests/fixtures/test_image.pbm", "tests/fixtures/test_image1.pbm"],
                            "forbidden": violations},
         "limitation": "listing/policy check only; package build and unpacked tests are recorded separately; no publication performed"})
    print(f"inventoried {len(packages)} dependencies, {len(files)} source files and {len(manifests)} repository manifests")


if __name__ == "__main__":
    main()
