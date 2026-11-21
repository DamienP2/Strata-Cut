import { lang, t } from "./i18n";

/**
 * The server's journal (Système › Santé) in the interface's language. The server writes it in
 * English, for the support; its usual sentences are put in French here, the rest (FFmpeg's own
 * lines) stays as written.
 */
type Rule = [RegExp, (m: RegExpMatchArray) => string];

/** A server message known to the interface ("udp: nothing received …"), else as it is. */
const known = (s: string) => {
  if (FFMPEG[s.trim()]) return FFMPEG[s.trim()];
  const out = t(s);
  if (out !== s) return out.replace(/\.$/, "");
  const tail = s.match(/^(.+?): (.+)$/);
  if (tail && t(tail[2]) !== tail[2])
    return `${tail[1]} : ${t(tail[2]).replace(/\.$/, "")}`;
  return s;
};
/** FFmpeg's own sentences that come back often, said plainly. */
const FFMPEG: Record<string, string> = {
  "Nothing was written into output file, because at least one of its streams received no packets.":
    "rien n’a été envoyé : la source n’a rien transmis",
  "Error opening input files: Input/output error":
    "impossible d’ouvrir la source (erreur d’entrée/sortie)",
};
const exit = (s: string) =>
  s.replace(/^exit status: (\d+)$/, "code de sortie $1");
const REASON: Record<string, string> = {
  cpu: "processeur",
  memory: "mémoire",
  upload: "débit sortant",
  out: "débit sortant",
};
const CONTENT: Record<string, string> = {
  "black picture": "image noire",
  "frozen picture": "image figée",
  silence: "silence",
};
const moments = (s: string) =>
  s.replace(
    / \(in (\d+) moments?, up to (\d+) at once\)/,
    " (en $1 moments, jusqu’à $2 d’un coup)",
  );

const RULES: Rule[] = [
  [
    /^the server stopped unexpectedly$/,
    () => "Le serveur s’est arrêté de façon inattendue",
  ],
  [
    /^process (\d+) \(version ([^)]+)\) started at (\S+) did not stop by itself: killed, out of memory, power cut or crash$/,
    (m) =>
      `Le processus ${m[1]} (version ${m[2]}), démarré le ${m[3]}, ne s’est pas arrêté de lui-même : tué, mémoire saturée, coupure de courant ou plantage`,
  ],
  [
    /^streaming to (.+?) again after the restart \(started by (.+?)\): (.+)$/,
    (m) => `Diffusion de ${m[3]} vers ${m[1]} reprise après le redémarrage (lancée par ${m[2]})`,
  ],
  [
    /^streaming to (.+?): not started again after the restart: (.+)$/,
    (m) => `Diffusion vers ${m[1]} : non reprise après le redémarrage : ${known(m[2])}`,
  ],
  [
    /^streaming to (.+?): could not start: (.+)$/,
    (m) => `Diffusion vers ${m[1]} : n’a pas pu démarrer : ${known(m[2])}`,
  ],
  [
    /^streaming to (.+?): (.+)$/,
    (m) => `Diffusion vers ${m[1]} : ${known(m[2])}`,
  ],
  [
    /^(.+?) sends (.+?) to (.+)$/,
    (m) => `${m[1]} diffuse ${m[2]} vers ${m[3]}`,
  ],
  [
    /^(.+?) stops the stream to (.+)$/,
    (m) => `${m[1]} arrête la diffusion vers ${m[2]}`,
  ],
  [
    /^(.+?): damaged in the last 10 minutes: (\d+) packets of picture, (\d+) of sound$/,
    (m) =>
      `${m[1]} : abîmés ces 10 dernières minutes : ${m[2]} paquets d’image, ${m[3]} de son`,
  ],
  [
    /^(?:ndi|decklink) (.+?): (\d+) picture\(s\) repeated in the last minute .*$/,
    (m) =>
      `${m[1]} : ${m[2]} image(s) répétée(s) dans la dernière minute (non reçues, ou l’encodeur n’a pas suivi : le serveur manque de processeur)`,
  ],
  [
    /^decklink (.+?): the encoder could not keep up with (\d+) picture\(s\) .*$/,
    (m) =>
      `${m[1]} : l’encodeur n’a pas suivi ${m[2]} image(s) dans la dernière minute (répétées à leur place : le serveur manque de processeur)`,
  ],
  [
    /^decklink (.+?): (\d+) frame\(s\) lost in the last minute.*$/,
    (m) =>
      `${m[1]} : ${m[2]} image(s) perdue(s) dans la dernière minute : la capture n’a pas suivi (le serveur manque de processeur)`,
  ],
  [
    /^(.+?): the source's clock jumped \(([\d.]+) s → ([\d.]+) s\): a new recording starts$/,
    (m) =>
      `${m[1]} : l’horloge de la source a sauté (${m[2]} s → ${m[3]} s) : un nouvel enregistrement commence`,
  ],
  [
    /^(.+?): the server is too busy, the preview is (\d+) s behind the recording \(the recording is whole\)$/,
    (m) =>
      `${m[1]} : serveur trop chargé, l’aperçu a ${m[2]} s de retard sur l’enregistrement (l’enregistrement est complet)`,
  ],
  [
    /^(.+?): the server is too busy, the preview drops some of the stream \(the recording is whole\)$/,
    (m) =>
      `${m[1]} : serveur trop chargé, l’aperçu perd une partie du flux (l’enregistrement est complet)`,
  ],
  [
    /^(.+?): no signal on the card's input \(waiting for it in (.+)\)$/,
    (m) =>
      `${m[1]} : aucun signal sur l’entrée de la carte (attendu en ${m[2]})`,
  ],
  [
    /^(.+?): no signal on the card's input$/,
    (m) => `${m[1]} : aucun signal sur l’entrée de la carte`,
  ],
  [
    /^(.+?): the card's input has a signal again$/,
    (m) => `${m[1]} : l’entrée de la carte a de nouveau un signal`,
  ],
  [
    /^(.+?): ffmpeg ended \(((?:exit status|signal): [^()]*(?:\([^)]*\))?)\):?\s*(.*)$/,
    (m) =>
      `${m[1]} : FFmpeg s’est arrêté (${exit(m[2])})${m[3] ? ` : ${known(m[3])}` : ""}`,
  ],
  [
    /^(.+?): the preview stopped \(((?:exit status|signal): [^()]*(?:\([^)]*\))?)\):?\s*(.*)$/,
    (m) =>
      `${m[1]} : l’aperçu s’est arrêté (${exit(m[2])})${m[3] ? ` : ${known(m[3])}` : ""}`,
  ],
  [
    /^disk space: the oldest recordings were removed \((\d+) MB\) to stay within the limits$/,
    (m) =>
      `Espace disque : les enregistrements les plus anciens ont été supprimés (${m[1]} Mo) pour rester dans les limites`,
  ],
  [/^(.+?): loop emptied$/, (m) => `${m[1]} : boucle vidée`],
  [
    /^(.+?): its loop was deleted \((.+)\)$/,
    (m) => `${m[1]} : sa boucle a été supprimée (${m[2]})`,
  ],
  [
    /^(.+?): recording \((.+)\)$/,
    (m) => `${m[1]} : enregistrement en cours (${m[2]})`,
  ],
  [/^(.+?): the source stopped$/, (m) => `${m[1]} : la source s’est arrêtée`],
  [
    /^(.+?): no audio in the source$/,
    (m) => `${m[1]} : la source n’a pas de son`,
  ],
  [
    /^(.+?): end of the programmed slot \(or of its last stream\)$/,
    (m) =>
      `${m[1]} : fin de l’enregistrement programmé (ou de sa dernière diffusion)`,
  ],
  [
    /^(.+?): recording without the programme guide \(FFmpeg did not start with it\)$/,
    (m) =>
      `${m[1]} : enregistrement sans le guide des programmes (FFmpeg n’a pas démarré avec)`,
  ],
  [
    /^(.+?): the source carries a programme guide$/,
    (m) => `${m[1]} : la source transmet un guide des programmes`,
  ],
  [
    /^(.+?): this UDP source sends RTP: read as RTP .*$/,
    (m) =>
      `${m[1]} : cette source UDP envoie du RTP : lue en RTP (les datagrammes arrivés dans le désordre sont remis dans l’ordre)`,
  ],
  [
    /^(.+?): as it arrives on the network card, in the last 10 minutes: (\d+) packets missing(.*?), (\d+) sent twice, (\d+) marked in error by the source: the source or the network, not this server$/,
    (m) =>
      `${m[1]} : à l’arrivée sur la carte réseau, ces 10 dernières minutes : ${m[2]} paquets manquants${moments(m[3])}, ${m[4]} reçus deux fois, ${m[5]} marqués en erreur par la source : la source ou le réseau, pas ce serveur`,
  ],
  [
    /^(.+?): RTP, as it arrives on the network card, in the last 10 minutes: (\d+) datagrams never came, (\d+) came twice, (\d+) packets marked in error by the source: the source or the network, not this server$/,
    (m) =>
      `${m[1]} : RTP, à l’arrivée sur la carte réseau, ces 10 dernières minutes : ${m[2]} datagrammes jamais arrivés, ${m[3]} reçus deux fois, ${m[4]} paquets marqués en erreur par la source : la source ou le réseau, pas ce serveur`,
  ],
  [
    /^(.+?): the stream arrives whole on the network card \((\d+) datagrams checked in the last 10 minutes\): the damage comes from its reading on this server$/,
    (m) =>
      `${m[1]} : le flux arrive entier sur la carte réseau (${m[2]} datagrammes vérifiés ces 10 dernières minutes) : les dégâts viennent de sa lecture sur ce serveur`,
  ],
  [
    /^the network cards dropped (\d+) packets in the last 10 minutes, before any program read them .*$/,
    (m) =>
      `Les cartes réseau ont perdu ${m[1]} paquets ces 10 dernières minutes, avant toute lecture (anneaux de réception trop petits, ou serveur à court de processeur)`,
  ],
  [
    /^guard: (recording|stream) (.+?) refused \((\w+): (.+)\)$/,
    (m) =>
      `Garde-fou : ${m[1] === "stream" ? "diffusion" : "enregistrement"} « ${m[2]} » refusé (${REASON[m[3]] ?? m[3]} : ${m[4]})`,
  ],
  [
    /^(.+?): reception problems in the last 10 minutes: (\d+) packets lost by this server, (\d+) damaged on arrival$/,
    (m) =>
      `${m[1]} : problèmes de réception ces 10 dernières minutes : ${m[2]} paquets perdus par ce serveur, ${m[3]} abîmés à l’arrivée`,
  ],
  [
    /^the system dropped (\d+) UDP packets in the last 10 minutes .*$/,
    (m) =>
      `Le système a perdu ${m[1]} paquets UDP ces 10 dernières minutes (tampons réseau pleins : serveur à court de processeur, ou tampons trop petits)`,
  ],
  [
    /^(.+?) signed in from (.+)$/,
    (m) => `${m[1]} s’est connecté depuis ${m[2]}`,
  ],
  [
    /^(.+?): (black picture|frozen picture|silence) for (\d+) s$/,
    (m) => `${m[1]} : ${CONTENT[m[2]]} depuis ${m[3]} s`,
  ],
  [
    /^(.+?): (black picture|frozen picture|silence) ended$/,
    (m) =>
      `${m[1]} : fin ${m[2] === "silence" ? "du silence" : `de l’${CONTENT[m[2]]}`}`,
  ],
  [
    /^(.+?): (\d+) sound track\(s\) in the source now$/,
    (m) => `${m[1]} : ${m[2]} piste(s) son dans la source désormais`,
  ],
  [/^listening on (.+)$/, (m) => `À l’écoute sur ${m[1]}`],
  [/^stopping: recorders stopped$/, () => "Arrêt : enregistreurs arrêtés"],
  [
    /^stopping: web server closed, the streams and the recorders stop$/,
    () =>
      "Arrêt : serveur web fermé, arrêt des diffusions et des enregistreurs",
  ],
  [/^stopping$/, () => "Arrêt"],
  [/^stopped$/, () => "Arrêté"],
  [
    /^update: version (.+?) installed, restarting$/,
    (m) => `Mise à jour : version ${m[1]} installée, redémarrage`,
  ],
  [
    /^update: version (.+?) confirmed$/,
    (m) => `Mise à jour : version ${m[1]} confirmée`,
  ],
  [
    /^update: version (.+?) is available$/,
    (m) => `Mise à jour : la version ${m[1]} est disponible`,
  ],
  [/^restart asked by (.+)$/, (m) => `Redémarrage demandé par ${m[1]}`],
  [/^settings changed by (.+)$/, (m) => `Réglages modifiés par ${m[1]}`],
];

export function logText(message: string): string {
  if (lang() !== "fr" || !message) return message;
  const s = message.trim();
  for (const [re, make] of RULES) {
    const m = s.match(re);
    if (m) return make(m);
  }
  return known(s);
}
