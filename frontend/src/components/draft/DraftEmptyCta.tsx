import { Button } from "../ui";

interface DraftEmptyCtaProps {
  message: string;
  buttonLabel: string;
  busyLabel: string;
  producing: boolean;
  onProduce: () => void;
  historyCount: number;
}

export default function DraftEmptyCta({
  message,
  buttonLabel,
  busyLabel,
  producing,
  onProduce,
  historyCount,
}: DraftEmptyCtaProps) {
  return (
    <div className="flex items-center gap-3">
      <span className="text-xs text-zinc-400">{message}</span>
      <Button
        variant="primary"
        size="sm"
        busy={producing}
        onClick={onProduce}
      >
        {producing ? busyLabel : buttonLabel}
      </Button>
      {historyCount > 0 ? (
        <span className="text-xs text-zinc-400">
          {historyCount} rejected draft{historyCount > 1 ? "s" : ""} in history
        </span>
      ) : null}
    </div>
  );
}
