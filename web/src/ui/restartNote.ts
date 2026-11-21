import { tn } from '../i18n';
import { api, useApp } from '../store';

/** What a restart of the server cuts now (the lives going out, the clips being made): said in the
 * confirmation; empty when nothing would be cut. */
export async function restartCuts(): Promise<string> {
  let lives = 0;
  try {
    const list = await api<{ state: string }[]>('GET', '/api/streams');
    lives = list.filter((s) => s.state !== 'failed').length;
  } catch {
    /* not allowed to see them, or the server busy: nothing said */
  }
  const jobs = useApp.getState().jobs.filter((j) => j.state === 'queued' || j.state === 'running').length;
  const parts: string[] = [];
  if (lives) parts.push(tn(lives, '{n} live stream is cut and does not start again by itself.', '{n} live streams are cut and do not start again by themselves.'));
  if (jobs) parts.push(tn(jobs, '{n} clip being made is lost.', '{n} clips being made are lost.'));
  return parts.join(' ');
}
