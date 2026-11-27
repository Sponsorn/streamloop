import { describe, expect, test } from 'vitest';
import { variantsAfter } from './install-kit.mjs';

describe('variantsAfter', () => {
  test('numbers the test builds above the release under test', () => {
    // Arrange
    const release = '3.1.1';

    // Act
    const v = variantsAfter(release);

    // Assert
    expect([v.n1, v.n2, v.n3, v.n4].map((b) => b.version)).toEqual(['3.1.2', '3.1.3', '3.1.4', '3.1.5']);
    expect(v['n1-supervisor-fails']).toEqual({ version: '3.1.2', feature: 'test-supervisor-fails' });
    expect(v['n1-control-fails']).toEqual({ version: '3.1.2', feature: 'test-control-fails' });
  });

  test('counts a dev build from its release number', () => {
    // Act
    const v = variantsAfter('3.1.0-dev');

    // Assert
    expect(v.n1.version).toBe('3.1.1');
  });
});
