import { useEffect, useMemo, useRef, useState } from "react";
import {
  GETTING_AROUND_TOPIC_ID,
  buildHelpTopics,
  searchHelpTopics,
  type HelpTopic,
} from "../help/topics";

interface HelpDrawerProps {
  open: boolean;
  onClose: () => void;
  enabledSlices: readonly string[] | null;
  initialTopicId?: string;
}

export default function HelpDrawer({
  open,
  onClose,
  enabledSlices,
  initialTopicId = GETTING_AROUND_TOPIC_ID,
}: HelpDrawerProps) {
  const [query, setQuery] = useState("");
  const [topicId, setTopicId] = useState(initialTopicId);
  const inputRef = useRef<HTMLInputElement>(null);
  const restoreFocusRef = useRef<HTMLElement | null>(null);

  const topics = useMemo(
    () => buildHelpTopics({ enabledSlices }),
    [enabledSlices],
  );
  const results = useMemo(
    () => searchHelpTopics(topics, query),
    [topics, query],
  );
  const displayedTopics = query.trim() ? results : topics;

  const selectedTopic =
    displayedTopics.find((topic) => topic.id === topicId) ??
    displayedTopics[0] ??
    null;

  useEffect(() => {
    if (!open) return;
    restoreFocusRef.current = document.activeElement as HTMLElement | null;
    setQuery("");
    setTopicId(
      topics.some((topic) => topic.id === initialTopicId)
        ? initialTopicId
        : GETTING_AROUND_TOPIC_ID,
    );
    const id = setTimeout(() => inputRef.current?.focus(), 0);
    return () => clearTimeout(id);
  }, [open, initialTopicId, topics]);

  useEffect(() => {
    if (open) return;
    restoreFocusRef.current?.focus?.();
    restoreFocusRef.current = null;
  }, [open]);

  useEffect(() => {
    if (!open) return;
    const onKey = (e: KeyboardEvent) => {
      if (e.key === "Escape") {
        e.preventDefault();
        onClose();
      }
    };
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  }, [open, onClose]);

  if (!open) return null;

  return (
    <div
      className="fixed inset-0 z-50 bg-black/50"
      onMouseDown={(e) => {
        if (e.target === e.currentTarget) onClose();
      }}
    >
      <aside
        className="ml-auto flex h-full w-full max-w-3xl flex-col border-l border-zinc-800 bg-zinc-950 shadow-2xl"
        aria-label="Help"
      >
        <div className="border-b border-zinc-800 px-4 py-3">
          <div className="flex items-center justify-between gap-3">
            <h2 className="text-sm font-semibold text-zinc-100">Help</h2>
            <button
              onClick={onClose}
              className="rounded-md px-2 py-1 text-sm text-zinc-400 hover:bg-zinc-900 hover:text-zinc-100 focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-sky-500/70"
              aria-label="Close help"
            >
              Close
            </button>
          </div>
          <input
            ref={inputRef}
            type="text"
            value={query}
            onChange={(e) => setQuery(e.target.value)}
            placeholder="Search help..."
            className="mt-3 w-full rounded-md border border-zinc-800 bg-zinc-900 px-3 py-2 text-sm text-zinc-100 placeholder:text-zinc-500 focus:border-sky-600 focus:outline-none"
            aria-label="Search help"
          />
        </div>

        <div className="flex min-h-0 flex-1 flex-col md:flex-row">
          <TopicList
            topics={displayedTopics}
            selectedId={selectedTopic?.id ?? null}
            searchActive={query.trim().length > 0}
            onSelect={(topic) => setTopicId(topic.id)}
          />
          <div className="min-h-0 flex-1 overflow-y-auto px-5 py-4">
            {selectedTopic ? (
              <article className="max-w-2xl">
                <RenderedMarkdown body={selectedTopic.body} />
              </article>
            ) : (
              <p className="text-sm text-zinc-400">No help topics available.</p>
            )}
          </div>
        </div>
      </aside>
    </div>
  );
}

function TopicList({
  topics,
  selectedId,
  searchActive,
  onSelect,
}: {
  topics: readonly HelpTopic[];
  selectedId: string | null;
  searchActive: boolean;
  onSelect: (topic: HelpTopic) => void;
}) {
  if (topics.length === 0) {
    return (
      <div className="border-b border-zinc-800 p-4 text-sm text-zinc-400 md:w-64 md:flex-none md:border-b-0 md:border-r">
        No matching help topics.
      </div>
    );
  }

  const groups = searchActive
    ? [{ label: "Results", topics }]
    : groupedTopics(topics);

  return (
    <nav className="max-h-56 overflow-y-auto border-b border-zinc-800 p-3 md:max-h-none md:w-64 md:flex-none md:border-b-0 md:border-r">
      {groups.map((group) => (
        <div key={group.label} className="mb-3 last:mb-0">
          <div className="mb-1 px-2 text-xs font-semibold uppercase tracking-wide text-zinc-500">
            {group.label}
          </div>
          <div className="flex flex-col gap-0.5">
            {group.topics.map((topic) => (
              <button
                key={topic.id}
                onClick={() => onSelect(topic)}
                className={`rounded-md px-2 py-1.5 text-left text-sm transition focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-sky-500/70 ${
                  topic.id === selectedId
                    ? "bg-zinc-800 text-zinc-100"
                    : "text-zinc-400 hover:bg-zinc-900 hover:text-zinc-100"
                }`}
              >
                {topic.title}
              </button>
            ))}
          </div>
        </div>
      ))}
    </nav>
  );
}

function groupedTopics(topics: readonly HelpTopic[]) {
  const groups: { label: string; topics: HelpTopic[] }[] = [];
  for (const topic of topics) {
    let group = groups.find((candidate) => candidate.label === topic.group);
    if (!group) {
      group = { label: topic.group, topics: [] };
      groups.push(group);
    }
    group.topics.push(topic);
  }
  return groups;
}

function RenderedMarkdown({ body }: { body: string }) {
  const blocks = parseMarkdownBlocks(body);
  return (
    <div className="space-y-4">
      {blocks.map((block, index) => {
        if (block.kind === "heading") {
          const cls =
            block.level === 1
              ? "text-lg font-semibold text-zinc-100"
              : block.level === 2
              ? "text-sm font-semibold text-zinc-100"
              : "text-sm font-medium text-zinc-200";
          const Tag = `h${block.level}` as "h1" | "h2" | "h3";
          return (
            <Tag key={index} className={cls}>
              {block.text}
            </Tag>
          );
        }
        if (block.kind === "list") {
          return (
            <ul key={index} className="list-disc space-y-1 pl-5 text-sm text-zinc-300">
              {block.items.map((item, itemIndex) => (
                <li key={itemIndex}>{item}</li>
              ))}
            </ul>
          );
        }
        return (
          <p key={index} className="text-sm leading-6 text-zinc-300">
            {block.text}
          </p>
        );
      })}
    </div>
  );
}

type MarkdownBlock =
  | { kind: "heading"; level: 1 | 2 | 3; text: string }
  | { kind: "list"; items: string[] }
  | { kind: "paragraph"; text: string };

function parseMarkdownBlocks(body: string): MarkdownBlock[] {
  const blocks: MarkdownBlock[] = [];
  const lines = body.replace(/\r\n/g, "\n").split("\n");
  let paragraph: string[] = [];
  let list: string[] = [];

  const flushParagraph = () => {
    if (paragraph.length === 0) return;
    blocks.push({ kind: "paragraph", text: paragraph.join(" ") });
    paragraph = [];
  };
  const flushList = () => {
    if (list.length === 0) return;
    blocks.push({ kind: "list", items: list });
    list = [];
  };

  for (const line of lines) {
    const trimmed = line.trim();
    if (!trimmed) {
      flushParagraph();
      flushList();
      continue;
    }

    const heading = trimmed.match(/^(#{1,3})\s+(.+)$/);
    if (heading) {
      flushParagraph();
      flushList();
      blocks.push({
        kind: "heading",
        level: heading[1].length as 1 | 2 | 3,
        text: heading[2],
      });
      continue;
    }

    if (trimmed.startsWith("- ")) {
      flushParagraph();
      list.push(trimmed.slice(2));
      continue;
    }

    flushList();
    paragraph.push(trimmed);
  }

  flushParagraph();
  flushList();
  return blocks;
}
