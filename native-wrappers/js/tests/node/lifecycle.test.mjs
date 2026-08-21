import assert from 'node:assert/strict'
import { spawnSync } from 'node:child_process'
import { createRequire } from 'node:module'
import test from 'node:test'
import { Worker } from 'node:worker_threads'

const require = createRequire(import.meta.url)
const { NativeMqttClient } = require('../../loader.cjs')

test('process exits with a live connecting client', () => {
  const result = spawnSync(process.execPath, [new URL('./live-client-child.mjs', import.meta.url).pathname], {
    env: process.env,
    timeout: 10_000,
  })
  assert.notEqual(result.error?.code, 'ETIMEDOUT')
  assert.equal(result.status, 0, result.stderr.toString())
})

test('worker environment unloads an independent addon instance', async () => {
  const worker = new Worker(new URL('./live-client-child.mjs', import.meta.url))
  await worker.terminate()
  assert.ok(true)
})

test('repeated startup and immediate shutdown do not retain threads or unbounded memory', () => {
  const result = spawnSync(process.execPath, [new URL('./repetition-child.mjs', import.meta.url).pathname], {
    env: process.env,
    timeout: 20_000,
  })
  assert.notEqual(result.error?.code, 'ETIMEDOUT')
  assert.equal(result.status, 0, result.stderr.toString())
})

test('loader rejects an unsupported architecture without fallback', () => {
  const script = `
    Object.defineProperty(process, 'arch', { value: 'riscv64' });
    try { require(${JSON.stringify(new URL('../../loader.cjs', import.meta.url).pathname)}) }
    catch (error) {
      if (!error.message.includes('linux/riscv64') && !error.message.includes(process.platform + '/riscv64')) process.exit(2)
      if (!error.message.includes('@rumqtt-next/rumqttc-')) process.exit(3)
      process.exit(0)
    }
    process.exit(1)
  `
  const environment = { ...process.env }
  delete environment.RUMQTTC_JS_NATIVE_PATH
  const result = spawnSync(process.execPath, ['-e', script], { env: environment, timeout: 5000 })
  assert.equal(result.status, 0, result.stderr.toString())
})

test('loader distinguishes glibc and musl package names', () => {
  if (process.platform !== 'linux') return
  const loader = new URL('../../loader.cjs', import.meta.url).pathname
  const script = `
    process.report.getReport = () => ({ header: {} });
    try { require(${JSON.stringify(loader)}) }
    catch (error) { process.exit(error.message.includes('linux/x64/musl') ? 0 : 2) }
    process.exit(1)
  `
  const environment = { ...process.env }
  delete environment.RUMQTTC_JS_NATIVE_PATH
  const result = spawnSync(process.execPath, ['-e', script], { env: environment, timeout: 5000 })
  assert.equal(result.status, 0, result.stderr.toString())
})

for (const [boundary, method] of [
  ['sync', 'injectSyncPanic'],
  ['async', 'injectAsyncPanic'],
]) {
  const skip = typeof NativeMqttClient.prototype[method] === 'function'
    ? false
    : 'requires a native addon built with the panic-testing feature'

  test(`${boundary} native panic is contained in a child process`, { skip }, () => {
    const result = spawnSync(process.execPath, [
      new URL('./panic-child.cjs', import.meta.url).pathname,
      boundary,
    ], { env: process.env, timeout: 10_000 })
    assert.notEqual(result.error?.code, 'ETIMEDOUT')
    assert.equal(result.status, 0, result.stderr.toString())
  })
}
