from __future__ import annotations

import importlib.metadata
from pathlib import Path

import rumqttc

distribution = importlib.metadata.distribution("rumqttc")
root = Path(str(distribution.locate_file("")))
package = root / "rumqttc"
assert package.is_dir()
assert (package / "py.typed").is_file()
assert (package / "_native.pyi").is_file()
assert any(path.name.startswith("_native.") and path.suffix in {".so", ".pyd"} for path in package.iterdir())
assert rumqttc.MqttClient.__module__ == "rumqttc._client"
assert not any("native-wrappers/python/python" in str(path) for path in map(Path, __import__("sys").path))
