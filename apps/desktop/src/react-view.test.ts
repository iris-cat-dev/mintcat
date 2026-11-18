// @vitest-environment jsdom
import { afterEach, describe, expect, it } from 'vitest';
import './ui-runtime';
import { getSharedRuntime, mountReactView, setUiTheme } from '@mintcat/sdk-ui';
import type { Disposable } from '@mintcat/sdk-ui';

const { React, flushSync } = getSharedRuntime();
let view: Disposable | undefined;
afterEach(() => { view?.dispose(); view = undefined; document.body.replaceChildren(); setUiTheme('Light'); });

describe('React page lifecycle', () => {
  it('reports a failed initial render before the Workbench presents the page', () => {
    const root = document.createElement('section'); document.body.append(root);
    function BrokenModule(): never { throw new Error('Broken page'); }
    expect(() => mountReactView(root, React.createElement(BrokenModule))).toThrow(Error);
    expect(root.childElementCount).toBe(0);
  });
  it('preserves an edited component across a theme change instead of remounting its draft', () => {
    const root = document.createElement('section'); document.body.append(root);
    function Draft() {
      const [value, update] = React.useState('original');
      return React.createElement('button', { onClick: () => update('edited') }, value);
    }
    view = mountReactView(root, React.createElement(Draft));
    flushSync(() => root.querySelector('button')!.click());
    expect(root.querySelector('button')!.textContent).toBe('edited');
    setUiTheme('Dark');
    expect(root.querySelector('button')!.textContent).toBe('edited');
  });
});
