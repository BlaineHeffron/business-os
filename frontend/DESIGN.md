# Frontend design conventions

Single source of truth for UI polish. Pattern doctrine (AGENTS.md): copy
category winners — Linear (queue/list+detail, keyboard), Stripe (dense tables,
plain-language empty states), Gmail/Superhuman (triage), Attio (CRM). Novelty
in UX is a cost.

## Primitives — use these, never inline-restyle

All shared UI lives in `src/components/ui/`. If a view needs a button, badge,
card, table, empty state, spinner, or confirm dialog, it imports from here.
Adding a one-off inline variant is a code-shape smell.

- `Button` — variants: `primary` (sky), `secondary` (zinc border), `danger`
  (red), `success` (emerald), `ghost`; sizes: `sm` (px-2.5 py-1 text-xs),
  `md` (px-3 py-1.5 text-sm). Disabled = `disabled:cursor-not-allowed
  disabled:opacity-50`, always. Focus = `focus-visible:ring-2
  focus-visible:ring-sky-500/70 focus-visible:outline-none`.
- `StatusBadge` — tone + label, never color alone. Tones map ONE way
  (`src/lib/status.ts`):
  - `ok` → emerald · `warning` → amber (never yellow/orange) · `critical` →
    red · `info` → sky · `ai` → violet (AI-originated things only) ·
    `neutral` → zinc · `progress` → sky + `animate-pulse` dot
  - Recipe: `bg-{c}-500/10 text-{c}-300 ring-1 ring-inset ring-{c}-500/30`
    with a 1.5px dot. Enumerate full class strings (Tailwind JIT).
- `Card`, `KpiCard` — the one card container (`rounded-lg border
  border-zinc-800 bg-zinc-900/40 p-4`); KpiCard replaces the copies in
  Inventory/Accounting.
- `Table` helpers — exported class constants: `theadCls` =
  `bg-zinc-900 text-xs uppercase tracking-wide text-zinc-400`, `cellCls` =
  `px-3 py-2`, numeric cells add `text-right tabular-nums`. Sticky header on
  scrollable tables.
- `EmptyState` — three flavors, Stripe-style: first-use ("No rules yet." +
  one sentence on where items come from + one CTA), filtered-empty ("No
  results match this filter." + clear-filter affordance), good-empty
  (celebrate: "Queue clear — nothing needs you."). One CTA max, never blame
  the user.
- `SkeletonRows` — table/list first-load placeholder (no layout shift).
  Spinners only inside buttons during in-flight actions ("Approving…").
- `ConfirmDialog` — replaces `window.confirm`. Restates the consequence,
  verb-labeled danger button ("Delete rule", never "OK"). Only for
  irreversible + consequential; approvals stay zero-friction.
- Draft panels: the produce→review→approve skeleton is shared in
  `src/components/draft/` — `useDraftPanel` (load, active selection,
  delivery + produce polling, approve-with-edits revision chaining,
  401/409/error handling), `useDraftEdit` (staged edit buffer),
  `DraftPanelShell`/`DraftEmptyCta`/`DraftStatusHeader`/`DraftActionFooter`/
  `OutboxStateLine`, `draftTone`. A new produce vertical's panel composes
  these and keeps only its field rendering, edit shape, gates, and copy
  local. Re-duplicating the skeleton is a review flag.
- Output workspaces use `src/components/output/OutputComposerShell`: blank
  Create Output and Queue/context editors share kind tabs, collapsible governed
  context, one dominant typed editor, Escape-to-close behavior, and a persistent
  footer. Output kinds remain typed owner-slice adapters; no generic JSON editor.

## Typography (4 sizes per screen, max)

- Page title: `text-lg font-semibold text-zinc-100`
- Section header: `text-sm font-semibold text-zinc-200`
- Table header: `text-xs uppercase tracking-wide text-zinc-400`
- Body / cells: `text-sm`; metadata/captions: `text-xs text-zinc-400`;
  footnotes: `text-xs text-zinc-500`
- Banned: `text-[10px]`, `text-[11px]` (→ `text-xs`), `text-zinc-600` for
  anything readable (→ `text-zinc-500`), secondary copy in `text-zinc-500`
  (→ `text-zinc-400`). WCAG AA on zinc-950 fails below zinc-400 at body size.
- Every numeric column: right-aligned + `tabular-nums`. Dates left, text left.

## Color

- Surfaces: page `bg-zinc-950`, card `bg-zinc-900/40`, elevated
  (popover/modal/hover) `bg-zinc-900`. Borders `border-zinc-800`
  (`zinc-700` on interactive). No pure black/white; elevation by lighter
  surface, not shadow.
- One accent: sky. Primary buttons, focus rings, active nav, links.
- Status hues only via `lib/status.ts` tones. Decorative hues (pipeline
  stages, category colors) stay, but statuses never improvise.

## Layout & navigation

- App shell: grouped left sidebar (Linear/Stripe), `w-56`, sections —
  **Work**: Inbox, Queue, Tasks · **Records**: Inventory, Accounting,
  Reports ·
  **Automation**: Rules, Categories · **System**: AI Usage, Users.
  Identity/connector chips + Settings live in the sidebar footer. Main
  content `px-6 py-6 max-w-screen-xl`.
- List+detail for triage surfaces (Inbox/Queue). Queue uses a compact navigator
  plus one dominant selected-item detail pane for sequential processing; tables
  paginate or "Show all", never infinite scroll; bulk/preview caps stated in
  the UI.

## Interaction

- Esc closes any popover, expanded panel, or inline editor. j/k stays in
  Queue (and Inbox list). Show the shortcut in `title` tooltips.
- Focus: every interactive element has a `focus-visible` ring (primitives
  carry it; bespoke elements must add it). Toggles need `aria-label`.
- Mutations: send idempotency key + expected_revision (existing rule);
  409 → amber "changed elsewhere — reloaded" banner (red = failures only).
- Buttons show in-flight state (disabled + "…" label) rather than going dead.
- Optimistic quick actions (Linear): toggle/status flips (Queue
  accept/dismiss/reopen + packet-kind chips, Tasks done/reopen, Rules
  enable/disable, Categories policy toggles) patch local state immediately —
  no row freeze, no awaited refetch. On success take `revision` from the
  MutationResponse and fire a SILENT non-awaited reconcile load where the
  server computes derived fields; on 409 amber banner + reload; on any other
  failure revert the snapshot by id + red banner. Double-fire guarded per row
  id. Structural changes (create/delete/form saves) keep the full awaited
  refetch — never go optimistic on those.

### Command palette & shortcuts

- **⌘K / Ctrl+K** opens the command palette (`src/components/CommandPalette.tsx`).
  In-house, no dependency. Commands are a typed array defined in `App.tsx`
  (`AppCommand` in `src/lib/commandTypes.ts`): one navigation entry per view + action
  entries (create output, refresh, log note, new rule, new category, inventory
  sync, accounting sync). Actions that require a specific view set the tab first,
  then dispatch via `src/lib/commands.ts`'s intent bus (`dispatchAppCommand`).
  Views register handlers with `useAppCommand`; pending commands are consumed
  on mount so navigate-then-act works without a router.
- **?** (no modifiers, not in an input) opens the shortcut-help dialog
  (`src/components/ShortcutHelp.tsx`): static table of all keyboard shortcuts.
- "/" focus-search is deliberately absent until a view grows a search input.

## States

- Loading: `SkeletonRows` on first load; subsequent polls update silently.
- Errors: red banner for load failures, amber for conflicts; inline near the
  failed thing; keep user input intact.
- Empty: `EmptyState`, copy per flavor above.
