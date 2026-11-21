import { Alert, Anchor, Button, Card, Center, Divider, PasswordInput, Stack, Text, Title } from '@mantine/core';
import { IconKey, IconLogin2 } from '@tabler/icons-react';
import { useEffect, useState, type SyntheticEvent } from 'react';
import { safeReturn, setToken } from '../lib/authToken';

interface AuthConfig {
  oidc: boolean;
}

async function loadConfig(): Promise<AuthConfig> {
  try {
    const res = await fetch('/auth/config', { headers: { Accept: 'application/json' }, credentials: 'omit' });
    if (!res.ok) return { oidc: false };
    const body = (await res.json()) as Partial<AuthConfig>;
    return { oidc: body.oidc === true };
  } catch {
    return { oidc: false };
  }
}

/** The sign-in page: single sign-on when the server has it, and an API token always. */
export default function SignInPage() {
  const params = new URLSearchParams(window.location.search);
  const target = safeReturn(params.get('return'));
  const expired = params.get('reason') === 'expired';
  const [config, setConfig] = useState<AuthConfig | null>(null);
  const [token, setTokenValue] = useState('');
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);

  useEffect(() => {
    void loadConfig().then(setConfig);
  }, []);

  const submit = async (e: SyntheticEvent) => {
    e.preventDefault();
    const t = token.trim();
    if (!t) {
      setError('Paste an API token.');
      return;
    }
    setBusy(true);
    setError(null);
    try {
      const res = await fetch('/api/v1/me', { headers: { Accept: 'application/json', Authorization: `Bearer ${t}` }, credentials: 'omit' });
      if (res.status === 401) {
        setError('The server did not accept this token. Check that it is complete and has not expired or been revoked.');
      } else if (!res.ok) {
        setError(`The server answered ${String(res.status)}. Try again later.`);
      } else if (!setToken(t)) {
        setError('This browser does not allow session storage for this site, so the token cannot be kept. Allow site data and try again.');
      } else {
        window.location.assign(target);
        return;
      }
    } catch {
      setError('The server could not be reached.');
    }
    setBusy(false);
  };

  return (
    <Center mih="100dvh" p="md">
      <Card withBorder radius="md" padding="lg" w="100%" maw={420} data-testid="sign-in">
        <Stack gap="md">
          <div>
            <Title order={2} size="h3">
              Sign in to FastProof
            </Title>
            <Text size="sm" c="dimmed">
              Test results, failures and trends of your projects.
            </Text>
          </div>
          {expired && (
            <Alert color="yellow" variant="light" data-testid="sign-in-expired">
              Your token is no longer accepted. Sign in again.
            </Alert>
          )}
          {config?.oidc && (
            <>
              <Button component="a" href={`/auth/login?return=${encodeURIComponent(target)}`} leftSection={<IconLogin2 size={16} />} data-testid="sign-in-sso">
                Sign in with SSO
              </Button>
              <Divider label="or" labelPosition="center" />
            </>
          )}
          <form onSubmit={(e) => void submit(e)}>
            <Stack gap="sm">
              <PasswordInput
                label="Use an API token"
                description="An admin token from FASTPROOF_ADMIN_TOKENS_FILE or a token created under Settings → API tokens. It is kept in this tab only and sent as a bearer token."
                value={token}
                onChange={(e) => setTokenValue(e.currentTarget.value)}
                autoComplete="off"
                spellCheck={false}
                error={error}
                data-testid="sign-in-token"
                data-autofocus
              />
              <Button type="submit" variant={config?.oidc ? 'default' : 'filled'} loading={busy} leftSection={<IconKey size={16} />} data-testid="sign-in-submit">
                Sign in with token
              </Button>
            </Stack>
          </form>
          <Text size="xs" c="dimmed">
            The API is described at{' '}
            <Anchor href="/api/docs/" size="xs">
              /api/docs
            </Anchor>
            .
          </Text>
        </Stack>
      </Card>
    </Center>
  );
}
