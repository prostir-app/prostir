#!/usr/bin/env python3
"""Writes THIRD-PARTY-LICENSES.txt: the licenses and copyright notices of everything
that ends up inside Prostir's release files besides Prostir itself.

    scripts/third-party-licenses.py [-o FILE] [--openxr-aar AAR]

It walks the normal (not dev, not build) dependencies of the PC program
(prostir, with the settings window, for x86_64 Linux) and the headset app
(prostir-client for Android), reads each crate's license files from the local Cargo
registry, and adds the font (in the APK and the settings window) and the Khronos
OpenXR loader that the APK bundles. Only the Python standard library and
`cargo metadata` are used. It fails when a crate's license text cannot be found, so a
new dependency without one is noticed before a release.
"""

import argparse
import hashlib
import json
import os
import re
import subprocess
import sys
import zipfile

ROOT = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
PARTS = [
    ("prostir", "x86_64-unknown-linux-gnu", "PC"),
    ("prostir-client", "aarch64-linux-android", "headset"),
]
# For "A OR B" the first license in this list that a crate offers is the one we use.
PREFERENCE = ["MIT", "Apache-2.0", "BSD-3-Clause", "BSD-2-Clause", "ISC", "Zlib",
              "0BSD", "Unlicense", "Unicode-3.0", "BSL-1.0"]
LICENSE_FILE = re.compile(r"^(licen[cs]e|copying|copyright|notice|unlicense|.*-licen[cs]e)", re.I)

MIT_TEXT = """Permission is hereby granted, free of charge, to any person obtaining a copy
of this software and associated documentation files (the "Software"), to deal
in the Software without restriction, including without limitation the rights
to use, copy, modify, merge, publish, distribute, sublicense, and/or sell
copies of the Software, and to permit persons to whom the Software is
furnished to do so, subject to the following conditions:

The above copyright notice and this permission notice shall be included in all
copies or substantial portions of the Software.

THE SOFTWARE IS PROVIDED "AS IS", WITHOUT WARRANTY OF ANY KIND, EXPRESS OR
IMPLIED, INCLUDING BUT NOT LIMITED TO THE WARRANTIES OF MERCHANTABILITY,
FITNESS FOR A PARTICULAR PURPOSE AND NONINFRINGEMENT. IN NO EVENT SHALL THE
AUTHORS OR COPYRIGHT HOLDERS BE LIABLE FOR ANY CLAIM, DAMAGES OR OTHER
LIABILITY, WHETHER IN AN ACTION OF CONTRACT, TORT OR OTHERWISE, ARISING FROM,
OUT OF OR IN CONNECTION WITH THE SOFTWARE OR THE USE OR OTHER DEALINGS IN THE
SOFTWARE.
"""


def classify_name(name):
    n = name.lower()
    for token, lic in [("apache", "Apache-2.0"), ("mit", "MIT"), ("bsd", "BSD"),
                       ("zlib", "Zlib"), ("isc", "ISC"), ("unlicense", "Unlicense"),
                       ("unicode", "Unicode-3.0"), ("boost", "BSL-1.0"), ("notice", "NOTICE")]:
        if token in n:
            return lic
    return None


def classify_text(text):
    t = " ".join(text.split())
    # First: the Unicode license grants its permission in the MIT license's words.
    if "UNICODE LICENSE" in t.upper() and "UNICODE, INC." in t.upper():
        return "Unicode-3.0"
    if "Apache License" in t and "Version 2.0" in t and "TERMS AND CONDITIONS" in t:
        return "Apache-2.0"
    if "Permission is hereby granted, free of charge" in t:
        return "MIT"
    if "Redistribution and use in source and binary forms" in t:
        return "BSD"
    if "Permission to use, copy, modify, and/or distribute this software" in t:
        return "ISC"
    if "This is free and unencumbered software" in t:
        return "Unlicense"
    if "This software is provided 'as-is'" in t:
        return "Zlib"
    return None


def matches(file_license, wanted):
    if file_license is None:
        return False
    if file_license == "BSD":
        return wanted.startswith("BSD") or wanted == "0BSD"
    return file_license == wanted


def chosen_licenses(expr):
    """The licenses we rely on for an SPDX expression: every AND term, and for an OR
    the preferred option."""
    expr = expr.replace("/", " OR ")
    chosen = []
    for term in re.split(r"\s+AND\s+", expr):
        term = term.strip().strip("()")
        options = [o.strip().strip("()") for o in re.split(r"\s+OR\s+", term)]
        options = [o.split(" WITH ")[0].strip() for o in options]
        pick = next((p for p in PREFERENCE if p in options), options[0])
        chosen.append(pick)
    return chosen


def dependencies(package, target):
    meta = json.loads(subprocess.check_output(
        ["cargo", "metadata", "--format-version", "1", "--locked", "--filter-platform", target],
        cwd=ROOT))
    packages = {p["id"]: p for p in meta["packages"]}
    nodes = {n["id"]: n for n in meta["resolve"]["nodes"]}
    start = next(p["id"] for p in meta["packages"] if p["name"] == package)
    seen, todo = set(), [start]
    while todo:
        pid = todo.pop()
        if pid in seen:
            continue
        seen.add(pid)
        for dep in nodes[pid]["deps"]:
            if any(k["kind"] is None for k in dep["dep_kinds"]):
                todo.append(dep["pkg"])
    # Path dependencies are Prostir's own crates.
    return [packages[i] for i in seen if packages[i]["source"] is not None]


def apache_text(near):
    """The Apache License 2.0 text from another crate in the same registry."""
    registry = os.path.dirname(near)
    for crate in sorted(os.listdir(registry)):
        for name in ("LICENSE-APACHE", "LICENSE-APACHE.txt", "LICENSE-APACHE-2.0"):
            path = os.path.join(registry, crate, name)
            if os.path.isfile(path) and classify_text(read(path)) == "Apache-2.0":
                return read(path)
    raise SystemExit(f"no Apache License 2.0 text anywhere in {registry}")


def read(path):
    with open(path, encoding="utf-8", errors="replace") as f:
        return f.read().replace("\r\n", "\n").strip() + "\n"


def crate_texts(pkg):
    """[(title, text)] for one crate, or raises when nothing fits its license."""
    directory = os.path.dirname(pkg["manifest_path"])
    expr = pkg["license"]
    files = sorted(f for f in os.listdir(directory)
                   if LICENSE_FILE.match(f) and os.path.isfile(os.path.join(directory, f)))
    if pkg.get("license_file"):
        files = sorted(set(files) | {pkg["license_file"]})
    if not expr:
        if not files:
            raise SystemExit(f"{pkg['name']} {pkg['version']}: no license and no license file")
        return "see text", [(f, read(os.path.join(directory, f))) for f in files]
    wanted = chosen_licenses(expr)
    texts, covered, unknown = [], set(), []
    for f in files:
        text = read(os.path.join(directory, f))
        lic = classify_name(f)
        if lic in (None, "NOTICE") or lic == "BSD":
            lic = classify_text(text) or lic
        if lic == "NOTICE":
            texts.append((f, text))
            continue
        hit = [w for w in wanted if matches(lic, w)]
        if hit:
            texts.append((f, text))
            covered.update(hit)
        elif lic is None:
            unknown.append((f, text))
    missing = [w for w in wanted if w not in covered]
    if missing:
        # A file we could not classify (a pointer like ring's LICENSE, or a COPYING that
        # explains a dual license) goes in as is.
        texts.extend(unknown)
        if missing == ["MIT"] and not unknown:
            authors = ", ".join(pkg.get("authors") or []) or f"the {pkg['name']} authors"
            texts.append(("MIT license (the crate ships no license file)",
                          f"Copyright (c) {authors}\n\n{MIT_TEXT}"))
        elif missing == ["Apache-2.0"] and not unknown:
            # The license text is the same for everyone; the crate only names it.
            texts.append(("Apache License 2.0 (the crate ships no license file)",
                          apache_text(directory)))
        elif not unknown:
            raise SystemExit(f"{pkg['name']} {pkg['version']} ({expr}): no license text for "
                             f"{', '.join(missing)} in {directory}")
    return " AND ".join(wanted), texts


def main():
    parser = argparse.ArgumentParser(description=__doc__.split("\n\n")[0])
    parser.add_argument("-o", "--output", default=os.path.join(ROOT, "build", "THIRD-PARTY-LICENSES.txt"))
    parser.add_argument("--openxr-aar", help="the Khronos OpenXR loader AAR the APK takes its loader from")
    args = parser.parse_args()

    crates = {}
    for package, target, part in PARTS:
        for pkg in dependencies(package, target):
            key = (pkg["name"], pkg["version"])
            crates.setdefault(key, (pkg, set()))[1].add(part)

    unique = {}  # text hash -> (text, [crate labels])
    rows = []
    for key in sorted(crates):
        pkg, parts = crates[key]
        used, texts = crate_texts(pkg)
        label = f"{pkg['name']} {pkg['version']}"
        rows.append((label, pkg["license"] or "see text", used, " + ".join(sorted(parts))))
        for title, text in texts:
            h = hashlib.sha256(" ".join(text.split()).encode()).hexdigest()
            unique.setdefault(h, [text, title, []])[2].append(label)

    out = []
    w = out.append
    w("THIRD-PARTY LICENSES")
    w("====================")
    w("")
    w("Prostir is free software under the GNU General Public License, version 3 or (at")
    w("your option) any later version, with an additional permission for linking the")
    w("Meta Platform SDK. The full texts are in the files LICENSE and")
    w("LICENSE-EXCEPTION.md next to this one, and at")
    w("https://github.com/prostir-app/prostir. The source code of every release is")
    w("there too.")
    w("")
    w("Prostir's release files also contain the third-party software listed below. Each")
    w("part keeps its own license. Where a crate lets you choose between licenses, the one")
    w("in the \"Used\" column is the one these release files rely on.")
    w("")
    w("PC: the prostir program (tar.gz, .deb, .rpm, AUR).")
    w("headset: the Quest app (.apk).")
    w("")
    w("")
    w("1. Bundled with the headset app (and the font with the PC settings window)")
    w("--------------------------------------------------------------------------")
    w("")
    w("Noto Sans (NotoSans-Regular.ttf, NotoSans-Medium.ttf), SIL Open Font License 1.1:")
    w("")
    w(read(os.path.join(ROOT, "crates", "prostir-client", "assets", "OFL-NotoSans.txt")))
    w("")
    w("Khronos OpenXR loader for Android (libopenxr_loader.so), Apache License 2.0,")
    w("https://github.com/KhronosGroup/OpenXR-SDK-Source")
    w("")
    if args.openxr_aar:
        with zipfile.ZipFile(args.openxr_aar) as aar:
            names = [n for n in aar.namelist() if re.search(r"(LICENSE|NOTICE)[^/]*$", n)]
            if not names:
                raise SystemExit(f"{args.openxr_aar}: no LICENSE inside")
            for n in sorted(names):
                w(aar.read(n).decode("utf-8", "replace").replace("\r\n", "\n").strip())
                w("")
    else:
        w("(Its license text is added when the APK is built; see scripts/build-apk.sh.)")
        w("")
    w("")
    w("2. Rust crates")
    w("--------------")
    w("")
    width = max(len(r[0]) for r in rows)
    lwidth = max(len(r[2]) for r in rows)
    w(f"{'Crate'.ljust(width)}  {'Used'.ljust(lwidth)}  {'Part'.ljust(12)}  Declared license")
    for label, declared, used, part in rows:
        w(f"{label.ljust(width)}  {used.ljust(lwidth)}  {part.ljust(12)}  {declared}")
    w("")
    w("")
    w("3. License texts and copyright notices of the crates")
    w("----------------------------------------------------")
    for text, title, labels in sorted(unique.values(), key=lambda u: (u[2][0], u[1])):
        w("")
        w("=" * 78)
        w(f"{title}, used by:")
        for line in wrap(", ".join(labels), 78):
            w(line)
        w("=" * 78)
        w("")
        w(text.rstrip())
    w("")

    os.makedirs(os.path.dirname(os.path.abspath(args.output)), exist_ok=True)
    with open(args.output, "w", encoding="utf-8") as f:
        f.write("\n".join(out))
    print(f">> {args.output}: {len(rows)} crates, {len(unique)} license texts", file=sys.stderr)


def wrap(text, width):
    lines, line = [], ""
    for word in text.split(" "):
        if line and len(line) + 1 + len(word) > width:
            lines.append(line)
            line = word
        else:
            line = f"{line} {word}" if line else word
    if line:
        lines.append(line)
    return lines


if __name__ == "__main__":
    main()
