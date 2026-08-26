import type { ReactNode } from "react";

interface EmptyStateProps {
  title: string;
  children?: ReactNode;
  action?: ReactNode;
  variant?: "default" | "celebrate";
}

export default function EmptyState({
  title,
  children,
  action,
  variant = "default",
}: EmptyStateProps) {
  if (variant === "celebrate") {
    return (
      <div className="flex flex-col items-center justify-center py-16 text-center">
        <div className="text-3xl">🎉</div>
        <p className="mt-3 text-sm font-semibold text-zinc-200">{title}</p>
        {children ? (
          <p className="mt-1 text-sm text-zinc-400">{children}</p>
        ) : null}
        {action ? <div className="mt-4">{action}</div> : null}
      </div>
    );
  }

  return (
    <div className="rounded-lg border border-dashed border-zinc-700 bg-zinc-900/20 px-6 py-12 text-center">
      <p className="text-sm font-semibold text-zinc-200">{title}</p>
      {children ? (
        <p className="mt-1 text-sm text-zinc-400">{children}</p>
      ) : null}
      {action ? <div className="mt-4">{action}</div> : null}
    </div>
  );
}
