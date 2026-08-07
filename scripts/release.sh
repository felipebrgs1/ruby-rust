#!/bin/bash
# calisto — release (Fase Q): monta o tarball de distribuicao.
#
# Uso: scripts/release.sh [VERSION]
#   VERSION (default: versao do Cargo.toml) — tag do release no GitHub.
#
# Produz em dist/:
#   calisto-linux-x86_64.tar.gz        — binario + vendor/ (rubies empacotados)
#   calisto-ruby-<v>-linux-x86_64.tar.gz — ruby <v> isolado (upgrade baixa
#                                          estes; layout: ruby-<v>/ na raiz,
#                                          extraido em <vendor>/)
#   *.sha256                            — verificacao do instalador/upgrade
#
# O binario acha o vendor subindo do proprio caminho (vendor_root), entao o
# tarball traz bin/calisto + vendor/ lado a lado. Relocavel por design (a
# Fase L dlopen a libruby do vendor pela localizacao da propria .so).
set -euo pipefail
ROOT="$(cd "$(dirname "$0")/.." && pwd)"
cd "$ROOT"

VERSION="${1:-$(grep -m1 '^version' Cargo.toml | sed 's/.*= *"\(.*\)".*/\1/')}"
ARCH="$(uname -m)"
NAME="calisto-linux-${ARCH}"
OUT="dist/$NAME"

log() { printf '\033[1;32mcalisto release:\033[0m %s\n' "$*"; }

log "build release ($VERSION)"
cargo build --release

log "montando $OUT"
rm -rf "$OUT" dist/*.tar.gz dist/*.sha256
mkdir -p "$OUT/bin"
cp target/release/calisto "$OUT/bin/calisto"
# rubies empacotados: todos os vendor/ruby-<v> + o symlink current
if [ -d vendor/current ]; then
  mkdir -p "$OUT/vendor"
  cp -a vendor/current "$OUT/vendor/"
  for d in vendor/ruby-*; do
    [ -d "$d" ] && cp -a "$d" "$OUT/vendor/"
  done
else
  log "aviso: vendor/ ausente — rode scripts/build-ruby.sh primeiro"
fi

log "tarballs + sha256"
tar -czf "dist/$NAME.tar.gz" -C dist "$NAME"
(
  cd dist
  for d in "$NAME"/vendor/ruby-*; do
    [ -d "$d" ] || continue
    v="${d#*/vendor/ruby-}"
    tar -czf "calisto-ruby-${v}-linux-${ARCH}.tar.gz" -C "$NAME/vendor" "ruby-${v}"
  done
  sha256sum *.tar.gz > SHA256SUMS
)
# o upgrade baixa "<tarball>.sha256" com "<hash>  <nome>" (formato do sha256sum -c)
for t in dist/*.tar.gz; do
  n="$(basename "$t")"
  grep "  $n$" dist/SHA256SUMS > "dist/$n.sha256"
done

log "pronto:"
ls -la dist/*.tar.gz dist/*.sha256
log "publique com: gh release create v$VERSION dist/*.tar.gz dist/*.sha256"
