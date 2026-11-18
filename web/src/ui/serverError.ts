// The server's errors (short English text) in the interface language.
import { t } from '../i18n';
import { useApp } from '../store';

const KNOWN: [RegExp, (m: RegExpMatchArray) => string][] = [
  [/failed to fetch|networkerror|load failed|network request failed/i, () => t('The server does not answer')],
  [/^login required$/i, () => t('Please sign in again')],
  [/^you do not have the rights for this$/i, () => t('You do not have the rights for this')],
  [/^wrong user name or password$/i, () => t('Wrong user name or password.')],
  [/^too many tries: wait (\d+) ?s$/i, (m) => t('Too many tries: wait {n} s', { n: m[1] })],
  [/^request sent by another site refused$/i, () => t('Request sent by another site refused')],
  [/^no such (\w+)$/i, () => t('Not found')],
  [/^you cannot delete your own account$/i, () => t('You cannot delete your own account')],
  [/last (active )?admin/i, () => t('There must always be an active administrator')],
  [/password.*(at least|8)/i, () => t('At least 8 characters.')],
  [/(user name|username).*(taken|exists|used)/i, () => t('This user name is already taken')],
  [/(user name|username).*(letters|invalid|characters)/i, () => t('User name: letters, digits, . - _ @')],
  [/^the server name cannot be empty$/i, () => t('The server name cannot be empty')],
  [/^a folder is written as an absolute path$/i, () => t('A folder is written as an absolute path')],
  [/^the repository is written owner\/name$/i, () => t('The repository is written owner/name')],
  [/^github refused the token/i, () => t('GitHub refused the token (expired or revoked?)')],
  [/^github refused the request/i, () => t('GitHub refused the request (rate limit, or the token cannot read the repository)')],
  [/not found: the repository is private/i, () => t('Repository not found: if it is private, add a read-only token')],
  [/^already set up$/i, () => t('An account already exists')],
  [/^sign in with (.+)$/i, (m) => t('Sign in with {name}', { name: m[1] })],
  [/^unknown language$/i, () => t('Unknown language')],
  [/^unknown theme$/i, () => t('Unknown theme')],
  [/^the volume could not be extended: (.+)$/i, (m) => t('The volume could not be extended: {why}', { why: m[1] })],
];

/** The French (or English) text for a raw server error; unknown errors stay as they are. */
export function friendlyError(raw: string): string {
  const s = (raw ?? '').trim();
  for (const [re, f] of KNOWN) {
    const m = s.match(re);
    if (m) return f(m);
  }
  // The other answers of the engine are keys of the dictionary (business.ts › Server answers).
  return s ? t(s) : t('Something went wrong');
}

/** The message of a caught error, translated. */
export function errorText(e: unknown): string {
  return friendlyError(e instanceof Error ? e.message : String(e));
}

/** Shows a caught error as a red toast. */
export function showError(e: unknown) {
  useApp.getState().toast(errorText(e), 'error');
}
