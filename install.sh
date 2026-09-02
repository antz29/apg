#!/bin/sh
#
# apg — program graph scanner + LadybugDB query CLI
#
# Linux installer. Downloads prebuilt apg binaries and language frontends
# from GitHub releases, verifies tarball checksums, and installs them.
#
# Components can be installed together or separately (matching Homebrew):
#
#   # Install core scanner only (no heavy frontends):
#   curl -fsSL https://raw.githubusercontent.com/antz29/apg/main/install.sh | sudo sh -s --
#   curl -fsSL https://raw.githubusercontent.com/antz29/apg/main/install.sh | sh -s -- --user
#
#   # Install specific frontends (installs scanner too if missing):
#   curl -fsSL https://raw.githubusercontent.com/antz29/apg/main/install.sh | sh -s -- --user go rust
#   curl -fsSL https://raw.githubusercontent.com/antz29/apg/main/install.sh | sh -s -- --user --frontends go,rust
#
#   # Install everything (scanner + all 6 frontends):
#   curl -fsSL https://raw.githubusercontent.com/antz29/apg/main/install.sh | sh -s -- --user all
#
# Layout (mirrors the brew formula): the real binary lives in
# $prefix/libexec/apg/ with frontends/ beside it, and $prefix/bin/apg is a
# small wrapper that points the binary at them via APG_FRONTEND_DIR.
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
raw_frontends=""
install_all=0
components=""

ALL_FRONTENDS="cpp go rust csharp java ts"

usage() {
    cat <<'EOF'
Usage: install.sh [options] [component...]

Installs apg and scanner frontends for Linux from GitHub releases.
Components can be installed individually or all together.

Components:
  scanner         The core apg CLI (default if none specified)
  go              Go scanner frontend (gofrontend)
  rust            Rust scanner frontend (rustfrontend)
  cpp             C++ scanner frontend (cppfrontend)
  csharp          C# scanner frontend (csharpfrontend)
  java            Java scanner frontend (java-classes)
  ts              TypeScript scanner frontend (tsfrontend)
  all             Core scanner + all frontends

Options:
  --frontends L,L Comma-separated frontends to install (e.g. --frontends go,rust)
  --all           Install core scanner and all frontends
  --version V     Install a specific release tag, e.g. --version 0.7.1
  --user          Install under ~/.local (no root required)
  --prefix DIR    Install under DIR instead of /usr/local
  --uninstall     Remove specified components (or entire install if none specified)
  --force         Overwrite existing files without prompting
  --no-verify     Skip sha256 verification (unsafe; debugging only)
  -h, --help      Show this help

Examples:
  # Install core scanner CLI
  curl -fsSL https://raw.githubusercontent.com/antz29/apg/main/install.sh | sudo sh -s --
  curl -fsSL https://raw.githubusercontent.com/antz29/apg/main/install.sh | sh -s -- --user

  # Install only Go and Rust frontends
  curl -fsSL https://raw.githubusercontent.com/antz29/apg/main/install.sh | sh -s -- --user go rust

  # Install everything
  curl -fsSL https://raw.githubusercontent.com/antz29/apg/main/install.sh | sh -s -- --user all
EOF
}

die() {
    echo "install.sh: $*" >&2
    exit 1
}

have() {
    command -v "$1" >/dev/null 2>&1
}

normalize_component() {
    case "$1" in
        scanner | apg) echo "scanner" ;;
        go | apg-go) echo "go" ;;
        rust | apg-rust) echo "rust" ;;
        cpp | c++ | apg-cpp) echo "cpp" ;;
        csharp | cs | "c#" | apg-csharp) echo "csharp" ;;
        java | apg-java) echo "java" ;;
        ts | typescript | apg-ts) echo "ts" ;;
        all) echo "all" ;;
        *) die "unknown component: $1 (valid: scanner, go, rust, cpp, csharp, java, ts, all)" ;;
    esac
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
        --frontends)
            [ $# -ge 2 ] || die "--frontends requires an argument"
            raw_frontends="$2"
            shift 2
            ;;
        --all) install_all=1; shift ;;
        --user) user_install=1; shift ;;
        --uninstall) uninstall=1; shift ;;
        --force) force=1; shift ;;
        --no-verify) verify=0; shift ;;
        -h | --help) usage; exit 0 ;;
        -*) die "unknown option: $1 (see --help)" ;;
        *)
            norm="$(normalize_component "$1")"
            components="${components}${components:+ }${norm}"
            shift
            ;;
    esac
done

if [ -n "$raw_frontends" ]; then
    old_ifs="$IFS"
    IFS=','
    for f in $raw_frontends; do
        norm="$(normalize_component "$f")"
        components="${components}${components:+ }${norm}"
    done
    IFS="$old_ifs"
fi

if [ "$install_all" -eq 1 ]; then
    components="all"
fi

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
    if [ -z "$components" ] || [ "$components" = "all" ]; then
        rm -rf "$real_dir"
        rm -f "$wrapper"
        rmdir "$bin_dir" 2>/dev/null || true
        echo "apg completely removed from ${prefix}."
        exit 0
    fi

    for c in $components; do
        case "$c" in
            scanner)
                rm -f "$real_bin" "$wrapper"
                echo "Removed scanner from ${prefix}."
                ;;
            cpp)
                rm -f "$frontends_dir/cppfrontend"
                echo "Removed C++ frontend."
                ;;
            go)
                rm -f "$frontends_dir/gofrontend"
                echo "Removed Go frontend."
                ;;
            rust)
                rm -f "$frontends_dir/rustfrontend"
                echo "Removed Rust frontend."
                ;;
            csharp)
                rm -f "$frontends_dir/csharpfrontend" "$frontends_dir/csharpfrontend.exe"
                echo "Removed C# frontend."
                ;;
            java)
                rm -rf "$frontends_dir/java-classes"
                echo "Removed Java frontend."
                ;;
            ts)
                rm -rf "$frontends_dir/tsfrontend"
                echo "Removed TypeScript frontend."
                ;;
        esac
    done
    exit 0
fi

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

# Expand components list
target_scanner=0
target_frontends=""

if [ -z "$components" ]; then
    # Default is just the core scanner
    target_scanner=1
elif [ "$components" = "all" ]; then
    target_scanner=1
    target_frontends="$ALL_FRONTENDS"
else
    for c in $components; do
        if [ "$c" = "scanner" ]; then
            target_scanner=1
        elif [ "$c" = "all" ]; then
            target_scanner=1
            target_frontends="$ALL_FRONTENDS"
        else
            # If frontend specified, and neither in target_frontends yet
            case " $target_frontends " in
                *" $c "*) ;;
                *) target_frontends="${target_frontends}${target_frontends:+ }$c" ;;
            esac
        fi
    done
fi

# If installing frontends and scanner binary is not installed yet, also install scanner
if [ -n "$target_frontends" ] && [ ! -x "$real_bin" ]; then
    target_scanner=1
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

if [ -n "${APG_INSTALL_BASE_URL:-}" ]; then
    release_url="${APG_INSTALL_BASE_URL}"
else
    release_url="${GITHUB}/releases/download/${release}"
fi

# --- download + verify helpers ---------------------------------------------

work="$(mktemp -d "${TMPDIR:-/tmp}/apg-install.XXXXXX")"
trap 'rm -rf "$work"' EXIT

if [ "$verify" -eq 1 ]; then
    curl -fsSL -o "$work/sha256sums.txt" "${release_url}/sha256sums.txt" ||
        die "could not download sha256sums.txt from ${release_url}"
fi

fetch_and_verify() {
    tarball="$1"
    echo "Downloading ${release}/${tarball} from ${GITHUB}..."
    curl -fsSL -o "$work/$tarball" "${release_url}/${tarball}" ||
        die "could not download ${release_url}/${tarball}"

    if [ "$verify" -eq 1 ]; then
        have sha256sum || die "sha256sum is required for verification (or re-run with --no-verify)"
        expected="$(awk -v f="$tarball" '$2 == f || $3 == f { print $1 }' "$work/sha256sums.txt" | head -n 1)"
        [ -n "$expected" ] || die "no checksum for ${tarball} in sha256sums.txt"
        actual="$(sha256sum "$work/$tarball" | awk '{ print $1 }')"
        [ "$actual" = "$expected" ] ||
            die "checksum mismatch for ${tarball}:\n  expected ${expected}\n  got      ${actual}"
        echo "Checksum OK for ${tarball} (sha256 $(printf '%s' "$actual" | cut -c1-12)...)."
    fi
}

# --- prepare directories ---------------------------------------------------

if ! mkdir -p "$bin_dir" "$real_dir" "$frontends_dir" 2>/dev/null; then
    die "cannot write to ${prefix} — re-run with sudo ('curl ... | sudo sh -s --') or use --user"
fi

# --- install scanner -------------------------------------------------------

installed_components=""

if [ "$target_scanner" -eq 1 ]; then
    if [ "$force" -eq 0 ] && [ -e "$real_bin" ]; then
        echo "apg scanner is already installed at ${real_bin}." >&2
        printf 'Overwrite scanner binary? [y/N] ' >&2
        answer=""
        read -r answer </dev/tty 2>/dev/null || answer=""
        case "$answer" in
            y | Y | yes | YES) : ;;
            *)
                echo "Skipping scanner binary installation." >&2
                target_scanner=0
                ;;
        esac
    fi
fi

if [ "$target_scanner" -eq 1 ]; then
    tarball="apg-linux-${arch}.tar.gz"
    fetch_and_verify "$tarball"

    mkdir -p "$work/extract_scanner"
    tar -xzf "$work/$tarball" -C "$work/extract_scanner"
    [ -x "$work/extract_scanner/apg" ] || die "${tarball} does not contain an executable 'apg'"

    install -m 0755 "$work/extract_scanner/apg" "$real_bin"
    {
        printf '#!/bin/sh\n'
        printf 'export APG_FRONTEND_DIR="%s"\n' "$frontends_dir"
        printf 'exec "%s" "$@"\n' "$real_bin"
    } >"$wrapper"
    chmod 0755 "$wrapper"

    installed_components="${installed_components}${installed_components:+, }scanner"
fi

# --- install frontends -----------------------------------------------------

for f in $target_frontends; do
    tarball="apg-${f}-linux-${arch}.tar.gz"
    fetch_and_verify "$tarball"

    mkdir -p "$work/extract_${f}"
    tar -xzf "$work/$tarball" -C "$work/extract_${f}"

    case "$f" in
        cpp)
            [ -x "$work/extract_${f}/cppfrontend" ] || die "${tarball} does not contain 'cppfrontend'"
            install -m 0755 "$work/extract_${f}/cppfrontend" "$frontends_dir/cppfrontend"
            ;;
        go)
            [ -x "$work/extract_${f}/gofrontend" ] || die "${tarball} does not contain 'gofrontend'"
            install -m 0755 "$work/extract_${f}/gofrontend" "$frontends_dir/gofrontend"
            ;;
        rust)
            [ -x "$work/extract_${f}/rustfrontend" ] || die "${tarball} does not contain 'rustfrontend'"
            install -m 0755 "$work/extract_${f}/rustfrontend" "$frontends_dir/rustfrontend"
            ;;
        csharp)
            [ -x "$work/extract_${f}/csharpfrontend" ] || die "${tarball} does not contain 'csharpfrontend'"
            install -m 0755 "$work/extract_${f}/csharpfrontend" "$frontends_dir/csharpfrontend"
            ;;
        java)
            [ -d "$work/extract_${f}/java-classes" ] || die "${tarball} does not contain 'java-classes/'"
            rm -rf "$frontends_dir/java-classes"
            cp -r "$work/extract_${f}/java-classes" "$frontends_dir/"
            ;;
        ts)
            [ -d "$work/extract_${f}/tsfrontend" ] || die "${tarball} does not contain 'tsfrontend/'"
            rm -rf "$frontends_dir/tsfrontend"
            cp -r "$work/extract_${f}/tsfrontend" "$frontends_dir/"
            ;;
    esac

    installed_components="${installed_components}${installed_components:+, }${f}"
done

# --- post-install checks ---------------------------------------------------

if [ "$target_scanner" -eq 1 ] || [ -e "$wrapper" ]; then
    libssl="$( { ldconfig -p 2>/dev/null || /sbin/ldconfig -p 2>/dev/null; } |
        grep 'libssl\.so\.3' | head -n 1 || true)"
    if [ -z "$libssl" ]; then
        echo "warning: libssl.so.3 not found on this system." >&2
        echo "apg links OpenSSL dynamically; install it, e.g.:" >&2
        echo "  apt install libssl3      # Debian/Ubuntu" >&2
        echo "  dnf install openssl-libs # Fedora" >&2
    fi

    if [ -x "$wrapper" ] && ! "$wrapper" --version >/dev/null 2>&1; then
        echo "warning: installed apg did not run (${wrapper} --version failed)." >&2
        echo "Check for missing shared libraries, e.g. 'ldd ${real_bin}'." >&2
    fi
fi

# Check available frontends in frontends_dir
installed_langs=""
missing_langs=""
for lang in $ALL_FRONTENDS; do
    present=0
    case "$lang" in
        cpp) [ -x "$frontends_dir/cppfrontend" ] && present=1 ;;
        go) [ -x "$frontends_dir/gofrontend" ] && present=1 ;;
        rust) [ -x "$frontends_dir/rustfrontend" ] && present=1 ;;
        csharp) { [ -x "$frontends_dir/csharpfrontend" ] || [ -x "$frontends_dir/csharpfrontend.exe" ]; } && present=1 ;;
        java) [ -d "$frontends_dir/java-classes" ] && present=1 ;;
        ts) [ -d "$frontends_dir/tsfrontend" ] && present=1 ;;
    esac
    if [ "$present" -eq 1 ]; then
        installed_langs="${installed_langs}${installed_langs:+ }$lang"
    else
        missing_langs="${missing_langs}${missing_langs:+ }$lang"
    fi
done

# Language-specific warnings
case " $installed_langs " in
    *" java "*)
        if ! have java; then
            echo "note: Java frontend installed, but 'java' was not found on PATH." >&2
            echo "Scanning Java projects requires a Java runtime (JRE/JDK 17+)." >&2
        fi
        ;;
esac
case " $installed_langs " in
    *" ts "*)
        if ! have node; then
            echo "note: TypeScript frontend installed, but 'node' was not found on PATH." >&2
            echo "Scanning TypeScript projects requires Node.js." >&2
        fi
        ;;
esac

cat <<EOF

apg ${release} (${installed_components:-no changes}) installed to ${prefix}.

  Binary:    ${real_bin}
  Wrapper:   ${wrapper}
  Frontends: ${frontends_dir}
             Installed: ${installed_langs:-none}
EOF

if [ -n "$missing_langs" ]; then
    cat <<EOF
             Available: ${missing_langs}

To install additional frontends:
  curl -fsSL https://raw.githubusercontent.com/antz29/apg/main/install.sh | sh -s -- $([ "$user_install" -eq 1 ] && echo "--user ")$(echo "$missing_langs" | tr ' ' ' ')
EOF
fi

cat <<EOF

Next steps:
  - ensure ${bin_dir} is on your PATH
  - run 'apg init' in a project, then 'apg scan'
  - apg ships a suite of opencode tools and a codebase-navigator agent
    (installed per-project by 'apg init')
EOF
