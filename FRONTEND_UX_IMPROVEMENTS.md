# Frontend UX Improvements

This document tracks user-facing workflow improvements in the frontend. It is the owning documentation for the capabilities below and is kept compatible with existing interfaces unless a change is explicitly versioned.

## Bulk action review and per-item outcomes (#267)

Multi-stream workflows now use a review-then-confirm flow with a result for every requested action.

### Preflight summary

Before anything executes, the bulk action dialog lists every requested action together with the affected stream, so users can review the full set before committing. The summary is rendered as a labelled list so assistive technology can enumerate each entry.

### Explicit confirmation

No bulk action runs until the user confirms. Cancelling (via the Cancel control or `Escape`) closes the dialog without executing anything and without submitting to the wallet.

### Accessibility

- The review surface is a modal dialog with `role="dialog"`, `aria-modal="true"`, `aria-labelledby` pointing at the dialog title, and `aria-describedby` pointing at the preflight summary.
- Keyboard support: focus moves into the dialog on open and is trapped while it is open; `Escape` cancels; `Enter`/`Space` activate the focused control.
- Progress and results are announced through a polite live region (`aria-live="polite"`), and failures are surfaced in an assertive region so screen-reader users hear them promptly.

### Per-item outcomes

After execution each requested action reports its own outcome:

- **Succeeded** — the action completed.
- **Failed** — the action did not complete; the reason is shown next to the item.
- **Cancelled** — the action was not attempted (for example, the user cancelled mid-run).

Partial failures are expected: successful items stay successful while failed items are reported individually, and the summary reflects the counts.

### Duplicate submission prevention

While a bulk action is in flight the confirm control is disabled and guarded against re-entry, so a second wallet submission cannot be triggered by repeated clicks or key presses. The guard is released only after the run settles.

### Testing

Focused automated coverage accompanies the change and includes:

- partial failure (some items succeed, some fail) producing correct per-item outcomes;
- cancellation before execution and mid-run;
- prevention of duplicate wallet submissions from repeated confirmation.

### Compatibility

Existing single-stream action interfaces are unchanged. The bulk review flow is additive and does not weaken wallet authorization, chain/network validation, financial accounting invariants, or secret-handling requirements.
