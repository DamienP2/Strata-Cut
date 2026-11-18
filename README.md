# Strata Cut

Enregistre des flux vidéo en continu, en boucle (24 h par défaut, réglable par source : le plus ancien est effacé au fur et à mesure), et en extrait des passages à l'image près.

- Sources : UDP, RTP (MPEG-TS), RTMP et SRT (écoute ou appel). NDI, SDI (Blackmagic) et ST 2110 viendront plus tard.
- Enregistrement : le flux tel quel, sans réencodage (segments MPEG-TS de 6 s), plus un proxy léger pour le navigateur.
- Extraits : MP4 H.264 haute qualité (CRF 16) + AAC 256 kb/s, coupés sur l'image exacte de l'entrée, téléchargeables.
- Serveur Rust (axum) qui embarque l'interface web (React). Linux.

## Installer sur un serveur Ubuntu

Le script `scripts/install-ubuntu.sh` télécharge le code par SSH depuis GitHub, le compile et installe Strata Cut comme service systemd. Guide complet : [docs/INSTALL-UBUNTU.md](docs/INSTALL-UBUNTU.md).

1. Copiez le script sur le serveur (depuis un poste qui a le dépôt) :
   ```bash
   scp scripts/install-ubuntu.sh utilisateur@serveur:
   ```
2. Sur le serveur, lancez-le depuis votre compte habituel :
   ```bash
   sudo bash install-ubuntu.sh --admin admin
   ```
3. La première fois, le script crée une clé SSH et l'affiche : ajoutez-la sur GitHub dans **Settings › Deploy keys › Add deploy key** du dépôt (lecture seule, sans cocher « Allow write access »), puis appuyez sur Entrée. Il clone le dépôt dans `~/Strata-Cut`, compile et installe.

Mettre à jour (le script récupère la dernière version par SSH, puis réinstalle) :

```bash
sudo ~/Strata-Cut/scripts/install-ubuntu.sh
```

Si le compte a déjà une clé SSH acceptée par GitHub, aucune clé de déploiement n'est créée. Options utiles : `--branch NOM`, `--src DIR`, `--repo URL`, `--no-pull` ; `--help` pour la liste complète.

## Prérequis (développement)

`ffmpeg` et `ffprobe` dans le `PATH` (avec libsrt et libx264), ou `SC_FFMPEG=/chemin/ffmpeg`.

## Lancer

```bash
cd web && npm install && npm run build && cd ..
cargo run --release -p strata-cut-server -- --data-dir data
```

Puis ouvrir http://localhost:8080 sur la machine elle-même pour créer le compte administrateur (ou `strata-cut user add admin`).

Service systemd : `sudo strata-cut service install --bind 0.0.0.0:8080`.

## Vérifier

```bash
cargo clippy --workspace --all-targets -- -D warnings && cargo test --workspace
cd web && npm run build && node scripts/i18n-check.mjs
```
