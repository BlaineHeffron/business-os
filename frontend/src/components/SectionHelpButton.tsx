interface SectionHelpButtonProps {
  topicId: string | undefined;
  onOpenHelp: (topicId: string) => void;
  label?: string;
}

export default function SectionHelpButton({
  topicId,
  onOpenHelp,
  label = "Open help for this section",
}: SectionHelpButtonProps) {
  if (topicId === undefined) return null;
  return (
    <button
      type="button"
      onClick={() => onOpenHelp(topicId)}
      className="inline-flex h-6 w-6 items-center justify-center rounded-full border border-zinc-700 text-xs font-semibold text-zinc-400 hover:border-zinc-500 hover:bg-zinc-900 hover:text-zinc-100 focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-sky-500/70"
      aria-label={label}
      title={label}
    >
      ?
    </button>
  );
}
