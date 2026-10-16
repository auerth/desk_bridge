import { ExternalLink, Code2 } from 'lucide-react';
import { getBackend } from '../api';
import logoNavy from '../brand/logos/softwelop-logo-navy.svg';
import logoWhite from '../brand/logos/softwelop-logo-weiss.svg';
import appIcon from '../brand/app-icon/variant-a.svg';
import { COMPANY, COMPANY_URL, PRODUCT_NAME, REPO_URL } from '../brand';
import { Button, Card, PageHeader } from '../components/ui';
import { useT } from '../i18n/useT';
import { useResolvedTheme } from '../lib/theme';
import { useStore } from '../store';

const LICENSES: { name: string; license: string }[] = [
  { name: 'Desk Bridge', license: 'MIT OR Apache-2.0' },
  { name: 'Montserrat (Julieta Ulanovsky et al.)', license: 'SIL Open Font License 1.1' },
  { name: 'Tauri', license: 'MIT OR Apache-2.0' },
  { name: 'React', license: 'MIT' },
  { name: 'Radix UI', license: 'MIT' },
  { name: 'Motion', license: 'MIT' },
  { name: 'Zustand', license: 'MIT' },
  { name: 'Lucide', license: 'ISC' },
  { name: 'softwelop icon set', license: '© softwelop' },
];

export function About() {
  const t = useT();
  const theme = useResolvedTheme();
  const version = useStore((s) => s.state?.device.version ?? s.appInfo?.version ?? '');
  const backend = getBackend();

  return (
    <>
      <PageHeader title={t('about.title')} />
      <Card className="about-hero">
        <img src={appIcon} alt="" width={96} height={96} className="about-hero__icon" />
        <div>
          <h2 className="t-h1">{PRODUCT_NAME}</h2>
          <p className="muted">{t('about.tagline')}</p>
          <p className="num">{t('about.version', { version })}</p>
          <div className="about-by">
            <span>{t('about.by')}</span>
            <img src={theme === 'dark' ? logoWhite : logoNavy} alt={COMPANY} className="about-by__logo" />
          </div>
        </div>
      </Card>
      <Card>
        <p>{t('about.privacy')}</p>
        <p className="muted">{t('about.licenseOwn')}</p>
        <div className="btn-group">
          <Button icon={<ExternalLink size={16} aria-hidden />} onClick={() => void backend.openUrl(COMPANY_URL)}>
            {t('about.website')}
          </Button>
          <Button variant="ghost" icon={<Code2 size={16} aria-hidden />} onClick={() => void backend.openUrl(REPO_URL)}>
            {t('about.source')}
          </Button>
        </div>
      </Card>
      <Card title={t('about.licenses')} subtitle={t('about.licensesNote')}>
        <table className="license-table">
          <tbody>
            {LICENSES.map((l) => (
              <tr key={l.name}>
                <th scope="row">{l.name}</th>
                <td>{l.license}</td>
              </tr>
            ))}
          </tbody>
        </table>
      </Card>
    </>
  );
}
