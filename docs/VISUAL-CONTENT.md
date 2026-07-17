# Visual content plan

Placement suggestions for screenshots, terminal recordings, and video
walkthroughs that would strengthen Aion's documentation.

## README

| Location | Content | Format |
|---|---|---|
| After "What you get" | Terminal recording: `aion deploy` → `aion start` → `aion describe --pretty` showing a completed run | asciinema or GIF, ~30s |
| After "Honest limits" | Screenshot of the ops console dashboard (when ready) | PNG, light and dark variants |

## Quickstart (AWL)

| Location | Content | Format |
|---|---|---|
| After "Write a workflow" | AWL syntax-highlighted in an editor with LSP hover visible | PNG screenshot |
| After "Deploy and run" | Terminal recording: deploy, start, describe cycle | asciinema or GIF, ~20s |
| After "Prove it survives" | Terminal recording: start → kill -9 → restart → run resumes | asciinema or GIF, ~30s (the money shot) |

## Getting started (Gleam)

| Location | Content | Format |
|---|---|---|
| Before prerequisites | Architecture diagram: workflow → server → worker, with arrows showing the deploy/start/signal flow | SVG |
| After step 6 | Terminal recording of the full deploy → start → query → signal → describe cycle | asciinema or GIF, ~45s |
| After step 7 | Terminal recording: kill -9 → restart → query shows same state | asciinema or GIF, ~20s |

## AWL language docs

| Location | Content | Format |
|---|---|---|
| AWL-2-SPEC.md top | The cargo_gates.awl example rendered with syntax highlighting (tree-sitter) | PNG or SVG |
| AWL-BIG-PICTURE.md | Diagram: the three-way split (YAML ← AWL → prose-for-LLM) showing what AWL replaces | SVG |

## Video walkthrough ideas

| Topic | Duration | Audience |
|---|---|---|
| "Zero to durable workflow" — install, write AWL, deploy, crash-proof demo | 5 min | Developers new to Aion |
| "AWL language tour" — types, pipes, fork/join, loops, outcomes | 10 min | Developers evaluating AWL |
| "Why durable execution matters" — the problem statement, with a live failure demo | 3 min | Technical decision-makers |
| "Aion vs Temporal" — architectural comparison, the BEAM advantage | 8 min | Infrastructure engineers evaluating alternatives |

## Tools

- **Terminal recordings**: [asciinema](https://asciinema.org) (renders as text, accessible) or [VHS](https://github.com/charmbracelet/vhs) (GIF/MP4 from a script)
- **Architecture diagrams**: hand-drawn SVG or [Excalidraw](https://excalidraw.com) for the sketch aesthetic
- **Screenshots**: macOS with a clean terminal (ghostty or iTerm2, dark theme matching the Ablative brand)
