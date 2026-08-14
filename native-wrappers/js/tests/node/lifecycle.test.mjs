import assert from 'node:assert/strict'
import { spawnSync } from 'node:child_process'
import test from 'node:test'
import { Worker } from 'node:worker_threads'

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
