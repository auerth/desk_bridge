# Architecture Decision Records

Anything that deviates from [`../PLAN.md`](../PLAN.md), or that the plan leaves open, is recorded
as an ADR before it is implemented. If an ADR changes the plan, update the plan in the same pull
request.

* One file per decision: `NNNN-short-title.md`, numbered in order of creation, never renumbered.
* Statuses: `Proposed` -> `Accepted` | `Rejected`; later `Superseded by NNNN` or `Deprecated`.
  Accepted ADRs are not edited except for status changes and typo fixes; a changed decision is a
  new ADR that supersedes the old one.
* Keep it short. Link to code, issues and spike results instead of pasting them.

## Index

| # | Title | Status |
|---|---|---|
| [0001](0001-tech-stack.md) | Technology stack | Accepted |
| [0002](0002-audio-loopback-and-mute.md) | Laptop sound via WASAPI loopback, mute behaviour | Accepted (laptop check pending) |
| [0003](0003-virtual-audio-driver.md) | Virtual audio driver / virtual microphone (F3) | Proposed |
| [0004](0004-virtual-display-driver.md) | Virtual display driver for focus mode | Proposed |
| [0005](0005-video-spike.md) | Video spike: 5120x1440@120 capture, encode, decode | see file |
| [0006](0006-service-and-secure-desktop.md) | SYSTEM service, secure-desktop helper, Ctrl+Alt+Del | Accepted |

Add new rows when you add a file.

## Template

```markdown
# ADR NNNN - Title

* Status: Proposed | Accepted | Rejected | Superseded by NNNN
* Date: YYYY-MM-DD
* Deciders: names
* Related: plan sections, issues, other ADRs

## Context

What problem are we solving, what forces are at play (requirements, constraints, measurements)?

## Decision

What we will do, stated in the active voice ("We use ...").

## Consequences

Positive, negative and follow-up work. Include risks and how we will notice if the decision turns
out wrong.

## Alternatives considered

Each with the reason it was not chosen.
```
