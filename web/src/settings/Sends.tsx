/*
 * Système › Flux › Envois des extraits: the places clips are put (a NAS's share, SFTP, FTP/FTPS,
 * WebDAV, a folder of the server), each in a sub-folder made of the clip's source, date, kind,
 * maker; a clip goes there when asked, or by itself (every new clip, or those of some sources).
 */
import { useEffect, useState } from 'react';
import { AlertTriangle, Check, MoreHorizontal, Pencil, Plug, Plus, Trash2, Upload, X } from 'lucide-react';
import { api, useApp } from '../store';
import { t, tn } from '../i18n';
import { PageHead } from '../pages/SidePage';
import { NumberField, Segmented } from '../ui/fields';
import { confirmDialog } from '../ui/dialogs';
import { openContextMenu } from '../ui/ContextMenu';
import { Field } from './ui';
import { MASKED } from '../streams';
import { blankTarget, KIND_TAG, useSendTargets, type SendKind, type SendTarget } from '../sends';
import { loadGroups, useGroups } from '../pages/Groups';
import { showError } from '../ui/serverError';

const KINDS: { kind: SendKind; label: string; sub: string }[] = [
  { kind: 'smb', label: 'NAS share', sub: 'SMB / Windows' },
  { kind: 'sftp', label: 'SFTP', sub: 'SSH' },
  { kind: 'ftp', label: 'FTP', sub: 'FTP / FTPS' },
  { kind: 'webdav', label: 'WebDAV', sub: 'Nextcloud…' },
  { kind: 'dir', label: 'Folder', sub: 'of the server' },
];
const PORT: Record<SendKind, number> = { smb: 445, sftp: 22, ftp: 21, webdav: 443, dir: 0 };
const TOKENS = ['{source}', '{date}', '{kind}', '{user}'];

/** When, in a few words: « il y a 4 min », « hier 18:02 ». */
function when(ms: number): string {
  const s = Math.max(0, (Date.now() - ms) / 1000);
  const rtf = new Intl.RelativeTimeFormat(undefined, { numeric: 'auto' });
  if (s < 60) return rtf.format(0, 'second');
  if (s < 3600) return rtf.format(-Math.floor(s / 60), 'minute');
  if (s < 86400) return rtf.format(-Math.floor(s / 3600), 'hour');
  return rtf.format(-Math.floor(s / 86400), 'day');
}

export function Sends() {
  const [list] = useSendTargets<SendTarget>();
  const [edit, setEdit] = useState<SendTarget | null>(null);
  const auto = (list ?? []).filter((x) => x.auto).length;
  return (
    <>
      <PageHead title={t('Clip sending')} sub={list ? `${tn(list.length, '{n} place', '{n} places')} · ${tn(auto, '{n} automatic', '{n} automatic')}` : undefined}>
        <button className="btn primary" onClick={() => setEdit(blankTarget())}>
          <Plus size={14} /> {t('New place')}
        </button>
      </PageHead>
      {list && list.length === 0 && (
        <div className="set-card health-empty">
          <div className="empty big">
            <Upload size={22} />
            <p>{t('No place yet: a NAS, an SFTP or FTP server, a WebDAV, a folder of the server, where clips are put.')}</p>
          </div>
        </div>
      )}
      <div className="io-grid">
        {(list ?? []).map((x) => (
          <SendCard key={x.id} x={x} onEdit={() => setEdit(x)} />
        ))}
      </div>
      {edit && <SendModal target={edit} onClose={() => setEdit(null)} />}
    </>
  );
}

function SendCard({ x, onEdit }: { x: SendTarget; onEdit: () => void }) {
  const recorders = useApp((s) => s.recorders);
  const [test, setTest] = useState<{ busy: boolean; ok?: boolean; text?: string }>({ busy: false });
  const run = async () => {
    setTest({ busy: true });
    try {
      const r = await api<{ ms: number }>('POST', '/api/send-targets/test', x);
      setTest({ busy: false, ok: true, text: t('Written then removed in {ms} ms', { ms: r.ms }) });
    } catch (e) {
      setTest({ busy: false, ok: false, text: t((e as Error).message) });
    }
  };
  const remove = async () => {
    if (!(await confirmDialog({ title: t('Delete the place “{name}”?', { name: x.name }), message: t('Its sends waiting stop; the files already there stay.'), confirm: t('Delete'), danger: true }))) return;
    try {
      await api('DELETE', `/api/send-targets/${x.id}`);
    } catch (e) {
      showError(e);
    }
  };
  const sources = x.sources.map((id) => recorders.find((r) => r.id === id)?.name ?? id);
  return (
    <article className="project-card io-dest">
      <div className="project-body">
        <div className="io-dest-top">
          <span className="io-proto">{KIND_TAG[x.kind]}</span>
          <strong title={x.name}>{x.name}</strong>
          {x.last ? <span className={`sc-state ${x.last.ok ? 'ok' : 'bad'}`}>{x.last.ok ? t('Ready') : t('Failed')}</span> : <span className="sc-state">{t('Ready')}</span>}
        </div>
        <code className="io-addr">{x.address}</code>
        <div className="io-metrics">
          <div className="wide">
            <span>{t('Folder')}</span>
            <b className="mono">{x.folder || '—'}</b>
          </div>
          <div className="wide">
            <span>{t('Sending')}</span>
            <b className={x.auto ? 'accent' : undefined}>{x.auto ? `${t('automatic')} · ${sources.length ? sources.join(', ') : t('all the clips')}` : t('when asked')}</b>
          </div>
        </div>
        {x.last && !x.last.ok && !test.text && (
          <span className="io-err">
            <AlertTriangle size={12} /> {t(x.last.message)}
          </span>
        )}
        {test.text && <span className={`io-test-line ${test.ok ? 'ok' : 'bad'}`}>{test.ok ? <Check size={12} /> : <AlertTriangle size={12} />} {test.text}</span>}
      </div>
      <div className="sc-card-foot">
        <button className="btn small" disabled={test.busy} onClick={() => void run()} data-tip={t('Writes a small file there, then removes it')}>
          <Plug size={13} className={test.busy ? 'spin' : undefined} /> {test.busy ? t('Testing…') : t('Test')}
        </button>
        {x.last && <span className="muted io-last">{when(x.last.at)} {x.last.ok ? '✓' : '✕'}</span>}
        <div className="spacer" />
        <button className="icon-btn ghost" title={t('Edit')} onClick={onEdit}>
          <Pencil size={14} />
        </button>
        <button className="icon-btn ghost" title={t('More')} onClick={(e) => openContextMenu(e, [{ label: t('Delete'), icon: <Trash2 size={14} />, danger: true, onClick: () => void remove() }])}>
          <MoreHorizontal size={14} />
        </button>
      </div>
    </article>
  );
}

/** The sub-folder and name a clip would have there (an example). */
function preview(x: SendTarget): string {
  const v: Record<string, string> = { '{source}': 'Plateau 1', '{date}': new Date().toISOString().slice(0, 10), '{kind}': t('Video'), '{user}': 'admin' };
  const sub = x.folder
    .split(/[/\\]/)
    .map((p) => TOKENS.reduce((a, k) => a.split(k).join(v[k]), p).trim())
    .filter(Boolean);
  const base = x.kind === 'dir' ? x.path.replace(/\/+$/, '') : [x.kind === 'smb' ? x.share : '', x.path].filter(Boolean).join('/').replace(/\/+/g, '/');
  return [base, ...sub, 'Interview 14-32.mp4'].filter(Boolean).join('/');
}

function SendModal({ target, onClose }: { target: SendTarget; onClose: () => void }) {
  const recorders = useApp((s) => s.recorders);
  const groups = useGroups((s) => s.groups) ?? [];
  useEffect(() => void loadGroups(), []);
  const [x, setX] = useState<SendTarget>(target);
  const [busy, setBusy] = useState(false);
  const [test, setTest] = useState<{ busy: boolean; ok?: boolean; text?: string }>({ busy: false });
  const set = (p: Partial<SendTarget>) => {
    setX((v) => ({ ...v, ...p }));
    setTest({ busy: false });
  };
  const remote = x.kind !== 'dir';
  const save = async () => {
    setBusy(true);
    try {
      if (x.id) await api('PUT', `/api/send-targets/${x.id}`, x);
      else await api('POST', '/api/send-targets', x);
      onClose();
    } catch (e) {
      showError(e);
    } finally {
      setBusy(false);
    }
  };
  const run = async () => {
    setTest({ busy: true });
    try {
      const r = await api<{ ms: number }>('POST', '/api/send-targets/test', x);
      setTest({ busy: false, ok: true, text: t('Written then removed in {ms} ms', { ms: r.ms }) });
    } catch (e) {
      setTest({ busy: false, ok: false, text: t((e as Error).message) });
    }
  };
  const toggleIn = (list: string[], id: string) => (list.includes(id) ? list.filter((v) => v !== id) : [...list, id]);
  const ready = x.name.trim() && (remote ? x.host.trim() && (x.kind !== 'smb' || x.share.trim()) : x.path.trim().startsWith('/'));
  return (
    <div className="modal-back" onPointerDown={onClose}>
      <form
        className="modal xwide snd-modal"
        onPointerDown={(e) => e.stopPropagation()}
        onSubmit={(e) => {
          e.preventDefault();
          void save();
        }}
      >
        <header>
          <h2>{x.id ? t('Edit {name}', { name: target.name }) : t('New place')}</h2>
          <button type="button" className="icon-btn ghost" title={t('Close')} onClick={onClose}>
            <X size={16} />
          </button>
        </header>
        <div className="snd-body set-grid two">
          <div className="set-field span2">
            <span className="set-label">{t('Type')}</span>
            <div className="snd-kinds">
              {KINDS.map((k) => (
                <button key={k.kind} type="button" className={x.kind === k.kind ? 'on' : ''} onClick={() => set({ kind: k.kind, port: 0, tls: k.kind === 'webdav' ? 'tls' : 'explicit' })}>
                  <b>{t(k.label)}</b>
                  <small>{t(k.sub)}</small>
                </button>
              ))}
            </div>
          </div>
          <Field label={t('Name')}>
            <input value={x.name} autoFocus={!x.id} placeholder={t('NAS Montage')} onChange={(e) => set({ name: e.target.value })} />
          </Field>
          {remote ? (
            <Field label={t('Server')}>
              <input value={x.host} placeholder={x.kind === 'smb' ? 'nas-montage' : 'server.example.org'} spellCheck={false} onChange={(e) => set({ host: e.target.value })} />
            </Field>
          ) : (
            <Field label={t('Folder of the server')}>
              <input value={x.path} placeholder="/mnt/nas/rushs" spellCheck={false} onChange={(e) => set({ path: e.target.value })} />
            </Field>
          )}
          {x.kind === 'smb' && (
            <Field label={t('Share (SMB)')}>
              <input value={x.share} placeholder="Rushs" spellCheck={false} onChange={(e) => set({ share: e.target.value })} />
            </Field>
          )}
          {remote && (
            <Field label={t('Folder there')}>
              <input value={x.path} placeholder={x.kind === 'sftp' ? '/var/www/videos' : x.kind === 'smb' ? 'Strata' : 'depot'} spellCheck={false} onChange={(e) => set({ path: e.target.value })} />
            </Field>
          )}
          {remote && (
            <Field label={t('Port')}>
              <NumberField value={x.port || (x.kind === 'ftp' && x.tls === 'implicit' ? 990 : x.kind === 'webdav' && x.tls === 'none' ? 80 : PORT[x.kind])} min={1} max={65535} step={1} onChange={(v) => set({ port: v })} />
            </Field>
          )}
          {x.kind === 'ftp' && (
            <Field label={t('Encryption')} group>
              <Segmented value={x.tls} options={[{ value: 'explicit', label: t('Explicit FTPS') }, { value: 'implicit', label: t('Implicit FTPS') }, { value: 'none', label: t('No encryption') }]} onChange={(tls) => set({ tls, port: 0 })} />
            </Field>
          )}
          {x.kind === 'webdav' && (
            <Field label={t('Encryption')} group>
              <Segmented value={x.tls} options={[{ value: 'tls', label: 'HTTPS' }, { value: 'none', label: 'HTTP' }]} onChange={(tls) => set({ tls, port: 0 })} />
            </Field>
          )}
          {remote && (
            <>
              <Field label={t('User')}>
                <input value={x.user} autoComplete="off" spellCheck={false} onChange={(e) => set({ user: e.target.value })} />
              </Field>
              <Field label={t('Password')}>
                <input type="password" value={x.password} autoComplete="new-password" onFocus={() => x.password === MASKED && set({ password: '' })} onChange={(e) => set({ password: e.target.value })} />
              </Field>
            </>
          )}
          <Field label={t('Sub-folder')} group className="span2">
            <input value={x.folder} spellCheck={false} placeholder="{source}/{date}" onChange={(e) => set({ folder: e.target.value })} />
            <div className="snd-tokens">
              {TOKENS.map((k) => (
                <button key={k} type="button" onClick={() => set({ folder: x.folder ? `${x.folder.replace(/\/$/, '')}/${k}` : k })}>
                  {k}
                </button>
              ))}
            </div>
            <span className="snd-preview">→ {preview(x)}</span>
          </Field>
          <Field label={t('A file of that name there')} group>
            <Segmented value={x.exists} options={[{ value: 'rename', label: t('Rename') }, { value: 'replace', label: t('Replace') }, { value: 'skip', label: t('Do not send') }]} onChange={(exists) => set({ exists })} />
          </Field>
          <Field label={t('Sending')} group>
            <Segmented value={x.auto ? 'auto' : 'asked'} options={[{ value: 'asked', label: t('When asked') }, { value: 'auto', label: t('Automatic') }]} onChange={(v) => set({ auto: v === 'auto' })} />
          </Field>
          {x.auto && (
            <Field label={t('Clips sent by themselves')} group className="span2">
              <div className="snd-chips">
                <button type="button" className={x.sources.length === 0 ? 'on' : ''} onClick={() => set({ sources: [] })}>
                  {t('All the sources')}
                </button>
                {recorders.map((r) => (
                  <button key={r.id} type="button" className={x.sources.includes(r.id) ? 'on' : ''} onClick={() => set({ sources: toggleIn(x.sources, r.id) })}>
                    {r.name}
                  </button>
                ))}
              </div>
            </Field>
          )}
          <Field label={t('Who may send there when they want')} group className="span2">
            <Segmented value={x.who} options={[{ value: 'extract', label: t('Who extracts') }, { value: 'groups', label: t('Groups…') }, { value: 'admins', label: t('Admins') }]} onChange={(who) => set({ who })} />
            {x.who === 'groups' && (
              <div className="snd-chips">
                {groups.length === 0 && <span className="muted">{t('No group yet: an admin makes them in Système › Groupes.')}</span>}
                {groups.map((g) => (
                  <button key={g.id} type="button" className={x.groups.includes(g.id) ? 'on' : ''} onClick={() => set({ groups: toggleIn(x.groups, g.id) })}>
                    {g.name}
                  </button>
                ))}
              </div>
            )}
          </Field>
        </div>
        <footer>
          <button type="button" className="btn" disabled={test.busy || !ready} onClick={() => void run()}>
            <Plug size={13} className={test.busy ? 'spin' : undefined} /> {test.busy ? t('Testing…') : t('Test')}
          </button>
          {test.text && (
            <span className={`io-test-line ${test.ok ? 'ok' : 'bad'}`}>
              {test.ok ? <Check size={12} /> : <AlertTriangle size={12} />} {test.text}
            </span>
          )}
          <div className="spacer" />
          <button type="button" className="btn" onClick={onClose}>
            {t('Cancel')}
          </button>
          <button type="submit" className="btn primary" disabled={busy || !ready}>
            {x.id ? t('Save') : (<><Plus size={14} /> {t('Create the place')}</>)}
          </button>
        </footer>
      </form>
    </div>
  );
}
