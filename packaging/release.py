"""Native desktop packages. Uses Python's standard library and OS packaging tools."""
import argparse
import hashlib
import os
import plistlib
import re
import shutil
import subprocess
import sys
import tarfile
import tempfile
import zipfile
from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]
TARGET = ROOT / "target"
BIN = TARGET / "dist" / "release"
DOWNLOADS = TARGET / "downloads"
PLATFORMS = ("windows", "macos", "linux")
ARCHES = ("x64", "arm64")


def version():
    manifest = (ROOT / "Cargo.toml").read_text(encoding="utf-8")
    section = re.search(r"(?ms)^\[workspace\.package\]\s*\n(.*?)(?=^\[|\Z)", manifest)
    match = re.search(r'^version\s*=\s*"(\d+\.\d+\.\d+)"', section[1], re.MULTILINE)
    if not match:
        raise ValueError("Expected a stable workspace version in Cargo.toml")
    value = match[1]
    if os.environ.get("GITHUB_REF_TYPE") == "tag":
        if os.environ["GITHUB_REF_NAME"] != f"v{value}":
            raise ValueError(f"Tag does not match workspace version v{value}")
    return value


def run(*args, **kwargs):
    return subprocess.run([str(arg) for arg in args], check=True, **kwargs)


def payload(destination, platform):
    destination.mkdir(parents=True, exist_ok=True)
    suffix = ".exe" if platform == "windows" else ""
    for name in ("quarry", "quarry-mcp"):
        shutil.copy2(BIN / (name + suffix), destination / (name + suffix))
    for name in ("LICENSE", "README.md"):
        shutil.copy2(ROOT / name, destination / name)
    (destination / "docs").mkdir()
    for name in ("AI.md", "ARCHITECTURE.md"):
        shutil.copy2(ROOT / "docs" / name, destination / "docs" / name)


def windows(work, arch, value):
    installer = BIN.parent / "installer" / f"QuarrySetup-{value}-{arch}.exe"
    shutil.copy2(installer, DOWNLOADS / installer.name)
    stage = work / "portable"
    payload(stage, "windows")
    with zipfile.ZipFile(DOWNLOADS / f"Quarry-{value}-windows-{arch}.zip", "w", zipfile.ZIP_DEFLATED) as archive:
        for path in sorted(stage.rglob("*")):
            if path.is_file():
                archive.write(path, path.relative_to(stage).as_posix())


def macos(work, arch, value):
    app = work / "Quarry.app"
    contents = app / "Contents"
    executables = contents / "MacOS"
    resources = contents / "Resources"
    executables.mkdir(parents=True)
    resources.mkdir()
    for name in ("quarry", "quarry-mcp"):
        shutil.copy2(BIN / name, executables / name)
    for name in ("LICENSE", "README.md"):
        shutil.copy2(ROOT / name, resources / name)
    (resources / "docs").mkdir()
    for name in ("AI.md", "ARCHITECTURE.md"):
        shutil.copy2(ROOT / "docs" / name, resources / "docs" / name)
    info = {
        "CFBundleIdentifier": "io.github.dipendra-creator.quarry",
        "CFBundleName": "Quarry",
        "CFBundleDisplayName": "Quarry",
        "CFBundleExecutable": "quarry",
        "CFBundleIconFile": "quarry.icns",
        "CFBundlePackageType": "APPL",
        "CFBundleShortVersionString": value,
        "CFBundleVersion": value,
        "LSMinimumSystemVersion": os.environ.get("MACOSX_DEPLOYMENT_TARGET", "12.0"),
        "LSApplicationCategoryType": "public.app-category.developer-tools",
        "NSHighResolutionCapable": True,
        "NSSupportsAutomaticGraphicsSwitching": True,
        "NSPrincipalClass": "NSApplication",
        "NSHumanReadableCopyright": "MIT License; Quarry contributors",
    }
    (contents / "Info.plist").write_bytes(plistlib.dumps(info))
    iconset = work / "quarry.iconset"
    iconset.mkdir()
    for size in (16, 32, 128, 256, 512):
        for scale in (1, 2):
            name = f"icon_{size}x{size}" + ("@2x" if scale == 2 else "") + ".png"
            run("sips", "-z", size * scale, size * scale, ROOT / "packaging" / "quarry.png",
                "--out", iconset / name, stdout=subprocess.DEVNULL)
    run("iconutil", "-c", "icns", iconset, "-o", resources / "quarry.icns")
    # Ad hoc signatures support native Apple silicon execution; these are not notarized.
    for executable in (executables / "quarry-mcp", executables / "quarry", app):
        run("codesign", "--force", "--sign", "-", "--timestamp=none", executable)
    run("codesign", "--verify", "--deep", "--strict", app)
    run(sys.executable, ROOT / "packaging" / "verify.py", "--platform", "macos",
        "--arch", arch, "--binary-dir", executables)
    run("ditto", "-c", "-k", "--sequesterRsrc", "--keepParent", app,
        DOWNLOADS / f"Quarry-{value}-macos-{arch}.zip")
    dmg_root = work / "dmg"
    dmg_root.mkdir()
    shutil.copytree(app, dmg_root / "Quarry.app", symlinks=True)
    (dmg_root / "Applications").symlink_to("/Applications", target_is_directory=True)
    (dmg_root / "Install.txt").write_text(
        "Drag Quarry.app to Applications. These builds are ad hoc signed, not Apple notarized.\n"
        "If macOS blocks launch, use System Settings > Privacy & Security > Open Anyway.\n"
        "The MCP executable is /Applications/Quarry.app/Contents/MacOS/quarry-mcp.\n",
        encoding="utf-8",
    )
    dmg = DOWNLOADS / f"Quarry-{value}-macos-{arch}.dmg"
    run("hdiutil", "create", "-quiet", "-volname", f"Quarry {value}", "-srcfolder",
        dmg_root, "-format", "UDZO", dmg)
    run("hdiutil", "verify", dmg)


def desktop_entry():
    return (
        "[Desktop Entry]\nType=Application\nName=Quarry\n"
        "Comment=Native database client with an AI assistant\nExec=quarry\n"
        "Icon=quarry\nTerminal=false\nCategories=Development;Database;\n"
        "StartupWMClass=io.github.dipendra-creator.quarry\n"
    )


def tar_owner(member):
    member.uid = member.gid = 0
    member.uname = member.gname = "root"
    return member


def linux(work, arch, value):
    deb_arch = "amd64" if arch == "x64" else "arm64"
    actual_arch = subprocess.check_output(["dpkg", "--print-architecture"], text=True).strip()
    if actual_arch != deb_arch:
        raise ValueError(f"Expected native Debian architecture {deb_arch}, got {actual_arch}")
    stage = work / f"Quarry-{value}-linux-{arch}"
    payload(stage, "linux")
    shutil.copy2(ROOT / "packaging" / "quarry.png", stage / "quarry.png")
    (stage / "quarry.desktop").write_text(desktop_entry(), encoding="utf-8")
    with tarfile.open(DOWNLOADS / f"Quarry-{value}-linux-{arch}.tar.gz", "w:gz") as archive:
        archive.add(stage, arcname=stage.name, filter=tar_owner)
    deb_root = work / "deb"
    for directory in ("usr/bin", "usr/share/applications", "usr/share/pixmaps", "usr/share/doc/quarry", "DEBIAN"):
        (deb_root / directory).mkdir(parents=True, exist_ok=True)
    for name in ("quarry", "quarry-mcp"):
        shutil.copy2(BIN / name, deb_root / "usr/bin" / name)
        (deb_root / "usr/bin" / name).chmod(0o755)
    shutil.copy2(stage / "quarry.desktop",
                 deb_root / "usr/share/applications/io.github.dipendra-creator.quarry.desktop")
    shutil.copy2(stage / "quarry.png", deb_root / "usr/share/pixmaps/quarry.png")
    for name in ("LICENSE", "README.md"):
        shutil.copy2(ROOT / name, deb_root / "usr/share/doc/quarry" / name)
    for name in ("AI.md", "ARCHITECTURE.md"):
        shutil.copy2(ROOT / "docs" / name, deb_root / "usr/share/doc/quarry" / name)
    maintainer = "Dipendra Bhardwaj <66115431+Dipendra-creator@users.noreply.github.com>"
    (work / "debian").mkdir()
    (work / "debian/control").write_text(
        f"Source: quarry\nSection: database\nPriority: optional\nMaintainer: {maintainer}\n\n"
        "Package: quarry\nArchitecture: any\nDescription: Native database client\n",
        encoding="utf-8",
    )
    result = run("dpkg-shlibdeps", "-O", "-e" + str(BIN / "quarry"),
                 "-e" + str(BIN / "quarry-mcp"), cwd=work, capture_output=True, text=True)
    if result.stderr:
        print(result.stderr, file=sys.stderr)
    dependencies = result.stdout.strip().removeprefix("shlibs:Depends=")
    dependencies += ", ca-certificates, libvulkan1"
    installed_size = sum(path.stat().st_size for path in deb_root.rglob("*") if path.is_file()) // 1024
    (deb_root / "DEBIAN/control").write_text(
        f"Package: quarry\nVersion: {value}\nSection: database\nPriority: optional\n"
        f"Architecture: {deb_arch}\nMaintainer: {maintainer}\n"
        f"Installed-Size: {installed_size}\nDepends: {dependencies}\n"
        "Recommends: xdg-desktop-portal\nHomepage: https://github.com/Dipendra-creator/quarry\n"
        "Description: Native database client with an AI assistant\n"
        " Browse and edit PostgreSQL, MySQL, MariaDB and SQLite databases.\n"
        " Includes a SQL editor, approval-gated AI tools and an MCP server.\n",
        encoding="utf-8",
    )
    deb = DOWNLOADS / f"Quarry-{value}-linux-{arch}.deb"
    run("dpkg-deb", "--root-owner-group", "--build", deb_root, deb)
    run("dpkg-deb", "--info", deb)


def expected_assets(value):
    names = set()
    for arch in ARCHES:
        names.update((f"QuarrySetup-{value}-{arch}.exe", f"Quarry-{value}-windows-{arch}.zip"))
        for extension in ("zip", "dmg"):
            names.add(f"Quarry-{value}-macos-{arch}.{extension}")
        for extension in ("tar.gz", "deb"):
            names.add(f"Quarry-{value}-linux-{arch}.{extension}")
    return names


def checksums(value):
    expected = expected_assets(value)
    actual = {path.name for path in DOWNLOADS.iterdir() if path.is_file()} - {"SHA256SUMS.txt"}
    if actual != expected:
        raise ValueError(f"Incomplete release: missing {sorted(expected - actual)}; unexpected {sorted(actual - expected)}")
    lines = []
    for name in sorted(expected):
        path = DOWNLOADS / name
        if path.stat().st_size == 0:
            raise ValueError(f"Empty release asset: {name}")
        with path.open("rb") as stream:
            digest = hashlib.file_digest(stream, "sha256").hexdigest() if hasattr(hashlib, "file_digest") else None
            if digest is None:
                stream.seek(0)
                hasher = hashlib.sha256()
                for chunk in iter(lambda: stream.read(1024 * 1024), b""):
                    hasher.update(chunk)
                digest = hasher.hexdigest()
        lines.append(f"{digest}  {name}\n")
    (DOWNLOADS / "SHA256SUMS.txt").write_text("".join(lines), encoding="ascii")
    print(f"Verified all {len(expected)} platform packages and wrote SHA256SUMS.txt")


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    commands = parser.add_subparsers(dest="command", required=True)
    info = commands.add_parser("version")
    info.add_argument("--github-env", action="store_true")
    package = commands.add_parser("package")
    package.add_argument("--platform", choices=PLATFORMS, required=True)
    package.add_argument("--arch", choices=ARCHES, required=True)
    commands.add_parser("checksums")
    args = parser.parse_args()
    value = version()
    if args.command == "version":
        if args.github_env:
            with open(os.environ["GITHUB_ENV"], "a", encoding="utf-8") as stream:
                stream.write(f"RELEASE_VERSION={value}\n")
        print(value)
    elif args.command == "checksums":
        checksums(value)
    else:
        DOWNLOADS.mkdir(parents=True, exist_ok=True)
        work_root = TARGET / "package-work"
        work_root.mkdir(exist_ok=True)
        with tempfile.TemporaryDirectory(dir=work_root) as folder:
            work = Path(folder).resolve()
            # Verify the absolute cleanup target stays inside this workspace's build directory.
            if not work.is_relative_to(work_root.resolve()):
                raise ValueError("Package staging directory escaped target/package-work")
            {"windows": windows, "macos": macos, "linux": linux}[args.platform](work, args.arch, value)


if __name__ == "__main__":
    main()
