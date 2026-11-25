import { common } from './common';
import { users } from './users';
import { system } from './system';
import { updates } from './updates';
import { business } from './business';
import { streaming } from './streaming';
import { sends } from './sends';

/** Every French text, by part of the interface. */
export const fr: Record<string, string> = { ...common, ...users, ...system, ...updates, ...business, ...streaming, ...sends };
