import { render, screen } from '@testing-library/react';
import { describe, expect, it } from 'vitest';
import { Providers } from './test/render';
import { Shell } from './Shell';

describe('shell', () => {
  it('shows the server version in the footer', async () => {
    render(
      <Providers route="/">
        <Shell>
          <p>page</p>
        </Shell>
      </Providers>,
    );
    expect(await screen.findByTestId('server-version')).toHaveTextContent('FastProof 0.2.0-dev1');
  });
});
