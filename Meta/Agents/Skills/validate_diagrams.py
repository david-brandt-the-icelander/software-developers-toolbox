"""Validate architecture and behavior schemas, examples, diagrams and actor files. Requires jsonschema.

Usage: python3 Meta/Agents/Skills/validate_diagrams.py [repository-root]

Component, class, state and deployment diagrams and use-case, user-journey, capability, feature and
operation views are checked. The skills are found next to this script. Every `.dev/*.json` file in the repository is checked,
except under directories that never hold source: .git, node_modules, build output and the skills
themselves. If the repository has an `Actors/` folder, its files are checked too: every link to
`Actors/<name>.md`, in a diagram's `actor` field or in a Markdown file, must land on a file, and every
file in `Actors/` must be linked from at least one other file.
"""

import json
import os
import re
import sys
from collections import Counter
from pathlib import Path

from jsonschema import Draft202012Validator

KINDS = ("component", "class", "state", "deployment")
BEHAVIOR_KINDS = ("use-case", "user-journey", "capability", "feature", "operation")
SKILL_FOLDERS = {**{kind: f"describe-{kind}-diagram" for kind in KINDS},
                 **{kind: f"describe-{kind}" for kind in BEHAVIOR_KINDS}}
ACTORS_DIR = "Actors"
# A Markdown link to an actor: inline, [text](../Actors/name.md#part "title"), or a reference
# definition, [label]: Actors/name.md. A path written in code, or in plain text, is not a link.
ACTOR_LINK = re.compile(
    r"\]\(\s*<?(?:\.{1,2}/|/)*(Actors/[A-Za-z0-9._-]+\.md)(?:#[^\s)>]*)?>?(?:\s+[\"'(][^)]*)?\)"
    r"|^\s{0,3}\[[^\]]+\]:\s*<?(?:\.{1,2}/|/)*(Actors/[A-Za-z0-9._-]+\.md)",
    re.MULTILINE)
CODE = re.compile(r"^(```|~~~).*?^\1[^\n]*$|`[^`\n]+`", re.MULTILINE | re.DOTALL)


def actor_links(text):
    """The actor files a Markdown text links to, leaving out code blocks and code spans."""
    return {a or b for a, b in ACTOR_LINK.findall(CODE.sub("", text))}
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


def require(condition, message):
    if not condition:
        raise ValueError(message)


def repo_path(root, value):
    require(not Path(value).is_absolute(), f"Expected repository-relative path: {value}")
    resolved = (root / value).resolve()
    require(resolved.is_relative_to(root), f"Path escapes repository: {value}")
    require(resolved.exists(), f"Missing path: {value}")
    return resolved


def check_graph(document):
    collection = {"component": "components", "class": "types", "state": "states",
                  "deployment": "nodes"}.get(document["kind"], "entities")
    ids = [item["id"] for item in document[collection]]
    require(len(ids) == len(set(ids)), f"Duplicate IDs in {collection}")
    edges = document.get("relationships", document.get("transitions", []))
    for edge in edges:
        require(edge["from"] in ids, f"Unknown source: {edge['from']}")
        require(edge["to"] in ids, f"Unknown target: {edge['to']}")
    if document["kind"] == "state":
        require(document["initialState"] in ids, "Unknown initial state")
    for entity in document.get("entities", []):
        for collection in ("flow", "stages"):
            steps = [step["id"] for step in entity.get(collection, [])]
            require(len(steps) == len(set(steps)), f"Duplicate {collection} IDs in {entity['id']}")
        for alternative in entity.get("alternatives", []):
            require(alternative["atStep"] in [step["id"] for step in entity["flow"]],
                    f"Unknown step: {alternative['atStep']}")
    if document["kind"] == "deployment":
        artifacts = document["artifacts"]
        artifact_ids = [item["id"] for item in artifacts]
        require(len(artifact_ids) == len(set(artifact_ids)), "Duplicate artifact IDs")
        for artifact in artifacts:
            require(artifact["deployedTo"] in ids, "Unknown deployment target")


def source_files(root, skills):
    for directory, subdirs, files in os.walk(root):
        here = Path(directory)
        subdirs[:] = sorted(d for d in subdirs if d not in SKIPPED_DIRS and (here / d).resolve() != skills)
        yield here, sorted(files)


def diagram_files(root, skills):
    for here, files in source_files(root, skills):
        if here.name == ".dev":
            yield from (here / f for f in files if f.endswith(".json"))


def markdown_files(root, skills):
    for here, files in source_files(root, skills):
        yield from (here / f for f in files if f.endswith(".md"))


def actor_reference(root, value):
    """Check one `actor` field or link and return the actor file it names."""
    require(Path(value).parts[:1] == (ACTORS_DIR,), f"Actor reference is not under {ACTORS_DIR}/: {value}")
    require(value.endswith(".md"), f"Actor reference is not a Markdown file: {value}")
    target = repo_path(root, value)
    require(target.is_file(), f"Actor is not a file: {value}")
    return target


def check_actors(root, skills, referenced):
    """Every actor file is linked from some other file, and every link lands on a file."""
    actors_dir = root / ACTORS_DIR
    if not actors_dir.is_dir():
        require(not referenced, f"Diagrams name actors but there is no {ACTORS_DIR}/ folder")
        return 0
    actors = sorted(p for p in actors_dir.iterdir() if p.is_file() and p.name != "README.md")
    for actor in actors:
        require(actor.suffix == ".md", f"{ACTORS_DIR}/{actor.name}: not a Markdown file")
    for path in markdown_files(root, skills):
        for link in sorted(actor_links(path.read_text())):
            try:
                target = actor_reference(root, link)
            except Exception as error:
                raise ValueError(f"{path.relative_to(root)}: {error}") from error
            if target != path.resolve():
                referenced.add(target)
    for actor in actors:
        require(actor.resolve() in referenced,
                f"{ACTORS_DIR}/{actor.name}: nothing links to this actor; delete it or link it")
    return len(actors)


def check_behavior_references(root, value, referenced_actors):
    """Every source a behavior view cites is a file, and every actorDocument is an actor file."""
    if isinstance(value, dict):
        if isinstance(value.get("path"), str) and "symbol" in value:
            require(repo_path(root, value["path"]).is_file(), f"Source is not a file: {value['path']}")
        if "actorDocument" in value:
            referenced_actors.add(actor_reference(root, value["actorDocument"]))
        for child in value.values():
            check_behavior_references(root, child, referenced_actors)
    elif isinstance(value, list):
        for child in value:
            check_behavior_references(root, child, referenced_actors)


def validate(root):
    skills = Path(__file__).resolve().parent
    schemas = {}
    for kind in SKILL_FOLDERS:
        folder = skills / SKILL_FOLDERS[kind]
        schema = read_json(folder / "schema.json")
        Draft202012Validator.check_schema(schema)
        validator = Draft202012Validator(schema)
        example = read_json(folder / "example.json")
        validator.validate(example)
        check_graph(example)
        require((folder / example["$schema"]).resolve() == (folder / "schema.json").resolve(),
                "Example references the wrong schema")
        schemas[kind] = validator

    # Knowledge files are validated by Agents/Knowledge/validate_knowledge.py, so their kinds are skipped here.
    knowledge = skills.parent / "Knowledge" / "schemas"
    counts = Counter()
    referenced_actors = set()
    for path in diagram_files(root, skills):
        document = read_json(path)
        try:
            kind = document["kind"]
            if kind not in schemas and (knowledge / f"{kind}.schema.json").exists():
                continue
            require(kind in schemas, f"Unknown diagram kind: {kind}")
            schemas[kind].validate(document)
            check_graph(document)
            expected_schema = skills / SKILL_FOLDERS[kind] / "schema.json"
            require((path.parent / document["$schema"]).resolve() == expected_schema.resolve(),
                    "Diagram references the wrong schema")
            require(repo_path(root, document["folder"]) == path.parent.parent,
                    "Folder does not match diagram location")
            expected_name = f"{kind}-diagram.json" if kind in KINDS else f"{kind}.json"
            require(path.name.endswith(".state-diagram.json") if kind == "state"
                    else path.name == expected_name, "Unexpected diagram filename")
            for source in document["sources"]:
                require(repo_path(root, source["path"]).is_file(), f"Source is not a file: {source['path']}")
            for item in document.get("types", []):
                for source in item["sources"]:
                    require(repo_path(root, source).is_file(), f"Type source is not a file: {source}")
            for item in document.get("components", []) + document.get("nodes", []):
                if "actor" in item:
                    referenced_actors.add(actor_reference(root, item["actor"]))
            for component in document.get("components", []):
                if "folder" in component:
                    require(repo_path(root, component["folder"]).is_dir(),
                            f"Component folder is not a directory: {component['folder']}")
                if "diagram" in component:
                    target = read_json(repo_path(root, component["diagram"]))
                    require(target["kind"] == "component" and target["folder"] == component.get("folder"),
                            "Component reference describes a different folder or diagram kind")
            if kind in BEHAVIOR_KINDS:
                check_behavior_references(root, document["entities"], referenced_actors)
            for related in document.get("relatedViews", []):
                target_path = repo_path(root, related["path"])
                target = read_json(target_path)
                require(target["kind"] == related["kind"] and target["folder"] == related["folder"],
                        f"Related view describes a different folder or kind: {related['path']}")
                require(repo_path(root, related["folder"]) == target_path.parent.parent,
                        f"Related view's folder does not match its location: {related['path']}")
            for artifact in document.get("artifacts", []):
                require(repo_path(root, artifact["source"]).is_file(), f"Artifact source is not a file: {artifact['source']}")
        except Exception as error:
            raise ValueError(f"{path.relative_to(root)}: {getattr(error, 'message', error)}") from error
        counts[kind] += 1
    require(sum(counts.values()) > 0, "No diagrams found")
    actors = check_actors(root, skills, referenced_actors)
    print(f"Validated {len(SKILL_FOLDERS)} schemas and examples; " + ", ".join(
        f"{count} {kind}" for kind, count in sorted(counts.items()))
        + f"; {actors} actors.")


if __name__ == "__main__":
    repository = Path(sys.argv[1]).resolve() if len(sys.argv) > 1 else Path(__file__).resolve().parents[3]
    validate(repository)
