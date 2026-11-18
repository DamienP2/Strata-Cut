/* The help page: its title and the keyboard shortcuts. */
import { Keyboard } from 'lucide-react';
import { t } from '../i18n';
import { Card } from '../settings/ui';
import { ShortcutList } from '../ui/Shortcuts';
import { PageHead } from './SidePage';

export function Help() {
  return (
    <main className="library">
      <div className="library-inner">
        <PageHead title={t('Help')} />
        <Card icon={<Keyboard size={16} />} title={t('Keyboard shortcuts')}>
          <ShortcutList />
        </Card>
      </div>
    </main>
  );
}
