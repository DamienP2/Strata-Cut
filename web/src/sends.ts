/*
 * Sending the clips (Système › Envois des extraits): the places, the sends of the clips this
 * account sees, and the menu « Envoyer vers » of the Extraits page.
 */
import { useCallback, useEffect, useState } from 'react';
import { api, useChanged } from './store';
import { t, tn } from './i18n';
import { showError } from './ui/serverError';
import { useApp } from './store';

export type SendKind = 'smb' | 'sftp' | 'ftp' | 'webdav' | 'dir';

/** A place, as an admin sets it (its password masked). */
export interface SendTarget {
  id: string;
  name: string;
  kind: SendKind;
  host: string;
  port: number;
  share: string;
  path: string;
  user: string;
  password: string;
  tls: string;
  folder: string;
  exists: 'rename' | 'replace' | 'skip';
  auto: boolean;
  sources: string[];
  who: 'extract' | 'groups' | 'admins';
  groups: string[];
  /** Read only: where it is, in words. */
  address?: string;
  /** Read only: its last send. */
  last?: { at: number; ok: boolean; message: string };
  usable: boolean;
}

/** A place, as anyone who may send there sees it. */
export interface UsableTarget {
  id: string;
  name: string;
  kind: SendKind;
  folder: string;
  /** Every new clip goes there by itself (of `sources`; none: of all). */
  auto: boolean;
  sources: string[];
  /** This account may send there when it wants. */
  usable: boolean;
}

/** Do the clips of this source go there by themselves? */
export const autoFor = (p: UsableTarget, source: string) => p.auto && (p.sources.length === 0 || p.sources.includes(source));

export interface SendJob {
  id: string;
  clip: string;
  clipName: string;
  target: string;
  targetName: string;
  state: 'queued' | 'running' | 'done' | 'error';
  /** 0 to 1; null: not known. */
  progress: number | null;
  speed: number;
  message: string;
  remote: string;
  by: string;
  at: number;
  ended: number | null;
  /** Asked by this account (or sent by itself for its clip). */
  mine: boolean;
}

export const KIND_TAG: Record<SendKind, string> = { smb: 'SMB', sftp: 'SFTP', ftp: 'FTP', webdav: 'DAV', dir: 'DIR' };

export function blankTarget(): SendTarget {
  return { id: '', name: '', kind: 'smb', host: '', port: 0, share: '', path: '', user: '', password: '', tls: 'explicit', folder: '{source}/{date}', exists: 'rename', auto: false, sources: [], who: 'extract', groups: [], usable: true };
}

/** The places this account may send to (an admin: all, set as they are). */
export function useSendTargets<T extends UsableTarget = UsableTarget>(): [T[] | null, () => void] {
  const [list, setList] = useState<T[] | null>(null);
  const load = useCallback(() => {
    api<T[]>('GET', '/api/send-targets').then(setList, () => setList([]));
  }, []);
  useEffect(load, [load]);
  useChanged('sends', load);
  return [list, load];
}

/** The sends of the clips this account sees. */
export function useSends(): SendJob[] {
  const [list, setList] = useState<SendJob[]>([]);
  const load = useCallback(() => {
    api<SendJob[]>('GET', '/api/sends').then(setList, () => {});
  }, []);
  useEffect(load, [load]);
  useChanged('sends', load);
  return list;
}

/** Sends clips to a place (they wait their turn there). */
export async function sendClips(ids: string[], target: UsableTarget) {
  try {
    await api('POST', '/api/sends', { clips: ids, target: target.id });
    useApp.getState().toast(tn(ids.length, '{n} clip sent to {place}', '{n} clips sent to {place}', { place: target.name }));
  } catch (e) {
    showError(e);
  }
}

/** « 38 Mo/s » */
export function speedText(bytes: number, fr: boolean): string {
  if (bytes <= 0) return '';
  const mb = bytes / 1e6;
  return mb >= 1 ? `${mb.toFixed(mb >= 10 ? 0 : 1).replace('.', fr ? ',' : '.')} ${fr ? 'Mo/s' : 'MB/s'}` : `${Math.max(1, Math.round(bytes / 1e3))} ${fr ? 'ko/s' : 'kB/s'}`;
}

/** What a send's state says, in a few words. */
export function sendStateText(j: SendJob): string {
  if (j.state === 'running') return j.progress != null ? `${Math.round(j.progress * 100)} %` : t('Sending…');
  if (j.state === 'queued') return t('Waiting');
  if (j.state === 'error') return t(j.message || 'Failed');
  return j.message ? t(j.message) : t('Sent');
}
