interface MetricHelpProps {
  label: string;
  children: string;
  align?: "left" | "right";
}

export default function MetricHelp({
  label,
  children,
  align = "right",
}: MetricHelpProps) {
  const panelAlign = align === "left" ? "left-0" : "right-0";
  return (
    <details className="group/help relative inline-block align-middle">
      <summary
        className="inline-flex h-4 w-4 cursor-pointer list-none items-center justify-center rounded-full border border-zinc-700 text-[10px] font-semibold leading-none text-zinc-500 transition hover:border-zinc-500 hover:text-zinc-200 focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-sky-500/70 [&::-webkit-details-marker]:hidden"
        aria-label={label}
        title={label}
      >
        ?
      </summary>
      <div
        className={`absolute ${panelAlign} z-30 mt-1 w-64 rounded-md border border-zinc-700 bg-zinc-950 p-2 text-left text-xs font-normal normal-case leading-snug tracking-normal text-zinc-300 shadow-xl`}
      >
        {children}
      </div>
    </details>
  );
}
