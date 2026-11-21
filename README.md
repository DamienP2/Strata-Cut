# Strata Cut

Enregistre des flux vidéo en continu, en boucle (24 h par défaut, réglable par source : le plus ancien est effacé au fur et à mesure), et en extrait des passages à l'image près.

- Sources : UDP, RTP (MPEG-TS), RTMP et SRT (écoute ou appel), flux HLS (m3u8), directs YouTube, cartes Blackmagic DeckLink, UltraStudio et Intensity (SDI, HDMI, du PAL au 8K), mire générée (barres, mouvement, synchro image/son, logo, du 480i au 2160p50). NDI et ST 2110 viendront plus tard.
- Enregistrement : le flux tel quel, sans réencodage (segments MPEG-TS de 6 s), plus un proxy léger pour le navigateur. Une carte DeckLink donne des images brutes : elles sont encodées en H.264 au débit choisi (25 Mb/s par défaut).
- Extraits : MP4 H.264 haute qualité (CRF 16) + AAC 256 kb/s, coupés sur l'image exacte de l'entrée, téléchargeables.
- Programmation : chaque source s'enregistre en continu, pendant ses créneaux, ou pas du tout (elle reste reçue pour l'aperçu et la diffusion).
- Streamer : une source diffusée en direct vers une ou plusieurs destinations (RTMP, RTMPS, SRT), en copie du flux ou réencodée ; clé de stream fixe ou donnée par l'opérateur à chaque direct, test avant la mise en ligne. La diffusion ne touche jamais l'enregistrement.
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

### Cartes Blackmagic (DeckLink, UltraStudio, Intensity) : le pilote Desktop Video

Blackmagic distribue son pilote Linux après un formulaire : il se télécharge sur votre ordinateur, puis se copie sur le serveur par SSH. Le script d'installation guide et attend.

1. **Sur votre ordinateur**, ouvrez la page Support de Blackmagic (https://www.blackmagicdesign.com/fr/support/family/capture-and-playback) et téléchargez **Desktop Video** pour **Linux** (Blackmagic demande ses coordonnées et l'accord de sa licence). Vous obtenez une archive `Blackmagic_Desktop_Video_Linux_<version>.tar.gz`.
2. **Copiez-la sur le serveur par SSH**, depuis un terminal de votre ordinateur (`<compte>` : le compte avec lequel vous vous connectez au serveur, `<serveur>` : son adresse) :

   macOS / Linux :

   ```bash
   scp ~/Downloads/Blackmagic_Desktop_Video_Linux_*.tar.gz <compte>@<serveur>:~/
   ```

   Windows (PowerShell) :

   ```powershell
   scp $HOME\Downloads\Blackmagic_Desktop_Video_Linux_*.tar.gz <compte>@<serveur>:~/
   ```

   (WinSCP ou FileZilla en SFTP font la même chose.)
3. **Lancez le script d'installation** (ou relancez-le). S'il voit une carte Blackmagic sans pilote, il affiche ces mêmes étapes avec la bonne commande `scp` pour ce serveur, puis **attend** : appuyez sur Entrée quand la copie est finie. Il vérifie que l'archive est là et complète (sinon il le dit et attend encore), installe le pilote, construit son module pour le noyau en cours, puis continue l'installation. Tapez `p` pour passer sans pilote.

   L'archive peut aussi être copiée avant de lancer le script : il la trouve seul (dossier personnel, `/home/*`, `/root`, `/tmp`), ou on l'indique avec `--desktop-video /chemin/de/l/archive`. Sans carte détectée (par exemple avant de l'installer), `--desktop-video wait` demande quand même l'archive.
4. **Si le script le demande, redémarrez** le serveur (le module du pilote se charge au démarrage), puis relancez le script. Avec Secure Boot actif, le module doit être signé (clé MOK enrôlée au redémarrage).

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

Le menu **Système › Maintenance** des serveurs installés propose les **releases GitHub** du dépôt, pas les simples commits. Pour en publier une, au choix :

- **depuis GitHub** : onglet **Actions › Release › Run workflow**, branche par défaut, version `0.3.0`, puis **Run workflow** ;
- **depuis un poste** :
  ```bash
  scripts/set-version.sh v0.3.0 && git commit -am "Version 0.3.0" && git push
  git tag v0.3.0 && git push origin v0.3.0
  ```

Le workflow `Release` construit l'interface et le serveur, puis publie `Strata-Cut-0.3.0-linux-x86_64.tar.gz` et `…-linux-arm64.tar.gz` dans la release (une dizaine de minutes). Les serveurs la voient à la vérification suivante (automatique toutes les 6 h, ou « Vérifier maintenant ») et l'installent en un clic, avec retour arrière possible.

Pensez à passer aussi la version dans le code (`scripts/set-version.sh`, premier cas : faites-le avant ou après) pour que les serveurs mis à jour par le script d'installation affichent le même numéro.
