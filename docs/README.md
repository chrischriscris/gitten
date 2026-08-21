# plait docs

`AGENTS.md` holds the philosophy — three rules, the boundary, the *don't*s. It is
short on purpose and stays short. This directory holds everything that would
otherwise bloat it: how each system actually works, what was decided and why,
which numbers those decisions rest on, and the diagrams.

## Systems

| | |
|---|---|
| [architecture.md](architecture.md) | crates, the boundary, what flows where |
| [diff-pipeline.md](diff-pipeline.md) | raw diff → rows on screen, stage by stage |
| [syntax-highlighting.md](syntax-highlighting.md) | the scanner, the tables, the routing |
| [theming.md](theming.md) | colour as data, surfaces, contrast resolution |
| [commit-graph.md](commit-graph.md) | lane assignment, hues, the cap, the drawing |
| [terminal.md](terminal.md) | the terminal frontend, and what writing it moved into `core` |
| [extending.md](extending.md) | every seam, with a worked example each |

## Decisions

[decisions/](decisions/) — one file per decision, newest last. Read these before
changing anything they cover; each records what was tried, what the numbers said
and what it would take to revisit.

## Evidence

[measurements.md](measurements.md) — every number quoted anywhere else, with the
command that produced it. A figure in a decision record that is not reproducible
here is a figure to distrust.

## Keeping this honest

Three habits, and they matter more than completeness:

**Write what is, not what is planned.** A doc describing an unbuilt feature is
worse than no doc, because it costs a reader real time before they find out. Say
"not built" and move on — [architecture.md](architecture.md) has a section for it.

**Numbers carry their command.** Every figure names how to reproduce it. Hardware
changes, fixtures change, and an unreproducible number quietly becomes folklore.

**Detail belongs next to the code first.** If a rule only matters while reading
one function, it is a doc comment, not a page here. These pages are for what
spans files: a pipeline, a boundary, a decision, a measurement.
