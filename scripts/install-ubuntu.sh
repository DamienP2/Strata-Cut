#!/usr/bin/env bash
# Strata Cut — installation sur un serveur Ubuntu (22.04 ou 24.04, 64 bits).
#
# À lancer depuis le dossier du dépôt :   sudo ./scripts/install-ubuntu.sh
# Relancer le même script met à jour une installation existante (les données restent).
#
# Options :
#   --port N              port de l'interface web (défaut 8080)
#   --data-dir DIR        dossier des données (défaut /var/lib/strata-cut)
#   --recordings-dir DIR  dossier des boucles (défaut <données>/loops) — idéalement un disque dédié
#   --clips-dir DIR       dossier des extraits (défaut <données>/clips)
#   --admin NOM           crée ce compte administrateur à la fin (demande le mot de passe)
#   --open-ports LISTE    ports à ouvrir dans le pare-feu ufw, ex. "9000-9010/udp,1935/tcp"
#   --package FICHIER     installe un paquet déjà construit (.tar.gz ou binaire) au lieu de compiler
#   --no-apt              n'installe aucun paquet (dépendances déjà en place)
#   --no-service          n'installe pas le service systemd (essai, conteneur)
#   --uninstall           retire le service et le programme (les données restent)
#   --purge               avec --uninstall : supprime aussi les données et le compte de service
#   -h, --help            cette aide
set -euo pipefail

BRAND="Strata Cut"
INSTALL_DIR="/opt/strata-cut"
BIN="$INSTALL_DIR/strata-cut"
LINK="/usr/local/bin/strata-cut"
SERVICE_USER="strata-cut"
UNIT="strata-cut"
SYSCTL_FILE="/etc/sysctl.d/90-strata-cut.conf"
NODE_MAJOR=22
RUST_MIN="1.87.0"
NODE_MIN="20.19.0"

PORT=8080
DATA_DIR="/var/lib/strata-cut"
RECORDINGS_DIR=""
CLIPS_DIR=""
ADMIN=""
OPEN_PORTS=""
PACKAGE=""
NO_APT=0
NO_SERVICE=0
UNINSTALL=0
PURGE=0

SRC_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"

# ---------------------------------------------------------------- affichage
if [ -t 1 ]; then B=$'\e[1m'; G=$'\e[32m'; Y=$'\e[33m'; R=$'\e[31m'; N=$'\e[0m'; else B=; G=; Y=; R=; N=; fi
step() { printf '\n%s==> %s%s\n' "$B" "$*" "$N"; }
ok() { printf '%s  ✓ %s%s\n' "$G" "$*" "$N"; }
warn() { printf '%s  ! %s%s\n' "$Y" "$*" "$N" >&2; }
die() { printf '%s  ✗ %s%s\n' "$R" "$*" "$N" >&2; exit 1; }
usage() { sed -n '2,22p' "${BASH_SOURCE[0]}" | sed 's/^# \{0,1\}//'; exit 0; }

# ---------------------------------------------------------------- options
while [ $# -gt 0 ]; do
  case "$1" in
    --port) PORT="${2:?}"; shift 2 ;;
    --data-dir) DATA_DIR="${2:?}"; shift 2 ;;
    --recordings-dir) RECORDINGS_DIR="${2:?}"; shift 2 ;;
    --clips-dir) CLIPS_DIR="${2:?}"; shift 2 ;;
    --admin) ADMIN="${2:?}"; shift 2 ;;
    --open-ports) OPEN_PORTS="${2:?}"; shift 2 ;;
    --package) PACKAGE="${2:?}"; shift 2 ;;
    --no-apt) NO_APT=1; shift ;;
    --no-service) NO_SERVICE=1; shift ;;
    --uninstall) UNINSTALL=1; shift ;;
    --purge) PURGE=1; shift ;;
    -h | --help) usage ;;
    *) die "option inconnue : $1 (voir --help)" ;;
  esac
done

[[ "$PORT" =~ ^[0-9]+$ ]] && [ "$PORT" -ge 1 ] && [ "$PORT" -le 65535 ] || die "port invalide : $PORT"
for d in "$DATA_DIR" "$RECORDINGS_DIR" "$CLIPS_DIR"; do
  [ -z "$d" ] || [[ "$d" = /* ]] || die "un dossier s'écrit en chemin absolu : $d"
done
[ -z "$PACKAGE" ] || PACKAGE="$(realpath "$PACKAGE")"
[ "$(id -u)" -eq 0 ] || die "lancez ce script avec sudo : sudo $0 $*"

# Le compte qui a lancé sudo compile (pas root).
BUILD_USER="${SUDO_USER:-root}"
as_builder() {
  if [ "$BUILD_USER" = root ]; then bash -lc "$1"; else sudo -u "$BUILD_USER" -H bash -lc "$1"; fi
}

has_systemd() { [ -d /run/systemd/system ]; }

# Compare deux versions : vrai si $1 >= $2.
version_ge() { [ "$(printf '%s\n%s\n' "$1" "$2" | sort -V | head -1)" = "$2" ]; }

# ---------------------------------------------------------------- désinstallation
if [ "$UNINSTALL" -eq 1 ]; then
  step "Désinstallation de $BRAND"
  if has_systemd && systemctl list-unit-files "$UNIT.service" >/dev/null 2>&1; then
    systemctl disable --now "$UNIT" 2>/dev/null || true
    rm -f "/etc/systemd/system/$UNIT.service"
    systemctl daemon-reload
    ok "service retiré"
  fi
  rm -rf "$INSTALL_DIR" "$LINK" "$SYSCTL_FILE"
  ok "programme retiré ($INSTALL_DIR)"
  if [ "$PURGE" -eq 1 ]; then
    rm -rf "$DATA_DIR"
    id -u "$SERVICE_USER" >/dev/null 2>&1 && userdel "$SERVICE_USER" 2>/dev/null || true
    ok "données supprimées ($DATA_DIR) — les dossiers des boucles et des extraits placés ailleurs restent"
  else
    ok "les données restent dans $DATA_DIR (--purge pour les supprimer)"
  fi
  exit 0
fi

# ---------------------------------------------------------------- 1. système
step "1/8  Vérification du système"
if [ -r /etc/os-release ]; then
  . /etc/os-release
  if [ "${ID:-}" != ubuntu ]; then warn "système $PRETTY_NAME : ce script vise Ubuntu, il peut fonctionner sur Debian"; fi
  case "${VERSION_ID:-}" in 22.04 | 24.04) ok "$PRETTY_NAME" ;; *) warn "$PRETTY_NAME : testé sur Ubuntu 22.04 et 24.04" ;; esac
fi
case "$(uname -m)" in x86_64 | aarch64) ok "processeur $(uname -m)" ;; *) die "processeur $(uname -m) non pris en charge (64 bits requis)" ;; esac
if [ "$NO_SERVICE" -eq 0 ] && ! has_systemd; then die "systemd ne tourne pas sur cette machine (utilisez --no-service pour un essai)"; fi
if [ -z "$PACKAGE" ] && [ ! -f "$SRC_DIR/Cargo.toml" ]; then die "lancez ce script depuis le dépôt Strata Cut ($SRC_DIR ne contient pas Cargo.toml), ou utilisez --package"; fi

# ---------------------------------------------------------------- 2. paquets
step "2/8  Paquets Ubuntu"
apt_install() {
  local missing=()
  for p in "$@"; do dpkg -s "$p" >/dev/null 2>&1 || missing+=("$p"); done
  if [ ${#missing[@]} -gt 0 ]; then
    echo "  installation : ${missing[*]}"
    DEBIAN_FRONTEND=noninteractive apt-get install -y -qq "${missing[@]}" >/dev/null
  fi
}
if [ "$NO_APT" -eq 1 ]; then
  ok "installation des paquets sautée (--no-apt)"
else
  apt-get update -qq
  apt_install ca-certificates curl ffmpeg python3
  if [ -z "$PACKAGE" ]; then apt_install git build-essential pkg-config; fi
  ok "ffmpeg, python3 et outils de base en place"
fi

# FFmpeg doit savoir recevoir et encoder ce que le serveur lui demande.
command -v ffmpeg >/dev/null || die "ffmpeg est introuvable"
command -v ffprobe >/dev/null || die "ffprobe est introuvable"
FF_VERSION="$(ffmpeg -hide_banner -version | head -1 | awk '{print $3}')"
protocols="$(ffmpeg -hide_banner -protocols 2>/dev/null)"
encoders="$(ffmpeg -hide_banner -encoders 2>/dev/null)"
for p in udp rtp rtmp srt; do
  grep -qw "$p" <<<"$protocols" || warn "ffmpeg $FF_VERSION ne connaît pas le protocole $p : les sources $p ne marcheront pas"
done
grep -qw libx264 <<<"$encoders" || die "ffmpeg $FF_VERSION n'a pas l'encodeur libx264"
grep -qw aac <<<"$encoders" || die "ffmpeg $FF_VERSION n'a pas l'encodeur aac"
ffmpeg -hide_banner -h muxer=image2 2>/dev/null | grep -q atomic_writing || warn "ffmpeg $FF_VERSION est ancien (option atomic_writing absente) : Ubuntu 24.04 est conseillé"
ok "ffmpeg $FF_VERSION"

# ---------------------------------------------------------------- 3. compilation
step "3/8  Programme"
if [ -n "$PACKAGE" ]; then
  [ -f "$PACKAGE" ] || die "paquet introuvable : $PACKAGE"
  TMP="$(mktemp -d)"
  trap 'rm -rf "$TMP"' EXIT
  if [ "$(head -c4 "$PACKAGE" | od -An -c | tr -d ' ')" = '177ELF' ]; then
    cp "$PACKAGE" "$TMP/strata-cut"
  else
    tar -xzf "$PACKAGE" -C "$TMP"
    found="$(find "$TMP" -type f -name strata-cut | head -1)"
    [ -n "$found" ] || die "le paquet ne contient pas le programme strata-cut"
    [ "$found" = "$TMP/strata-cut" ] || mv "$found" "$TMP/strata-cut"
  fi
  NEW_BIN="$TMP/strata-cut"
  chmod 0755 "$NEW_BIN"
  ok "paquet : $("$NEW_BIN" --version)"
else
  # Node.js (pour construire l'interface web).
  node_ok=0
  if command -v node >/dev/null && version_ge "$(node -v | tr -d v)" "$NODE_MIN"; then node_ok=1; fi
  if [ "$node_ok" -eq 0 ]; then
    [ "$NO_APT" -eq 0 ] || die "Node.js $NODE_MIN ou plus récent est requis"
    echo "  installation de Node.js $NODE_MAJOR (NodeSource)"
    curl -fsSL "https://deb.nodesource.com/setup_${NODE_MAJOR}.x" | bash - >/dev/null
    apt_install nodejs
  fi
  ok "Node.js $(node -v)"
  # Rust (pour le serveur), dans le compte qui compile.
  if ! as_builder 'command -v cargo >/dev/null'; then
    echo "  installation de Rust (rustup) pour $BUILD_USER"
    as_builder 'curl -fsSL https://sh.rustup.rs | sh -s -- -y --profile minimal >/dev/null'
  fi
  rust_v="$(as_builder 'source "$HOME/.cargo/env" 2>/dev/null; rustc --version' | awk '{print $2}')"
  if ! version_ge "$rust_v" "$RUST_MIN"; then
    as_builder 'source "$HOME/.cargo/env" 2>/dev/null; rustup update stable >/dev/null' || die "Rust $rust_v est trop ancien ($RUST_MIN requis) et rustup n'a pas pu le mettre à jour"
    rust_v="$(as_builder 'source "$HOME/.cargo/env" 2>/dev/null; rustc --version' | awk '{print $2}')"
  fi
  ok "Rust $rust_v"
  echo "  construction de l'interface web…"
  as_builder "cd '$SRC_DIR/web' && npm ci --no-audit --no-fund --loglevel=error && npm run build >/dev/null"
  ok "interface web construite"
  echo "  compilation du serveur (quelques minutes la première fois)…"
  as_builder "source \"\$HOME/.cargo/env\" 2>/dev/null; cd '$SRC_DIR' && cargo build --release --locked -p strata-cut-server --quiet"
  NEW_BIN="$SRC_DIR/target/release/strata-cut"
  [ -x "$NEW_BIN" ] || die "la compilation n'a pas produit $NEW_BIN"
  ok "serveur compilé : $("$NEW_BIN" --version)"
fi

# ---------------------------------------------------------------- 4. réglages système
step "4/8  Réglages du système"
# Gros tampons UDP : FFmpeg en demande 8 Mo par source (UDP, RTP, SRT).
cat >"$SYSCTL_FILE" <<'EOF'
# Strata Cut : tampons de réception pour les flux UDP / RTP / SRT
net.core.rmem_max = 33554432
net.core.rmem_default = 1048576
net.core.wmem_max = 33554432
EOF
if sysctl -q -p "$SYSCTL_FILE" 2>/dev/null; then ok "tampons réseau agrandis ($SYSCTL_FILE)"; else warn "tampons réseau non appliqués (conteneur ?)"; fi
# L'heure de la machine fait le timecode : elle doit être synchronisée.
if command -v timedatectl >/dev/null && has_systemd; then
  timedatectl set-ntp true 2>/dev/null || true
  if [ "$(timedatectl show -p NTPSynchronized --value 2>/dev/null)" = yes ]; then ok "heure synchronisée (NTP)"; else warn "heure pas encore synchronisée : vérifiez « timedatectl » (le timecode en dépend)"; fi
  ok "fuseau horaire : $(timedatectl show -p Timezone --value 2>/dev/null)"
fi

# ---------------------------------------------------------------- 5. installation
step "5/8  Installation dans $INSTALL_DIR"
was_running=0
if has_systemd && systemctl is-active --quiet "$UNIT" 2>/dev/null; then
  was_running=1
  systemctl stop "$UNIT"
  ok "service arrêté pour la mise à jour"
fi
if ! id -u "$SERVICE_USER" >/dev/null 2>&1; then
  useradd --system --no-create-home --home-dir "$DATA_DIR" --shell /usr/sbin/nologin "$SERVICE_USER"
  ok "compte de service « $SERVICE_USER » créé"
fi
mkdir -p "$INSTALL_DIR"
if [ -f "$BIN" ]; then cp -p "$BIN" "$BIN.previous"; fi
install -m 0755 "$NEW_BIN" "$BIN.new"
mv -f "$BIN.new" "$BIN"
# Le service remplace lui-même son programme lors des mises à jour depuis l'interface.
chown -R "$SERVICE_USER:$SERVICE_USER" "$INSTALL_DIR"
ln -sf "$BIN" "$LINK"
ok "$("$BIN" --version) → $BIN (commande « strata-cut »)"

# ---------------------------------------------------------------- 6. données
step "6/8  Dossiers des données"
mkdir -p "$DATA_DIR"
for d in "$RECORDINGS_DIR" "$CLIPS_DIR"; do
  [ -n "$d" ] || continue
  mkdir -p "$d"
  chown "$SERVICE_USER:$SERVICE_USER" "$d"
done
chown -R "$SERVICE_USER:$SERVICE_USER" "$DATA_DIR"
if [ -n "$RECORDINGS_DIR$CLIPS_DIR" ]; then
  # Écrit les dossiers dans config.json en gardant le reste des réglages.
  python3 - "$DATA_DIR/config.json" "$RECORDINGS_DIR" "$CLIPS_DIR" <<'EOF'
import json, os, sys
path, rec, clips = sys.argv[1:4]
try:
    with open(path) as f:
        cfg = json.load(f)
except (OSError, ValueError):
    cfg = {}
if rec:
    cfg["recordingsDir"] = rec
if clips:
    cfg["clipsDir"] = clips
tmp = path + ".tmp"
with open(tmp, "w") as f:
    json.dump(cfg, f, indent=2, ensure_ascii=False)
os.chmod(tmp, 0o600)
os.replace(tmp, path)
EOF
  chown "$SERVICE_USER:$SERVICE_USER" "$DATA_DIR/config.json"
fi
ok "données : $DATA_DIR"
ok "boucles : ${RECORDINGS_DIR:-$DATA_DIR/loops}"
ok "extraits : ${CLIPS_DIR:-$DATA_DIR/clips}"
rec_dir="${RECORDINGS_DIR:-$DATA_DIR}"
free_gb="$(df -BG --output=avail "$rec_dir" | tail -1 | tr -dc 0-9)"
if [ -n "$free_gb" ]; then ok "espace libre pour les boucles : ${free_gb} Go (≈ 108 Go par source à 10 Mb/s sur 24 h)"; fi

# ---------------------------------------------------------------- 7. service
step "7/8  Service systemd"
if [ "$NO_SERVICE" -eq 1 ]; then
  warn "service non installé (--no-service). Pour lancer à la main :"
  echo "    sudo -u $SERVICE_USER $BIN run --bind 0.0.0.0:$PORT --data-dir $DATA_DIR"
else
  "$BIN" service install --bind "0.0.0.0:$PORT" --data-dir "$DATA_DIR" >/dev/null
  # Le programme et ses données appartiennent au compte de service.
  chown -R "$SERVICE_USER:$SERVICE_USER" "$INSTALL_DIR" "$DATA_DIR"
  systemctl restart "$UNIT"
  for _ in $(seq 1 30); do
    curl -fs "http://127.0.0.1:$PORT/api/health" >/dev/null 2>&1 && break
    sleep 1
  done
  if curl -fs "http://127.0.0.1:$PORT/api/health" >/dev/null 2>&1; then
    ok "service « $UNIT » en marche (démarrage automatique activé)"
  else
    warn "le service ne répond pas encore sur le port $PORT : journalctl -u $UNIT -n 50"
  fi
  if [ "$was_running" -eq 1 ]; then ok "mise à jour terminée"; fi
fi

# ---------------------------------------------------------------- 8. pare-feu
step "8/8  Pare-feu"
if command -v ufw >/dev/null && ufw status 2>/dev/null | grep -q 'Status: active'; then
  ufw allow "$PORT/tcp" comment "$BRAND web" >/dev/null
  ok "ufw : $PORT/tcp ouvert (interface web)"
  IFS=',' read -ra extra <<<"$OPEN_PORTS"
  for p in "${extra[@]}"; do
    p="$(echo "$p" | tr -d ' ')"
    [ -n "$p" ] || continue
    ufw allow "${p/-/:}" comment "$BRAND source" >/dev/null && ok "ufw : $p ouvert"
  done
  [ -n "$OPEN_PORTS" ] || warn "pensez à ouvrir les ports des sources SRT (UDP) et RTMP (TCP) : --open-ports \"9000-9010/udp,1935/tcp\""
else
  ok "ufw inactif : rien à ouvrir sur cette machine"
fi

# ---------------------------------------------------------------- compte admin
if [ -n "$ADMIN" ]; then
  step "Compte administrateur « $ADMIN »"
  "$BIN" user add "$ADMIN" --role admin --data-dir "$DATA_DIR"
  chown "$SERVICE_USER:$SERVICE_USER" "$DATA_DIR/users.json" 2>/dev/null || true
fi

# ---------------------------------------------------------------- résumé
ips="$(hostname -I 2>/dev/null | tr ' ' '\n' | grep -E '^[0-9]+\.' | head -3)"
printf '\n%s%s est installé.%s\n\n' "$B" "$BRAND" "$N"
echo "  Interface web :"
for ip in $ips; do echo "    http://$ip:$PORT/"; done
echo "    http://localhost:$PORT/   (sur le serveur lui-même)"
if [ -z "$ADMIN" ] && [ ! -s "$DATA_DIR/users.json" ]; then
  echo
  echo "  Créez le compte administrateur :"
  echo "    sudo strata-cut user add admin --data-dir $DATA_DIR"
fi
echo
echo "  Service :   systemctl status $UNIT   ·   journalctl -u $UNIT -f"
echo "  Mettre à jour : git pull, puis relancer ce script."
