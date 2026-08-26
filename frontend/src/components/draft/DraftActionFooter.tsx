import { Button } from "../ui";

interface DraftActionFooterProps {
  /** Render null unless true. */
  visible: boolean;
  busy: boolean;
  dirty: boolean;
  approveLabel: string;
  approveDirtyLabel: string;
  approveTitle?: string;
  /** Additional disable condition (e.g. claim's packet gate). */
  approveDisabled?: boolean;
  onApprove: () => void;
  onReject: () => void;
  /** Optional exact-revision save without approving. */
  onSave?: () => void;
  saving?: boolean;
  saveDisabled?: boolean;
  saveTitle?: string;
  /** When provided and dirty, renders a ghost "Reset edits" button. */
  onResetEdits?: () => void;
}

export default function DraftActionFooter({
  visible,
  busy,
  dirty,
  approveLabel,
  approveDirtyLabel,
  approveTitle,
  approveDisabled,
  onApprove,
  onReject,
  onSave,
  saving = false,
  saveDisabled = false,
  saveTitle,
  onResetEdits,
}: DraftActionFooterProps) {
  if (!visible) return null;

  return (
    <div className="sticky bottom-0 z-[1] -mx-4 flex items-center gap-2 border-t border-zinc-800 bg-zinc-950/95 px-4 py-3 backdrop-blur">
      {dirty && onSave ? (
        <Button
          variant="secondary"
          size="sm"
          busy={saving}
          disabled={busy || saving || saveDisabled}
          onClick={onSave}
          title={saveTitle}
        >
          {saving ? "Saving…" : "Save draft"}
        </Button>
      ) : null}
      <Button
        variant="success"
        size="sm"
        busy={busy}
        disabled={busy || saving || (approveDisabled ?? false)}
        onClick={onApprove}
        title={approveTitle}
      >
        {busy ? "Approving…" : dirty ? approveDirtyLabel : approveLabel}
      </Button>
      <Button
        variant="danger"
        size="sm"
        busy={busy}
        disabled={busy || saving}
        onClick={onReject}
      >
        {busy ? "Rejecting…" : "Reject"}
      </Button>
      {dirty && onResetEdits ? (
        <Button
          variant="ghost"
          size="sm"
          disabled={busy || saving}
          onClick={onResetEdits}
        >
          Reset edits
        </Button>
      ) : null}
    </div>
  );
}
