<div align="center">
  <img alt="rumqtt Logo" src="docs/rumqtt.png" width="60%" />
</div>
<div align="center">
  <a href="https://github.com/thehouseisonfire/rumqtt/actions/workflows/ci.yml">
    <img alt="build status" src="https://github.com/thehouseisonfire/rumqtt/actions/workflows/ci.yml/badge.svg">
  </a>
  <a href="https://coveralls.io/github/thehouseisonfire/rumqtt?branch=main">
    <img src="https://coveralls.io/repos/github/thehouseisonfire/rumqtt/badge.svg?branch=main" alt="Coverage Status" />
  </a>
  <a href="https://discord.gg/mpkSqDg">
    <img alt="Discord chat" src="https://img.shields.io/discord/633193308033646605?style=flat">
  </a>
</div>
<br/>

## What is rumqtt?

rumqtt is an opensource set of libraries written in rust-lang to implement the MQTT standard while striving to be simple, robust and performant.

| Crate | Description | Version |
| -- | -- | -- |
| [rumqttc-v4](./rumqttc-v4/) | MQTT 3.1.1 client | [![crates.io page](https://img.shields.io/crates/v/rumqttc-v4.svg)](https://crates.io/crates/rumqttc-v4) |
| [rumqttc-v5](./rumqttc-v5/) | MQTT 5 client | [![crates.io page](https://img.shields.io/crates/v/rumqttc-v5.svg)](https://crates.io/crates/rumqttc-v5) |

## Installation and Usage

### rumqttc-v4

Add the v4 client crate:

```bash
cargo add rumqttc-v4
```

### rumqttc-v5

Add the v5 client crate:

```bash
cargo add rumqttc-v5
```

For more details, see [rumqttc-v4/README.md](https://github.com/thehouseisonfire/rumqtt/blob/main/rumqttc-v4/README.md) and [rumqttc-v5/README.md](https://github.com/thehouseisonfire/rumqtt/blob/main/rumqttc-v5/README.md).

## Features

### rumqttc-v4
- [x] MQTT 3.1.1

### rumqttc-v5
- [x] MQTT 5

## Spec Compliance Docs

- [MQTT 3.1.1 compliance digest](docs/spec/mqtt-v3.1.1.md)
- [MQTT 5.0 compliance digest](docs/spec/mqtt-v5.0.md)
- [Generator usage and schema details](docs/spec/README.md)

## Community

- Follow us on [Twitter](https://twitter.com/bytebeamhq)
- Connect with us on [LinkedIn](https://www.linkedin.com/company/bytebeam/)
- Chat with us on [Discord](https://discord.gg/mpkSqDg)
- Read our official [Blog](https://bytebeam.io/blog/)

## Contributing

Please follow the [code of conduct](docs/CoC.md) while opening issues to report bugs or before you contribute fixes, and read our [contributor guide](CONTRIBUTING.md).

## License

This project is released under The Apache License, Version 2.0 ([LICENSE](./LICENSE) or http://www.apache.org/licenses/LICENSE-2.0)
