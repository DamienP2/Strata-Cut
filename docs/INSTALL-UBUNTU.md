# Installer Strata Cut sur un serveur Ubuntu — pas à pas

Ce guide installe Strata Cut sur un serveur Ubuntu Server 22.04 ou 24.04 (24.04 conseillé), comme service qui démarre tout seul et redémarre en cas de problème. Comptez 15 à 20 minutes, dont l'essentiel en compilation.

Le script `scripts/install-ubuntu.sh` fait tout le travail ; les étapes ci-dessous expliquent quoi préparer, quoi lancer et quoi vérifier.

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

## 2. Mettre le script sur le serveur

Le script télécharge lui-même le code par SSH depuis GitHub (`git@github.com:DamienP2/Strata-Cut.git`). Il suffit de copier ce seul fichier sur le serveur, au choix :

- **depuis un poste qui a le dépôt** :
  ```bash
  scp scripts/install-ubuntu.sh utilisateur@serveur:
  ```
- **depuis GitHub dans le navigateur** : ouvrez `scripts/install-ubuntu.sh`, cliquez **Raw**, copiez tout, puis sur le serveur :
  ```bash
  nano install-ubuntu.sh   # collez, puis Ctrl-O, Entrée, Ctrl-X
  ```

Le serveur doit pouvoir sortir en SSH vers `github.com` (port 22). Vérification :
```bash
ssh -T git@github.com
```
Une réponse « Permission denied (publickey) » est normale à ce stade : elle prouve que GitHub répond. Un délai dépassé signifie que le pare-feu de sortie bloque le port 22.

### L'accès au dépôt privé : la clé de déploiement

À la première installation, si le compte n'a pas déjà une clé SSH acceptée par GitHub, le script :

1. vérifie la clé d'hôte de `github.com` (empreinte publiée par GitHub), pour ne pas se connecter à un faux serveur ;
2. crée une clé dédiée `~/.ssh/strata-cut_deploy` et l'affiche ;
3. vous demande de l'ajouter au dépôt :
   - GitHub › dépôt **Strata-Cut** › **Settings** › **Deploy keys** › **Add deploy key** (le script affiche le lien direct) ;
   - **Title** : `Strata Cut <nom du serveur>` ;
   - **Key** : la ligne `ssh-ed25519 …` affichée ;
   - ne cochez **pas** « Allow write access » : le serveur n'a besoin que de lire ;
4. attend que vous appuyiez sur Entrée, vérifie l'accès, puis clone le dépôt dans `~/Strata-Cut`.

La clé ne sert qu'à ce dépôt et en lecture seule ; elle se retire à tout moment dans les **Deploy keys**. Le dépôt cloné la réutilise pour les mises à jour.

Vous avez déjà votre propre clé SSH sur le serveur ? Le script s'en sert et ne crée rien. Pour utiliser l'agent SSH de votre poste (connexion `ssh -A`), lancez le script avec `sudo --preserve-env=SSH_AUTH_SOCK bash install-ubuntu.sh`.

Lancez le script depuis votre compte habituel (pas root) : il télécharge et compile avec ce compte, et n'utilise `sudo` que pour installer.

## 3. Lancer l'installation

Le cas le plus courant (boucles sur le disque dédié, compte admin créé à la fin, ports SRT et RTMP ouverts) :

```bash
sudo bash install-ubuntu.sh \
  --recordings-dir /srv/strata-cut/loops \
  --clips-dir /srv/strata-cut/clips \
  --admin admin \
  --open-ports "9000-9010/udp,1935/tcp"
```

Le script :

1. vérifie le système (Ubuntu, 64 bits, systemd) ;
2. installe les paquets nécessaires : FFmpeg, git, Node.js 22, Rust, outils de compilation ;
3. télécharge le code par SSH dans `~/Strata-Cut` (ou le met à jour s'il est déjà là) ;
4. construit l'interface web et compile le serveur ;
5. agrandit les tampons réseau (indispensable pour les flux UDP et SRT) et active la synchronisation de l'heure ;
6. installe le programme dans `/opt/strata-cut` (commande `strata-cut`) ;
7. crée le compte de service `strata-cut` et les dossiers des données ;
8. installe et démarre le service systemd `strata-cut`, puis ouvre les ports dans le pare-feu `ufw` s'il est actif ;
9. crée le compte administrateur (il demande le mot de passe deux fois, 8 caractères au moins).

À la fin, il affiche les adresses de l'interface web.

### Toutes les options

| Option | Rôle | Défaut |
| --- | --- | --- |
| `--repo URL` | dépôt git en SSH | `git@github.com:DamienP2/Strata-Cut.git` |
| `--branch NOM` | branche à installer | branche par défaut du dépôt |
| `--src DIR` | dossier du code source | `~/Strata-Cut` |
| `--no-pull` | n'actualise pas le code | — |
| `--port N` | port de l'interface web | `8080` |
| `--data-dir DIR` | réglages, comptes, journaux | `/var/lib/strata-cut` |
| `--recordings-dir DIR` | boucles | `<données>/loops` |
| `--clips-dir DIR` | extraits | `<données>/clips` |
| `--admin NOM` | crée ce compte administrateur | — |
| `--open-ports LISTE` | ports des sources à ouvrir dans `ufw` | — |
| `--package FICHIER` | installe un paquet déjà construit au lieu de compiler | — |
| `--no-apt` | n'installe aucun paquet | — |
| `--no-service` | n'installe pas le service (essai) | — |
| `--uninstall` / `--purge` | désinstalle (avec `--purge` : efface aussi les données) | — |

---

## 4. Premier accès

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

## 5. Ajouter les sources

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

## 6. Extraire

1. **Extraction** (ou **Extraire** sur la carte d'une source).
2. Placez-vous dans la boucle : clic sur la vue d'ensemble des 24 h, zoom de **24 h** à **10 s**, puis glisser sur la timeline.
3. Ajustez à l'image : `←` `→` (image par image), `Maj` + flèche (une seconde), `J` `K` `L`, `Espace`.
4. **I** marque l'entrée, **O** la sortie (ou tapez les timecodes dans les champs).
5. Donnez un nom et cliquez **Extraire**. La coupe se fait sur l'image exacte, en MP4 H.264 haute qualité + AAC.
6. Le fichier se télécharge depuis la file des extractions ou depuis **Extraits**.

Touche `?` : liste des raccourcis.

---

## 7. Exploiter au quotidien

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

## 8. Mettre à jour

```bash
sudo ~/Strata-Cut/scripts/install-ubuntu.sh
```

Le script récupère la dernière version par SSH (`git pull`), recompile, remplace le programme et redémarre le service. Les réglages, les comptes, les boucles et les extraits restent. Le programme précédent est gardé dans `/opt/strata-cut/strata-cut.previous`.

- Autre branche : `--branch NOM`.
- Réinstaller sans télécharger : `--no-pull`.
- Si le dossier `~/Strata-Cut` a des modifications locales, le code n'est pas actualisé (le script le signale).

Quand des versions publiées existeront sur GitHub, **Système › Mises à jour** permettra aussi de vérifier et d'installer une nouvelle version depuis l'interface, avec retour arrière.

Avec un paquet déjà construit (`Strata-Cut-<version>-linux-x86_64.tar.gz`) :
```bash
sudo ./scripts/install-ubuntu.sh --package Strata-Cut-0.2.0-linux-x86_64.tar.gz
```

---

## 9. En cas de problème

| Symptôme | À vérifier |
| --- | --- |
| La page ne s'ouvre pas | `systemctl status strata-cut` ; port ouvert (`sudo ufw status`) ; bonne adresse IP. |
| « Sans signal » sur une source UDP | Le flux arrive-t-il ? (`ffprobe` ci-dessus) ; interface réseau ; IGMP ; `rp_filter`. |
| « En attente de la source » en SRT / RTMP | Port ouvert dans le pare-feu (SRT en **UDP**, RTMP en **TCP**) ; même phrase secrète des deux côtés ; adresse donnée à la source. |
| Erreur rouge sur la carte | Le message vient de FFmpeg ; « Address already in use » : port déjà pris par un autre programme. |
| Images saccadées, sources qui décrochent | Processeur saturé (**Système › Performances**) : moins de sources par serveur ou machine plus puissante. |
| La boucle n'atteint pas 24 h | Disque plein : **Système › Stockage**, `df -h`. |
| Timecode décalé | `timedatectl` : heure synchronisée et bon fuseau. |
| « accès au dépôt refusé » | La clé affichée est-elle dans **Deploy keys** du bon dépôt ? `ssh -T git@github.com` répond-il ? |
| « impossible de joindre github.com en SSH » | Le pare-feu de sortie bloque le port 22 : ouvrez-le, ou copiez le dépôt sur le serveur et lancez `sudo ./scripts/install-ubuntu.sh` depuis ce dossier. |
| Mot de passe admin perdu | `sudo strata-cut user passwd admin --data-dir /var/lib/strata-cut` |

Les journaux détaillés : `journalctl -u strata-cut -n 200` et `/var/lib/strata-cut/logs/strata-cut.log`.

---

## 10. Désinstaller

```bash
sudo ./scripts/install-ubuntu.sh --uninstall           # garde les données
sudo ./scripts/install-ubuntu.sh --uninstall --purge   # efface aussi /var/lib/strata-cut
```

Les dossiers placés ailleurs (`/srv/strata-cut/...`) et le code source (`~/Strata-Cut`) ne sont jamais effacés par le script : supprimez-les à la main si besoin, et retirez la clé de déploiement sur GitHub.
