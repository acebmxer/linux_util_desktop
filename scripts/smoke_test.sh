#!/usr/bin/env bash
# Installs a built bundle in a clean distro container and checks the app starts.
#
#   scripts/smoke_test.sh <ubuntu|fedora|opensuse|arch> <bundle-dir>
#
# <bundle-dir> holds the release files (one .deb, .rpm and .AppImage). Ubuntu
# installs the .deb, Fedora and openSUSE the .rpm, Arch runs the AppImage.
# Each run: install with the distro's own package manager, check the bundled
# linux_util and desktop entry are in place, then start the app as a normal
# user on a virtual X display and fail if it exits within LAUNCH_SECONDS.
# Uses podman, or docker when podman is not installed.
set -euo pipefail

usage() { echo "Usage: $0 <ubuntu|fedora|opensuse|arch> <bundle-dir>" >&2; exit 2; }
[[ $# -eq 2 ]] || usage
distro=$1
bundles=$(cd "$2" && pwd) || { echo "No such directory: $2" >&2; exit 2; }
launch_seconds=${LAUNCH_SECONDS:-15}

engine=$(command -v podman || command -v docker || true)
[[ -n "$engine" ]] || { echo "Needs podman or docker" >&2; exit 2; }

find_one() {
    # shellcheck disable=SC2206 # $1 is a glob pattern
    local found=("$bundles"/$1)
    [[ ${#found[@]} -eq 1 && -f "${found[0]}" ]] \
        || { echo "Expected exactly one $1 in $bundles" >&2; exit 1; }
    basename "${found[0]}"
}

case "$distro" in
    ubuntu)
        image=docker.io/library/ubuntu:24.04
        pkg=$(find_one '*.deb')
        install="export DEBIAN_FRONTEND=noninteractive; apt-get update -qq \
            && apt-get install -y -qq \"/b/$pkg\" xvfb >/dev/null"
        ;;
    fedora)
        image=registry.fedoraproject.org/fedora:latest
        pkg=$(find_one '*.rpm')
        install="dnf install -y -q \"/b/$pkg\" xorg-x11-server-Xvfb"
        ;;
    opensuse)
        image=registry.opensuse.org/opensuse/tumbleweed:latest
        pkg=$(find_one '*.rpm')
        install="zypper --non-interactive --quiet install --allow-unsigned-rpm \
            \"/b/$pkg\" xorg-x11-server-Xvfb shadow"
        ;;
    arch)
        image=docker.io/library/archlinux:latest
        pkg=$(find_one '*.AppImage')
        install="pacman -Syu --noconfirm --needed -q sudo xorg-server-xvfb gtk3 mesa >/dev/null"
        ;;
    *) usage ;;
esac

# Runs inside the container as root. $1 = package file name, $2 = launch seconds.
read -r -d '' check <<'EOF' || true
set -euo pipefail
pkg=$1 launch_seconds=$2
want=$(cat /src/vendor/linux_util.VERSION)

if [[ "$pkg" == *.AppImage ]]; then
    install -m 0755 "/b/$pkg" /opt/app.AppImage
    app=(/opt/app.AppImage)
    # No FUSE in a container; the AppImage runtime unpacks itself instead.
    export APPIMAGE_EXTRACT_AND_RUN=1
    root=$(cd /tmp && /opt/app.AppImage --appimage-extract >/dev/null && echo /tmp/squashfs-root)
    vendor="$root/usr/lib/Linux_Util Desktop/vendor"
    desktop=$(ls "$root"/*.desktop)
else
    app=(linux_util_desktop)
    vendor="/usr/lib/Linux_Util Desktop/vendor"
    desktop="/usr/share/applications/Linux_Util Desktop.desktop"
fi

[[ -x "$vendor/linux_util.sh" ]] || { echo "FAIL: $vendor/linux_util.sh missing"; exit 1; }
got=$(cat "$vendor/linux_util.VERSION")
[[ "$got" == "$want" ]] || { echo "FAIL: bundled linux_util $got, expected $want"; exit 1; }
grep -q '^Exec=linux_util_desktop' "$desktop" || { echo "FAIL: bad desktop entry $desktop"; exit 1; }
echo "ok: bundled linux_util $got, desktop entry $(basename "$desktop")"

id tester >/dev/null 2>&1 || useradd -m tester
Xvfb :99 -screen 0 1280x800x24 >/dev/null 2>&1 &
sleep 2
log=/tmp/app.log
su tester -c "cd ~ && DISPLAY=:99 APPIMAGE_EXTRACT_AND_RUN=1 ${app[*]}" >"$log" 2>&1 &
pid=$!
for _ in $(seq "$launch_seconds"); do
    if ! kill -0 "$pid" 2>/dev/null; then
        wait "$pid" && rc=0 || rc=$?
        echo "FAIL: app exited after start (status $rc). Output:"
        cat "$log"
        exit 1
    fi
    sleep 1
done
echo "ok: app still running after ${launch_seconds}s"
[[ -s "$log" ]] && { echo "--- app output ---"; cat "$log"; }
exit 0
EOF

echo "== $distro: $pkg on $image"
"$engine" run --rm --security-opt label=disable \
    -v "$bundles":/b:ro \
    -v "$(cd "$(dirname "$0")/.." && pwd)":/src:ro \
    -e SMOKE_CHECK="$check" \
    "$image" bash -c "$install && bash -c \"\$SMOKE_CHECK\" check \"$pkg\" $launch_seconds" \
    </dev/null 2>&1 | sed 's/^/  /'
