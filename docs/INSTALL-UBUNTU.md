# Installer Strata Cut sur un serveur Ubuntu — pas à pas

Ce guide installe Strata Cut sur un serveur Ubuntu Server 22.04 ou 24.04 (24.04 conseillé), comme service qui démarre tout seul et redémarre en cas de problème. Comptez 15 à 20 minutes, dont l'essentiel en compilation.

L'installation tient en une commande (§ 2) ; les autres étapes expliquent quoi préparer avant et quoi vérifier après.

---

## 1. Préparer le serveur

### Matériel

| | Minimum | Conseillé |
| --- | --- | --- |
| Processeur | 4 cœurs | 1 cœur par source + 4 pour les extractions |
| Mémoire | 4 Go | 8 Go + 1 Go par source |
| Disque des boucles | voir ci-dessous | disque dédié, monté par exemple sur `/srv/strata-cut` |
| Réseau | 1 Gb/s | carte dédiée à la réception des flux multicast |

Chaque source enregistrée tourne en permanence : elle reçoit le flux, l'écrit tel quel et prépare une copie légère pour le navigateur (environ un cœur de processeur par source en 1080).

### Espace disque

La boucle garde le flux tel qu'il arrive. Pour une boucle de 24 h :

| Débit de la source | Espace pour 24 h |
| --- | --- |
| 5 Mb/s | ≈ 55 Go |
| 10 Mb/s | ≈ 110 Go |
| 20 Mb/s | ≈ 220 Go |
| 50 Mb/s | ≈ 540 Go |

Formule : débit (Mb/s) × 10,8 = Go par 24 h. Ajoutez environ 2 % pour la copie de lecture, puis 20 % de marge, et la place des extraits.
Une fois installé, la page **Système › Stockage** affiche ce que prend chaque boucle et ce qu'elle prendra pleine.

### Système

1. Installez Ubuntu Server 24.04 LTS (64 bits) et mettez-le à jour :
   ```bash
   sudo apt update && sudo apt full-upgrade -y
   ```
2. Réglez le fuseau horaire : le timecode affiché est l'heure du serveur.
   ```bash
   sudo timedatectl set-timezone Europe/Paris
   ```
3. Vérifiez que l'heure est synchronisée (le script active NTP) :
   ```bash
   timedatectl
   ```
   La ligne `System clock synchronized: yes` doit apparaître. Si votre réseau a son propre serveur de temps, indiquez-le dans `/etc/systemd/timesyncd.conf` (`NTP=ntp.mon-entreprise.fr`) puis `sudo systemctl restart systemd-timesyncd`.

### Disque des boucles (conseillé)

Mettez les boucles sur un disque à part, pour qu'un disque plein ne bloque jamais le système. Exemple avec un disque `/dev/sdb` vide :

```bash
sudo mkfs.ext4 -L strata-loops /dev/sdb
sudo mkdir -p /srv/strata-cut
echo 'LABEL=strata-loops /srv/strata-cut ext4 defaults,noatime 0 2' | sudo tee -a /etc/fstab
sudo mount -a
df -h /srv/strata-cut
```

> Attention : `mkfs` efface le disque. Vérifiez son nom avec `lsblk` avant.

---

## 2. Installer en une commande

Connectez-vous au serveur en SSH avec votre compte habituel (pas root), puis collez :

```bash
curl -fsSL https://raw.githubusercontent.com/DamienP2/Strata-Cut/HEAD/scripts/install-ubuntu.sh | sudo bash -s -- --admin admin
```

C'est tout. Comptez 5 à 15 minutes selon la machine (la compilation prend l'essentiel). À la fin, le script demande deux fois le mot de passe du compte `admin` (8 caractères au moins), puis affiche les adresses de l'interface web.

Le cas conseillé, avec les boucles sur le disque dédié (§ 1) et les ports des sources ouverts dans le pare-feu :

```bash
curl -fsSL https://raw.githubusercontent.com/DamienP2/Strata-Cut/HEAD/scripts/install-ubuntu.sh | sudo bash -s -- \
  --admin admin \
  --recordings-dir /srv/strata-cut/loops \
  --clips-dir /srv/strata-cut/clips \
  --open-ports "9000-9010/udp,1935/tcp"
```

Tout ce qui suit `bash -s --` sont les options du script (tableau ci-dessous). Sans `curl`, `wget -qO- <même adresse> | sudo bash -s -- …` fait la même chose.

> Prudence habituelle : cette commande exécute en administrateur un script téléchargé. Vous pouvez d'abord le lire : `curl -fsSL https://raw.githubusercontent.com/DamienP2/Strata-Cut/HEAD/scripts/install-ubuntu.sh | less`.

### Ce que fait le script

1. vérifie le système (Ubuntu, 64 bits, systemd) ;
2. installe les paquets nécessaires : FFmpeg, git, Node.js 22, Rust, outils de compilation ;
3. télécharge le code dans `~/Strata-Cut` (ou le met à jour s'il est déjà là) ;
4. construit l'interface web et compile le serveur, avec votre compte (pas root) ;
5. agrandit les tampons réseau (indispensable pour les flux UDP et SRT) et active la synchronisation de l'heure ;
6. installe le programme dans `/opt/strata-cut` (commande `strata-cut`) ;
7. crée le compte de service `strata-cut` et les dossiers des données ;
8. installe et démarre le service systemd `strata-cut`, puis ouvre les ports dans le pare-feu `ufw` s'il est actif ;
9. crée le compte administrateur.

### Toutes les options

| Option | Rôle | Défaut |
| --- | --- | --- |
| `--admin NOM` | crée ce compte administrateur (demande le mot de passe) | — |
| `--recordings-dir DIR` | boucles | `<données>/loops` |
| `--clips-dir DIR` | extraits | `<données>/clips` |
| `--open-ports LISTE` | ports des sources à ouvrir dans `ufw` | — |
| `--port N` | port de l'interface web | `8080` |
| `--data-dir DIR` | réglages, comptes, journaux | `/var/lib/strata-cut` |
| `--branch NOM` | branche à installer | branche par défaut du dépôt |
| `--src DIR` | dossier du code source | `~/Strata-Cut` |
| `--repo URL` | dépôt git (HTTPS, ou SSH `git@…` avec clé de déploiement créée au besoin) | `https://github.com/DamienP2/Strata-Cut.git` |
| `--no-pull` | réinstalle sans télécharger | — |
| `--package FICHIER` | installe un paquet déjà construit au lieu de compiler | — |
| `--no-apt` | n'installe aucun paquet | — |
| `--no-service` | n'installe pas le service (essai) | — |
| `--uninstall` / `--purge` | désinstalle (avec `--purge` : efface aussi les données) | — |

### Sans accès à GitHub depuis le serveur

Si le serveur ne sort pas sur Internet, préparez le code ailleurs puis copiez-le (`scp -r Strata-Cut serveur:`), et lancez depuis ce dossier : `sudo ./Strata-Cut/scripts/install-ubuntu.sh --no-pull`. Les paquets Ubuntu (FFmpeg, Node.js, Rust) doivent alors venir d'un miroir local.

## 3. Premier accès

1. Si vous n'avez pas mis `--admin`, créez le compte administrateur :
   ```bash
   sudo strata-cut user add admin --data-dir /var/lib/strata-cut
   ```
   (La page de création du premier compte ne s'ouvre que sur le serveur lui-même, par sécurité.)
2. Depuis un poste du réseau, ouvrez `http://<adresse-du-serveur>:8080/` et connectez-vous.
3. Menu utilisateur (en haut à droite) : choisissez la langue et le thème.
4. **Système › Utilisateurs** : créez les comptes de l'équipe.
   - **Administrateur** : tout, y compris les enregistreurs et le système.
   - **Opérateur** : regarde les boucles, crée et supprime des extraits.
   - **Lecteur** : regarde les boucles et télécharge les extraits.
5. **Système › Stockage** : vérifiez les dossiers et la place libre.

---

## 4. Ajouter les sources

**Enregistreurs › + Nouvel enregistreur.** Réglez la durée de la boucle (24 h par défaut), puis selon le protocole :

### UDP / RTP multicast (MPEG-TS)

- **Adresse** : `udp://239.1.1.10:1234` (ou `rtp://239.1.1.10:5004`)
- **Interface réseau** : l'adresse IP de la carte qui reçoit le multicast, si le serveur en a plusieurs.

Vérifier que le flux arrive bien (Ctrl-C pour arrêter) :
```bash
ffprobe -hide_banner "udp://239.1.1.10:1234?timeout=5000000"
```
Si rien n'arrive : la carte doit être sur le bon VLAN, le commutateur doit transmettre le multicast (IGMP), et le filtrage de route inverse peut bloquer les flux venant d'un autre réseau :
```bash
echo 'net.ipv4.conf.all.rp_filter = 2' | sudo tee /etc/sysctl.d/91-multicast.conf && sudo sysctl --system
```

### SRT

- **Écoute** : la source se connecte au serveur. Choisissez un **port** (9000, 9001…) ; l'interface affiche l'adresse à donner à la source : `srt://<serveur>:9001`. Le port doit être ouvert en **UDP**.
- **Appel** : le serveur va chercher la source à l'adresse `srt://<source>:<port>`.
- **Latence** : 120 ms sur un réseau local, 500 à 2000 ms sur Internet.
- **Phrase secrète** : facultative, 10 à 79 caractères, la même des deux côtés.

Test depuis un poste avec FFmpeg :
```bash
ffmpeg -re -f lavfi -i testsrc2=size=1920x1080:rate=25 -f lavfi -i sine -c:v libx264 -preset veryfast -b:v 8M -c:a aac -f mpegts "srt://<serveur>:9001?mode=caller&latency=120000"
```

### RTMP

- **Écoute** : port `1935` et chemin `live/<nom>`. Dans OBS ou l'encodeur : serveur `rtmp://<serveur>:1935/live`, clé `<nom>`. Le port doit être ouvert en **TCP**.
- **Appel** : le serveur lit un flux RTMP existant (`rtmp://serveur/app/clé`).

Une seule source par enregistreur, et un port par enregistreur en écoute.

Dès qu'une source envoie, sa carte passe en **REC** et la boucle se remplit. Si la source s'arrête, la carte indique **Sans signal** et l'enregistrement reprend tout seul à son retour.

---

## 5. Extraire

1. **Extraction** (ou **Extraire** sur la carte d'une source).
2. Placez-vous dans la boucle : clic sur la vue d'ensemble des 24 h, zoom de **24 h** à **10 s**, puis glisser sur la timeline.
3. Ajustez à l'image : `←` `→` (image par image), `Maj` + flèche (une seconde), `J` `K` `L`, `Espace`.
4. **I** marque l'entrée, **O** la sortie (ou tapez les timecodes dans les champs).
5. Donnez un nom et cliquez **Extraire**. La coupe se fait sur l'image exacte, en MP4 H.264 haute qualité + AAC.
6. Le fichier se télécharge depuis la file des extractions ou depuis **Extraits**.

Touche `?` : liste des raccourcis.

---

## 6. Exploiter au quotidien

| Pour | Commande |
| --- | --- |
| État du service | `systemctl status strata-cut` |
| Journal en direct | `journalctl -u strata-cut -f` |
| Redémarrer | `sudo systemctl restart strata-cut` |
| Arrêter / démarrer | `sudo systemctl stop strata-cut` · `sudo systemctl start strata-cut` |
| Gérer les comptes | `sudo strata-cut user list --data-dir /var/lib/strata-cut` (aussi `add`, `passwd`, `delete`) |
| Place disque | `df -h /srv/strata-cut` |

Dans l'interface, **Système › Santé** regroupe les erreurs et les plantages, et **Système › Performances** montre le processeur et la mémoire.

### Sauvegarder

Les réglages, comptes et la liste des extraits tiennent dans quelques fichiers du dossier des données :

```bash
sudo tar -czf strata-cut-reglages-$(date +%F).tar.gz -C /var/lib/strata-cut config.json users.json recorders.json clips.json prefs update
```

Les boucles se renouvellent d'elles-mêmes : inutile de les sauvegarder. Sauvegardez le dossier des extraits si vous les gardez sur le serveur.

---

## 7. Mettre à jour

Relancez la commande d'installation, ou plus court :

```bash
sudo ~/Strata-Cut/scripts/install-ubuntu.sh
```

Le script récupère la dernière version (`git pull`), recompile, remplace le programme et redémarre le service. Les réglages, les comptes, les boucles et les extraits restent. Le programme précédent est gardé dans `/opt/strata-cut/strata-cut.previous`.

- Autre branche : `--branch NOM`.
- Réinstaller sans télécharger : `--no-pull`.
- Si le dossier `~/Strata-Cut` a des modifications locales, le code n'est pas actualisé (le script le signale).

Quand des versions publiées existeront sur GitHub, **Système › Mises à jour** permettra aussi de vérifier et d'installer une nouvelle version depuis l'interface, avec retour arrière.

Avec un paquet déjà construit (`Strata-Cut-<version>-linux-x86_64.tar.gz`) :
```bash
sudo ./scripts/install-ubuntu.sh --package Strata-Cut-0.2.0-linux-x86_64.tar.gz
```

---

## 8. En cas de problème

| Symptôme | À vérifier |
| --- | --- |
| La page ne s'ouvre pas | `systemctl status strata-cut` ; port ouvert (`sudo ufw status`) ; bonne adresse IP. |
| « Sans signal » sur une source UDP | Le flux arrive-t-il ? (`ffprobe` ci-dessus) ; interface réseau ; IGMP ; `rp_filter`. |
| « En attente de la source » en SRT / RTMP | Port ouvert dans le pare-feu (SRT en **UDP**, RTMP en **TCP**) ; même phrase secrète des deux côtés ; adresse donnée à la source. |
| Erreur rouge sur la carte | Le message vient de FFmpeg ; « Address already in use » : port déjà pris par un autre programme. |
| Images saccadées, sources qui décrochent | Processeur saturé (**Système › Performances**) : moins de sources par serveur ou machine plus puissante. |
| La boucle n'atteint pas 24 h | Disque plein : **Système › Stockage**, `df -h`. |
| Timecode décalé | `timedatectl` : heure synchronisée et bon fuseau. |
| « impossible de lire le dépôt » | Le serveur sort-il sur Internet ? `curl -I https://github.com` ; derrière un proxy, exportez `https_proxy` et lancez `sudo -E bash`. |
| Mot de passe admin perdu | `sudo strata-cut user passwd admin --data-dir /var/lib/strata-cut` |

Les journaux détaillés : `journalctl -u strata-cut -n 200` et `/var/lib/strata-cut/logs/strata-cut.log`.

---

## 9. Désinstaller

```bash
sudo ./scripts/install-ubuntu.sh --uninstall           # garde les données
sudo ./scripts/install-ubuntu.sh --uninstall --purge   # efface aussi /var/lib/strata-cut
```

Les dossiers placés ailleurs (`/srv/strata-cut/...`) et le code source (`~/Strata-Cut`) ne sont jamais effacés par le script : supprimez-les à la main si besoin.
