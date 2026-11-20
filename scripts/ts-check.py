#!/usr/bin/env python3
"""Compte les paquets perdus d'un flux MPEG-TS en UDP (ou RTP), là où il est lancé.

Lancé au même moment à plusieurs endroits du chemin (le serveur Strata Cut, l'hôte de la VM,
le poste où VLC est propre), il dit où les paquets se perdent : là où il en compte, et pas
avant.

    python3 ts-check.py 239.1.1.10:1234                 # 60 s, la carte par défaut
    python3 ts-check.py 239.1.1.10:1234 -i 10.0.0.5 -t 300

Aucune dépendance (Python 3.6 et plus ; Linux, macOS). Toutes les 10 s : paquets manquants (en combien de
moments, et combien au plus d'un coup), reçus deux fois, marqués en erreur par la source.
"""
import argparse
import socket
import struct
import sys
import time


def main():
    ap = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    ap.add_argument("address", help="groupe:port (ou adresse:port), ex. 239.1.1.10:1234")
    ap.add_argument("-i", "--iface", default="0.0.0.0", help="adresse IP de la carte qui reçoit le multicast")
    ap.add_argument("-t", "--time", type=float, default=60, help="durée en secondes (60)")
    a = ap.parse_args()
    host, port = a.address.replace("udp://", "").replace("rtp://", "").lstrip("@").rsplit(":", 1)
    port = int(port)

    s = socket.socket(socket.AF_INET, socket.SOCK_DGRAM, socket.IPPROTO_UDP)
    s.setsockopt(socket.SOL_SOCKET, socket.SO_REUSEADDR, 1)
    # Le plus grand tampon que le système accepte (macOS en refuse de trop grands).
    for size in (32 << 20, 8 << 20, 4 << 20, 2 << 20, 1 << 20):
        try:
            s.setsockopt(socket.SOL_SOCKET, socket.SO_RCVBUF, size)
            break
        except OSError:
            continue
    if hasattr(socket, "SO_REUSEPORT"):
        try:
            s.setsockopt(socket.SOL_SOCKET, socket.SO_REUSEPORT, 1)  # macOS : à côté de VLC
        except OSError:
            pass
    multicast = 224 <= int(host.split(".")[0]) <= 239
    s.bind((host if multicast else "0.0.0.0", port))
    if multicast:
        s.setsockopt(socket.IPPROTO_IP, socket.IP_ADD_MEMBERSHIP, socket.inet_aton(host) + socket.inet_aton(a.iface))
    s.settimeout(1.0)

    last_cc = {}
    rtp_last = None
    total = {"datagrams": 0, "missing": 0, "twice": 0, "marked": 0, "moments": 0}
    win = dict.fromkeys(total, 0)
    win["worst"] = 0
    moment_at, moment_lost = 0.0, 0
    kind = None
    start = report = time.time()
    print("à l'écoute de %s:%d (carte %s) pendant %d s…" % (host, port, a.iface, a.time), flush=True)
    while time.time() - start < a.time:
        try:
            d = s.recv(65536)
        except socket.timeout:
            print("  rien reçu depuis 1 s", flush=True)
            continue
        now = time.time()
        total["datagrams"] += 1
        win["datagrams"] += 1
        missing = twice = marked = 0
        ts = d
        # RTP (en-tête numéroté devant les paquets MPEG-TS) : ses numéros disent ce qui manque.
        if len(d) >= 12 + 188 and d[0] >> 6 == 2 and d[1] & 0x7F == 33:
            kind = kind or "RTP"
            head = 12 + 4 * (d[0] & 0x0F)
            if d[0] & 0x10:
                head += 4 + 4 * struct.unpack("!H", d[head + 2:head + 4])[0]
            ts = d[head:]
            n = struct.unpack("!H", d[2:4])[0]
            if rtp_last is not None:
                gap = (n - rtp_last) & 0xFFFF
                if gap == 0:
                    twice += 1
                elif 1 < gap < 1000:
                    missing += gap - 1
            rtp_last = n
        else:
            kind = kind or "UDP (MPEG-TS seul)"
        for i in range(0, len(ts) - 187, 188):
            p = ts[i:i + 188]
            if p[0] != 0x47:
                break
            if p[1] & 0x80:
                marked += 1
            if kind != "UDP (MPEG-TS seul)":
                continue
            pid = ((p[1] & 0x1F) << 8) | p[2]
            afc = (p[3] >> 4) & 3
            cc = p[3] & 0x0F
            if pid == 0x1FFF or not afc & 1:
                continue
            announced = afc == 3 and p[4] > 0 and p[5] & 0x80
            if pid in last_cc and not announced:
                expected = (last_cc[pid] + 1) & 0x0F
                if cc == last_cc[pid]:
                    twice += 1
                elif cc != expected:
                    missing += (cc - expected) & 0x0F
            last_cc[pid] = cc
        if missing:
            if now - moment_at > 0.1:
                moment_lost = 0
                win["moments"] += 1
                total["moments"] += 1
            moment_at = now
            moment_lost += missing
            win["worst"] = max(win["worst"], moment_lost)
        for k, v in (("missing", missing), ("twice", twice), ("marked", marked)):
            total[k] += v
            win[k] += v
        if now - report >= 10:
            print("%s  %s : %d datagrammes, %d paquets manquants%s, %d reçus deux fois, %d marqués en erreur" % (
                time.strftime("%H:%M:%S"), kind, win["datagrams"], win["missing"],
                " (en %d moments, jusqu'à %d d'un coup)" % (win["moments"], win["worst"]) if win["missing"] else "",
                win["twice"], win["marked"]), flush=True)
            win = dict.fromkeys(win, 0)
            report = now
    print("TOTAL %s en %d s : %d datagrammes, %d paquets manquants en %d moments, %d reçus deux fois, %d marqués en erreur" % (
        kind or "rien", a.time, total["datagrams"], total["missing"], total["moments"], total["twice"], total["marked"]))
    return 0 if total["datagrams"] else 1


if __name__ == "__main__":
    sys.exit(main())
