---
name: describe-actor
description: Create or maintain Actors/<name>.md files, one per actor named by a user journey, use case or diagram, when documenting or changing who the system is for.
---

# Actors

An actor is someone or something outside the system that pursues a goal through it: a person in a role (resident, operator, back office), an external system with goals of its own toward this one (a vendor's cloud that calls in, a payment provider the system serves), or another agent that uses the system. Nothing inside the system is an actor: a component, a scheduled job or an agent the system runs is a component. Managed infrastructure the system merely uses (a database, a broker, a cache) is an external component, not an actor.

Every actor named anywhere in the repository has exactly one file, `Actors/<kebab-case-name>.md`, in the `Actors/` folder at the repository root. The folder is at the root because an actor is not next to any code: the same resident appears in several folders' diagrams and several journeys. Whatever names an actor links to its file: a use case or user journey (its `actorDocument` and `supportingActors`), a step by an actor in a use case, a `client` or `external-service` node in a deployment diagram (its `actor` field) and an external component in a component diagram (its `actor` field). The file's title is the name the other files use.

Write the file from [template.md](template.md); [example.md](example.md) is a complete fictional example, not evidence about this system. Fill in the actor's kind; the evidence, meaning where the code knows about them (a role, a claim, a client credential, a webhook sender, a registered client), as repository-relative paths and symbols sufficient to verify the file; what they want from the system, in their own terms; how they reach it, only the channels the sources show; and which other actors they are easily confused with, with the distinction the code does or does not make. Link the use cases, journeys and features that concern them, by the path of their `.dev/` file. Do not describe what the system does for them: that belongs to those views, which the file links to and never restates.

Do not invent the actors every system is assumed to have. There is no Admin until a journey, a use case or the code says there is. Do not split one person into roles the code does not distinguish, and do not merge two that it does. An actor is a role, never a named person: no personal names, addresses, account identifiers or credentials. Delete the file of an actor nothing references any more, in the same commit as the reference that went; do not keep it. Do not run the application to find its actors; read the authorization code, the client registrations, the journeys and the diagrams.

Check the links in both directions: every link to `Actors/` lands on a file, and every file in `Actors/` is linked from at least one other file. `python3 Meta/Agents/Skills/validate_diagrams.py`, run from the repository root with `jsonschema` installed, checks both, along with the `actor` fields of the diagrams. A file that passes is well-formed, not true: truth is checked by reading the evidence it cites.
