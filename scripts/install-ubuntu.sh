#!/usr/bin/env bash
# Strata Cut — installation sur un serveur Ubuntu (22.04 ou 24.04, 64 bits).
#
# En une ligne, dans un terminal (SSH) sur le serveur :
#   curl -fsSL https://raw.githubusercontent.com/DamienP2/Strata-Cut/HEAD/scripts/install-ubuntu.sh | sudo bash -s -- --admin admin
# Le script télécharge le dépôt dans ~/Strata-Cut, compile et installe le service.
# Depuis le dépôt :  sudo ./scripts/install-ubuntu.sh
# Relancer le script met à jour le code (git pull) puis l'installation ; les données restent.
#
# Options :
#   --repo URL            dépôt git (défaut https://github.com/DamienP2/Strata-Cut.git ;
#                         une adresse SSH git@… marche aussi, avec clé de déploiement créée au besoin)
#   --branch NOM          branche à installer (défaut : la branche par défaut du dépôt)
#   --src DIR             dossier du code source (défaut : le dépôt du script, sinon ~/Strata-Cut)
#   --no-pull             n'actualise pas le code source
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
NTP_HELPER="/usr/local/lib/strata-cut/apply-ntp"
NTP_UNIT="strata-cut-ntp"
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

REPO="https://github.com/DamienP2/Strata-Cut.git"
BRANCH=""
SRC_DIR=""
NO_PULL=0
# Empreinte publiée par GitHub de sa clé d'hôte ED25519.
GITHUB_ED25519="SHA256:+DiY3wvvV6TuJJhbpZisF/zLDA0zPMSvHdkr4UvCOqU"

# Le script est-il dans un dépôt Strata Cut ?
SCRIPT_REPO=""
if [ -f "${BASH_SOURCE[0]:-}" ]; then SCRIPT_REPO="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"; fi

# ---------------------------------------------------------------- affichage
if [ -t 1 ]; then B=$'\e[1m'; G=$'\e[32m'; Y=$'\e[33m'; R=$'\e[31m'; N=$'\e[0m'; else B=; G=; Y=; R=; N=; fi
step() { printf '\n%s==> %s%s\n' "$B" "$*" "$N"; }
ok() { printf '%s  ✓ %s%s\n' "$G" "$*" "$N"; }
warn() { printf '%s  ! %s%s\n' "$Y" "$*" "$N" >&2; }
die() { printf '%s  ✗ %s%s\n' "$R" "$*" "$N" >&2; exit 1; }
usage() {
  if [ -f "${BASH_SOURCE[0]:-}" ]; then sed -n '2,/^set -euo/p' "${BASH_SOURCE[0]}" | sed '$d; s/^# \{0,1\}//'; else echo "Aide : https://github.com/DamienP2/Strata-Cut/blob/HEAD/docs/INSTALL-UBUNTU.md"; fi
  exit 0
}
# Lancé par « curl … | bash », l'entrée est le script lui-même : les questions passent par le terminal.
TTY=""
if [ -t 0 ]; then TTY=/dev/stdin; elif { : </dev/tty; } 2>/dev/null; then TTY=/dev/tty; fi

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
    --repo) REPO="${2:?}"; shift 2 ;;
    --branch) BRANCH="${2:?}"; shift 2 ;;
    --src) SRC_DIR="${2:?}"; shift 2 ;;
    --no-pull) NO_PULL=1; shift ;;
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
[ "$(id -u)" -eq 0 ] || die "lancez ce script avec sudo (… | sudo bash)"

# Le compte qui a lancé sudo compile (pas root).
BUILD_USER="${SUDO_USER:-root}"
BUILD_HOME="$(getent passwd "$BUILD_USER" | cut -d: -f6)"
as_builder() {
  if [ "$BUILD_USER" = root ]; then bash -lc "$1"; else sudo -u "$BUILD_USER" -H bash -lc "$1"; fi
}

# Le code source : le dépôt du script, sinon ~/Strata-Cut du compte qui compile.
if [ -z "$SRC_DIR" ]; then
  if [ -n "$SCRIPT_REPO" ] && [ -f "$SCRIPT_REPO/Cargo.toml" ]; then SRC_DIR="$SCRIPT_REPO"; else SRC_DIR="$BUILD_HOME/Strata-Cut"; fi
fi
[[ "$SRC_DIR" = /* ]] || SRC_DIR="$(pwd)/$SRC_DIR"

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
  if has_systemd; then
    systemctl disable --now "$NTP_UNIT.path" 2>/dev/null || true
    rm -f "/etc/systemd/system/$NTP_UNIT.path" "/etc/systemd/system/$NTP_UNIT.service"
    systemctl daemon-reload
  fi
  rm -rf "$INSTALL_DIR" "$LINK" "$SYSCTL_FILE" "$(dirname "$NTP_HELPER")"
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
step "1/9  Vérification du système"
if [ -r /etc/os-release ]; then
  . /etc/os-release
  if [ "${ID:-}" != ubuntu ]; then warn "système $PRETTY_NAME : ce script vise Ubuntu, il peut fonctionner sur Debian"; fi
  case "${VERSION_ID:-}" in 22.04 | 24.04) ok "$PRETTY_NAME" ;; *) warn "$PRETTY_NAME : testé sur Ubuntu 22.04 et 24.04" ;; esac
fi
case "$(uname -m)" in x86_64 | aarch64) ok "processeur $(uname -m)" ;; *) die "processeur $(uname -m) non pris en charge (64 bits requis)" ;; esac
if [ "$NO_SERVICE" -eq 0 ] && ! has_systemd; then die "systemd ne tourne pas sur cette machine (utilisez --no-service pour un essai)"; fi

# ---------------------------------------------------------------- 2. paquets
step "2/9  Paquets Ubuntu"
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
  if [ -z "$PACKAGE" ]; then apt_install git openssh-client build-essential pkg-config; fi
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

# ---------------------------------------------------------------- 3. code source (git par SSH)
# Lance git en tant que compte qui compile, avec la commande SSH retenue.
GIT_SSH="ssh -o BatchMode=yes -o ConnectTimeout=20"
as_git() {
  local cmd env
  cmd="$(printf '%q ' git "$@")"
  env="export GIT_SSH_COMMAND=$(printf '%q' "$GIT_SSH");"
  if [ -n "${SSH_AUTH_SOCK:-}" ]; then env="$env export SSH_AUTH_SOCK=$(printf '%q' "$SSH_AUTH_SOCK");"; fi
  as_builder "$env $cmd"
}

# "git@github.com:owner/repo.git" ou "ssh://git@host:22/owner/repo.git" → host, port, owner/repo.
repo_host() {
  local r="$1" h
  case "$r" in
    ssh://*) h="${r#ssh://}"; h="${h#*@}"; h="${h%%/*}"; echo "${h%%:*}" ;;
    *@*:*) h="${r#*@}"; echo "${h%%:*}" ;;
    *) echo "" ;;
  esac
}
repo_port() {
  local r="$1" h
  case "$r" in ssh://*) h="${r#ssh://}"; h="${h#*@}"; h="${h%%/*}"; [[ "$h" == *:* ]] && echo "${h##*:}" || echo 22 ;; *) echo 22 ;; esac
}
repo_path() {
  local r="$1"
  case "$r" in ssh://*) r="${r#ssh://}"; r="${r#*/}" ;; *@*:*) r="${r#*:}" ;; esac
  echo "${r%.git}"
}

# Ajoute la clé d'hôte du serveur git aux hôtes connus (vérifiée pour github.com).
trust_host() {
  local host="$1" port="$2" entry fp
  local name="$host"; [ "$port" = 22 ] || name="[$host]:$port"
  as_builder "mkdir -p ~/.ssh && chmod 700 ~/.ssh && touch ~/.ssh/known_hosts && chmod 600 ~/.ssh/known_hosts"
  if as_builder "ssh-keygen -F '$name' -f ~/.ssh/known_hosts >/dev/null"; then return 0; fi
  entry="$(ssh-keyscan -p "$port" -t ed25519 "$host" 2>/dev/null | grep -v '^#' || true)"
  [ -n "$entry" ] || die "impossible de joindre $host:$port en SSH (réseau, pare-feu sortant ?)"
  fp="$(ssh-keygen -lf - <<<"$entry" | awk '{print $2}')"
  if [ "$host" = github.com ] && [ "$fp" != "$GITHUB_ED25519" ]; then
    die "la clé d'hôte reçue pour github.com ($fp) n'est pas celle de GitHub : connexion interceptée ?"
  fi
  printf '%s\n' "$entry" | as_builder "cat >> ~/.ssh/known_hosts"
  ok "hôte $host ajouté aux hôtes connus ($fp)"
}

if [ -n "$PACKAGE" ]; then
  : # rien à télécharger
elif [ -d "$SRC_DIR/.git" ] && [ "$NO_PULL" -eq 1 ]; then
  step "3/9  Code source"
  ok "$SRC_DIR (non actualisé : --no-pull)"
elif [ -f "$SRC_DIR/Cargo.toml" ] && [ ! -d "$SRC_DIR/.git" ]; then
  step "3/9  Code source"
  ok "$SRC_DIR (copie sans git : non actualisée)"
else
  step "3/9  Code source"
  command -v git >/dev/null || die "git est introuvable"
  # Un dépôt déjà cloné garde son adresse et sa clé.
  if [ -d "$SRC_DIR/.git" ]; then
    REPO="$(as_builder "git -C '$SRC_DIR' remote get-url origin")"
    key_cmd="$(as_builder "git -C '$SRC_DIR' config --get core.sshCommand" || true)"
    [ -z "$key_cmd" ] || GIT_SSH="$key_cmd -o BatchMode=yes -o ConnectTimeout=20"
  fi
  case "$REPO" in
    https://* | http://*) ssh_repo=0 ;;
    *) ssh_repo=1 ;;
  esac
  if [ "$ssh_repo" -eq 1 ]; then
  host="$(repo_host "$REPO")"
  [ -n "$host" ] || die "adresse de dépôt non reconnue : $REPO"
  port="$(repo_port "$REPO")"
  trust_host "$host" "$port"

  # L'accès : les clés du compte (ou l'agent SSH), sinon une clé de déploiement dédiée.
  KEY="$BUILD_HOME/.ssh/strata-cut_deploy"
  KEY_SSH="ssh -i $KEY -o IdentitiesOnly=yes"
  if ! as_git ls-remote --exit-code "$REPO" HEAD >/dev/null 2>&1; then
    GIT_SSH="$KEY_SSH -o BatchMode=yes -o ConnectTimeout=20"
    if [ ! -f "$KEY" ]; then
      as_builder "ssh-keygen -q -t ed25519 -N '' -C 'strata-cut@$(hostname)' -f '$KEY'"
      ok "clé de déploiement créée : $KEY"
    fi
    tries=0
    until as_git ls-remote --exit-code "$REPO" HEAD >/dev/null 2>&1; do
      tries=$((tries + 1))
      repo="$(repo_path "$REPO")"
      printf "\n%s  Ce serveur n'a pas encore accès au dépôt %s.%s\n" "$Y" "$repo" "$N"
      if [ "$host" = github.com ]; then
        echo "  Ajoutez cette clé sur GitHub, en lecture seule :"
        echo "    https://github.com/$repo/settings/keys/new"
        echo "    Title : Strata Cut $(hostname) — Key : la ligne ci-dessous — ne cochez pas « Allow write access »"
      else
        echo "  Ajoutez cette clé publique au dépôt sur $host (clé de déploiement en lecture seule) :"
      fi
      echo
      echo "    $(cat "$KEY.pub")"
      echo
      if [ -n "$TTY" ] && [ "$tries" -lt 10 ]; then
        read -r -p "  Appuyez sur Entrée une fois la clé ajoutée (Ctrl-C pour arrêter)… " _ <"$TTY"
      else
        die "accès au dépôt refusé : ajoutez la clé ci-dessus puis relancez le script"
      fi
    done
    ok "accès au dépôt par la clé de déploiement"
  else
    ok "accès au dépôt $REPO"
  fi
  else
    as_git ls-remote --exit-code "$REPO" HEAD >/dev/null 2>&1 || die "impossible de lire le dépôt $REPO (adresse, réseau, proxy ?)"
    ok "accès au dépôt $REPO"
  fi

  if [ -d "$SRC_DIR/.git" ]; then
    if [ -n "$(as_builder "git -C '$SRC_DIR' status --porcelain --untracked-files=no")" ]; then
      warn "$SRC_DIR a des modifications locales : code non actualisé"
    else
      as_git -C "$SRC_DIR" fetch --quiet origin
      if [ -n "$BRANCH" ] && [ "$(as_builder "git -C '$SRC_DIR' rev-parse --abbrev-ref HEAD")" != "$BRANCH" ]; then
        as_git -C "$SRC_DIR" checkout --quiet "$BRANCH"
      fi
      if as_git -C "$SRC_DIR" pull --quiet --ff-only; then
        ok "code actualisé : $(as_builder "git -C '$SRC_DIR' log -1 --format='%h %s'")"
      else
        warn "la mise à jour du code a échoué : installation du code déjà présent"
      fi
    fi
  else
    [ ! -e "$SRC_DIR" ] || [ -z "$(ls -A "$SRC_DIR" 2>/dev/null)" ] || die "$SRC_DIR existe et n'est pas un dépôt git : choisissez un autre dossier (--src)"
    clone_args=(clone --quiet)
    [ -z "$BRANCH" ] || clone_args+=(--branch "$BRANCH")
    as_git "${clone_args[@]}" "$REPO" "$SRC_DIR"
    # Les mises à jour suivantes réutilisent la même clé.
    if [ "$GIT_SSH" != "ssh -o BatchMode=yes -o ConnectTimeout=20" ]; then
      as_builder "git -C '$SRC_DIR' config core.sshCommand $(printf '%q' "$KEY_SSH")"
    fi
    ok "dépôt cloné dans $SRC_DIR : $(as_builder "git -C '$SRC_DIR' log -1 --format='%h %s'")"
  fi
  [ -f "$SRC_DIR/Cargo.toml" ] || die "$SRC_DIR ne contient pas Strata Cut (Cargo.toml absent)"
fi

# ---------------------------------------------------------------- 4. compilation
step "4/9  Compilation"
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

# ---------------------------------------------------------------- 5. réglages système
step "5/9  Réglages du système"
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

# ---------------------------------------------------------------- 6. installation
step "6/9  Installation dans $INSTALL_DIR"
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

# ---------------------------------------------------------------- 7. données
step "7/9  Dossiers des données"
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

# ---------------------------------------------------------------- 8. service
step "8/9  Service systemd"
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

  # Serveurs NTP choisis dans l'interface (Système › Général) : le service, sans droits,
  # écrit <données>/ntp.conf ; cette unité root vérifie la liste et l'applique.
  mkdir -p "$(dirname "$NTP_HELPER")"
  cat >"$NTP_HELPER" <<'EOF'
#!/bin/sh
# Strata Cut : applique les serveurs NTP demandés dans l'interface ($1 = <données>/ntp.conf).
set -eu
servers=""
if [ -f "$1" ]; then
  for s in $(head -c 2048 "$1"); do
    case "$s" in
      -* | *[!A-Za-z0-9.:-]*) echo "serveur NTP refusé : $s" >&2; exit 1 ;;
    esac
    servers="$servers $s"
  done
fi
servers="${servers# }"
if systemctl is-active --quiet chrony 2>/dev/null; then
  f=/etc/chrony/sources.d/strata-cut.sources
  mkdir -p /etc/chrony/sources.d
  if [ -n "$servers" ]; then
    for s in $servers; do echo "server $s iburst"; done >"$f"
  else
    rm -f "$f"
  fi
  chronyc reload sources >/dev/null 2>&1 || systemctl restart chrony
else
  d=/etc/systemd/timesyncd.conf.d
  mkdir -p "$d"
  if [ -n "$servers" ]; then
    printf '# Strata Cut (Système › Général)\n[Time]\nNTP=%s\n' "$servers" >"$d/50-strata-cut.conf"
  else
    rm -f "$d/50-strata-cut.conf"
  fi
  timedatectl set-ntp true 2>/dev/null || true
  systemctl restart systemd-timesyncd
fi
EOF
  chmod 0755 "$NTP_HELPER"
  chown root:root "$NTP_HELPER"
  cat >"/etc/systemd/system/$NTP_UNIT.service" <<EOF
[Unit]
Description=$BRAND: NTP servers chosen in the interface

[Service]
Type=oneshot
ExecStart=$NTP_HELPER $DATA_DIR/ntp.conf
EOF
  cat >"/etc/systemd/system/$NTP_UNIT.path" <<EOF
[Unit]
Description=$BRAND: watches the NTP servers chosen in the interface

[Path]
PathChanged=$DATA_DIR/ntp.conf

[Install]
WantedBy=multi-user.target
EOF
  systemctl daemon-reload
  systemctl enable --now "$NTP_UNIT.path" >/dev/null 2>&1
  ok "serveurs NTP réglables dans Système › Général"
fi

# ---------------------------------------------------------------- 9. pare-feu
step "9/9  Pare-feu"
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
  if [ -z "$TTY" ]; then
    warn "pas de terminal pour saisir le mot de passe : sudo strata-cut user add $ADMIN --data-dir $DATA_DIR"
  elif "$BIN" user list --data-dir "$DATA_DIR" 2>/dev/null | awk -v n="$ADMIN" 'NR > 1 && $1 == n { f = 1 } END { exit !f }'; then
    ok "le compte « $ADMIN » existe déjà"
  else
    "$BIN" user add "$ADMIN" --role admin --data-dir "$DATA_DIR" <"$TTY"
  fi
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
echo "  Mettre à jour : sudo $SRC_DIR/scripts/install-ubuntu.sh   (télécharge la dernière version)"
