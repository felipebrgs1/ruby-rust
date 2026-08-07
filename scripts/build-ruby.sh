#!/usr/bin/env bash
#
# Builds a pinned, self-contained CRuby into vendor/.
#
#   - Pin: RUBY_VERSION (default 3.4.10), verified against SHA-256.
#   - Vendors libyaml when the system has no yaml-0.1 (needed by stdlib psych/yaml).
#   - Installs into vendor/ruby-<version>/ and symlinks vendor/current -> the
#     DEFAULT pin (3.4.10). Building uma versao extra (Fase I: multi-versoes)
#     nao troca o vendor/current — o calisto seleciona por .ruby-version/Gemfile.
#
# Usage:
#   scripts/build-ruby.sh            # build pinned 3.4.10
#   RUBY_VERSION=3.4.4 scripts/build-ruby.sh   # build 3.4.4 (sha conhecido)
#   RUBY_VERSION=3.4.0 RUBY_SHA256=... scripts/build-ruby.sh  # qualquer versao
#
# Requirements: gcc, make, pkg-config, autoconf, bison, curl, tar, xz.
set -euo pipefail

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
VENDOR="$ROOT/vendor"
SRC="$VENDOR/src"
JOBS="${JOBS:-$(nproc)}"

DEFAULT_RUBY_VERSION="${DEFAULT_RUBY_VERSION:-3.4.10}"
RUBY_VERSION="${RUBY_VERSION:-3.4.10}"
# sha256 conhecidos (o calisto ainda nao tem instalador proprio); sobrescreva
# com RUBY_SHA256 para versoes fora desta lista.
case "$RUBY_VERSION" in
  3.4.10) RUBY_SHA256="${RUBY_SHA256:-ecee2d072a14f2d14347dd56dfd8fe5c3130abf5117bfaacbda0f4ef9cc429ec}" ;;
  3.4.4)  RUBY_SHA256="${RUBY_SHA256:-a0597bfdf312e010efd1effaa8d7f1d7833146fdc17950caa8158ffa3dcbfa85}" ;;
esac
RUBY_URL="https://cache.ruby-lang.org/pub/ruby/3.4/ruby-${RUBY_VERSION}.tar.gz"

LIBYAML_VERSION="0.2.5"
LIBYAML_SHA256="c642ae9b75fee120b2d96c712538bd2cf283228d2337df2cf2988e3c02678ef4"
LIBYAML_URL="https://github.com/yaml/libyaml/releases/download/${LIBYAML_VERSION}/yaml-${LIBYAML_VERSION}.tar.gz"

PREFIX="$VENDOR/ruby-$RUBY_VERSION"
mkdir -p "$SRC"

log() { printf '\033[1;34m[calisto]\033[0m %s\n' "$*"; }
fail() { printf '\033[1;31m[calisto]\033[0m error: %s\n' "$*" >&2; exit 1; }

# --- downloads (idempotent; verifies sha256) ---------------------------------
fetch() { # url, file, sha256
  local url="$1" file="$2" sha="$3"
  if [[ -f "$file" ]]; then
    local got; got="$(sha256sum "$file" | cut -d' ' -f1)"
    [[ "$got" == "$sha" ]] || fail "checksum mismatch for $file (got $got, want $sha)"
    log "cached: $file"
  else
    log "download: $url"
    curl -fsSL --retry 3 -o "$file" "$url"
    local got; got="$(sha256sum "$file" | cut -d' ' -f1)"
    [[ "$got" == "$sha" ]] || fail "checksum mismatch for $file (got $got, want $sha)"
  fi
}

# --- vendored libyaml (only if system lacks yaml-0.1) -------------------------
if pkg-config --exists yaml-0.1 2>/dev/null; then
  log "using system libyaml (yaml-0.1)"
  export PKG_CONFIG_PATH="${PKG_CONFIG_PATH:-}"
else
  log "system libyaml missing -> vendoring libyaml $LIBYAML_VERSION"
  fetch "$LIBYAML_URL" "$SRC/yaml-${LIBYAML_VERSION}.tar.gz" "$LIBYAML_SHA256"
  local_yaml="$(find "$SRC" -maxdepth 1 -type d -name 'yaml-*' -print -quit)"
  if [[ -z "$local_yaml" || ! -x "$local_yaml/install/bin/yaml-config" ]]; then
    [[ -z "$local_yaml" ]] || rm -rf "$local_yaml"
    tar -xzf "$SRC/yaml-${LIBYAML_VERSION}.tar.gz" -C "$SRC"
    local_yaml="$(find "$SRC" -maxdepth 1 -type d -name 'yaml-*' -print -quit)"
    ( cd "$local_yaml" \
      && ./configure --prefix="$local_yaml/install" --disable-shared --enable-static --quiet \
      && make -j"$JOBS" --quiet \
      && make install --quiet )
  fi
  export PKG_CONFIG_PATH="$local_yaml/install/lib/pkgconfig"
fi

# --- CRuby --------------------------------------------------------------------
RUBY_TARBALL="$SRC/ruby-${RUBY_VERSION}.tar.gz"
fetch "$RUBY_URL" "$RUBY_TARBALL" "$RUBY_SHA256"

if [[ -x "$PREFIX/bin/ruby" ]]; then
  log "already built: $PREFIX/bin/ruby"
else
  log "extracting ruby $RUBY_VERSION"
  rm -rf "$SRC/ruby-$RUBY_VERSION"
  tar -xzf "$RUBY_TARBALL" -C "$SRC"

  log "configuring (prefix=$PREFIX, jobs=$JOBS)"
  ( cd "$SRC/ruby-$RUBY_VERSION" \
    && ./configure \
        --prefix="$PREFIX" \
        --disable-install-doc \
        --quiet )

  log "building (make -j$JOBS)"
  ( cd "$SRC/ruby-$RUBY_VERSION" && make -j"$JOBS" --quiet )

  log "installing"
  ( cd "$SRC/ruby-$RUBY_VERSION" && make install --quiet )
fi

# vendor/current e o DEFAULT (pin): so e repontado quando o alvo e o default
# ou quando ainda nao existe — construir uma versao extra (Fase I) nao troca
# o default que o calisto usa sem .ruby-version/Gemfile.
if [[ "$RUBY_VERSION" == "$DEFAULT_RUBY_VERSION" || ! -e "$VENDOR/current" ]]; then
  ln -sfn "$PREFIX" "$VENDOR/current"
  log "current -> ruby-$RUBY_VERSION (default)"
fi
log "done: $PREFIX"
"$PREFIX/bin/ruby" -v
