# Strata Cut

Enregistre des flux vidéo en continu, en boucle (24 h par défaut, réglable par source : le plus ancien est effacé au fur et à mesure), et en extrait des passages à l'image près.

- Sources : UDP, RTP (MPEG-TS), RTMP et SRT (écoute ou appel). NDI, SDI (Blackmagic) et ST 2110 viendront plus tard.
- Enregistrement : le flux tel quel, sans réencodage (segments MPEG-TS de 6 s), plus un proxy léger pour le navigateur.
- Extraits : MP4 H.264 haute qualité (CRF 16) + AAC 256 kb/s, coupés sur l'image exacte de l'entrée, téléchargeables.
- Serveur Rust (axum) qui embarque l'interface web (React). Linux.

## Installer sur un serveur Ubuntu

Connecté en SSH au serveur (Ubuntu 22.04 ou 24.04), une seule commande :

```bash
curl -fsSL https://raw.githubusercontent.com/DamienP2/Strata-Cut/HEAD/scripts/install-ubuntu.sh | sudo bash -s -- --admin admin
```

Elle télécharge le code dans `~/Strata-Cut`, installe ce qui manque (FFmpeg, Node.js, Rust), compile, installe le service systemd `strata-cut`, puis demande le mot de passe du compte `admin`. À la fin, elle affiche l'adresse de l'interface web (port 8080).

Avec un disque dédié aux boucles et les ports des sources ouverts dans le pare-feu :

```bash
curl -fsSL https://raw.githubusercontent.com/DamienP2/Strata-Cut/HEAD/scripts/install-ubuntu.sh | sudo bash -s -- --admin admin --recordings-dir /srv/strata-cut/loops --open-ports "9000-9010/udp,1935/tcp"
```

Mettre à jour (récupère la dernière version, recompile, redémarre ; les données restent) : relancez la même commande, ou

```bash
sudo ~/Strata-Cut/scripts/install-ubuntu.sh
```

Guide pas à pas complet : [docs/INSTALL-UBUNTU.md](docs/INSTALL-UBUNTU.md).

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

## Publier une version (mises à jour depuis l'interface)

Le menu **Système › Mises à jour** des serveurs installés propose les **releases GitHub** du dépôt, pas les simples commits. Pour en publier une, au choix :

- **depuis GitHub** : onglet **Actions › Release › Run workflow**, branche par défaut, version `0.3.0`, puis **Run workflow** ;
- **depuis un poste** :
  ```bash
  scripts/set-version.sh v0.3.0 && git commit -am "Version 0.3.0" && git push
  git tag v0.3.0 && git push origin v0.3.0
  ```

Le workflow `Release` construit l'interface et le serveur, puis publie `Strata-Cut-0.3.0-linux-x86_64.tar.gz` et `…-linux-arm64.tar.gz` dans la release (une dizaine de minutes). Les serveurs la voient à la vérification suivante (automatique toutes les 6 h, ou « Vérifier maintenant ») et l'installent en un clic, avec retour arrière possible.

Pensez à passer aussi la version dans le code (`scripts/set-version.sh`, premier cas : faites-le avant ou après) pour que les serveurs mis à jour par le script d'installation affichent le même numéro.
