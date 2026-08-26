interface SkeletonListProps {
  rows?: number;
}

export default function SkeletonList({ rows = 5 }: SkeletonListProps) {
  return (
    <div aria-label="Loading" className="divide-y divide-zinc-800" role="status">
      {Array.from({ length: rows }).map((_, index) => (
        <div key={index} className="animate-pulse px-4 py-3">
          <div className="h-3 rounded bg-zinc-800/70" style={{ width: `${74 - (index % 3) * 9}%` }} />
          <div className="mt-2 h-2.5 w-2/5 rounded bg-zinc-800/50" />
        </div>
      ))}
      <span className="sr-only">Loading items…</span>
    </div>
  );
}
