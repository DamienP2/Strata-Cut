// The accounts (admins): GET /api/users, reloaded when the server says `users` changed.
import { create } from 'zustand';
import { api, type PublicUser } from '../store';

export const useUserList = create<{ users: PublicUser[] | null }>(() => ({ users: null }));

export async function loadUsers() {
  try {
    useUserList.setState({ users: await api<PublicUser[]>('GET', '/api/users') });
  } catch {
    /* kept as it was */
  }
}
