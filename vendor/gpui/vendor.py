#!/usr/bin/env python3
"""Copies GPUI from a Zed checkout into vendor/gpui and writes its manifest.

    vendor/gpui/vendor.py ZED_CHECKOUT REV

Zed's crates name their dependencies `workspace = true`; outside Zed's workspace those are spelled
out here from Zed's root Cargo.toml, path dependencies becoming git dependencies on Zed at REV.
Dev-dependencies, examples and benches are left out. Rhein's changes (marked `Rhein:`) have to be
applied again afterwards; see RHEIN.md.
"""
import json, shutil, sys, tomllib
from pathlib import Path

zed, rev = Path(sys.argv[1]), sys.argv[2]
here = Path(__file__).resolve().parent
src = zed / "crates/gpui"
root = tomllib.loads((zed / "Cargo.toml").read_text())
workspace = root["workspace"]["dependencies"]
manifest = tomllib.loads((src / "Cargo.toml").read_text())
ZED = "https://github.com/zed-industries/zed"

def resolve(name, spec):
    if not (isinstance(spec, dict) and spec.get("workspace")):
        return spec
    base = workspace[name]
    base = {"version": base} if isinstance(base, str) else dict(base)
    if "path" in base:
        base.pop("path")
        base.pop("version", None)
        base.update(git=ZED, rev=rev)
    for key, value in spec.items():
        if key == "workspace":
            continue
        if key == "features":
            base["features"] = sorted(set(base.get("features", [])) | set(value))
        else:
            base[key] = value
    return base

def value(v):
    if isinstance(v, bool):
        return "true" if v else "false"
    if isinstance(v, str):
        return json.dumps(v)
    if isinstance(v, list):
        return "[" + ", ".join(value(x) for x in v) + "]"
    if isinstance(v, dict):
        return "{ " + ", ".join(f"{k} = {value(x)}" for k, x in v.items()) + " }"
    raise TypeError(v)

def table(name, deps):
    out = [f"[{name}]"]
    for dep, spec in deps.items():
        out.append(f"{dep} = {value(resolve(dep, spec))}")
    return "\n".join(out) + "\n"

package = dict(manifest["package"])
package["edition"] = root["workspace"]["package"]["edition"]
package["publish"] = False
package.pop("autobenches", None)
out = [f"# GPUI from Zed {rev[:7]} with Rhein's patch; see RHEIN.md. Written by vendor.py.\n"]
out.append("[package]\n" + "\n".join(f"{k} = {value(v)}" for k, v in package.items()) + "\nautobenches = false\nautoexamples = false\nautotests = false\n")
out.append("[features]\n" + "\n".join(f"{json.dumps(k)} = {value(v)}" for k, v in manifest["features"].items()) + "\n")
out.append('[lib]\npath = "src/gpui.rs"\n')
out.append(table("dependencies", manifest["dependencies"]))
for target, sections in manifest.get("target", {}).items():
    if "dependencies" in sections:
        out.append(table(f"target.{json.dumps(target)}.dependencies", sections["dependencies"]))
if "build-dependencies" in manifest:
    out.append(table("build-dependencies", manifest["build-dependencies"]))
(here / "Cargo.toml").write_text("\n".join(out))

for part in ["src", "resources", "docs"]:
    shutil.rmtree(here / part, ignore_errors=True)
    shutil.copytree(src / part, here / part)
for f in ["build.rs", "README.md"]:
    shutil.copy(src / f, here / f)
shutil.copy(zed / "LICENSE-APACHE", here / "LICENSE-APACHE")
