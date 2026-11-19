#!/usr/bin/env bash
# Package a Linux desktop bundle already built from this checkout.
set -euo pipefail

if (( $# < 1 )); then
    printf 'Usage: %s BUNDLE_DIR [dpkg-buildpackage options]\n' "$0" >&2
    exit 2
fi
bundle=$(realpath -e -- "$1")
shift
project_root=$(cd -- "$(dirname -- "$0")/.." && pwd)
output="$project_root/dist/dav-debian"
work=$(mktemp -d)
trap 'rm -rf -- "$work"' EXIT
tree="$work/source"
mkdir -p "$tree/debian" "$tree/dav-assets"
cp "$project_root/debian/"{control,copyright,rules,twodrive-dav.install,twodrive-dav.docs} "$tree/debian/"
cp -r "$project_root/debian/source" "$tree/debian/"
cp -r "$project_root/packaging/gnome-tray-anchor" "$tree/"
install -m 0755 "$project_root/packaging/development/twodrive-dav" "$tree/dav-assets/"
cp "$project_root/packaging/development/app.twodrive.twodrive_dav_packaged.desktop" "$tree/dav-assets/"
ln -s "$bundle" "$tree/bundle"

icon="$bundle/data/flutter_assets/assets/app-icon.svg"
if [[ ! -f "$icon" ]]; then
    icon="$bundle/data/flutter_assets/assets/cloud-connected.svg"
fi
install -m 0644 "$icon" "$tree/dav-assets/twodrive-dav.svg"

changelog="$project_root/debian/changelog"
version=$(dpkg-parsechangelog -l "$changelog" -S Version)
maintainer=$(dpkg-parsechangelog -l "$changelog" -S Maintainer)
revision=$(git -C "$project_root" rev-parse --short=12 HEAD)
timestamp=$(git -C "$project_root" show -s --format=%ct HEAD)
date=$(git -C "$project_root" show -s --format=%cD HEAD)
version+="+dav${timestamp}.git${revision}"
{
    printf 'twodrive (%s) UNRELEASED; urgency=medium\n\n' "$version"
    printf '  * Package the portable DAV desktop for checkout %s.\n\n' "$revision"
    printf ' -- %s  %s\n\n' "$maintainer" "$date"
    cat "$changelog"
} > "$tree/debian/changelog"

(
    cd "$tree"
    dpkg-buildpackage --build=any --build-profiles=pkg.twodrive.dav --no-sign "$@"
)
package="$work/twodrive-dav_${version}_amd64.deb"
python3 "$project_root/scripts/test-debian-package.py" --dav-bundle "$bundle" "$package"
mkdir -p "$output"
cp "$package" "$work/"*.buildinfo "$work/"*.changes "$output/"
printf 'DAV desktop test package: %s/%s\n' "$output" "${package##*/}"
