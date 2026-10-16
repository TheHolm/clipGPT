#!/usr/bin/env python3
"""Build a FreeBSD `.pkg` file from a staged install tree.

It needs no FreeBSD host or `pkg` binary: the on-disk `.pkg` format (a zstd tar
archive with a JSON manifest) is written directly with GNU `tar` and `zstd`.
This is a best-effort cross-build intended for a Linux CI runner.

Usage:
    build-freebsd-pkg.py --name clip_gpt --origin sysutils/clip_gpt \
        --version 0.1.0 --stage-root ./stage-root \
        --comment "..." --desc "..." --maintainer "..." --www "..." \
        --output dist/clip_gpt-0.1.0-freebsd-amd64.pkg

`--stage-root` must contain the package's files laid out at their final
absolute install paths (e.g. `<stage-root>/usr/local/bin/<name>`). Every
regular file found by walking `--stage-root` is included in the package, using
its on-disk permission bits - there is no separate file list to keep in sync.

`--deps-file` optionally points at a JSON object mapping FreeBSD package names
to `{"origin", "version"}` (written by `fetch-freebsd-deps.py`); it becomes the
manifest's `deps`, so the `.pkg` records its runtime library dependencies.
"""

import argparse
import hashlib
import json
import os
import subprocess
import sys


def sha256_of(path: str) -> str:
    """Return the "1$<hex sha256>" digest string `pkg` uses for a file's `sum`."""
    digest = hashlib.sha256()
    with open(path, "rb") as f:
        for chunk in iter(lambda: f.read(1 << 20), b""):
            digest.update(chunk)
    return "1$" + digest.hexdigest()


def collect_files(stage_root: str) -> dict:
    """Walk `stage_root` and build the manifest's `files` dict.

    Keys are absolute install paths (leading `/`, matching `stage_root`-relative
    paths); values carry the checksum/ownership/permission/mtime fields `pkg`
    expects. Ownership is always root:wheel, matching every FreeBSD base-system
    package.
    """
    files = {}
    for dirpath, _dirnames, filenames in os.walk(stage_root):
        for name in filenames:
            full = os.path.join(dirpath, name)
            rel = os.path.relpath(full, stage_root)
            abs_install_path = "/" + rel
            st = os.stat(full)
            files[abs_install_path] = {
                "sum": sha256_of(full),
                "uname": "root",
                "gname": "wheel",
                "perm": oct(st.st_mode & 0o7777)[2:].zfill(4),
                "mtime": int(st.st_mtime),
            }
    return files


def build_manifest(args: argparse.Namespace, files: dict, deps: dict) -> dict:
    """Assemble the `+MANIFEST` JSON document."""
    flatsize = sum(
        os.path.getsize(os.path.join(args.stage_root, rel.lstrip("/")))
        for rel in files
    )
    manifest = {
        "name": args.name,
        "origin": args.origin,
        "version": args.version,
        "comment": args.comment,
        "maintainer": args.maintainer,
        "www": args.www,
        "abi": f"FreeBSD:{args.freebsd_major}:{args.arch}",
        "arch": f"freebsd:{args.freebsd_major}:{args.arch_alias}",
        "prefix": "/usr/local",
        "flatsize": flatsize,
        "licenselogic": "single",
        "licenses": [args.license],
        "desc": args.desc,
        "categories": [args.category],
        "files": files,
    }
    if deps:
        manifest["deps"] = deps
    return manifest


def write_pkg(manifest: dict, stage_root: str, output: str) -> None:
    """Write the manifests + staged files into a zstd-compressed tar at `output`.

    Reproduces the required tar invocation, including the leading-slash gotcha:
    staged files need a real leading `/` in the tar member name
    (`-P --transform 's,^usr/,/usr/,'`) while `+MANIFEST`/`+COMPACT_MANIFEST`
    must stay slash-less at the tar root.
    """
    compact = {k: v for k, v in manifest.items() if k not in ("files", "scripts")}

    work_dir = os.path.dirname(os.path.abspath(output)) or "."
    manifest_dir = os.path.join(work_dir, ".pkg-manifest-stage")
    os.makedirs(manifest_dir, exist_ok=True)
    with open(os.path.join(manifest_dir, "+MANIFEST"), "w") as f:
        json.dump(manifest, f, separators=(",", ":"))
    with open(os.path.join(manifest_dir, "+COMPACT_MANIFEST"), "w") as f:
        json.dump(compact, f, separators=(",", ":"))

    # GNU tar's `-C` is positional/cumulative, not reset per use: a later
    # `-C <relative-dir>` resolves relative to wherever the *previous* `-C`
    # left tar's directory context, not the original invocation cwd. Since
    # the first `-C` above already points at `manifest_dir` (an absolute
    # path), every later `-C stage_root` must also be absolute, or tar goes
    # looking for `stage_root` *inside* `manifest_dir` and fails with
    # "Cannot open: No such file or directory".
    stage_root_abs = os.path.abspath(stage_root)

    tar_cmd = [
        "tar", "--numeric-owner", "--owner=0", "--group=0",
        "-P", "--transform=s,^usr/,/usr/,",
        "-C", manifest_dir, "-cf", "-", "+MANIFEST", "+COMPACT_MANIFEST",
    ]
    for rel in manifest["files"]:
        tar_cmd += ["-C", stage_root_abs, rel.lstrip("/")]

    os.makedirs(os.path.dirname(os.path.abspath(output)) or ".", exist_ok=True)
    with open(output, "wb") as out:
        tar_proc = subprocess.Popen(tar_cmd, stdout=subprocess.PIPE)
        zstd_proc = subprocess.run(
            ["zstd", "-19", "-q", "-c"], stdin=tar_proc.stdout, stdout=out
        )
        tar_proc.wait()
        if tar_proc.returncode != 0:
            raise subprocess.CalledProcessError(tar_proc.returncode, tar_cmd)
        if zstd_proc.returncode != 0:
            raise subprocess.CalledProcessError(zstd_proc.returncode, ["zstd"])


def parse_args(argv: list) -> argparse.Namespace:
    """Parse command-line arguments. No per-product defaults for identifying
    flags, so this script stays reusable if this repo ever packages more than
    one program."""
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--name", required=True)
    parser.add_argument("--origin", required=True, help="e.g. sysutils/clip_gpt")
    parser.add_argument("--version", required=True, help="e.g. 0.1.0 (no leading v)")
    parser.add_argument("--stage-root", required=True)
    parser.add_argument("--output", required=True)
    parser.add_argument("--deps-file", help="JSON {name: {origin, version}} for deps")
    parser.add_argument("--comment", required=True, help="short one-line summary")
    parser.add_argument("--maintainer", default="TheHolm <theholm@github.com>")
    parser.add_argument("--www", required=True)
    parser.add_argument("--license", default="AGPL-3.0-or-later")
    parser.add_argument("--category", default="sysutils")
    parser.add_argument("--desc", required=True, help="longer description")
    parser.add_argument("--freebsd-major", default="15")
    parser.add_argument("--arch", default="amd64")
    parser.add_argument("--arch-alias", default="x86:64")
    return parser.parse_args(argv)


def main(argv: list) -> int:
    """Entry point: build the manifest from --stage-root and write the .pkg."""
    args = parse_args(argv)
    deps = {}
    if args.deps_file:
        with open(args.deps_file) as f:
            deps = json.load(f)
    files = collect_files(args.stage_root)
    if not files:
        print(f"error: no files found under --stage-root {args.stage_root}", file=sys.stderr)
        return 1
    manifest = build_manifest(args, files, deps)
    write_pkg(manifest, args.stage_root, args.output)
    print(f"wrote {args.output} ({len(files)} files)")
    return 0


if __name__ == "__main__":
    sys.exit(main(sys.argv[1:]))
