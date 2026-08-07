#!/bin/sh
# calisto — instalador (Fase Q.2): baixa o release, verifica sha256 e
# instala em ~/.calisto (CALISTO_HOME); shim em ~/.local/bin/calisto.
#
# Uso:
#   curl -fsSL https://github.com/felipebrgs1/ruby-rust/releases/latest/download/install.sh | sh
#
# Env overrides:
#   CALISTO_VERSION   versao a instalar (default: latest)
#   CALISTO_BASE      base de download (default: releases/download do repo)
#   CALISTO_HOME      dir de instalacao (default: ~/.calisto)
#   CALISTO_BIN_DIR   dir do shim (default: ~/.local/bin)
set -eu

BASE="${CALISTO_BASE:-https://github.com/felipebrgs1/ruby-rust/releases/download}"
HOME_DIR="${CALISTO_HOME:-$HOME/.calisto}"
BIN_DIR="${CALISTO_BIN_DIR:-$HOME/.local/bin}"
ARCH="$(uname -m)"
case "$ARCH" in
  x86_64) ;;
  *) echo "calisto: arquitetura $ARCH nao suportada (x86_64 apenas)" >&2; exit 1 ;;
esac
[ "$(uname -s)" = Linux ] || { echo "calisto: Linux apenas (fork)" >&2; exit 1; }

if [ -n "${CALISTO_VERSION:-}" ]; then
  VERSION="$CALISTO_VERSION"
  BASE_URL="$BASE/v$VERSION"
else
  # latest: o GitHub redireciona releases/latest/download/...
  BASE_URL="$BASE/latest/download"
fi

NAME="calisto-linux-$ARCH"
URL="$BASE_URL/$NAME.tar.gz"
TMP="$(mktemp -d)"
trap 'rm -rf "$TMP"' EXIT

echo "calisto: baixando $URL"
curl -fsSL -o "$TMP/$NAME.tar.gz" "$URL"
curl -fsSL -o "$TMP/$NAME.tar.gz.sha256" "$URL.sha256"

echo "calisto: verificando sha256"
( cd "$TMP" && sha256sum -c "$NAME.tar.gz.sha256" )

mkdir -p "$HOME_DIR" "$BIN_DIR"
echo "calisto: extraindo em $HOME_DIR"
tar -xzf "$TMP/$NAME.tar.gz" -C "$HOME_DIR" --strip-components=1 "$NAME"

cat > "$BIN_DIR/calisto" <<EOF
#!/bin/sh
# calisto shim (gerado pelo instalador) — o binario acha o vendor subindo
# do proprio caminho; CALISTO_HOME garante a base mesmo com o shim.
export CALISTO_HOME="$HOME_DIR"
exec "$HOME_DIR/bin/calisto" "\$@"
EOF
chmod +x "$BIN_DIR/calisto"

echo "calisto: instalado em $HOME_DIR (shim: $BIN_DIR/calisto)"
echo "calisto: adicione $BIN_DIR ao PATH e rode: calisto --version"
