#!/bin/sh
#
# apg — program graph scanner + LadybugDB query CLI
#
# Linux installer. Downloads a prebuilt apg binary plus the scanner frontends
# from a GitHub release, verifies the tarball checksum, and installs them.
#
#   # system-wide (requires root):
#   curl -fsSL https://raw.githubusercontent.com/antz29/apg/main/install.sh | sudo sh -s --
#
#   # user-level, no root:
#   curl -fsSL https://raw.githubusercontent.com/antz29/apg/main/install.sh | sh -s -- --user
#
# Layout (mirrors the brew formula): the real binary lives in
# $prefix/libexec/apg/ with frontends/ beside it, and $prefix/bin/apg is a
# small wrapper that points the binary at them via APG_FRONTEND_DIR. This is
# self-contained and survives upgrades (each install rewrites libexec/apg).
#
# For testing against a local/mirrored release, set APG_INSTALL_BASE_URL to the
# base URL of the release (must pair with --version, since "latest" is resolved
# via the GitHub API).

set -eu

REPO="antz29/apg"
GITHUB="https://github.com/${REPO}"
API="https://api.github.com/repos/${REPO}"

version="latest"
prefix=""
user_install=0
uninstall=0
force=0
verify=1

usage() {
    cat <<'EOF'
Usage: install.sh [options]

Installs apg for Linux from the latest (or a pinned) GitHub release.

Options:
  --version V     Install a specific release tag, e.g. --version 0.5.1
  --user          Install under ~/.local (no root required)
  --prefix DIR    Install under DIR instead of /usr/local
  --uninstall     Remove an existing install from the target prefix
  --force         Overwrite an existing install without prompting
  --no-verify     Skip sha256 verification (unsafe; debugging only)
  -h, --help      Show this help

Examples:
  curl -fsSL https://raw.githubusercontent.com/antz29/apg/main/install.sh | sudo sh -s --
  curl -fsSL https://raw.githubusercontent.com/antz29/apg/main/install.sh | sh -s -- --user
EOF
}

die() {
    echo "install.sh: $*" >&2
    exit 1
}

have() {
    command -v "$1" >/dev/null 2>&1
}

while [ $# -gt 0 ]; do
    case "$1" in
        --version)
            [ $# -ge 2 ] || die "--version requires an argument"
            version="$2"
            shift 2
            ;;
        --prefix)
            [ $# -ge 2 ] || die "--prefix requires an argument"
            prefix="$2"
            shift 2
            ;;
        --user) user_install=1; shift ;;
        --uninstall) uninstall=1; shift ;;
        --force) force=1; shift ;;
        --no-verify) verify=0; shift ;;
        -h | --help) usage; exit 0 ;;
        *) die "unknown option: $1 (see --help)" ;;
    esac
done

# Optional override for testing against a local release mirror.
if [ -n "${APG_INSTALL_BASE_URL:-}" ]; then
    GITHUB="$APG_INSTALL_BASE_URL"
    API=""
    [ "$version" != "latest" ] || die "APG_INSTALL_BASE_URL is set: pass --version (latest is resolved via the GitHub API)"
fi

have curl || die "curl is required (curl not found on PATH)"

os="$(uname -s)"
# Diagnostic/test hooks — force the platform instead of probing uname (use for
# testing from a non-Linux host; harmless when unset).
[ -n "${APG_INSTALL_TEST_OS:-}" ] && os="$APG_INSTALL_TEST_OS"
case "$os" in
    Linux) : ;;
    Darwin)
        die "macOS is not supported by this installer — use Homebrew instead: brew tap antz29/apg && brew install antz29/apg/scanner"
        ;;
    *) die "unsupported OS: ${os} (this installer targets Linux)" ;;
esac

mach="$(uname -m)"
[ -n "${APG_INSTALL_TEST_ARCH:-}" ] && mach="$APG_INSTALL_TEST_ARCH"
case "$mach" in
    x86_64 | amd64) arch="x86_64" ;;
    aarch64 | arm64) arch="aarch64" ;;
    *) die "unsupported architecture: ${mach} (need x86_64 or aarch64)" ;;
esac

if [ "$user_install" -eq 1 ]; then
    [ -z "$prefix" ] || die "--user and --prefix are mutually exclusive"
    prefix="${HOME}/.local"
fi
[ -n "$prefix" ] || prefix="/usr/local"

bin_dir="${prefix}/bin"
real_dir="${prefix}/libexec/apg"
real_bin="${real_dir}/apg"
frontends_dir="${real_dir}/frontends"
wrapper="${bin_dir}/apg"

# --- uninstall -------------------------------------------------------------

if [ "$uninstall" -eq 1 ]; then
    rm -rf "$real_dir"
    rm -f "$wrapper"
    rmdir "$real_dir" 2>/dev/null || true
    rmdir "$bin_dir" 2>/dev/null || true
    echo "apg removed from ${prefix}."
    exit 0
fi

# --- resolve version -------------------------------------------------------

case "$version" in
    v*) release="$version" ;;
    *) release="v${version}" ;;
esac

if [ -n "$API" ] && [ "$version" = "latest" ]; then
    echo "Resolving latest apg release from GitHub..."
    api_json="$(curl -fsSL "${API}/releases/latest")" ||
        die "could not fetch latest release info from ${API}/releases/latest"
    version="$(printf '%s' "$api_json" |
        sed -n 's/.*"tag_name"[[:space:]]*:[[:space:]]*"\([^"]*\)".*/\1/p' |
        head -n 1)"
    [ -n "$version" ] || die "could not parse latest release tag from GitHub API"
    release="$version"
fi

tarball="apg-linux-${arch}.tar.gz"
release_url="${GITHUB}/releases/download/${release}"

# --- download + verify -----------------------------------------------------

work="$(mktemp -d "${TMPDIR:-/tmp}/apg-install.XXXXXX")"
trap 'rm -rf "$work"' EXIT

echo "Downloading ${release}/${tarball} from ${GITHUB}..."

expected=""
if [ "$verify" -eq 1 ]; then
    curl -fsSL -o "$work/sha256sums.txt" "${release_url}/sha256sums.txt" ||
        die "could not download sha256sums.txt from ${release_url}"
    expected="$(awk -v f="$tarball" '$2 == f || $3 == f { print $1 }' "$work/sha256sums.txt" | head -n 1)"
    [ -n "$expected" ] || die "no checksum for ${tarball} in sha256sums.txt"
fi

curl -fsSL -o "$work/$tarball" "${release_url}/${tarball}" ||
    die "could not download ${release_url}/${tarball}"

if [ "$verify" -eq 1 ]; then
    have sha256sum || die "sha256sum is required for verification (or re-run with --no-verify)"
    actual="$(sha256sum "$work/$tarball" | awk '{ print $1 }')"
    [ "$actual" = "$expected" ] ||
        die "checksum mismatch for ${tarball}:\n  expected ${expected}\n  got      ${actual}"
    echo "Checksum OK (sha256 $(printf '%s' "$actual" | cut -c1-12)...)."
fi

mkdir -p "$work/extract"
tar -xzf "$work/$tarball" -C "$work/extract"
[ -x "$work/extract/apg" ] || die "${tarball} does not contain an executable 'apg'"
[ -d "$work/extract/frontends" ] || die "${tarball} does not contain a 'frontends/' directory"

# --- install ---------------------------------------------------------------

if ! mkdir -p "$bin_dir" "$real_dir" 2>/dev/null; then
    die "cannot write to ${prefix} — re-run with sudo ('curl ... | sudo sh -s --') or use --user"
fi

if [ "$force" -eq 0 ] && { [ -e "$wrapper" ] || [ -e "$real_dir" ]; }; then
    echo "apg appears to already be installed at ${prefix}." >&2
    printf 'Replace it? [y/N] ' >&2
    answer=""
    read -r answer </dev/tty 2>/dev/null || answer=""
    case "$answer" in
        y | Y | yes | YES) : ;;
        *)
            echo "Aborting. Re-run with --force to overwrite, or --uninstall to remove." >&2
            exit 1
            ;;
    esac
fi

rm -rf "$real_dir"
mkdir -p "$real_dir" "$bin_dir"
install -m 0755 "$work/extract/apg" "$real_bin"
cp -r "$work/extract/frontends" "$real_dir/"

{
    printf '#!/bin/sh\n'
    printf 'export APG_FRONTEND_DIR="%s"\n' "$frontends_dir"
    printf 'exec "%s" "$@"\n' "$real_bin"
} >"$wrapper"
chmod 0755 "$wrapper"

# --- post-install checks ---------------------------------------------------

libssl="$( { ldconfig -p 2>/dev/null || /sbin/ldconfig -p 2>/dev/null; } |
    grep 'libssl\.so\.3' | head -n 1 || true)"
if [ -z "$libssl" ]; then
    echo "warning: libssl.so.3 not found on this system." >&2
    echo "apg links OpenSSL dynamically; install it, e.g.:" >&2
    echo "  apt install libssl3      # Debian/Ubuntu" >&2
    echo "  dnf install openssl-libs # Fedora" >&2
fi

if ! "$wrapper" --version >/dev/null 2>&1; then
    echo "warning: installed apg did not run (${wrapper} --version failed)." >&2
    echo "Check for missing shared libraries, e.g. 'ldd ${real_bin}'." >&2
fi

cat <<EOF

apg ${release} installed to ${prefix}.

  ${real_bin}
  ${frontends_dir}
  ${wrapper}

Next steps:
  - ensure ${bin_dir} is on your PATH
  - run 'apg init' in a project, then 'apg scan'
  - apg ships a suite of opencode tools and a codebase-navigator agent
    (installed per-project by 'apg init')
EOF
