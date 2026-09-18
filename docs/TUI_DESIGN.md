# TUI design: component-owned rendering

Status: **ruled by the maintainer (2026-09-18), staged migration** — this document is the
contract TUI work is reviewed against. It supersedes the 805 centralization *as the target*;
the migration keeps main green at every stage.

## The maintainer's constraints, verbatim

From the #824 closure ruling and the 2026-09-18 design session:

> The real cut is the widget tree (#805): disclosure lives on the renderer; thinking chrome
> is a text/status widget, not a leaf `•` plus a label.

> Part of the original point of how the layout was constructed is that *ANYWHERE* in the
> code, the code could decide a typed object of a particular kind of data could be added to
> the terminal, retained, and updated over time. Not text-only.

> The *original* decision was to have all those types implement a dynamic trait where the
> renderer didn't *care* about what type of Message was being sent.

> Each widget/component maintains a ViewModel, and a renderer for the "chrome", and a set of
> subWidgets that render into a region inside, if there is any chrome or subwidgets. Rather
> than "Props", just passing the outer ViewModel into the subwidgets makes sense so they can
> choose to render or not based on that. A click would find the correct hitbox, and toggle a
> `showProgram` flag in the outer ViewModel. … we need to be aware of being able to translate
> this into a Tauri DOM — perhaps a different method entirely on the internal widgets that
> just returns some HTML as a different rendering mode with the same ViewModels.

Consequences the maintainer explicitly accepted after review: identity maps are keyed by
*derived* identity (`RowId` = message id + append-only semantic path — no UUIDs are minted at
paint time), and the claiming layout stays central because layout is composition, not
component semantics.

## What was wrong with each pole

**Object-built presentation trees (pre-805).** Messages built `TranscriptRow` trees and
stored disclosure on themselves. Renderer type-agnostic: honored. But three ownership stories
for open/closed (message, accordion map, renderer), chrome invented by data objects (#821's
double bullet, #232's completions-in-input), and every consumer rebuilt hit regions from
wrapped lines.

**Centralized projection (805–812, current).** One `project_work_unit` converts domain
snapshots to widget props; the renderer owns all UI state in maps keyed by `RowId`. Layout,
hitboxes, and paint are correct and tested. But component rendering must be implemented in
the central files — adding a surface means touching `view_model.rs` — and central rules can
drop information the component owns: the say-turn rule ("assistant-prose output suppresses
Program source rows") deleted the source and left a dead disclosure arrow. Exhibit A.

**Ruling: component-owned rendering on the shared engine.** Ownership of *semantics* moves
back to the component; ownership of *mechanics* (layout, paint, event routing, identity)
stays in the engine. Neither pole is restored wholesale.

## Architecture

Four layers, dependencies pointing downward only:

```
render modes        terminal (SGR → shadow buffer)   dom (HTML for Tauri #808)
engine              claiming layout · paint · hit-rect routing · canonical commit
components          WorkUnitComponent, OperationComponent, … (VM + chrome + subwidgets)
domain              OutputManager messages (WorkUnit, OperationMessage, …)
```

- **Domain messages** keep exactly their current role: typed objects pushed anywhere in the
  code, retained by the OutputManager, updated through handles. Streaming appends and status
  transitions mutate these as today.
- **A component** is the presentation half of one message type. It maintains:
  1. **a ViewModel** — a retained, plain-data struct living on (or beside) the message,
     behind the same lock discipline the message already uses (`Arc<Mutex<_>>`). It holds
     presentation state *and* ephemeral UI state (`show_program`, expanded flags, scroll
     offsets for its own subwidgets). Because the ViewModel is retained, component state
     needs **no external map** — the earlier RowId-keyed state maps exist only for rows that
     have not migrated yet.
  2. **a chrome renderer** — draws the card furniture (status glyph, elapsed, disclosure
     arrow) from the ViewModel. The chrome renders a disclosure affordance **only when a
     subwidget can be shown**; a dead arrow is impossible by construction because the
     affordance and the content are decided in the same function.
  3. **subwidgets** — constructed from the outer ViewModel each frame
     (`Subwidget::from_vm(&vm)`), so they *choose to render or not* based on VM data; a
     subwidget with nothing to show claims zero rows and stays in the tree. Subwidgets
     render **into a region inside** the chrome: the engine's claiming pass offers the
     component a box, the chrome reserves its furniture rows, the remainder is offered to
     the subwidgets.
- **The engine** stays central and type-agnostic: the claiming pass (`widgets.rs` — Stack,
  Track, Rect, depth-first hit rects), the paint pass, event routing, and the canonical
  commit pipeline. The renderer never matches on message type: it asks the `Message` trait
  for the component and hands it rects and events.
- **Event routing**: a click resolves its hitbox to `(RowId, opaque Action)` — the action
  vocabulary is *defined by the component* (the router carries it opaquely) — and the engine
  calls `component.handle(action, &vm)`, which mutates the ViewModel (e.g. `vm.show_program
  ^= true`). The next frame re-renders from the mutated ViewModel. No observers: repaints
  remain pull-per-frame via the existing dirty/redraw predicate.

### The reference example: a say turn (the maintainer's sketch, formalized)

```
WorkUnitViewModel {
    status: Running | Completed,          // set once, exactly-once, by the completion path
    program:  ProgramSourceVm,            // the (say "…") text
    output:   Option<OutputVm>,           // set when the program executes successfully
    show_program: bool,                   // toggled by click on the disclosure hitbox
}
```

- The component constructs **two subwidgets** from the outer ViewModel: `ProgramSource` and
  `Output`. `ProgramSource` renders only when `show_program` (default hidden for say turns —
  #350's prose ruling); `Output` renders when the output part is set. During streaming both
  are live: the VM is updated as the turn streams and every frame repaints from it.
- When the program executes successfully the run is marked complete and the output part is
  set — the `status — running` residue (#820) cannot survive because the status field is
  owned and transitioned here.
- Chrome: status glyph + elapsed + the disclosure arrow; the arrow exists only while
  `ProgramSource` can be shown. Click → hitbox → `(RowId, ToggleProgram)` → `vm.show_program
  ^= true`.
- The canonical record is unchanged: the settled turn spools once into native scrollback
  with the raw program and output text; the live card is the reader.

## Render modes (Tauri)

Widgets are **semantic**: text carries style spans and roles, never baked SGR bytes.
`Component::build_subtree` returns the same widget tree today's engine paints; the terminal
mode lowers it to styled cells, and the DOM mode (#808) lowers the identical tree to HTML
(Stack → flex column, card chrome → fieldset/summary, spans → styled spans). Events come
back from the DOM as the same opaque `(RowId, Action)` pairs — DOM click handlers carry the
element's data attributes. One ViewModel, two lowering methods, no fork. This is why the
`format()`-style pre-rendered SGR strings must migrate to spans as surfaces are converted.

### The DOM boundary (2026-09-18 refinement)

The component ViewModel stays **typed**; composability comes from trait-object components
and the manifest tree, not from untyped state. What is generalized is the serialization
boundary only:

- **Manifest node** — the engine lowers the widget tree to a serializable
  `DynamicUiNode { element_type, id, props: HashMap<String, Value>, children }`
  (serde; JSON-valued props, not strings). This is the wire format for Tauri.
- **Component registry** — the JSX side maps `element_type` to components via a registry
  (Vite glob imports keep it decentralized). Maintainer-accepted centralization.
- **Backpropagation** — one generic IPC command `propagate_ui_action(widget_id, action_id,
  payload)` routes to the component by id; the command is a pass-through and knows nothing
  about components, mirroring the terminal `(RowId, Action)` routing. A Tauri mutation
  wakes the same redraw mechanism the terminal event loop already uses.
- **Override hook** — a component may override the DOM lowering for bespoke nodes
  (`render_dom` per node); the default lowers the shared subtree. Prefer the default:
  two render methods per component are a drift hazard.
- **Topology** — components live in the daemon process; the Tauri binary is a thin client
  receiving the manifest over the daemon's IPC (#808 attach-or-spawn). The widget engine is
  never compiled into the GUI process.

## Dependency direction (the one structural move)

Components need the widget vocabulary without touching `crossterm` or the shadow buffer.
The vocabulary (`Rect`, `Track`, `Axis`, `Widget`, spans/styles) moves to a small internal
module both `cli/tui` and `cli/components` (or any future surface author) can depend on.
This is what makes "anywhere in the code can add a typed surface" true again: implement a
ViewModel + component, attach it via the message's component accessor, done.

## Migration stages

| Stage | Scope | Bugs it kills | Gates |
|---|---|---|---|
| 1 | `WorkUnitComponent` for say turns: VM + chrome + ProgramSource/Output subwidgets + click routing; delete the `is_assistant_prose` suppression | dead ▼ affordance; program text unrecoverable by click; #820-class status residue re-checked at the PTY/daemon boundary | existing pinned invariants; new component tests at the claiming boundary |
| 2 | Remaining WorkUnit presentations (program source/output turns, thinking section #749 as a subwidget, agent activity) | per-type rendering out of `view_model.rs` | `canonical_commit_marks_only_after_success_and_follows_resize_clear` untouched |
| 3 | `OperationMessage`, `LiveToolMessage`, `ProgressMessage`, `StaticMessage` (text is its view) get components — completing "every typed message has a view"; the `work_unit_view`-era trait hooks retire | structure flattened to text | RowId stability across message kinds |
| 4 | DOM lowering + spans migration → Tauri #808 | — | one tree, two modes |

Non-goals (unchanged): no observer/signal graph, no retained widget-object graph with
shared mutability, no ratatui, components do not paint raw cells (they emit subtrees), the
canonical commit pipeline keeps its semantics and invariants.

## Choices made on the maintainer's behalf (flagged for review)

1. Subwidgets are **widget-subtree builders over the shared engine**, not cell painters —
   keeps one paint/hitbox path and makes DOM lowering trivial. Cell-level freedom (e.g. the
   mascot art) stays a pre-rendered text block.
2. Component VMs live **on the message object** behind the message's existing lock — no
   external state map for migrated rows, matching "each component maintains a ViewModel".
3. The vocabulary extraction (dependency direction) is a stage-1 prerequisite, not a
   separate epic.
4. `RowId` stays the routing/identity key (derived, no minting) and the canonical-commit
   key stays the message id.
