# Client state: where a piece of state lives

Every piece of client state has exactly one home. Pick by kind, not by reach:

- **Server data → react-query.** Anything fetched from aion-server (workflows,
  events, versions, namespaces) is a server cache: queries/mutations own the
  fetching, staleness, and invalidation. Never copy server data into zustand.
- **Selection & navigation → the URL.** Which entity is open, the selected
  namespace, deep-link params (`?seq=N`, `?workflow_type=X`). If the state
  should survive copy-pasting the link to a teammate, it belongs in the URL.
- **UI / view / draft state → zustand (this directory).** State the user built
  by hand that must survive unmount — half-filled forms, composer drafts,
  view-local toggles. Local to the client, never sent to or derived from the
  server.
- **Truly ephemeral → `useState`.** Focus, hover, an in-flight validation
  error: anything that *should* reset on unmount stays component-local.

## Draft stores

`createDraftStore({ name, empty })` makes a zustand store persisted under
`aion:draft:<name>`; `useDraft(store)` bridges it into controlled inputs
(seed at mount, write-through on change, `clearDraft()` on confirmed submit).
Hydration is defensive: a corrupt or wrong-shaped blob degrades to `empty`,
never a crash. Current stores: `start-workflow`, `event-search`, `chat`
(the kit's chat-input drafts).

## sessionStorage vs localStorage

- **sessionStorage (the default here):** scoped to the tab, gone when it
  closes. Right for drafts — two tabs are two independent work surfaces, and
  an abandoned half-typed form should not resurrect days later.
- **localStorage:** cross-session, cross-tab preferences the operator expects
  to keep — keybinding overrides (`aion:keybindings`), palette recents. If it
  reads as a *setting*, it is localStorage; if it reads as *work in progress*,
  it is sessionStorage.
