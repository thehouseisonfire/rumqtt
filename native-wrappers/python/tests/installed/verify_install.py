from __future__ import annotations

import importlib.metadata
import importlib.util
from pathlib import Path

import rumqttc

distribution = importlib.metadata.distribution("rumqttc")
root = Path(str(distribution.locate_file("")))
package = root / "rumqttc"
assert package.is_dir()
assert (package / "py.typed").is_file()
assert (package / "_native.pyi").is_file()
assert any(path.name.startswith("_native.") and path.suffix in {".so", ".pyd"} for path in package.iterdir())
assert Path(rumqttc.__file__).resolve().is_relative_to(package.resolve())
native_spec = importlib.util.find_spec("rumqttc._native")
assert native_spec is not None and native_spec.origin is not None
assert Path(native_spec.origin).resolve().is_relative_to(package.resolve())
assert rumqttc.MqttClient.__module__ == "rumqttc._client"
assert not any("native-wrappers/python/python" in str(path) for path in map(Path, __import__("sys").path))
