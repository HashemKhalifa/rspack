import value, { subpath } from './a';
import stable, { subpath as stableSubpath } from './stable';

it('should provide only the current resolution while watching', async () => {
  await __webpack_init_sharing__('default');
  expect(value).toBe('a');
  expect(subpath).toBe('a-subpath');
  expect(stable).toBe('stable');
  expect(stableSubpath).toBe('stable-subpath');
  expect(Object.keys(__webpack_share_scopes__.default.package).sort()).toEqual([
    '1.0.0',
    '3.0.0',
  ]);
  expect(
    Object.keys(__webpack_share_scopes__.default['package/subpath']).sort(),
  ).toEqual([
    '1.0.0',
    '3.0.0',
  ]);
});
