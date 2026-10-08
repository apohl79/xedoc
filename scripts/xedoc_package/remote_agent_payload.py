"""Build and verify the deterministic remote-agent Python payload."""

from __future__ import annotations

import hashlib
import json
from pathlib import Path
import stat
import zipfile


REMOTE_AGENT_SOURCE = Path(__file__).resolve().parents[2] / "remote-agent"
REMOTE_AGENT_PACKAGE_SOURCE = REMOTE_AGENT_SOURCE / "xedoc_remote_agent"
SESSION_SCRIPT_SDK_SOURCE = (
    Path(__file__).resolve().parents[1] / "session_script_sdk.py"
)
REMOTE_TOOLS_SOURCE = REMOTE_AGENT_SOURCE / "remote-tools.json"
REMOTE_AGENT_VERSION = "0.1.0"
PAYLOAD_VERSION = "xedoc.remote-agent/v1"
CONTRACT_VERSION = 1
TOOLS_SCHEMA_VERSION = 1
PAYLOAD_PATH = Path("remote-agent") / "remote-agent.pyz"
TOOLS_PATH = Path("remote-agent") / "remote-tools.json"
MANIFEST_PATH = Path("remote-agent") / "remote-agent-manifest.json"
RUNTIME_PATH = Path("remote-agent") / "runtime"
PYZ_MODE = stat.S_IRUSR | stat.S_IWUSR | stat.S_IRGRP | stat.S_IROTH


def build_payload_archive(destination: Path, *, force: bool) -> str:
    """Write a stable zipapp containing only checked-in Python sources."""

    sources = _payload_sources()
    destination.parent.mkdir(parents=True, exist_ok=True)
    if destination.exists():
        if not force:
            raise RuntimeError(f"Remote-agent payload already exists: {destination}")
        destination.unlink()
    with zipfile.ZipFile(
        destination,
        "w",
        compression=zipfile.ZIP_STORED,
    ) as archive:
        for archive_name, source in sorted(sources.items()):
            info = zipfile.ZipInfo(archive_name, date_time=(1980, 1, 1, 0, 0, 0))
            info.compress_type = zipfile.ZIP_STORED
            info.external_attr = PYZ_MODE << 16
            archive.writestr(info, source.read_bytes())
    return sha256_file(destination)


def payload_sources() -> dict[str, Path]:
    """Return the archive source map for validation and release tooling."""

    return dict(_payload_sources())


def remote_tools_bytes() -> bytes:
    if not REMOTE_TOOLS_SOURCE.is_file():
        raise RuntimeError(f"Missing remote-agent tool schema: {REMOTE_TOOLS_SOURCE}")
    data = REMOTE_TOOLS_SOURCE.read_bytes()
    _validate_json_size(data, "remote-agent tool schema")
    try:
        value = json.loads(data)
    except (UnicodeError, json.JSONDecodeError) as error:
        raise RuntimeError(
            f"Invalid remote-agent tool schema: {REMOTE_TOOLS_SOURCE}"
        ) from error
    _validate_tools_value(value)
    return data


def write_manifest(
    destination: Path,
    *,
    package_version: str,
    target: str,
    runtime: dict[str, object],
    payload_sha256: str,
    tools_sha256: str,
    payload_size: int,
    tools_size: int,
) -> dict[str, object]:
    """Write the bounded package manifest and return its JSON value."""

    manifest: dict[str, object] = {
        "manifestVersion": 1,
        "contractVersion": PAYLOAD_VERSION,
        "packageVersion": package_version,
        "target": target,
        "payload": {
            "path": PAYLOAD_PATH.as_posix(),
            "version": REMOTE_AGENT_VERSION,
            "sha256": payload_sha256,
            "bytes": payload_size,
        },
        "tools": {
            "path": TOOLS_PATH.as_posix(),
            "schemaVersion": TOOLS_SCHEMA_VERSION,
            "sha256": tools_sha256,
            "bytes": tools_size,
        },
        "runtime": {
            "path": RUNTIME_PATH.as_posix(),
            **runtime,
        },
    }
    encoded = (
        json.dumps(manifest, ensure_ascii=False, indent=2, sort_keys=True) + "\n"
    ).encode("utf-8")
    if len(encoded) > 64 * 1024:
        raise RuntimeError("Remote-agent package manifest exceeds its size bound")
    destination.write_bytes(encoded)
    return manifest


def load_manifest(path: Path) -> dict[str, object]:
    try:
        data = path.read_bytes()
        _validate_json_size(data, "remote-agent package manifest")
        value = json.loads(data)
    except (OSError, UnicodeError, json.JSONDecodeError) as error:
        raise RuntimeError(f"Invalid remote-agent package manifest: {path}") from error
    if not isinstance(value, dict):
        raise RuntimeError("Remote-agent package manifest must be an object")
    return value


def validate_payload_archive(path: Path, expected_sha256: str) -> None:
    if not path.is_file():
        raise RuntimeError(f"Missing remote-agent payload: {path}")
    if sha256_file(path) != expected_sha256:
        raise RuntimeError(f"Remote-agent payload checksum mismatch: {path}")
    expected = set(_payload_sources())
    try:
        with zipfile.ZipFile(path) as archive:
            names = set(archive.namelist())
            if names != expected:
                missing = sorted(expected - names)
                unexpected = sorted(names - expected)
                raise RuntimeError(
                    "Remote-agent payload entries differ from the checked-in "
                    f"payload (missing={missing}, unexpected={unexpected})"
                )
            for info in archive.infolist():
                if (
                    info.is_dir()
                    or info.filename.startswith("/")
                    or ".." in Path(info.filename).parts
                ):
                    raise RuntimeError(
                        f"Remote-agent payload contains unsafe entry: {info.filename}"
                    )
    except (OSError, zipfile.BadZipFile) as error:
        raise RuntimeError(f"Invalid remote-agent payload archive: {path}") from error


def validate_tools_file(path: Path, expected_sha256: str) -> None:
    if not path.is_file():
        raise RuntimeError(f"Missing remote-agent tool schema: {path}")
    if sha256_file(path) != expected_sha256:
        raise RuntimeError(f"Remote-agent tool schema checksum mismatch: {path}")
    try:
        data = path.read_bytes()
        _validate_json_size(data, "remote-agent tool schema")
        value = json.loads(data)
    except (OSError, UnicodeError, json.JSONDecodeError) as error:
        raise RuntimeError(f"Invalid remote-agent tool schema: {path}") from error
    _validate_tools_value(value)


def validate_manifest_value(
    value: dict[str, object],
    *,
    package_version: str,
    target: str,
    payload_sha256: str,
    tools_sha256: str,
    payload_size: int,
    tools_size: int,
    dependency_lock_sha256: str,
    dependencies: tuple[str, ...],
    site_packages: str,
) -> None:
    if value.get("manifestVersion") != 1:
        raise RuntimeError("Unsupported remote-agent manifest version")
    if value.get("contractVersion") != PAYLOAD_VERSION:
        raise RuntimeError("Unsupported remote-agent contract version")
    if value.get("packageVersion") != package_version or value.get("target") != target:
        raise RuntimeError("Remote-agent manifest package identity mismatch")
    payload = _manifest_object(value, "payload")
    tools = _manifest_object(value, "tools")
    runtime = _manifest_object(value, "runtime")
    _validate_hash_entry(
        payload,
        path=PAYLOAD_PATH.as_posix(),
        expected_sha256=payload_sha256,
        expected_size=payload_size,
        version_key="version",
        version=REMOTE_AGENT_VERSION,
    )
    _validate_hash_entry(
        tools,
        path=TOOLS_PATH.as_posix(),
        expected_sha256=tools_sha256,
        expected_size=tools_size,
        version_key="schemaVersion",
        version=TOOLS_SCHEMA_VERSION,
    )
    if set(runtime) != {
        "dependencies",
        "dependencyLockSha256",
        "interpreter",
        "path",
        "pythonVersion",
        "runtimeId",
        "sha256",
        "sitePackages",
        "target",
    }:
        raise RuntimeError("Remote-agent runtime metadata fields are invalid")
    if runtime.get("path") != RUNTIME_PATH.as_posix():
        raise RuntimeError("Remote-agent runtime path is invalid")
    if runtime.get("target") != target:
        raise RuntimeError("Remote-agent runtime target mismatch")
    for key in ("runtimeId", "pythonVersion", "sha256"):
        value = runtime.get(key)
        if not isinstance(value, str) or not value:
            raise RuntimeError(f"Remote-agent runtime metadata is missing {key}")
    interpreter = runtime.get("interpreter")
    expected_interpreter = (
        "runtime/python/python.exe"
        if target.endswith("-windows-msvc")
        else "runtime/python/bin/python3"
    )
    if interpreter != expected_interpreter:
        raise RuntimeError("Remote-agent runtime interpreter is invalid")
    actual_dependencies = runtime.get("dependencies")
    if (
        not isinstance(actual_dependencies, list)
        or tuple(actual_dependencies) != dependencies
        or runtime.get("dependencyLockSha256") != dependency_lock_sha256
        or runtime.get("sitePackages") != site_packages
    ):
        raise RuntimeError("Remote-agent runtime dependencies are invalid")


def sha256_file(path: Path) -> str:
    with path.open("rb") as file:
        return hashlib.file_digest(file, "sha256").hexdigest()


def _payload_sources() -> dict[str, Path]:
    if not REMOTE_AGENT_PACKAGE_SOURCE.is_dir():
        raise RuntimeError(
            f"Missing remote-agent Python package: {REMOTE_AGENT_PACKAGE_SOURCE}"
        )
    if not SESSION_SCRIPT_SDK_SOURCE.is_file():
        raise RuntimeError(f"Missing session-script SDK: {SESSION_SCRIPT_SDK_SOURCE}")
    entries = {
        "__main__.py": REMOTE_AGENT_SOURCE / "__main__.py",
        "scripts/__init__.py": REMOTE_AGENT_SOURCE / "scripts" / "__init__.py",
        "scripts/session_script_sdk.py": SESSION_SCRIPT_SDK_SOURCE,
    }
    for path in sorted(REMOTE_AGENT_PACKAGE_SOURCE.rglob("*.py")):
        if "__pycache__" in path.parts:
            continue
        entries[
            f"xedoc_remote_agent/{path.relative_to(REMOTE_AGENT_PACKAGE_SOURCE)}"
        ] = path
    for archive_name, path in entries.items():
        if not path.is_file():
            raise RuntimeError(f"Missing remote-agent payload source: {path}")
    return entries


def _manifest_object(value: dict[str, object], key: str) -> dict[str, object]:
    nested = value.get(key)
    if not isinstance(nested, dict):
        raise RuntimeError(f"Remote-agent manifest field {key!r} is invalid")
    return nested


def _validate_hash_entry(
    value: dict[str, object],
    *,
    path: str,
    expected_sha256: str,
    expected_size: int,
    version_key: str,
    version: object,
) -> None:
    if (
        value.get("path") != path
        or value.get(version_key) != version
        or value.get("sha256") != expected_sha256
        or value.get("bytes") != expected_size
    ):
        raise RuntimeError(f"Remote-agent manifest entry is invalid for {path}")


def _validate_tools_value(value: object) -> None:
    if not isinstance(value, dict):
        raise RuntimeError("Remote-agent tool schema must be an object")
    if set(value) != {"schemaVersion", "contractVersion", "namespace", "tools"}:
        raise RuntimeError("Remote-agent tool schema has invalid top-level fields")
    if (
        type(value.get("schemaVersion")) is not int
        or value.get("schemaVersion") != TOOLS_SCHEMA_VERSION
    ):
        raise RuntimeError("Unsupported remote-agent tool schema version")
    if value.get("contractVersion") != PAYLOAD_VERSION:
        raise RuntimeError("Unsupported remote-agent tool contract version")
    if value.get("namespace") != "remote":
        raise RuntimeError("Remote-agent tool schema has an invalid namespace")
    tools = value.get("tools")
    if not isinstance(tools, list) or not tools or len(tools) > 64:
        raise RuntimeError("Remote-agent tool schema has an invalid tool list")
    names: set[str] = set()
    for tool in tools:
        if not isinstance(tool, dict):
            raise RuntimeError("Remote-agent tool schema has an invalid tool")
        if set(tool) != {"name", "description", "inputSchema"}:
            raise RuntimeError("Remote-agent tool schema has invalid tool fields")
        name = tool.get("name")
        if (
            not isinstance(name, str)
            or not name.startswith("remote_")
            or not 1 <= len(name) <= 128
            or name in names
        ):
            raise RuntimeError("Remote-agent tool schema has an invalid name")
        names.add(name)
        description = tool.get("description")
        if (
            not isinstance(description, str)
            or not description.strip()
            or len(description) > 1024
        ):
            raise RuntimeError("Remote-agent tool schema has an invalid description")
        _validate_input_schema(tool.get("inputSchema"))


def _validate_input_schema(value: object, *, depth: int = 0) -> None:
    if depth > 8 or not isinstance(value, dict):
        raise RuntimeError("Remote-agent tool schema has an invalid input schema")
    schema_type = value.get("type")
    if not isinstance(schema_type, str) or schema_type not in {
        "array",
        "integer",
        "object",
        "string",
    }:
        raise RuntimeError("Remote-agent tool schema has an unsupported schema type")
    allowed = {
        "object": {
            "additionalProperties",
            "description",
            "properties",
            "required",
            "type",
        },
        "array": {"description", "items", "maxItems", "minItems", "type"},
        "integer": {"description", "enum", "maximum", "minimum", "type"},
        "string": {
            "description",
            "enum",
            "maxLength",
            "minLength",
            "pattern",
            "type",
        },
    }[schema_type]
    if set(value) - allowed:
        raise RuntimeError("Remote-agent tool schema has invalid schema fields")
    description = value.get("description")
    if description is not None and (
        not isinstance(description, str)
        or not description.strip()
        or len(description) > 1024
    ):
        raise RuntimeError("Remote-agent schema description is invalid")
    if schema_type == "object":
        properties = value.get("properties")
        required = value.get("required")
        if (
            not isinstance(properties, dict)
            or len(properties) > 64
            or not isinstance(required, list)
            or len(required) > 64
            or value.get("additionalProperties") is not False
        ):
            raise RuntimeError("Remote-agent object schema is invalid")
        property_names: set[str] = set()
        for name, property_schema in properties.items():
            if (
                not isinstance(name, str)
                or not 1 <= len(name) <= 128
                or name in property_names
            ):
                raise RuntimeError("Remote-agent object schema has invalid properties")
            property_names.add(name)
            _validate_input_schema(property_schema, depth=depth + 1)
        required_names: set[str] = set()
        for name in required:
            if (
                not isinstance(name, str)
                or name in required_names
                or name not in property_names
            ):
                raise RuntimeError(
                    "Remote-agent object schema has invalid required fields"
                )
            required_names.add(name)
        return
    if "properties" in value or "required" in value or "additionalProperties" in value:
        raise RuntimeError("Remote-agent non-object schema has object fields")
    _validate_scalar_bounds(value, schema_type)
    if schema_type == "array":
        items = value.get("items")
        if not isinstance(items, dict):
            raise RuntimeError("Remote-agent array schema is missing items")
        _validate_input_schema(items, depth=depth + 1)


def _validate_scalar_bounds(value: dict[str, object], schema_type: str) -> None:
    if schema_type == "string":
        _validate_integer_bound(value, "minLength", minimum=0, maximum=65536)
        _validate_integer_bound(value, "maxLength", minimum=0, maximum=65536)
        _validate_ordered_bounds(value, "minLength", "maxLength")
        pattern = value.get("pattern")
        if pattern is not None and pattern != "^[0-9a-f]{64}$":
            raise RuntimeError("Remote-agent string schema has an invalid pattern")
    elif schema_type == "integer":
        _validate_integer_bound(value, "minimum", minimum=-65536, maximum=65536)
        _validate_integer_bound(value, "maximum", minimum=-65536, maximum=65536)
        _validate_ordered_bounds(value, "minimum", "maximum")
    elif schema_type == "array":
        _validate_integer_bound(value, "minItems", minimum=0, maximum=64)
        _validate_integer_bound(value, "maxItems", minimum=0, maximum=64)
        _validate_ordered_bounds(value, "minItems", "maxItems")
    enum = value.get("enum")
    if enum is not None:
        if not isinstance(enum, list) or not enum or len(enum) > 32:
            raise RuntimeError("Remote-agent schema enum is invalid")
        if any(
            not isinstance(item, (str, int))
            or isinstance(item, bool)
            or (isinstance(item, str) and len(item) > 1024)
            for item in enum
        ):
            raise RuntimeError("Remote-agent schema enum value is invalid")


def _validate_integer_bound(
    value: dict[str, object],
    key: str,
    *,
    minimum: int,
    maximum: int,
) -> None:
    bound = value.get(key)
    if bound is not None and (
        type(bound) is not int or not minimum <= bound <= maximum
    ):
        raise RuntimeError(f"Remote-agent schema bound {key!r} is invalid")


def _validate_ordered_bounds(
    value: dict[str, object],
    lower_key: str,
    upper_key: str,
) -> None:
    lower = value.get(lower_key)
    upper = value.get(upper_key)
    if lower is not None and upper is not None and lower > upper:
        raise RuntimeError(
            f"Remote-agent schema bounds {lower_key!r}/{upper_key!r} are invalid"
        )


def _validate_json_size(data: bytes, label: str) -> None:
    if len(data) > 64 * 1024:
        raise RuntimeError(f"{label} exceeds its size bound")
