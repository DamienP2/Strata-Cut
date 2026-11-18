/* Extraction (placeholder: replaced by the business screens). */
import { t } from '../i18n';
import { PageHead } from './SidePage';

export function Extract({ recorder }: { recorder?: string }) {
  return (
    <main className="library">
      <div className="library-inner wide">
        <PageHead title={t('Extraction')} sub={recorder} />
      </div>
    </main>
  );
}
