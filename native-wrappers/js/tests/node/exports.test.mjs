import assert from 'node:assert/strict'
import { createRequire } from 'node:module'
import { readFile } from 'node:fs/promises'
import test from 'node:test'

const require = createRequire(import.meta.url)

test('ESM, CommonJS, and value declarations export the same API', async () => {
  const declarations = await readFile(new URL('../../index.d.ts', import.meta.url), 'utf8')
  const declaredValues = [...declarations.matchAll(/^export (?:declare )?class (\w+)/gm)]
    .map(match => match[1]).sort()
  const esm = Object.keys(await import('../../index.js')).sort()
  const commonjs = Object.keys(require('../../index.cjs')).sort()
  assert.deepEqual(esm, declaredValues)
  assert.deepEqual(commonjs, declaredValues)
})
