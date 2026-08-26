interface SkeletonRowsProps {
  rows?: number;
  cols?: number;
}

export default function SkeletonRows({ rows = 5, cols = 4 }: SkeletonRowsProps) {
  return (
    <>
      {Array.from({ length: rows }).map((_, r) => (
        <tr key={r} className="animate-pulse">
          {Array.from({ length: cols }).map((_, c) => (
            <td key={c} className="px-3 py-2">
              <div
                className="h-3 rounded bg-zinc-800/60"
                style={{ width: `${60 + ((r * cols + c) % 5) * 8}%` }}
              />
            </td>
          ))}
        </tr>
      ))}
    </>
  );
}
