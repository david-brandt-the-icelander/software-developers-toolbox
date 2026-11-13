"""Validate HASD knowledge files: the schemas, their fictional examples and every knowledge file in a repository.

Usage: python3 Meta/Agents/Knowledge/validate_knowledge.py [repository-root]

Requires jsonschema. The schemas are found next to this script. A `.dev/*.json` file whose `kind` has a schema
here is checked against it: its name is `<kind>.json`, its `$schema` points at that schema, its `folder` is the
folder holding the `.dev` directory, entity IDs are unique, and every `path` it mentions exists, as does every
`entityId` named with a path to a JSON file. Files of other kinds (the diagrams and behavior views) are left to
Agents/Skills/validate_diagrams.py. A valid file is well-formed, not true: truth is checked by reading the sources.
"""

import json
import os
import sys
from pathlib import Path

from jsonschema import Draft202012Validator
from referencing import Registry, Resource

HERE = Path(__file__).resolve().parent
SKIPPED_DIRS = {".git", "node_modules", "dist", "dist-test", "build", "bin", "obj", ".wrangler", "__pycache__"}


def read_json(path):
    def unique_keys(pairs):
        result = {}
        for key, value in pairs:
            if key in result:
                raise ValueError(f"Duplicate JSON key: {key}")
            result[key] = value
        return result

    return json.loads(path.read_text(), object_pairs_hook=unique_keys)


def load_validators():
    schemas = {path: read_json(path) for path in sorted((HERE / "schemas").glob("*.schema.json"))}
    registry = Registry().with_resources(
        (schema["$id"], Resource.from_contents(schema)) for schema in schemas.values())
    validators = {}
    for path, schema in schemas.items():
        Draft202012Validator.check_schema(schema)
        kind = path.name.removesuffix(".schema.json")
        if kind != "common":
            validators[kind] = (path, Draft202012Validator(schema, registry=registry))
    return validators


def paths_in(value):
    """Every repository path an entity mentions, with the entity ID named beside it, if any."""
    if isinstance(value, dict):
        if isinstance(value.get("path"), str):
            yield value["path"], value.get("entityId")
        for item in value.values():
            yield from paths_in(item)
    elif isinstance(value, list):
        for item in value:
            yield from paths_in(item)


def check_document(path, document, validators, root=None):
    errors = []
    kind = document.get("kind")
    schema_path, validator = validators[kind]
    errors += [f"{'/'.join(map(str, e.absolute_path)) or '(document)'}: {e.message}"
               for e in validator.iter_errors(document)]
    if errors:
        return errors
    ids = [entity["id"] for entity in document["entities"]]
    errors += [f"Duplicate entity id: {i}" for i in sorted({i for i in ids if ids.count(i) > 1})]
    if (path.parent / document.get("$schema", "")).resolve() != schema_path:
        errors.append(f"$schema should point at {schema_path.name}")
    if root is None:
        return errors
    if path.name != f"{kind}.json":
        errors.append(f"A {kind} file is named {kind}.json")
    if (root / document["folder"]).resolve() != path.parent.parent:
        errors.append(f"folder should be {path.parent.parent.relative_to(root).as_posix() or '.'}")
    for value, entity_id in paths_in(document["entities"]):
        target = (root / value).resolve()
        if Path(value).is_absolute() or not target.is_relative_to(root) or not target.exists():
            errors.append(f"Missing or outside path: {value}")
        elif entity_id and target.suffix == ".json":
            if entity_id not in {e.get("id") for e in read_json(target).get("entities", [])}:
                errors.append(f"No entity {entity_id} in {value}")
    return errors


def dev_files(root):
    for directory, subdirs, files in os.walk(root):
        here = Path(directory)
        subdirs[:] = sorted(d for d in subdirs if d not in SKIPPED_DIRS)
        if here.name == ".dev":
            yield from (here / f for f in sorted(files) if f.endswith(".json"))


def main():
    root = Path(sys.argv[1] if len(sys.argv) > 1 else ".").resolve()
    validators = load_validators()
    failures = 0
    for kind in validators:
        example = HERE / "examples" / f"{kind}.json"
        problems = ["Missing example"] if not example.exists() else check_document(example, read_json(example), validators)
        for problem in problems:
            print(f"{example.relative_to(HERE)}: {problem}")
        failures += len(problems)
    checked = 0
    for path in dev_files(root):
        document = read_json(path)
        if document.get("kind") not in validators:
            continue
        checked += 1
        for problem in check_document(path, document, validators, root):
            print(f"{path.relative_to(root)}: {problem}")
            failures += 1
    print(f"{len(validators)} kinds, {len(validators)} examples, {checked} knowledge files checked, {failures} problems")
    return 1 if failures else 0


if __name__ == "__main__":
    sys.exit(main())
