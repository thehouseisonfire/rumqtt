'use strict'

const fs = require('node:fs')
const path = require('node:path')

function libc() {
  if (process.platform !== 'linux') return undefined
  if (process.report?.getReport) {
    return process.report.getReport().header.glibcVersionRuntime ? 'gnu' : 'musl'
  }
  return fs.existsSync('/etc/alpine-release') ? 'musl' : 'gnu'
}

const override = process.env.RUMQTTC_JS_NATIVE_PATH

if (override) {
  module.exports = require(path.resolve(override))
} else {
  const platform = process.platform
  const arch = process.arch
  const suffix = platform === 'linux' ? `${platform}-${arch}-${libc()}` :
    platform === 'win32' ? `${platform}-${arch}-msvc` : `${platform}-${arch}`
  const packageName = `@rumqtt-next/rumqttc-${suffix}`
  const local = path.join(__dirname, `rumqttc.${suffix}.node`)
  try {
    module.exports = fs.existsSync(local) ? require(local) : require(packageName)
  } catch (cause) {
    const error = new Error(
      `@rumqtt-next/rumqttc has no loadable native addon for ${platform}/${arch}` +
      (platform === 'linux' ? `/${libc()}` : '') + `. Expected ${packageName}.`,
    )
    error.cause = cause
    throw error
  }
}
