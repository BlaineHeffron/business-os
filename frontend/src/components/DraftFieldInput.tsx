/**
 * One editable AI-filled draft field with its provenance alongside --
 * "AI-produced fields remain editable until accepted." A backing source
 * quote marks the value as quoted; its absence marks it as inferred.
 */
export default function DraftFieldInput({
  label,
  value,
  onChange,
  quote,
  multiline = false,
  rows = 3,
  placeholder,
  disabled,
  hint,
  maxLength,
  showProvenance = true,
}: {
  label: string;
  value: string;
  onChange: (value: string) => void;
  /** Source quote backing the AI's fill; empty = inferred. */
  quote: string;
  multiline?: boolean;
  rows?: number;
  placeholder?: string;
  disabled?: boolean;
  /** Format hint, e.g. "YYYY-MM-DD, blank = no deadline". */
  hint?: string;
  maxLength?: number;
  /** Blank/manual fields have no AI provenance and suppress its warning. */
  showProvenance?: boolean;
}) {
  const inputCls =
    "w-full rounded-md border border-zinc-700 bg-zinc-950 px-2 py-1 text-xs text-zinc-200 placeholder:text-zinc-500 focus:border-sky-600 focus:outline-none disabled:opacity-40";
  return (
    <div className="flex flex-col gap-0.5">
      <span className="flex items-baseline gap-2">
        <span className="text-xs font-medium text-zinc-400">{label}</span>
        {hint ? <span className="text-xs text-zinc-500">{hint}</span> : null}
      </span>
      {multiline ? (
        <textarea
          value={value}
          onChange={(e) => onChange(e.target.value)}
          rows={rows}
          className={`${inputCls} resize-y leading-relaxed`}
          placeholder={placeholder}
          disabled={disabled}
          maxLength={maxLength}
        />
      ) : (
        <input
          value={value}
          onChange={(e) => onChange(e.target.value)}
          className={inputCls}
          placeholder={placeholder}
          disabled={disabled}
          maxLength={maxLength}
        />
      )}
      {!showProvenance ? null : quote ? (
        <span
          className="text-xs italic text-zinc-500"
          title="Source quote from the email/note this value came from"
        >
          &ldquo;{quote}&rdquo;
        </span>
      ) : (
        <span
          className="text-xs italic text-amber-400/70"
          title="No source quote -- the AI inferred this value; double-check it"
        >
          inferred &mdash; no source quote
        </span>
      )}
    </div>
  );
}
