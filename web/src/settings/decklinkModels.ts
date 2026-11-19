/*
 * Blackmagic's capture devices (Desktop Video), drawn simply in Système › Matériel: the shape
 * (full-height PCIe card, low-profile card, external box) and the connectors of the bracket or
 * front panel, in their order. A connector with `input` is the input of that sub-device (the
 * card's inputs as the driver numbers them, from 0); the others (outputs, reference, analogue
 * breakout) are drawn but not used. The main connectors only: a simplified picture.
 */

export type ConnKind = 'sdi' | 'sdi-out' | 'hdmi' | 'hdmi-out' | 'ref' | 'optical' | 'sfp' | 'analog';

export interface Connector {
  kind: ConnKind;
  /** The sub-device whose input it is (from 0). */
  input?: number;
}

export interface Model {
  name: string;
  /** Matched against the model the driver gives ("DeckLink Duo" for the sub-devices "DeckLink Duo (1)"…). */
  match: RegExp;
  /** When one name covers several cards (Duo / Duo 2): the number of inputs. */
  inputs?: number;
  form: 'full' | 'low' | 'box';
  connectors: Connector[];
}

const sdi = (...inputs: number[]): Connector[] => inputs.map((input) => ({ kind: 'sdi', input }));
const hdmi = (...inputs: number[]): Connector[] => inputs.map((input) => ({ kind: 'hdmi', input }));
const out = (kind: ConnKind, n = 1): Connector[] => Array.from({ length: n }, () => ({ kind }));
/** The usual "full" set: SDI in and out, HDMI in and out, reference, analogue breakout. */
const studio = (): Connector[] => [...sdi(0), ...out('sdi-out'), ...hdmi(0), ...out('hdmi-out'), ...out('ref'), ...out('analog')];

/** The most precise first: the first that matches is the one. */
export const MODELS: Model[] = [
  // Low-profile recorders.
  { name: 'DeckLink Mini Recorder 4K', match: /mini recorder 4k/i, form: 'low', connectors: [...sdi(0), ...hdmi(0)] },
  { name: 'DeckLink Mini Recorder HD', match: /mini recorder hd/i, form: 'low', connectors: [...sdi(0), ...hdmi(0)] },
  { name: 'DeckLink Mini Recorder', match: /decklink mini recorder/i, form: 'low', connectors: [...sdi(0), ...hdmi(0)] },
  { name: 'DeckLink Micro Recorder', match: /micro recorder/i, form: 'low', connectors: [...sdi(0), ...hdmi(0)] },
  { name: 'DeckLink SDI Micro', match: /sdi micro/i, form: 'low', connectors: [...sdi(0), ...out('sdi-out')] },
  { name: 'DeckLink SDI 4K', match: /sdi 4k/i, form: 'low', connectors: [...sdi(0), ...out('sdi-out'), ...out('ref')] },
  // Multi-channel SDI.
  { name: 'DeckLink 8K Pro Mini', match: /8k pro mini/i, form: 'low', connectors: [...sdi(0, 1, 2, 3), ...out('ref')] },
  { name: 'DeckLink 8K Pro', match: /8k pro/i, form: 'full', connectors: [...sdi(0, 1, 2, 3), ...out('ref')] },
  { name: 'DeckLink Quad HDMI Recorder', match: /quad hdmi/i, form: 'full', connectors: hdmi(0, 1, 2, 3) },
  { name: 'DeckLink Quad 2', match: /quad/i, inputs: 8, form: 'full', connectors: sdi(0, 1, 2, 3, 4, 5, 6, 7) },
  { name: 'DeckLink Quad', match: /quad/i, form: 'full', connectors: sdi(0, 1, 2, 3, 4, 5, 6, 7) },
  { name: 'DeckLink Duo 2 Mini', match: /duo 2 mini|duo mini/i, form: 'low', connectors: sdi(0, 1, 2, 3) },
  { name: 'DeckLink Duo 2', match: /duo/i, inputs: 4, form: 'full', connectors: sdi(0, 1, 2, 3) },
  { name: 'DeckLink Duo', match: /duo/i, form: 'full', connectors: [...sdi(0, 1), ...out('sdi-out', 2)] },
  // Single channel, every connector.
  { name: 'DeckLink 4K Extreme 12G', match: /4k extreme 12g/i, form: 'full', connectors: studio() },
  { name: 'DeckLink 4K Extreme', match: /4k extreme/i, form: 'full', connectors: studio() },
  { name: 'DeckLink 4K Pro', match: /4k pro/i, form: 'full', connectors: [...sdi(0, 0), ...out('sdi-out', 2), ...out('ref')] },
  { name: 'DeckLink Studio 4K', match: /studio 4k/i, form: 'full', connectors: studio() },
  { name: 'DeckLink Studio', match: /decklink studio/i, form: 'full', connectors: studio() },
  { name: 'DeckLink HD Extreme', match: /hd extreme/i, form: 'full', connectors: studio() },
  { name: 'DeckLink Optical Fibre', match: /optical/i, form: 'full', connectors: [{ kind: 'optical', input: 0 }, ...sdi(0), ...out('sdi-out'), ...out('ref')] },
  { name: 'DeckLink IP', match: /decklink ip/i, form: 'full', connectors: [{ kind: 'sfp', input: 0 }, { kind: 'sfp', input: 1 }] },
  { name: 'DeckLink SDI', match: /^decklink sdi$/i, form: 'full', connectors: [...sdi(0), ...out('sdi-out'), ...out('ref')] },
  // HDMI cards.
  { name: 'Intensity Pro 4K', match: /intensity pro 4k/i, form: 'low', connectors: [...hdmi(0), ...out('hdmi-out'), ...out('analog')] },
  { name: 'Intensity Pro', match: /intensity pro/i, form: 'low', connectors: [...hdmi(0), ...out('hdmi-out'), ...out('analog')] },
  // External boxes (Thunderbolt / USB).
  { name: 'Intensity Shuttle', match: /intensity shuttle/i, form: 'box', connectors: [...hdmi(0), ...out('hdmi-out'), ...out('analog')] },
  { name: 'Intensity Extreme', match: /intensity extreme/i, form: 'box', connectors: [...hdmi(0), ...out('hdmi-out'), ...out('analog')] },
  { name: 'UltraStudio Recorder 3G', match: /ultrastudio recorder/i, form: 'box', connectors: [...sdi(0), ...hdmi(0)] },
  { name: 'UltraStudio Mini Recorder', match: /ultrastudio mini recorder/i, form: 'box', connectors: [...sdi(0), ...hdmi(0)] },
  { name: 'UltraStudio HD Mini', match: /ultrastudio hd mini/i, form: 'box', connectors: studio() },
  { name: 'UltraStudio 4K Mini', match: /ultrastudio 4k mini/i, form: 'box', connectors: studio() },
  { name: 'UltraStudio 4K Extreme', match: /ultrastudio 4k extreme/i, form: 'box', connectors: studio() },
  { name: 'UltraStudio', match: /ultrastudio/i, form: 'box', connectors: studio() },
];

/**
 * The picture of a device: its catalogue entry, or a plain card with one connector per input.
 * Inputs the driver shows beyond the catalogue's (another profile) get a connector of their own.
 */
export function modelOf(name: string, inputs: number, hdmiGuess: boolean): Model {
  const known = MODELS.find((m) => m.match.test(name) && (m.inputs === undefined || m.inputs === inputs));
  const base: Model = known ?? { name, match: /$^/, form: 'full', connectors: Array.from({ length: inputs }, (_, i) => ({ kind: hdmiGuess ? 'hdmi' : 'sdi', input: i })) };
  const most = Math.max(-1, ...base.connectors.map((c) => c.input ?? -1));
  const extra: Connector[] = Array.from({ length: Math.max(0, inputs - 1 - most) }, (_, i) => ({ kind: hdmiGuess ? 'hdmi' : 'sdi', input: most + 1 + i }));
  return { ...base, connectors: [...base.connectors, ...extra] };
}
