"""Prepare a local, reviewable archive. This script never uploads or publishes."""
import argparse
import hashlib
from pathlib import Path
import re
import tarfile


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("--target", required=True, choices=["aarch64-apple-darwin", "x86_64-apple-darwin", "x86_64-unknown-linux-gnu"])
    args = parser.parse_args()
    root = Path(__file__).resolve().parents[1]
    version = re.search(r'^version = "([^"]+)"', (root / "Cargo.toml").read_text(), re.M).group(1)
    binary = root / "target" / args.target / "release" / "aitop"
    if not binary.is_file():
        parser.error(f"Build first: cargo build --release --locked --target {args.target}")
    dist = root / "dist"
    dist.mkdir(exist_ok=True)
    name = f"aitop-{version}-{args.target}"
    archive = dist / f"{name}.tar.gz"
    with tarfile.open(archive, "w:gz") as tar:
        sources=[(binary,"aitop")]+[(root/name,name) for name in ["LICENSE","README.md","README.ko.md","CHANGELOG.md","CONTRIBUTING.md","docs/data.md","docs/providers.md","docs/releasing.md","assets/pricing.json","assets/logo.svg","assets/dashboard.svg","assets/details.svg","assets/detail-usage.svg","assets/detail-calendar.svg"]]
        for source, dest in sources:
            tar.add(source, arcname=f"{name}/{dest}", recursive=False)
    digest = hashlib.sha256(archive.read_bytes()).hexdigest()
    checksum = archive.with_suffix(archive.suffix + ".sha256")
    checksum.write_text(f"{digest}  {archive.name}\n")
    print(archive)
    print(checksum)


if __name__ == "__main__":
    main()
