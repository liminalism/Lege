"""Compare two rendered pages (PPM/PNG) and report quality metrics.

usage: python3 -I metrics.py A B [--ref REF ...] [--label NAME] [--json]

A, B: images of identical size (e.g. cached-path vs fresh-backend render).
Reports: sha256 equality, mean absolute difference (MAD, 0-255 scale, over
all RGB channels), max abs diff, % pixels differing, PSNR(A,B), and a
sharpness metric for each image: mean gradient magnitude of luma
(central differences, |grad| averaged over the interior), plus the variance
of a 4-neighbour Laplacian. With --ref, also PSNR / MAD of A and of B
against each reference image (resized refs are rejected, not resampled).
"""
import hashlib
import json
import math
import sys

import numpy as np
from PIL import Image


def load(path):
    im = Image.open(path)
    im.load()
    return np.asarray(im.convert("RGB"), dtype=np.float64)


def sha(path):
    h = hashlib.sha256()
    with open(path, "rb") as f:
        for chunk in iter(lambda: f.read(1 << 20), b""):
            h.update(chunk)
    return h.hexdigest()


def luma(a):
    return 0.299 * a[..., 0] + 0.587 * a[..., 1] + 0.114 * a[..., 2]


def sharpness(a):
    y = luma(a)
    gx = (y[1:-1, 2:] - y[1:-1, :-2]) * 0.5
    gy = (y[2:, 1:-1] - y[:-2, 1:-1]) * 0.5
    grad = np.sqrt(gx * gx + gy * gy)
    lap = (y[1:-1, 2:] + y[1:-1, :-2] + y[2:, 1:-1] + y[:-2, 1:-1] - 4 * y[1:-1, 1:-1])
    return float(grad.mean()), float(lap.var())


def psnr_mad(a, b):
    d = a - b
    mse = float((d * d).mean())
    mad = float(np.abs(d).mean())
    psnr = float("inf") if mse == 0 else 10 * math.log10(255.0 * 255.0 / mse)
    return psnr, mad, mse


def main():
    args = sys.argv[1:]
    refs = []
    label = None
    as_json = False
    pos = []
    i = 0
    while i < len(args):
        if args[i] == "--ref":
            refs.append(args[i + 1])
            i += 2
        elif args[i] == "--label":
            label = args[i + 1]
            i += 2
        elif args[i] == "--json":
            as_json = True
            i += 1
        else:
            pos.append(args[i])
            i += 1
    pa, pb = pos
    a, b = load(pa), load(pb)
    out = {"label": label, "a": pa, "b": pb, "a_shape": list(a.shape), "b_shape": list(b.shape)}
    out["a_sha256"], out["b_sha256"] = sha(pa), sha(pb)
    out["identical_files"] = out["a_sha256"] == out["b_sha256"]
    if a.shape != b.shape:
        out["error"] = "shape mismatch"
        print(json.dumps(out) if as_json else out)
        return
    d = np.abs(a - b)
    psnr, mad, mse = psnr_mad(a, b)
    out.update(
        psnr_ab_db=psnr,
        mad_ab=mad,
        max_abs_diff=float(d.max()),
        pct_pixels_differing=float((d.max(axis=2) > 0).mean() * 100),
        pct_pixels_diff_gt8=float((d.max(axis=2) > 8).mean() * 100),
    )
    ga, la = sharpness(a)
    gb, lb = sharpness(b)
    out.update(
        sharp_grad_a=ga,
        sharp_grad_b=gb,
        sharp_grad_ratio_a_over_b=ga / gb if gb else float("nan"),
        lap_var_a=la,
        lap_var_b=lb,
    )
    for r in refs:
        ref = load(r)
        key = r.rsplit("/", 1)[-1]
        if ref.shape != a.shape:
            out["ref:" + key] = "shape mismatch %s" % (list(ref.shape),)
            continue
        pa_, ma_, _ = psnr_mad(a, ref)
        pb_, mb_, _ = psnr_mad(b, ref)
        gr, _ = sharpness(ref)
        out["ref:" + key] = {
            "psnr_a_db": pa_, "mad_a": ma_, "psnr_b_db": pb_, "mad_b": mb_, "sharp_grad_ref": gr,
        }
    if as_json:
        print(json.dumps(out))
    else:
        for k, v in out.items():
            print("%s: %s" % (k, v))


if __name__ == "__main__":
    main()
