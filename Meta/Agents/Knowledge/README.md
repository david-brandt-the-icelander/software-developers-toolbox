# Knowledge files

Everything a solution knows about itself, apart from its code, is kept as JSON in the `.dev/` folder of the folder it is about. Agents write these files and keep them current in the same commits as the code. The [describe-knowledge](../Skills/describe-knowledge/skill.md) skill tells an agent how, and [populate-dev](../Skills/populate-dev/skill.md) how to decide what each folder needs. People read them through tools such as the [Cyberspace explorer](https://github.com/david-brandt-the-icelander/cyberspace-explorer). A file holds one kind of knowledge, and is named after its kind: `.dev/backlog.json`, `.dev/decisions.json`.

The catalogue below is meant to cover what a whole solution needs. No single folder needs most of it. A folder has a file of a kind only when it has something of that kind to say, and nothing is created empty. The repository root's `.dev/` holds what is about the whole solution: its vision, roadmap and plans.

## The shape every file shares

[`schemas/common.schema.json`](schemas/common.schema.json) defines it. A file is a document:

| Field | Holds |
| --- | --- |
| `$schema` | The path from the file to its kind's schema |
| `kind` | The kind, matching the file name |
| `title` | What the file is |
| `folder` | The folder the `.dev/` directory belongs to, from the repository root; `.` for the root |
| `entities` | The records, at least one |

Every record (entity) has a standard set of fields:

| Field | Holds |
| --- | --- |
| `id`, `title`, `summary` | Required. The ID is unique in the file and kept when the record moves. The summary is what a person reads first. |
| `details` | The full explanation, as Markdown |
| `status` | Where the record stands; each kind's description says what its usual values are |
| `origin` | Where the record came from: who or what asked, and where. A sentence, or an object with `kind`, `by`, `date`, `reference` |
| `evidenceStatus` | `source-verified` (checked against the code), `documented` (written down, not checked) or `proposed` (intended, not yet true) |
| `sources` | The files and symbols the record was read from |
| `links` | Anything else that helps a reader: documents, designs, tickets, conversations |
| `relationships` | Typed links to other records: `depends-on`, `part-of`, `implements`, `verifies`, `blocks`, `supersedes` and so on, each a path and optionally an `entityId` |

Each kind adds its own fields, a few of them required. **Any field not named in a schema is allowed.** The readers are agents, and an agent that has something useful to record should record it rather than lose it. The schemas fix what tools and other agents rely on, and leave the rest open.

## The kinds

Each kind has a schema in [`schemas/`](schemas/) and a complete, fictional example in [`examples/`](examples/). All the examples describe the same made-up job scheduler, so they link to each other. Its worker lives in `src/ExampleWorker`.

### Direction

| Kind | Holds | Usually in | Required beyond id, title, summary |
| --- | --- | --- | --- |
| [`vision`](examples/vision.json) | Why the solution exists, for whom, what success looks like, and when it should be shut down. | Repository root | `problem`, `outcome` |
| [`stakeholders`](examples/stakeholders.json) | Who cares about the solution, what they want from it, and how much say they have. | Repository root | `role`, `interest` |
| [`risks`](examples/risks.json) | What could go wrong, how likely and how bad it is, and what is being done about it. | Repository root, or the folder the risk concerns | `likelihood`, `impact` |
| [`roadmap`](examples/roadmap.json) | Milestones in order, each with its goals and the backlog items it needs. | Repository root | `target`, `goals` |

### Requirements

| Kind | Holds | Usually in | Required beyond id, title, summary |
| --- | --- | --- | --- |
| [`requirements`](examples/requirements.json) | What the solution must do for its users, why, and how anyone can tell it does. | Repository root, or the folder that will meet them | `statement` |
| [`quality-requirements`](examples/quality-requirements.json) | How well the solution must run: availability, latency, capacity, security, privacy, cost and the like, each measurable. | Repository root, or the deployable's folder | `area`, `requirement`, `measuredBy` |
| [`glossary`](examples/glossary.json) | Terms, what they mean here, and what they must not be confused with. The title is the term; the summary is its meaning. | Any folder; the most specific folder that uses the term | nothing |

### Work

| Kind | Holds | Usually in | Required beyond id, title, summary |
| --- | --- | --- | --- |
| [`backlog`](examples/backlog.json) | Work somebody wants done: features, defects, debt and the rest. An item is deleted once a view describes it as true; git keeps the history. | The folder that will own the work once it is done | `type`, `origin` |
| [`plan`](examples/plan.json) | A chosen way to deliver some backlog items: the goal, the scope, the approach and its steps. | Repository root, or the folder the plan changes | `goal` |
| [`estimation`](examples/estimation.json) | How much something will take, in a stated unit, as a range with its assumptions. | Next to what is estimated: a plan, a roadmap or a backlog | `subject`, `unit`, `likely` |
| [`tasks`](examples/tasks.json) | Concrete units of work in progress: who has each, its state, and what it waits for. | Next to the plan they carry out | `state` |
| [`decisions`](examples/decisions.json) | Decisions taken, and decisions waiting on someone: context, options with their consequences, the choice and why. | The folder the decision is about | `status`, `context` |
| [`blockers`](examples/blockers.json) | What is blocked, by what, what would unblock it, and who can. | The folder whose work is blocked | `blocked`, `unblockWhen` |

### Structure

| Kind | Holds | Usually in | Required beyond id, title, summary |
| --- | --- | --- | --- |
| [`folder`](examples/folder.json) | What the folder is for, where its boundaries are, and its significant relationships. Children are found from the file tree, never listed here. | Every maintained folder | `purpose` |
| [`contracts`](examples/contracts.json) | Promises made to something outside the folder: an API, a message, a file format, a stored shape, and what may and may not change. | The folder that owns the contract | `contractType` |

### Verification

| Kind | Holds | Usually in | Required beyond id, title, summary |
| --- | --- | --- | --- |
| [`verification`](examples/verification.json) | How a behavior is checked: the criteria and the automated or manual checks. Results are readings, kept elsewhere. | The folder whose behavior is checked | `criteria`, `checks` |

### Delivery

| Kind | Holds | Usually in | Required beyond id, title, summary |
| --- | --- | --- | --- |
| [`pipeline`](examples/pipeline.json) | What the build pipeline does, in order: what triggers each stage, what it checks, and what it blocks. Also what it does not check. | The deployable's folder, or the repository root | `trigger`, `checks` |
| [`delivery`](examples/delivery.json) | Where the deployable runs and how a version gets there: each environment, how it is promoted to, and how it is rolled back. | The deployable's folder | `environment`, `rollback` |
| [`versions`](examples/versions.json) | Released versions of an application, a contract or a stored format, and what changed in each. | The folder that owns what is versioned | `version`, `versionOf` |

### Operation

| Kind | Holds | Usually in | Required beyond id, title, summary |
| --- | --- | --- | --- |
| [`configuration`](examples/configuration.json) | Settings the code reads: key, type, default, whether it is secret, and when a change takes effect. Never a secret's value. | The folder that reads the setting | `key`, `type`, `sensitive` |
| [`observability`](examples/observability.json) | Signals the code emits: what each means, its unit and dimensions, who can read it, and how long it is kept. Readings are kept elsewhere. | The folder that emits the signal | `signalType`, `meaning` |
| [`runbooks`](examples/runbooks.json) | Procedures for setting up, operating, diagnosing, recovering and rolling back, step by step. | The folder the procedure operates on | `purpose`, `steps` |
| [`limitations`](examples/limitations.json) | What the solution deliberately does not do or cannot do, with no promise to change it. | The folder whose behavior is limited | `impact` |
| [`contacts`](examples/contacts.json) | Who to reach about what, and how. Roles and channels, not personal details. | Repository root, or the deployable's folder | `role`, `channel` |

### Reference

| Kind | Holds | Usually in | Required beyond id, title, summary |
| --- | --- | --- | --- |
| [`references`](examples/references.json) | External specifications and documents the folder relies on, and why. | The folder that relies on the reference | `relevance` |

### Views drawn by the skills

The diagrams and behavior views have their own skills, schemas and examples in [`../Skills/`](../Skills/README.md). They are checked by `validate_diagrams.py`.

| Kind | File | Holds |
| --- | --- | --- |
| Component diagram | `component-diagram.json` | The parts of a folder and how they connect |
| Class diagram | `class-diagram.json` | The types a folder declares and their relationships |
| State diagram | `<Identifier>.state-diagram.json` | One state machine the code implements |
| Deployment diagram | `deployment-diagram.json` | Where a deployable runs and what it talks to |
| Use case | `use-case.json` | An actor's goal, step by step, with the branches the code has |
| User journey | `user-journey.json` | A person's experience over time, across use cases and touchpoints |
| Capability | `capability.json` | What a folder can do for someone, and who may ask |
| Feature | `feature.json` | Behavior the product offers, with its outcome and limits |
| Operation | `operation.json` | Callable entrypoints: routes, commands, handlers, messages |
| Actor | `Actors/<name>.md` | A person, outside system or agent that uses the system |

## Rules

- **One owner per fact.** A fact is written once, in the most specific folder it belongs to, and other records link to it with a `relationship`. A parent folder never copies its children.
- **Absent is not none.** A folder with no `backlog.json` has no recorded backlog. That says nothing about whether it has work outstanding. The same goes for every kind.
- **Evidence or a label.** A record checked against the code is `source-verified`, with its `sources`. Prose nobody has checked stays `documented`. Intent stays `proposed`. A record that mixes them takes the weakest.
- **Descriptions, not readings.** Knowledge files say what can be observed, built, checked or deployed. Measurements, test results, build outcomes and what is deployed right now are readings. They belong to the tools that produce them, with their own timestamps.
- **Work ends by being deleted.** A backlog item stays in the folder that will own the work until a view (a feature, a capability, a use case) describes the result as true. Then the item is deleted, and git and the change log keep its history. The same holds for plans, tasks and blockers once they are done.
- **Waiting is visible.** A decision that is waiting on someone is a `decisions` record with `status: open`, saying who it waits on and what it blocks. Anything else that stops work is a `blockers` record.
- **No secrets and no personal details.** Configuration names its secrets and never gives their values. Contacts are roles and channels.
- **IDs are kept.** A record that moves keeps its `id`, and every link to it is repaired in the same commit.

## Where the twelve deployable documents went

The [twelve documents](https://github.com/david-brandt-the-icelander/processes/tree/main/project-management/deployable-documentation) every deployable carries were the starting point for these kinds. They were adapted rather than copied:

| Document | Kinds |
| --- | --- |
| business-plan.md | `vision` |
| stakeholders.md | `stakeholders` |
| risk-assessment.md | `risks` |
| requirements.md | `requirements`, `backlog`, and the feature, capability and use-case views |
| non-functional-requirements.md | `quality-requirements` |
| architecture.md | the diagrams, `decisions`, `backlog` items of type `debt` |
| deployment.md | `delivery`, the deployment diagram |
| cicd.md | `pipeline` |
| development.md | `runbooks` with purpose `setup` |
| operations.md | `runbooks`, `limitations` |
| observability.md | `observability` |
| contacts.md | `contacts` |

## Installing and validating

Copy this folder to `Meta/Agents/Knowledge/` in the target repository, next to `Meta/Agents/Skills/`, and run from the repository root:

```bash
python3 Meta/Agents/Knowledge/validate_knowledge.py
```

It needs the `jsonschema` package. It checks the schemas and the examples, and then every `.dev/*.json` file whose kind is in this catalogue:

- the file is valid against its schema and is named after its kind;
- its `$schema` points at that schema, and its `folder` is where it lives;
- entity IDs are unique;
- every path it mentions exists, as does every `entityId` named beside a path to a JSON file.

Diagrams and behavior views are left to `Meta/Agents/Skills/validate_diagrams.py`. A valid file is well-formed, not true. Whether it is true is checked by reading the sources it cites.
