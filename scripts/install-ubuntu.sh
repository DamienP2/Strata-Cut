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
#   --no-grow             ne donne pas au volume des données tout l'espace libre du disque (LVM)
#   --desktop-video F     installe le pilote Blackmagic Desktop Video depuis cette archive
#                         (Blackmagic_Desktop_Video_Linux_*.tar.gz) ou ce paquet desktopvideo_*.deb ;
#                         « wait » : explique comment copier l'archive par SSH et attend qu'elle
#                         soit là (fait de lui-même si une carte Blackmagic est vue sans pilote)
#   --no-desktop-video    ne propose pas le pilote Blackmagic
#   --ndi-sdk F           installe la bibliothèque NDI (sources NDI) depuis l'installateur du SDK
#                         NDI (Install_NDI_SDK_v6_Linux.sh : sa licence s'accepte dans le terminal),
#                         son dossier « NDI SDK for Linux » déjà déballé, ou un libndi.so
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
STORAGE_HELPER="/usr/local/lib/strata-cut/prepare-folder"
STORAGE_UNIT="strata-cut-storage"
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
NO_GROW=0
DESKTOP_VIDEO=""
NO_DESKTOP_VIDEO=0
NDI_SDK=""
DV_CACHE="/var/cache/strata-cut"
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
ORIG_ARGS=("$@")
# L'empreinte du script lancé, pour savoir s'il a changé une fois le code actualisé.
SELF_SUM=""
if [ -f "${BASH_SOURCE[0]:-}" ]; then SELF_SUM="$(sha256sum <"${BASH_SOURCE[0]}")"; fi
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
    --no-grow) NO_GROW=1; shift ;;
    --desktop-video) DESKTOP_VIDEO="${2:?}"; shift 2 ;;
    --no-desktop-video) NO_DESKTOP_VIDEO=1; shift ;;
    --ndi-sdk) NDI_SDK="${2:?}"; shift 2 ;;
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
if [ -n "$DESKTOP_VIDEO" ] && [ "$DESKTOP_VIDEO" != wait ]; then
  [ -e "$DESKTOP_VIDEO" ] || die "pilote Blackmagic introuvable : $DESKTOP_VIDEO"
  DESKTOP_VIDEO="$(realpath "$DESKTOP_VIDEO")"
fi
if [ -n "$NDI_SDK" ]; then
  [ -e "$NDI_SDK" ] || die "SDK NDI introuvable : $NDI_SDK"
  NDI_SDK="$(realpath "$NDI_SDK")"
fi
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
    systemctl disable --now "$NTP_UNIT.path" "$STORAGE_UNIT.path" strata-cut-nic.service 2>/dev/null || true
    rm -f /etc/systemd/system/strata-cut-nic.service
    rm -f "/etc/systemd/system/$NTP_UNIT.path" "/etc/systemd/system/$NTP_UNIT.service" "/etc/systemd/system/$STORAGE_UNIT.path" "/etc/systemd/system/$STORAGE_UNIT.service"
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

# ---------------------------------------------------------------- pilote Blackmagic
# Desktop Video : le pilote des cartes DeckLink, UltraStudio et Intensity (module du noyau et
# bibliothèque libDeckLinkAPI que le serveur ouvre, comme le SDK de Blackmagic le prévoit). Blackmagic le distribue après un formulaire et l'accord de
# sa licence : depuis une archive déjà téléchargée, ou téléchargé ici avec vos coordonnées.
dv_installed() { dpkg -s desktopvideo >/dev/null 2>&1; }
dv_pci() { command -v lspci >/dev/null && lspci -mm -d bdbd: 2>/dev/null | head -1 | cut -d'"' -f6; }

# Une archive déjà là (dossier courant, compte qui installe, /root, /tmp, cache) : la plus récente.
dv_find() {
  local dirs=("$PWD" "$BUILD_HOME" /root /tmp "$DV_CACHE" /home/*) f best=""
  for d in "${dirs[@]}"; do
    for f in "$d"/Blackmagic_Desktop_Video_Linux_*.tar* "$d"/desktopvideo_*.deb; do
      [ -f "$f" ] || continue
      if [ -z "$best" ] || [ "$(printf '%s\n%s\n' "$best" "$f" | sort -V | tail -1)" = "$f" ]; then best="$f"; fi
    done
  done
  printf '%s' "$best"
}

ask() { # ask VARIABLE "question" [défaut]
  local answer=""
  read -r -p "  $2${3:+ [$3]} : " answer <"$TTY" || true
  printf -v "$1" '%s' "${answer:-${3:-}}"
}

# Pas à pas : l'archive est téléchargée sur votre ordinateur (Blackmagic la donne après un
# formulaire), copiée sur ce serveur par SSH (scp) ; le script attend qu'elle soit là, la vérifie,
# puis continue.
dv_wait_copy() {
  [ -n "$TTY" ] || { warn "pilote Blackmagic : pas de terminal pour attendre la copie de l'archive"; return 1; }
  local who host latest version="" answer f
  who="${SUDO_USER:-$(id -un)}"
  host="$(hostname -I 2>/dev/null | awk '{print $1}')"
  [ -n "$host" ] || host="$(hostname)"
  # La dernière version, pour savoir quoi télécharger (le site de Blackmagic la publie).
  latest="$(curl -fsS -m 15 https://www.blackmagicdesign.com/api/support/latest-version/desktop-video/linux 2>/dev/null)" || true
  version="$(python3 -c 'import json,sys; d=json.load(sys.stdin)["linux"]; print("%s.%s" % (d["major"], d["minor"]))' <<<"$latest" 2>/dev/null)" || true
  cat <<EOT

  ┌─ Pilote Blackmagic Desktop Video ─────────────────────────────────────────────
  │ 1. Sur votre ordinateur, ouvrez :
  │      https://www.blackmagicdesign.com/fr/support/family/capture-and-playback
  │    et téléchargez « Desktop Video ${version:-(la dernière version)} » pour Linux
  │    (Blackmagic demande ses coordonnées et l'accord de sa licence).
  │ 2. Copiez l'archive sur ce serveur, depuis un terminal de votre ordinateur :
  │      macOS / Linux :
  │        scp ~/Downloads/Blackmagic_Desktop_Video_Linux_*.tar.gz $who@$host:~/
  │      Windows (PowerShell) :
  │        scp \$HOME\Downloads\Blackmagic_Desktop_Video_Linux_*.tar.gz $who@$host:~/
  │    (Téléchargements peut s'appeler autrement ; WinSCP ou FileZilla en SFTP marchent aussi.)
  │ 3. Revenez ici et appuyez sur Entrée : l'archive est vérifiée, puis l'installation continue.
  └───────────────────────────────────────────────────────────────────────────────
EOT
  while true; do
    answer=""
    read -r -p "  Entrée quand l'archive est copiée (p : passer, sans pilote Blackmagic) : " answer <"$TTY" || return 1
    [[ "$answer" =~ ^[pP] ]] && return 1
    f="$(dv_find)"
    if [ -z "$f" ]; then
      warn "aucune archive Blackmagic_Desktop_Video_Linux_*.tar.gz dans $BUILD_HOME, /home/*, /root ni /tmp — la copie est-elle finie ?"
      continue
    fi
    # Une copie interrompue donne une archive illisible : elle est redemandée.
    if [[ "$f" != *.deb ]] && ! tar -tf "$f" >/dev/null 2>&1; then
      warn "$f est incomplète ou abîmée : copiez-la de nouveau"
      continue
    fi
    ok "archive trouvée : $f"
    DESKTOP_VIDEO="$f"
    return 0
  done
}

# Le paquet de l'archive (desktopvideo_*.deb, sans l'interface graphique) installé ; son module
# du noyau construit par DKMS (en-têtes du noyau en cours) et chargé.
dv_install() {
  local src="$1" arch deb member
  case "$(dpkg --print-architecture)" in amd64) arch=x86_64 ;; arm64) arch=arm64 ;; *) warn "pilote Blackmagic : processeur $(dpkg --print-architecture) non pris en charge"; return 1 ;; esac
  mkdir -p "$DV_CACHE"
  if [[ "$src" == *.deb ]]; then
    deb="$src"
  else
    member="$(tar -tf "$src" 2>/dev/null | grep -E "/deb/$arch/desktopvideo_[^/]*\.deb$" | grep -v -- -gui | sort -V | tail -1)" || true
    [ -n "$member" ] || { warn "pilote Blackmagic : aucun paquet desktopvideo pour $arch dans $src"; return 1; }
    deb="$DV_CACHE/$(basename "$member")"
    tar -xOf "$src" "$member" >"$deb"
  fi
  apt_install dkms "linux-headers-$(uname -r)" || warn "en-têtes du noyau $(uname -r) introuvables : le module Blackmagic ne pourra pas être construit"
  if command -v mokutil >/dev/null && mokutil --sb-state 2>/dev/null | grep -qi enabled; then
    warn "Secure Boot est actif : le module Blackmagic devra être signé (clé MOK enrôlée au redémarrage) pour se charger"
  fi
  echo "  installation de $(basename "$deb")"
  NEEDRESTART_SUSPEND=1 NEEDRESTART_MODE=l DEBIAN_FRONTEND=noninteractive LC_ALL=C apt-get install -y -q -o Dpkg::Options::=--force-confdef -o Dpkg::Options::=--force-confold "$deb" </dev/null 2>&1 | apt_progress || { warn "le paquet Desktop Video ne s'est pas installé"; return 1; }
  modprobe blackmagic 2>/dev/null || true
  modprobe blackmagic-io 2>/dev/null || true
  ok "pilote Blackmagic Desktop Video $(dpkg-query -W -f='${Version}' desktopvideo 2>/dev/null) installé"
  if [ ! -e /dev/blackmagic ]; then warn "le module Blackmagic n'est pas chargé : redémarrez le serveur, puis relancez ce script"; fi
  if command -v DesktopVideoUpdateTool >/dev/null; then echo "  firmware des cartes : « sudo DesktopVideoUpdateTool » dit si une mise à jour est nécessaire"; fi
}

desktop_video() {
  [ "$NO_DESKTOP_VIDEO" -eq 0 ] || return 0
  local card
  card="$(dv_pci || true)"
  if [ -z "$DESKTOP_VIDEO" ]; then
    # Rien de demandé : seulement si une carte Blackmagic est là sans son pilote.
    if dv_installed || [ -z "$card" ]; then
      if dv_installed; then ok "pilote Blackmagic Desktop Video $(dpkg-query -W -f='${Version}' desktopvideo 2>/dev/null)"; fi
      return 0
    fi
    echo "  carte Blackmagic sur le bus PCI : $card — pilote Desktop Video absent"
    DESKTOP_VIDEO="$(dv_find)"
    if [ -n "$DESKTOP_VIDEO" ]; then echo "  archive trouvée : $DESKTOP_VIDEO"; else DESKTOP_VIDEO="wait"; fi
  fi
  if [ "$DESKTOP_VIDEO" = wait ]; then
    if ! dv_wait_copy; then
      warn "pilote Blackmagic non installé : copiez l'archive Desktop Video sur ce serveur puis relancez ce script (voir docs/INSTALL-UBUNTU.md, « Carte Blackmagic »)"
      return 0
    fi
  fi
  dv_install "$DESKTOP_VIDEO" || true
}

# ---------------------------------------------------------------- 2. paquets
step "2/9  Paquets Ubuntu"
# La liste des paquets n'est relue qu'une fois, et seulement s'il en manque un (mises à jour rapides).
APT_UPDATED=0
apt_update_once() {
  [ "$APT_UPDATED" -eq 1 ] && return 0
  echo "  lecture de la liste des paquets…"
  LC_ALL=C apt-get update -qq </dev/null
  APT_UPDATED=1
}
# Ce qu'apt fait, sur une seule ligne (sinon un long téléchargement a l'air bloqué) ; ses erreurs en clair.
apt_progress() {
  local line n=0 total=""
  while IFS= read -r line; do
    case "$line" in
      *" newly installed"*) total="${line%% newly installed*}"; total="${total##*, }" ;;
      Get:*) n=$((n + 1)); printf '\r  téléchargement : %d/%s paquets…   ' "$n" "${total:-?}" ;;
      "Setting up "*) line="${line#Setting up }"; printf '\r  configuration : %-50.50s' "${line%% *}" ;;
      E:* | *"dpkg: error"*) printf '\n  %s\n' "$line" ;;
    esac
  done
  printf '\r%-78s\r' ''
}
# APT_LIGHT=1 : sans les paquets seulement « recommandés ».
apt_install() {
  local missing=() light=()
  for p in "$@"; do dpkg -s "$p" >/dev/null 2>&1 || missing+=("$p"); done
  if [ ${#missing[@]} -gt 0 ]; then
    apt_update_once
    echo "  installation : ${missing[*]}"
    [ "${APT_LIGHT:-0}" -eq 1 ] && light=(--no-install-recommends)
    # Aucune question ne doit attendre une réponse invisible : needrestart (Ubuntu 22.04+) est mis
    # en pause, dpkg garde les fichiers de réglages en place, et rien n'est lu au clavier.
    NEEDRESTART_SUSPEND=1 NEEDRESTART_MODE=l DEBIAN_FRONTEND=noninteractive LC_ALL=C \
      apt-get install -y -q "${light[@]}" -o Dpkg::Options::=--force-confdef -o Dpkg::Options::=--force-confold "${missing[@]}" </dev/null 2>&1 | apt_progress
  fi
}
if [ "$NO_APT" -eq 1 ]; then
  ok "installation des paquets sautée (--no-apt)"
else
  apt_install ca-certificates curl ffmpeg python3 cloud-guest-utils fonts-dejavu-core
  if [ -z "$PACKAGE" ]; then apt_install git openssh-client build-essential pkg-config; fi
  ok "ffmpeg, python3 et outils de base en place"
  # Cartes DeckLink (Blackmagic) : le serveur les capture lui-même avec le SDK de Blackmagic
  # (Desktop Video) et FFmpeg encode leurs images ; lspci les montre même sans pilote.
  APT_LIGHT=1 apt_install pciutils
  desktop_video
  # Sources « page web » : un navigateur sur un écran virtuel (Xorg « dummy », à la cadence de la
  # source ; Xvfb sinon), avec sa carte son.
  APT_LIGHT=1 apt_install xserver-xorg-core xserver-xorg-video-dummy xvfb pulseaudio fonts-noto-core fonts-noto-color-emoji
  if command -v google-chrome-stable >/dev/null || command -v chromium >/dev/null || command -v chromium-browser >/dev/null; then
    ok "navigateur des pages web en place"
  elif [ "$(dpkg --print-architecture)" = amd64 ]; then
    chrome_deb="$(mktemp -d)/google-chrome-stable_current_amd64.deb"
    if curl -fsSL --retry 3 -o "$chrome_deb" https://dl.google.com/linux/direct/google-chrome-stable_current_amd64.deb; then
      chmod 0644 "$chrome_deb"
      echo "  installation de Google Chrome (sources page web)"
      NEEDRESTART_SUSPEND=1 NEEDRESTART_MODE=l DEBIAN_FRONTEND=noninteractive LC_ALL=C apt-get install -y -q -o Dpkg::Options::=--force-confdef -o Dpkg::Options::=--force-confold "$chrome_deb" </dev/null 2>&1 | apt_progress || warn "Google Chrome ne s'est pas installé : les sources page web ne marcheront pas"
    else
      warn "Google Chrome non téléchargé : les sources page web ne marcheront pas"
    fi
    rm -f "$chrome_deb"; rmdir "$(dirname "$chrome_deb")" 2>/dev/null || true
    if command -v google-chrome-stable >/dev/null; then ok "Google Chrome $(google-chrome-stable --version 2>/dev/null | awk '{print $NF}') (sources page web)"; fi
  else
    warn "pas de Google Chrome pour $(dpkg --print-architecture) : installez Chromium (paquet .deb, pas snap) pour les sources page web"
  fi
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
devices="$(ffmpeg -hide_banner -devices 2>/dev/null)"
for d in x11grab pulse; do
  grep -qw "$d" <<<"$devices" || warn "ffmpeg $FF_VERSION n'a pas l'entrée $d : les sources page web ne marcheront pas"
done
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
  # Le code vient d'être actualisé : si le script lui-même a changé, la suite se fait avec la nouvelle version
  # (bash continue sinon l'ancienne, déjà ouverte, et poserait les anciens relais).
  NEW_SCRIPT="$SRC_DIR/scripts/install-ubuntu.sh"
  if [ -z "${SC_REEXEC:-}" ] && [ -f "$NEW_SCRIPT" ] && [ "$(sha256sum <"$NEW_SCRIPT")" != "$SELF_SUM" ]; then
    ok "script d'installation mis à jour : la suite se fait avec la nouvelle version"
    SC_REEXEC=1 exec bash "$NEW_SCRIPT" "${ORIG_ARGS[@]}" --src "$SRC_DIR" --no-pull
  fi
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
  # Les dépendances de l'interface ne sont réinstallées que si leur liste a changé.
  as_builder "cd '$SRC_DIR/web' && lock=\$(sha256sum package-lock.json | cut -c1-64) && if [ \"\$(cat node_modules/.strata-lock 2>/dev/null)\" != \"\$lock\" ]; then npm ci --no-audit --no-fund --loglevel=error && echo \"\$lock\" > node_modules/.strata-lock; fi && npm run build >/dev/null"
  ok "interface web construite"
  # Le numéro de version : celui de la dernière version publiée (étiquette vX.Y.Z), comme les paquets,
  # pour que Système › Mises à jour compare juste (Cargo.toml garde un numéro de départ).
  if [ -d "$SRC_DIR/.git" ] && [ "$NO_PULL" -eq 0 ]; then as_git -C "$SRC_DIR" fetch --quiet --tags origin 2>/dev/null || true; fi
  BUILD_VERSION="$(as_builder "git -C '$SRC_DIR' describe --tags --abbrev=0 --match 'v[0-9]*' 2>/dev/null" || true)"
  BUILD_VERSION="${BUILD_VERSION#v}"
  case "$BUILD_VERSION" in *[!0-9.]* | "") BUILD_VERSION="" ;; esac
  echo "  compilation du serveur (quelques minutes la première fois)…"
  as_builder "source \"\$HOME/.cargo/env\" 2>/dev/null; cd '$SRC_DIR' && STRATA_VERSION='$BUILD_VERSION' cargo build --release --locked -p strata-cut-server --quiet"
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
# Les rafales attendent le noyau au lieu d'être jetées.
net.core.netdev_max_backlog = 16384
net.core.netdev_budget = 600
EOF
if sysctl -q -p "$SYSCTL_FILE" 2>/dev/null; then ok "tampons réseau agrandis ($SYSCTL_FILE)"; else warn "tampons réseau non appliqués (conteneur ?)"; fi
# Anneaux de réception des cartes réseau au maximum : trop petits (souvent 256 ou 512 par
# défaut), une rafale des flux y est perdue avant tout programme (mosaïque qu'un poste VLC ne
# voit pas). Réappliqué à chaque démarrage.
if has_systemd; then
  apt_install ethtool
  mkdir -p /usr/local/lib/strata-cut
  cat >/usr/local/lib/strata-cut/nic-rings <<'EOF'
#!/bin/sh
# Strata Cut : anneaux de réception des cartes réseau à leur maximum.
for dev in /sys/class/net/*; do
  name=${dev##*/}
  [ -e "$dev/device" ] || continue
  max=$(ethtool -g "$name" 2>/dev/null | awk '/[Mm]aximums/{m=1} m && /^RX:/{print $2; exit}')
  case "$max" in ''|n/a|0) continue ;; esac
  ethtool -G "$name" rx "$max" 2>/dev/null || true
done
exit 0
EOF
  chmod 0755 /usr/local/lib/strata-cut/nic-rings
  cat >/etc/systemd/system/strata-cut-nic.service <<'EOF'
[Unit]
Description=Strata Cut: network cards' receive rings at their maximum
After=network-online.target
Wants=network-online.target

[Service]
Type=oneshot
ExecStart=/usr/local/lib/strata-cut/nic-rings

[Install]
WantedBy=multi-user.target
EOF
  systemctl daemon-reload
  systemctl enable strata-cut-nic.service >/dev/null 2>&1
  if systemctl start strata-cut-nic.service 2>/dev/null; then ok "anneaux de réception des cartes réseau au maximum"; else warn "anneaux des cartes réseau non réglés"; fi
fi
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
# Sources NDI : la bibliothèque NDI (libndi.so) vient du SDK NDI pour Linux (ndi.video). Son
# installateur (Install_NDI_SDK_v6_Linux.sh) fait lire et accepter sa licence, puis crée le
# dossier « NDI SDK for Linux ». Ce script prend la bibliothèque de ce dossier (ou un libndi.so
# posé seul à côté) et l'installe dans /usr/local/lib. Avec --ndi-sdk, il lance lui-même
# l'installateur du SDK : vous lisez et acceptez la licence dans le terminal.
case "$(uname -m)" in
  x86_64) ndi_arch=x86_64-linux-gnu ;;
  aarch64 | arm64) ndi_arch=aarch64 ;;
  *) ndi_arch="$(uname -m)" ;;
esac
# La bibliothèque d'un dossier « NDI SDK for Linux », pour ce processeur (le fichier, pas un lien).
ndi_from_sdk() {
  local d f best=""
  for d in "$1"/lib/"$ndi_arch"*; do
    for f in "$d"/libndi.so*; do
      [ -f "$f" ] && [ ! -L "$f" ] || continue
      if [ -z "$best" ] || [ "$(printf '%s\n%s\n' "$best" "$f" | sort -V | tail -1)" = "$f" ]; then best="$f"; fi
    done
  done
  printf '%s' "$best"
}
# Trouvée à côté : un SDK déjà déballé, ou un libndi.so posé seul.
ndi_search() {
  local d s f
  for d in "$PWD" "$BUILD_HOME" /root /tmp /home/*; do
    for s in "$d/NDI SDK for Linux" "$d"/*/"NDI SDK for Linux"; do
      [ -d "$s" ] || continue
      f="$(ndi_from_sdk "$s")"
      if [ -n "$f" ]; then printf '%s' "$f"; return; fi
    done
    for f in "$d"/libndi.so*; do
      if [ -f "$f" ]; then printf '%s' "$f"; return; fi
    done
  done
}
ndi_new=""
if [ -n "$NDI_SDK" ]; then
  if [ -d "$NDI_SDK" ]; then
    ndi_new="$(ndi_from_sdk "$NDI_SDK")"
  elif [[ "$NDI_SDK" == *.sh ]]; then
    (exec </dev/tty) 2>/dev/null || die "le SDK NDI demande d'accepter sa licence : lancez ce script dans un terminal (SSH)"
    echo
    echo "  SDK NDI : lisez sa licence (Espace pour avancer), puis acceptez-la pour l'installer."
    rm -rf /tmp/strata-cut-ndi
    mkdir -p /tmp/strata-cut-ndi
    (cd /tmp/strata-cut-ndi && bash "$NDI_SDK" </dev/tty >/dev/tty 2>&1) || warn "l'installateur du SDK NDI s'est arrêté"
    ndi_new="$(ndi_from_sdk "/tmp/strata-cut-ndi/NDI SDK for Linux")"
    [ -n "$ndi_new" ] || warn "SDK NDI : aucune bibliothèque pour ce processeur ($ndi_arch) — licence refusée ?"
  else
    ndi_new="$NDI_SDK"
  fi
fi
[ -n "$ndi_new" ] || ndi_new="$(ndi_search)"
if [ -n "$ndi_new" ]; then
  # Sous son nom à numéro de version (libndi.so.6), et libndi.so qui y renvoie.
  major="$(basename "$ndi_new" | sed -n 's/^libndi\.so\.\([0-9]*\).*/\1/p')"
  target="/usr/local/lib/libndi.so${major:+.$major}"
  if ! cmp -s "$ndi_new" "$target"; then
    install -m 0644 "$ndi_new" "$target"
    ok "NDI : bibliothèque installée ($(basename "$ndi_new") → $target)"
  fi
  [ "$target" = /usr/local/lib/libndi.so ] || ln -sf "$(basename "$target")" /usr/local/lib/libndi.so
  ldconfig
fi
ndi_lib=""
for f in /usr/local/lib/libndi.so* /usr/lib/libndi.so* /usr/lib/x86_64-linux-gnu/libndi.so* /usr/lib/aarch64-linux-gnu/libndi.so*; do
  [ -e "$f" ] && ndi_lib="$f"
done
if [ -n "$ndi_lib" ]; then
  # La découverte des sources passe par Avahi (mDNS).
  apt_install avahi-daemon libavahi-client3
  has_systemd && systemctl enable --now avahi-daemon >/dev/null 2>&1 || true
  ok "NDI : bibliothèque présente ($ndi_lib), découverte des sources par Avahi"
else
  sdk=""
  for d in "$PWD" "$BUILD_HOME" /root /tmp /home/*; do
    for f in "$d"/Install_NDI_SDK_*Linux*.sh; do [ -f "$f" ] && sdk="$f"; done
  done
  if [ -n "$sdk" ]; then
    echo "  NDI : installateur du SDK trouvé ($sdk) : relancez ce script avec  --ndi-sdk \"$sdk\"  pour l'installer (sa licence est à accepter)"
  else
    echo "  NDI : bibliothèque absente (facultatif : voir docs/INSTALL-UBUNTU.md, « NDI »)"
  fi
fi
# Cartes DeckLink : le pilote Desktop Video de Blackmagic s'installe à part (site de Blackmagic).
decklink_lib=""
for f in /usr/lib/libDeckLinkAPI.so /usr/lib64/libDeckLinkAPI.so /usr/lib/x86_64-linux-gnu/libDeckLinkAPI.so; do
  [ -e "$f" ] && decklink_lib="$f"
done
if [ -n "$decklink_lib" ]; then
  inputs=(/dev/blackmagic/io*)
  if [ -e "${inputs[0]}" ]; then
    ok "DeckLink : pilote Desktop Video et ${#inputs[@]} entrée(s) de carte"
    for d in "${inputs[@]}"; do
      runuser -u "$SERVICE_USER" -- test -r "$d" -a -w "$d" || warn "DeckLink : le compte $SERVICE_USER ne peut pas ouvrir $d (droits du périphérique)"
    done
  else
    warn "DeckLink : pilote présent mais aucune carte vue (carte en place ? dans une VM : passthrough PCI ; redémarrage après l'installation du pilote ?)"
  fi
else
  echo "  DeckLink : pilote Desktop Video absent (nécessaire seulement pour les cartes Blackmagic)"
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
# yt-dlp pour les directs YouTube, Facebook, Instagram, X et LinkedIn : dans le dossier des données,
# au compte du service, qui le tient à jour chaque jour (les plateformes changent souvent).
YTDLP="$DATA_DIR/bin/yt-dlp"
case "$(uname -m)" in aarch64) ytdlp_asset=yt-dlp_linux_aarch64 ;; *) ytdlp_asset=yt-dlp_linux ;; esac
mkdir -p "$DATA_DIR/bin"
if [ -x "$YTDLP" ]; then
  runuser -u "$SERVICE_USER" -- "$YTDLP" -U >/dev/null 2>&1 || true
elif curl -fsSL --retry 3 -o "$YTDLP.tmp" "https://github.com/yt-dlp/yt-dlp/releases/latest/download/$ytdlp_asset"; then
  chmod 0755 "$YTDLP.tmp"
  mv "$YTDLP.tmp" "$YTDLP"
else
  rm -f "$YTDLP.tmp"
  warn "yt-dlp non téléchargé : les directs web (YouTube, Facebook…) ne pourront pas être enregistrés"
fi
chown -R "$SERVICE_USER:$SERVICE_USER" "$DATA_DIR/bin"
if [ -x "$YTDLP" ]; then ok "yt-dlp $(runuser -u "$SERVICE_USER" -- "$YTDLP" --version 2>/dev/null) (directs web, mis à jour chaque jour)"; fi
# YouTube demande un moteur JavaScript à yt-dlp : Node.js (déjà là quand Strata Cut est compilé ici).
if ! command -v node >/dev/null && ! command -v deno >/dev/null; then
  if [ "$NO_APT" -eq 0 ]; then
    echo "  installation de Node.js $NODE_MAJOR (NodeSource) pour yt-dlp"
    curl -fsSL "https://deb.nodesource.com/setup_${NODE_MAJOR}.x" | bash - >/dev/null
    apt_install nodejs
  else
    warn "Node.js absent : les directs YouTube risquent de ne pas être lus"
  fi
fi

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

  # Disques des boucles et des extraits choisis dans l'interface (Système › Stockage) :
  # le service écrit le dossier voulu dans <données>/storage.request ; cette unité root
  # le crée et le donne au compte du service, seulement s'il finit par strata-cut/loops ou /clips.
  cat >"$STORAGE_HELPER" <<'EOF'
#!/bin/sh
# Strata Cut : relais root de Système › Stockage ($1 = <données>/storage.request, $2 = compte du service).
#   « /<disque>/strata-cut/loops » : crée le dossier et le donne au service ;
#   « extend /dev/mapper/<volume> » : donne au volume LVM tout l'espace libre du disque
#   (fin de disque hors partition : growpart + pvresize, puis lvextend -r : à chaud, sans perte).
# Le résultat (« ok » ou « error: … ») est écrit dans <données>/storage.result.
set -eu
PATH=/usr/local/sbin:/usr/local/bin:/usr/sbin:/usr/bin:/sbin:/bin
result="$(dirname "$1")/storage.result"
d="$(head -n 1 "$1" | head -c 1024)"
# Le résultat, écrit dans un fichier neuf : jamais à travers un lien que le service aurait mis
# à sa place (noclobber : la création échoue si le nom existe, lien compris).
say() { rm -f "$result"; ( set -C; printf '%s\n' "$1" >"$result" ); }

grow() {
  lv="$1"
  case "$lv" in /dev/mapper/*) ;; *) echo "volume refusé : $lv"; return 1 ;; esac
  case "$lv" in *[!A-Za-z0-9._/+-]*) echo "volume refusé : $lv"; return 1 ;; esac
  command -v lvs >/dev/null || { echo "lvm2 absent"; return 1; }
  # Le groupe et le nom du volume (lvextend veut « groupe/volume »).
  real="$(readlink -f "$lv")"
  # shellcheck disable=SC2046
  set -- $(lvs --noheadings -o vg_name,lv_name,lv_dm_path 2>/dev/null | while read -r vg name dm; do
    [ "$(readlink -f "$dm")" = "$real" ] && echo "$vg $name"; done)
  [ $# -eq 2 ] || { echo "pas un volume LVM : $lv"; return 1; }
  vg="$1"; name="$2"
  # Les partitions du groupe qui ont de la place après elles sur leur disque : on les agrandit.
  for pv in $(pvs --noheadings -o pv_name -S "vg_name=$vg" 2>/dev/null); do
    part="$(basename "$(readlink -f "$pv")")"
    [ -r "/sys/class/block/$part/partition" ] || continue
    num="$(cat "/sys/class/block/$part/partition")"
    disk="$(basename "$(readlink -f "/sys/class/block/$part/..")")"
    if command -v growpart >/dev/null; then
      out="$(growpart "/dev/$disk" "$num" 2>&1)" && pvresize "$pv" >/dev/null || case "$out" in *NOCHANGE*) ;; *) echo "growpart : $out" ;; esac
    fi
  done
  free="$(vgs --noheadings --units b --nosuffix -o vg_free "$vg" | tr -d ' ')"
  if [ "${free:-0}" -lt 1073741824 ]; then echo "aucun espace libre dans le groupe $vg"; return 1; fi
  out="$(lvextend -r -l +100%FREE "$vg/$name" 2>&1)" || { echo "$out" | tail -n 1; return 1; }
  echo "$out" | tail -n 3
}

case "$d" in
  "extend "*)
    if out="$(grow "${d#extend }" 2>&1)"; then say ok; echo "$out"; else say "error: $(echo "$out" | tail -n 1)"; echo "$out" >&2; exit 1; fi
    exit 0 ;;
esac
case "$d" in
  *..* | *//* ) echo "dossier refusé : $d" >&2; exit 1 ;;
  /etc/* | /usr/* | /bin/* | /sbin/* | /lib/* | /lib64/* | /boot/* | /proc/* | /sys/* | /dev/* | /run/* | /root/* ) echo "dossier refusé : $d" >&2; exit 1 ;;
  /*/strata-cut/loops | /*/strata-cut/clips | /*/loops | /*/clips ) ;;
  * ) echo "dossier refusé : $d" >&2; exit 1 ;;
esac
case "$d" in *[!A-Za-z0-9._/-]*) echo "dossier refusé : $d" >&2; exit 1 ;; esac
# Aucun lien symbolique sur le chemin : root ne doit pas donner un autre dossier au service.
p=""
for part in $(echo "${d#/}" | tr '/' ' '); do
  p="$p/$part"
  if [ -L "$p" ]; then echo "dossier refusé (lien symbolique) : $p" >&2; exit 1; fi
done
mkdir -p "$d"
if [ "$(realpath "$d")" != "$d" ]; then echo "dossier refusé : $d" >&2; exit 1; fi
parent="$(dirname "$d")"
[ "$(basename "$parent")" = strata-cut ] && chown -h "$2:$2" "$parent"
chown -h "$2:$2" "$d"
[ -L "$d" ] || chmod 0750 "$d"
say ok
EOF
  chmod 0755 "$STORAGE_HELPER"
  chown root:root "$STORAGE_HELPER"
  cat >"/etc/systemd/system/$STORAGE_UNIT.service" <<EOF
[Unit]
Description=$BRAND: prepares the folder chosen in the interface for the loops or the clips

[Service]
Type=oneshot
ExecStart=$STORAGE_HELPER $DATA_DIR/storage.request $SERVICE_USER
EOF
  cat >"/etc/systemd/system/$STORAGE_UNIT.path" <<EOF
[Unit]
Description=$BRAND: watches the folder chosen in the interface for the loops or the clips

[Path]
PathChanged=$DATA_DIR/storage.request

[Install]
WantedBy=multi-user.target
EOF
  systemctl daemon-reload
  systemctl enable --now "$STORAGE_UNIT.path" >/dev/null 2>&1
  ok "disques des boucles et des extraits réglables dans Système › Stockage"

  # Ubuntu ne donne souvent au volume système qu'une partie du disque (16 Go sur 300) :
  # le volume des données (et celui des boucles s'il est ailleurs) prend tout l'espace libre.
  if [ "$NO_GROW" -eq 0 ]; then
    for dir in "$DATA_DIR" "$RECORDINGS_DIR"; do
      [ -n "$dir" ] && [ -d "$dir" ] || continue
      src="$(findmnt -n -o SOURCE --target "$dir" 2>/dev/null || true)"
      case "$src" in /dev/mapper/*) ;; *) continue ;; esac
      before="$(df -h --output=size "$dir" | tail -n 1 | tr -d ' ')"
      tmp="$(mktemp -d)"
      echo "extend $src" >"$tmp/storage.request"
      if out="$("$STORAGE_HELPER" "$tmp/storage.request" "$SERVICE_USER" 2>&1)"; then
        ok "volume $src : $before → $(df -h --output=size "$dir" | tail -n 1 | tr -d ' ') (tout l'espace libre du disque)"
      else
        case "$out" in *"aucun espace libre"*) ok "volume $src : $before, il occupe déjà tout le disque" ;; *) warn "volume $src non agrandi : $(echo "$out" | tail -n 1)" ;; esac
      fi
      rm -f "$tmp/storage.request" "$tmp/storage.result"
      rmdir "$tmp"
    done
  fi
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
