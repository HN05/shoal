---
name: shoal
description: Reserve TCP ports and acquire Xcode simulators through Shoal when development or testing needs these resources in a Shoal-managed session.
---

# Shoal resources

Use `--json` and omit target arguments to use the current context. Request only
resources needed for the task; use the returned port numbers and simulator UDIDs.

## Ports

```sh
shoal --json ports
shoal --json port reserve web
shoal port release web
```

Use configured names/defaults when available. Repeating a reservation returns the
same port. Pass `--reason "purpose"` for an ad hoc reservation.

Exit 2 with `reserved: false` means a conflict suggestion, not a reservation.
If the suggested port suits the task, accept by repeating the request with
`--port <suggested_port>`. Always use the returned `port`; don't assume the
preferred number was allocated. New reservations do not update your current
environment: pass the number to the server explicitly.

## Simulators

```sh
shoal --json sim list
shoal --json sim acquire --wait 60
shoal sim release
```

Acquisition uses configured preferences and returns an exclusive lease. Use its
`udid` explicitly with app/test tools, never simctl's ambiguous `booted` selector.
Let Shoal manage device creation, boot, shutdown, erase, and deletion. Exit 2
means capacity is busy; don't bypass allocation or retry indefinitely.

Normal reuse preserves apps, data, and settings. Only request a clean device
when the task specifically requires pristine state:

```sh
shoal --json sim acquire --clean --reason "Verify first-launch permission prompts"
```

Give the actual task-specific reason; clean requests and their outcomes are
audited. Release an existing lease before requesting it clean. Shoal chooses
the device to minimize reinstalls. Only installed runtimes are supported.

Stop servers, app automation, and debuggers before releasing their resources.
Release when finished; reservations and leases outlive the requesting command.
