# Example operator

Fictional example; not a description of this repository.

**Kind:** person in a role
**Evidence:** `src/ExampleWorker/Authorization/Roles.cs` (`ExampleWorker.Authorization.Roles.Operator`), `src/ExampleWorker/Admin/OperatorController.cs` (`ExampleWorker.Admin.OperatorController`)

## What they want from the system

- To see which messages are pending and how long they have waited ([features](../src/ExampleWorker/.dev/feature.json), entity `queue-overview`).
- To retry or discard a message that keeps failing ([use cases](../src/ExampleWorker/.dev/use-case.json), entity `replay-message`).

## How they reach it

The admin pages served by `src/ExampleWorker/Admin`, signed in with the operator role. There is no API for operators; the one API client is the [example scheduler](example-scheduler.md).

## Not to be confused with

The [example scheduler](example-scheduler.md), an external system that submits messages over the API. An operator never submits a message; the code distinguishes the two by the operator role on the one hand and a client credential on the other.
